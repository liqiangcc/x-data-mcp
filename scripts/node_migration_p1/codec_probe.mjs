// NM-007 P1 codec validation: reproduce accepted P0 goldens
// (evidence/node-migration/p0/numeric/vectors.jsonl) in Node.js.
// Covers codec families only; arithmetic-migration families are NOT_RUN.
// Usage: node scripts/node_migration_p1/codec_probe.mjs <worktree-root>
// Emits one JSON line per vector: {family,id,status,detail,expected,actual}.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import canonicalize from "canonicalize";

const ROOT = process.argv[2] || ".";
const vectors = readFileSync(join(ROOT, "evidence/node-migration/p0/numeric/vectors.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);
const emit = (family, id, status, detail, expected, actual) =>
  console.log(JSON.stringify({ family, id, status, detail, expected, actual }));
const sha256hex = (b) => createHash("sha256").update(b).digest("hex");

// ---------- narrow RFC 8785 (JCS) subset ----------
// Covers the full JSON value domain: null/bool/number/string/array/object.
// Numbers: ECMAScript NumberToString == RFC 8785. Strings: JSON.stringify
// escapes match JCS (only " \ and control chars). Keys: sorted by UTF-16
// code units (Array.prototype.sort default). BigInt inputs are rejected —
// JCS has no integer type beyond number; adapters must serialize BigInt
// through string channels (see safeIntegers boundary).
function jcs(v) {
  if (v === null) return "null";
  const t = typeof v;
  if (t === "boolean") return v ? "true" : "false";
  if (t === "number") {
    if (!Number.isFinite(v)) throw new Error("JCS: non-finite number");
    return JSON.stringify(v);
  }
  if (t === "string") return JSON.stringify(v);
  if (t === "bigint") throw new Error("JCS: bigint not representable");
  if (Array.isArray(v)) return "[" + v.map(jcs).join(",") + "]";
  if (t === "object") {
    const keys = Object.keys(v).sort(); // UTF-16 code-unit order
    return "{" + keys.map(k => JSON.stringify(k) + ":" + jcs(v[k])).join(",") + "}";
  }
  throw new Error(`JCS: unsupported type ${t}`);
}

// ---------- serde_json::to_vec equivalent (Value domain) ----------
// serde_json Value objects are BTreeMap (sorted keys); output is compact
// with minimal escaping — identical bytes to the JCS subset above for
// this input domain. Difference note: serde_json escapes non-ASCII as
// UTF-8 too; both emit literal UTF-8.
const serdeJsonBytes = (v) => Buffer.from(jcs(v), "utf8");

// ---------- decimal helpers (mirroring rust_decimal lexical subset) ----------
// Rust Decimal::from_str accepts exponent forms; our lexical subset covers
// the vectors' domain: optional '-', digits, optional '.'+digits.
function parseDecimal(text) {
  if (typeof text !== "string" || !/^-?[0-9]+(\.[0-9]+)?$/.test(text)) {
    const e = new Error("invalid decimal"); e.code = "NOT_DECIMAL"; throw e;
  }
  const neg = text.startsWith("-");
  const [i, f = ""] = text.slice(neg ? 1 : 0).split(".");
  return { neg, int: BigInt(i), frac: f, scale: f.length };
}
// canonical_decimal — lexical mirror of src/compact_market.rs:849
function canonicalDecimal(text) {
  const neg = text.startsWith("-");
  const body = neg ? text.slice(1) : text;
  const dot = body.indexOf(".");
  const int = dot < 0 ? body : body.slice(0, dot);
  const frac = dot < 0 ? "" : body.slice(dot + 1);
  if (!int || !/^[0-9]+$/.test(int) || (int.length > 1 && int.startsWith("0"))) return false;
  if (dot >= 0 && (!frac || !/^[0-9]+$/.test(frac) || frac.endsWith("0"))) return false;
  if (neg && [...body].every(b => b === "0" || b === ".")) return false;
  return true;
}
// cents — price must be exactly 2dp and fit i64
function cents(text) {
  const d = parseDecimal(text);
  if (d.scale > 2) throw new Error("compact market data invalid: price is not exactly 2dp");
  const cents = (d.neg ? -1n : 1n) * (d.int * 100n + BigInt((d.frac + "00").slice(0, 2) || "0"));
  if (cents > 9223372036854775807n || cents < -9223372036854775808n)
    throw new Error("compact market data invalid: price exceeds i64 cents range");
  return cents;
}
function volume(text) {
  const d = parseDecimal(text);
  if (d.scale !== 0 && !/^0*$/.test(d.frac))
    throw new Error("compact market data invalid: volume is not integral");
  const v = (d.neg ? -1n : 1n) * d.int;
  if (v > 9223372036854775807n || v < -9223372036854775808n)
    throw new Error("compact market data invalid: volume exceeds i64 range");
  if (v < 0n) throw new Error("compact market data invalid: negative volume");
  return v;
}
function amount(text) {
  const d = parseDecimal(text);
  if (d.neg) throw new Error("compact market data invalid: negative amount");
  if (!canonicalDecimal(text))
    throw new Error("compact market data invalid: stored amount is not canonical");
  return text;
}

// ---------- f32 helpers ----------
const dv = new DataView(new ArrayBuffer(4));
const f32FromBits = (bits) => { dv.setUint32(0, bits); return dv.getFloat32(0); };
// exact dyadic -> BigInt rational num * 2^exp (v must be finite, >=0)
function f64Rational(v) {
  const b = new DataView(new ArrayBuffer(8));
  b.setFloat64(0, v);
  const hi = BigInt(b.getUint32(0)), lo = BigInt(b.getUint32(4));
  const E = Number((hi >> 20n) & 0x7ffn);
  const mant = ((hi & 0xfffffn) << 32n) | lo;
  if (E === 0) return { num: mant, exp: -1074 };      // subnormal
  return { num: mant | (1n << 52n), exp: E - 1075 }; // normal
}
// exact 2dp HALF_UP (away from zero) — mirrors gbbq::normalize_factor
function normalizeFactor(v) {
  if (!Number.isFinite(v)) return null;
  const neg = v < 0;
  const { num, exp } = f64Rational(Math.abs(v));
  // cents = round_half_up(num * 2^exp * 100)
  let cents;
  const scaled = num * 100n;
  if (exp >= 0) cents = scaled << BigInt(exp);
  else {
    const shift = BigInt(-exp);
    const half = 1n << (shift - 1n);
    cents = (scaled + half) >> shift; // exact half-up since scaled/2^shift is rational
  }
  const s = `${neg && cents !== 0n ? "-" : ""}${cents / 100n}.${String(cents % 100n).padStart(2, "0")}`;
  return s;
}

// ---------- date helpers ----------
function epochDay(ymd) {
  const y = Math.floor(ymd / 10000), m = Math.floor(ymd / 100) % 100, d = ymd % 100;
  const dt = new Date(Date.UTC(y, m - 1, d));
  if (dt.getUTCFullYear() !== y || dt.getUTCMonth() !== m - 1 || dt.getUTCDate() !== d)
    throw new Error(`invalid calendar date ${ymd}`);
  return dt.getTime() / 86400000;
}
const dayOfEpoch = (days) => new Date(days * 86400000).toISOString().slice(0, 10);

// ---------- frame helpers ----------
const pushBytes = (parts, b) => {
  const len = Buffer.alloc(4); len.writeUInt32BE(b.length);
  parts.push(len, Buffer.from(b));
};
const pushText = (parts, s) => pushBytes(parts, Buffer.from(s, "utf8"));
const pushI64 = (parts, v) => { const b = Buffer.alloc(8); b.writeBigInt64BE(v); parts.push(b); };

// ---------- probe driver ----------
const byFamily = {};
for (const r of vectors) (byFamily[r.family] ??= []).push(r);
const get = (f, id) => byFamily[f].find(r => r.id === id);

function check(v, fn) {
  // fn returns {ok: value} or throws {message}. Compare vs golden observed.
  try {
    const actual = fn();
    const ok = v.observed.status === "ok" &&
      jcs(actual) === jcs(v.observed.value);
    emit(v.family, v.id, ok ? "PASS" : "FAIL",
      ok ? null : "value mismatch", v.observed.value, actual);
  } catch (e) {
    const wantErr = v.observed.status === "error";
    const match = wantErr && v.observed.error === e.message;
    emit(v.family, v.id, match ? "PASS" : (wantErr ? "FAIL" : "FAIL"),
      match ? null : `error expected=${JSON.stringify(v.observed.error)} got=${e.message}`,
      v.observed.error, e.message);
  }
}

// ---- day_parse (4) ----
function dayParse(hex) {
  const bytes = Buffer.from(hex, "hex");
  if (bytes.length % 32 !== 0)
    throw new Error(`day file byte count ${bytes.length} is not divisible by 32`);
  const recs = []; let prev = -1;
  for (let i = 0; i < bytes.length; i += 32) {
    const w = [];
    for (let j = 0; j < 8; j++) w.push(bytes.readUInt32LE(i + j * 4));
    const ymd = w[0];
    try { var date = epochDay(ymd); } catch {
      throw new Error(`invalid calendar date ${ymd} at record ${i / 32}`);
    }
    if (date <= prev) throw new Error(`non-increasing date at record ${i / 32}`);
    prev = date;
    recs.push({
      date: dayOfEpoch(date), ohlc_raw: w.slice(1, 5),
      amount_float32_bits: w[5], volume_raw: w[6], r7_raw: w[7],
    });
  }
  return recs;
}
for (const v of byFamily.day_parse || []) {
  if (v.id === "truncated_31_bytes") {
    check(v, () => dayParse("00".repeat(31)));
  } else if (v.id === "invalid_month_13") {
    const b = Buffer.alloc(32); b.writeUInt32LE(20241301, 0);
    check(v, () => dayParse(b.toString("hex")));
  } else if (v.id === "non_increasing_date") {
    const b = Buffer.alloc(64);
    b.writeUInt32LE(20240103, 0); b.writeUInt32LE(20240102, 32);
    check(v, () => dayParse(b.toString("hex")));
  } else {
    check(v, () => dayParse(v.input.hex));
  }
}

// ---- amount_f32_projection (6) ----
for (const v of byFamily.amount_f32_projection || []) {
  check(v, () => {
    const f = f32FromBits(parseInt(v.input.f32_bits, 16));
    if (!Number.isFinite(f)) return null;
    const text = String(f); // shortest f64 round-trip of the exact f32 value
    if (!canonicalDecimal(text)) throw new Error(`non-canonical projection ${text}`);
    return text;
  });
}

// ---- gbbq_factor_normalize (6) ----
for (const v of byFamily.gbbq_factor_normalize || []) {
  check(v, () => v.input.f32 === null ? null : normalizeFactor(v.input.f32));
}

// ---- metric_ratio_change (6): exact rational (end-anchor)/anchor, HALF_EVEN 12dp ----
for (const v of byFamily.metric_ratio_change || []) {
  check(v, () => {
    const e = parseDecimal(v.input.end), a = parseDecimal(v.input.anchor);
    const toScaled = (d, ds) =>
      (d.neg ? -1n : 1n) * (d.int * 10n ** ds + BigInt(d.frac.padEnd(Number(ds), "0") || "0"));
    const ds = BigInt(Math.max(e.scale, a.scale));
    const E = toScaled(e, ds), A = toScaled(a, ds);
    if (A <= 0n || E <= 0n) throw new Error("INVALID_MARKET_COMMAND: positive ratio inputs required");
    const num = (E - A) * 10n ** 12n, den = A;
    let q = num / den, r = num % den;
    if (r < 0n) { q -= 1n; r += den; } // floor division (BigInt truncates toward zero)
    const twice = r * 2n;
    const rounded = q + (twice > den ? 1n : twice === den ? (q % 2n !== 0n ? 1n : 0n) : 0n);
    const neg = rounded < 0n; const abs = rounded < 0n ? -rounded : rounded;
    return `${neg ? "-" : ""}${abs / 10n ** 12n}.${String(abs % 10n ** 12n).padStart(12, "0")}`;
  });
}

// ---- digest_legacy_published (3) ----
for (const v of byFamily.digest_legacy_published || []) {
  check(v, () => {
    const raw = v.id === "fixture_record0"
      ? JSON.parse(readFileSync(join(ROOT, "tests/fixtures/issue79/legacy-records.json"), "utf8"))[0]
      : v.input.input;
    if (raw === null || typeof raw !== "object" || Array.isArray(raw))
      throw new Error("MARKET_INTEGRITY: record must be object");
    const copy = { ...raw }; delete copy.integrity_sha256;
    return sha256hex(serdeJsonBytes(copy));
  });
}

// ---- digest_canonical_evidence (4) ----
const EVIDENCE_FIELDS = ["price_currency","price_unit","volume_unit","amount_currency","amount_unit"];
function checkEvidenceSemantics(kind, value) {
  if (value === null || typeof value !== "object" || Array.isArray(value))
    throw new Error("compact market data invalid: evidence is not a JSON object");
  (function rejectBarArrays(x) {
    if (x && typeof x === "object") {
      for (const [k, item] of Object.entries(x)) {
        if ((k === "bars" || k === "records") && Array.isArray(item))
          throw new Error("compact market data invalid: evidence embeds a bars/records array");
        if (Array.isArray(item)) item.forEach(rejectBarArrays);
        else rejectBarArrays(item);
      }
    }
  })(value);
  if (kind === "version") {
    const hasShape = typeof value.source === "string" && typeof value.coverage_reason === "string"
      && value.unit_currency && typeof value.unit_currency === "object"
      && EVIDENCE_FIELDS.every(f => {
        const e = value.unit_currency[f];
        return e && typeof e === "object" && typeof e.status === "string" && typeof e.reason === "string"
          && ("value" in e ? (e.value === null || typeof e.value === "string") : true);
      })
      && value.adjustment_evidence && typeof value.adjustment_evidence === "object";
    if (!hasShape) throw new Error("compact market data invalid: version evidence shape mismatch");
  }
}
function evidenceDigest(kind, canonicalJson) {
  const parts = [Buffer.from("x-data-evidence-v1\0")];
  pushText(parts, kind);
  pushBytes(parts, Buffer.from(canonicalJson, "utf8"));
  return createHash("sha256").update(Buffer.concat(parts)).digest();
}
for (const v of byFamily.digest_canonical_evidence || []) {
  check(v, () => {
    let value;
    if (v.id === "version_kind_valid") {
      // evidence JSON reconstructed from the golden's canonical_json
      value = JSON.parse(v.observed.value.canonical_json);
    } else if (v.id === "embedded_records_reject") {
      value = { ...JSON.parse(get("digest_canonical_evidence", "version_kind_valid").observed.value.canonical_json),
                nested: { records: [1, 2] } };
    } else {
      value = v.input.value;
    }
    checkEvidenceSemantics("version", value);
    const cj = jcs(value);
    return { canonical_json: cj, digest: [...evidenceDigest("version", cj)] };
  });
}

// ---- digest_rfc8785 (1 + not_run marker) ----
for (const v of byFamily.digest_rfc8785 || []) {
  if (v.id === "reject_boundary") { emit(v.family, v.id, "NOT_RUN", "justified_not_run in golden; no in-domain rejection exists", null, null); continue; }
  check(v, () => {
    const mine = jcs(v.input.input);
    const lib = canonicalize(v.input.input);
    if (mine !== lib) throw new Error(`subset != canonicalize pkg: ${mine} vs ${lib}`);
    return mine;
  });
}

// ---- compact_bar (6) ----
const PERIOD_CODE = { day: 1, year: 2 };
function compactFromBar(bar, period) {
  if (period === "year" && bar.r7_raw != null)
    throw new Error("compact market data invalid: yearly bar must not carry r7_raw");
  const cb = {
    trade_day: epochDay(Number(bar.date.replaceAll("-", ""))),
    open_cents: cents(bar.open), high_cents: cents(bar.high),
    low_cents: cents(bar.low), close_cents: cents(bar.close),
    volume: bar.volume == null ? null : volume(bar.volume),
    amount: bar.amount == null ? null : amount(bar.amount),
    r7_raw: bar.r7_raw ?? null,
  };
  compactCheck(cb, period);
  return cb;
}
function compactCheck(cb, period) {
  if (!(cb.high_cents >= cb.open_cents && cb.high_cents >= cb.close_cents
        && cb.high_cents >= cb.low_cents && cb.low_cents <= cb.open_cents && cb.low_cents <= cb.close_cents))
    throw new Error("compact market data invalid: OHLC ordering violated");
  if (cb.volume != null && cb.volume < 0n)
    throw new Error("compact market data invalid: negative volume");
  if (cb.amount != null) {
    let parsed;
    try { parsed = parseDecimal(cb.amount); }
    catch { throw new Error("compact market data invalid: stored amount is not a decimal"); }
    if (parsed.neg) throw new Error("compact market data invalid: negative amount");
    if (!canonicalDecimal(cb.amount)) throw new Error("compact market data invalid: stored amount is not canonical");
  }
  if (period === "year" && cb.r7_raw != null)
    throw new Error("compact market data invalid: yearly bar must not carry r7_raw");
}
const BARSPEC = {
  from_bar_valid:   { date: "2024-01-02", open: "15.00", high: "15.20", low: "14.95", close: "15.10", volume: "1234567", amount: "150225000" },
  price_not_2dp_reject: { date: "2024-01-02", open: "15", high: "15.2", low: "14.9", close: "15.105", volume: null, amount: null },
  r7_on_yearly_reject:  { date: "2024-12-31", open: "10", high: "12", low: "9", close: "11", volume: null, amount: null, r7_raw: 0x10000 },
};
for (const v of byFamily.compact_bar || []) {
  if (v.id.startsWith("amount_")) {
    // to_bar(check) path on a stored compact bar
    check(v, () => {
      const cb = { trade_day: 19723, open_cents: 1500n, high_cents: 1520n, low_cents: 1495n,
                   close_cents: 1510n, volume: null, amount: v.input.amount, r7_raw: null };
      compactCheck(cb, "day");
      return null;
    });
  } else {
    check(v, () => {
      const b = BARSPEC[v.id];
      const cb = compactFromBar(b, v.input.period === "year" ? "year" : "day");
      return {
        trade_day: cb.trade_day, open_cents: Number(cb.open_cents), high_cents: Number(cb.high_cents),
        low_cents: Number(cb.low_cents), close_cents: Number(cb.close_cents),
        volume: cb.volume == null ? null : Number(cb.volume), amount: cb.amount, r7_raw: cb.r7_raw,
      };
    });
  }
}

// ---- segment_digest (3) ----
function segmentDigest(entity, period, bars) {
  const canonical = entity.length === 9 && (entity.startsWith("sh.") || entity.startsWith("sz."))
    && [...entity.slice(3)].every(c => c >= "0" && c <= "9");
  if (!canonical) throw new Error(`compact market data invalid: non-canonical entity ${entity}`);
  if (bars.length === 0 || bars.length > 10000)
    throw new Error("compact market data invalid: segment row_count out of bounds");
  for (let i = 1; i < bars.length; i++)
    if (bars[i].trade_day <= bars[i - 1].trade_day)
      throw new Error("compact market data invalid: segment bars not strictly increasing");
  const parts = [Buffer.from("x-data-segment-v1\0")];
  pushText(parts, entity);
  parts.push(Buffer.from([PERIOD_CODE[period]]));
  const rc = Buffer.alloc(4); rc.writeUInt32BE(bars.length); parts.push(rc);
  for (const b of bars) {
    compactCheck(b, period);
    parts.push(Buffer.from("x-data-bar-v1\0"));
    pushI64(parts, BigInt(b.trade_day));
    for (const c of [b.open_cents, b.high_cents, b.low_cents, b.close_cents]) pushI64(parts, c);
    if (b.volume == null) parts.push(Buffer.from([0]));
    else { parts.push(Buffer.from([1])); pushI64(parts, b.volume); }
    if (b.amount == null) parts.push(Buffer.from([0]));
    else { parts.push(Buffer.from([1])); pushText(parts, b.amount); }
    if (b.r7_raw == null) parts.push(Buffer.from([0]));
    else { parts.push(Buffer.from([1])); pushI64(parts, BigInt(b.r7_raw)); }
  }
  return sha256hex(Buffer.concat(parts));
}
for (const v of byFamily.segment_digest || []) {
  check(v, () => {
    if (v.id === "empty_reject") return segmentDigest("sh.600006", "day", []);
    if (v.id === "bad_entity_reject") return segmentDigest(v.input.entity, "day",
      [compactFromBar(BARSPEC.from_bar_valid, "day")]);
    const bars = [
      compactFromBar(BARSPEC.from_bar_valid, "day"),
      compactFromBar({ date: "2024-01-03", open: "15.12", high: "15.30", low: "15.08",
                       close: "15.25", volume: "987654", amount: "99750000" }, "day"),
    ];
    return segmentDigest(v.input.entity, "day", bars);
  });
}

// ---- NOT_RUN families (algorithm migration, out of codec slice) ----
for (const f of ["qfq_affine", "qfq_forward_adjust", "yearly_aggregate"]) {
  for (const v of byFamily[f] || [])
    emit(f, v.id, "NOT_RUN", "domain arithmetic (decimal affine/exact-sum) — algorithm migration, not codec validation", null, null);
}
