// NM-082 — serde-verbatim decoders + canonical encoders for the
// market_update decision layer. Reuses the proven JSON/Value primitives
// from mcp/serde_json.mjs (F64 markers, ryu float emission, BTreeMap key
// order); the decode machinery mirrors data_supply/wire.mjs conventions
// (kept local — wire.mjs internals are not exported and that file is
// another lane's surface). Error texts match serde_json::from_value
// verbatim (no line/column), verified against the Rust oracle.
//
// Rust grammar notes (oracle-verified):
//   NaiveDate::from_str — trims whitespace; optional sign; unsigned year
//     is 1-4 digits (5+ => "input contains invalid characters"), signed
//     year unlimited digits but bounded to -262143..=262142
//     ("input is out of range"); month/day are 1-2 digits; "-0000" is
//     year 0. Display: y<0 "-%04d"-ish (no '+' ever), 0..9999 "%04d",
//     >9999 "+%d".
//   DateTime<Utc> — RFC3339, accepts t/T/' ' and z/Z; emitted
//     AutoSi ("none/3/6/9 frac digits) with "Z" (chrono serde use_z).
//   rust_decimal::Decimal — string: [+-]?(d+(.d*)?|.d+)([eE][+-]?d+)?,
//     scale preserved ("1.230" -> "1.230"), ".5"->"0.5", "5."->"5",
//     "-0.00"->"0.00"; >28 frac digits round half-up; mantissa cap
//     79228162514264337593543950335; number input goes through f64
//     shortest-repr (ryu, e.g. 1e-7 -> "0.0000001").
import { F64, serF64, serValue, parseJsonBytes } from "../mcp/serde_json.mjs";

export { F64, serF64, serValue, parseJsonBytes };

export class Decode extends Error {}
const fail = (msg) => { throw new Decode(msg); };
export const decodeFail = fail;

export const isObj = (v) =>
  v !== null && typeof v === "object" && !Array.isArray(v) && !(v instanceof F64);
export const isArr = Array.isArray;

// serde de::Unexpected Display for a parsed Value (verbatim wire.mjs).
const rustStr = (s) =>
  '"' + s.replace(/[\\"\n\r\t]|[\x00-\x1f\x7f]/g, (c) => {
    if (c === '"') return '\\"';
    if (c === "\\") return "\\\\";
    if (c === "\n") return "\\n";
    if (c === "\r") return "\\r";
    if (c === "\t") return "\\t";
    return `\\u{${c.codePointAt(0).toString(16)}}`;
  }) + '"';
export const desc = (v) =>
  v === null || v === undefined ? "null" :
  typeof v === "boolean" ? `boolean \`${v}\`` :
  v instanceof F64 ? `floating point \`${serF64(v.v)}\`` :
  typeof v === "number" || typeof v === "bigint" ? `integer \`${v}\`` :
  typeof v === "string" ? `string ${rustStr(v)}` :
  isArr(v) ? "sequence" : "map";
export const invalidType = (v, expected) =>
  fail(`invalid type: ${desc(v)}, expected ${expected}`);
export const expectedFields = (xs) =>
  xs.length === 1 ? `\`${xs[0]}\`` :
  xs.length === 2 ? `\`${xs[0]}\` or \`${xs[1]}\`` :
  `one of ${xs.map((x) => `\`${x}\``).join(", ")}`;

// --------------------------------------------------------------- scalars
export const tString = (v) => (typeof v === "string" ? v : invalidType(v, "a string"));
export const tBool = (v) => (typeof v === "boolean" ? v : invalidType(v, "a boolean"));
const intKind = (v) =>
  typeof v === "bigint" ? v : typeof v === "number" && Number.isInteger(v) ? BigInt(v) : null;
const I64_MIN = -(2n ** 63n);
const U64_MAX = (1n << 64n) - 1n;
export const tInt = (name, lo, hi) => (v) => {
  const i = intKind(v);
  if (i !== null) {
    if (i >= lo && i <= hi) return Number(i);
    if (i >= I64_MIN) fail(`invalid value: integer \`${i}\`, expected ${name}`);
    invalidType(v, name);
  }
  invalidType(v, name);
};
export const tU32 = tInt("u32", 0n, (1n << 32n) - 1n);
export const tU64 = tInt("u64", 0n, U64_MAX);
export const tI32 = tInt("i32", -(2n ** 31n), 2n ** 31n - 1n);

export const tOpt = (inner) => (v) => (v === null || v === undefined ? null : inner(v));
export const tVec = (inner) => (v) => {
  if (!isArr(v)) invalidType(v, "a sequence");
  return v.map(inner);
};
// Any JSON value (Option<Value>/Value fields) — passthrough.
export const tValue = (v) => v;

// fields: [{name, dec, opt?, default?}] — opt = Option<T> (map-missing ->
// null but still consumes a seq slot); default = #[serde(default)]
// (defaults in BOTH forms). deny = #[serde(deny_unknown_fields)].
export const tStruct = (name, fields, deny = false) => (v) => {
  if (isObj(v)) {
    const out = {};
    let firstUnknown = null;
    for (const k of Object.keys(v)) {
      const f = fields.find((x) => x.name === k);
      if (!f) {
        // serde_derive deny_unknown_fields defers the unknown-field
        // error until after the map is consumed: a mid-loop value error
        // on a later known key still wins (oracle-verified).
        if (!deny) continue; // serde default: unknown fields ignored
        if (firstUnknown === null) firstUnknown = k;
        continue;
      }
      out[f.name] = f.dec(v[k]);
    }
    if (deny && firstUnknown !== null) {
      fail(fields.length === 0
        ? `unknown field \`${firstUnknown}\`, there are no fields`
        : `unknown field \`${firstUnknown}\`, expected ${expectedFields(fields.map((x) => x.name))}`);
    }
    for (const f of fields) {
      if (f.name in out) continue;
      if (f.opt) out[f.name] = null;
      else if (f.default !== undefined) out[f.name] = f.default();
      else fail(`missing field \`${f.name}\``);
    }
    return out;
  }
  if (isArr(v)) {
    const out = {};
    let i = 0;
    for (const f of fields) {
      if (i < v.length) { out[f.name] = f.dec(v[i]); i++; continue; }
      if (f.default !== undefined) { out[f.name] = f.default(); i++; continue; }
      fail(`invalid length ${i}, expected struct ${name} with ${fields.length} elements`);
    }
    if (v.length > i)
      fail(`invalid length ${v.length}, expected fewer elements in array`);
    return out;
  }
  invalidType(v, `struct ${name}`);
};

// Externally-tagged unit-only enum: accepts "v" and {"v":null}.
export const tEnumUnit = (name, variants) => (v) => {
  if (typeof v === "string") {
    if (variants.includes(v)) return v;
    fail(`unknown variant \`${v}\`, expected ${expectedFields(variants)}`);
  }
  if (isObj(v) && Object.keys(v).length === 1) {
    const k = Object.keys(v)[0];
    if (variants.includes(k)) {
      if (v[k] === null) return k;
      invalidType(v[k], "unit");
    }
    fail(`unknown variant \`${k}\`, expected ${expectedFields(variants)}`);
  }
  invalidType(v, "string or map");
};

// ------------------------------------------------------- chrono NaiveDate
// NaiveDate::from_str ("oracle-verified grammar above). Returns
// {y,mo,d,s} where s is the canonical Display string; ordering is on
// (y,mo,d) numerics — the string is NOT a total order (signed years).
const DIM = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
export const dateCanon = (y, mo, d) =>
  (y < 0 ? `-${String(-y).padStart(4, "0")}` : y > 9999 ? `+${y}` : String(y).padStart(4, "0")) +
  `-${String(mo).padStart(2, "0")}-${String(d).padStart(2, "0")}`;
export const dateCmp = (a, b) =>
  a.y - b.y || a.mo - b.mo || a.d - b.d;
export const dateEq = (a, b) => dateCmp(a, b) === 0;

export const tDate = (v) => {
  if (typeof v !== "string") invalidType(v, "a formatted date string");
  const s = v.trim();
  let i = 0;
  if (s === "") fail("premature end of input");
  let signed = false, neg = false;
  if (s[i] === "+" || s[i] === "-") { signed = true; neg = s[i] === "-"; i++; }
  const yStart = i;
  while (i < s.length && s[i] >= "0" && s[i] <= "9") i++;
  if (i === yStart) fail(i >= s.length ? "premature end of input" : "input contains invalid characters");
  if (!signed && i - yStart > 4) fail("input contains invalid characters");
  const digits = s.slice(yStart, i);
  const yAbs = BigInt(digits);
  const y = (neg ? -1n : 1n) * yAbs;
  if (y > 262142n || y < -262143n) fail("input is out of range");
  if (i >= s.length) fail("premature end of input");
  if (s[i] !== "-") fail("input contains invalid characters");
  i++;
  const mStart = i;
  while (i < s.length && s[i] >= "0" && s[i] <= "9" && i - mStart < 2) i++;
  if (i === mStart) fail(i >= s.length ? "premature end of input" : "input contains invalid characters");
  const mm = +s.slice(mStart, i);
  if (mm < 1 || mm > 12) fail("input is out of range");
  if (i >= s.length) fail("premature end of input");
  if (s[i] !== "-") fail("input contains invalid characters");
  i++;
  const dStart = i;
  while (i < s.length && s[i] >= "0" && s[i] <= "9" && i - dStart < 2) i++;
  if (i === dStart) fail(i >= s.length ? "premature end of input" : "input contains invalid characters");
  const dd = +s.slice(dStart, i);
  const leap = (y % 4n === 0n && y % 100n !== 0n) || y % 400n === 0n;
  const yy = Number(y);
  const leapY = yy % 4 === 0 && (yy % 100 !== 0 || yy % 400 === 0);
  if (dd < 1 || dd > DIM[mm - 1] + (mm === 2 && (leap || leapY) ? 1 : 0)) fail("input is out of range");
  if (i < s.length) fail("trailing input");
  return { y: yy, mo: mm, d: dd, s: dateCanon(yy, mm, dd) };
};

// Howard Hinnant days_from_civil / civil_from_days — proleptic Gregorian.
export const daysFromCivil = (y, m, d) => {
  y -= m <= 2;
  const era = Math.floor((y >= 0 ? y : y - 399) / 400);
  const yoe = y - era * 400;
  const doy = Math.floor((153 * (m > 2 ? m - 3 : m + 9) + 2) / 5) + d - 1;
  return era * 146097 + yoe * 365 + Math.floor(yoe / 4) - Math.floor(yoe / 100) + doy - 719468;
};
const civilFromDays = (z) => {
  z += 719468;
  const era = Math.floor((z >= 0 ? z : z - 146096) / 146097);
  const doe = z - era * 146097;
  const yoe = Math.floor((doe - Math.floor(doe / 1460) + Math.floor(doe / 36524) - Math.floor(doe / 146096)) / 365);
  const y = yoe + era * 400;
  const doy = doe - (365 * yoe + Math.floor(yoe / 4) - Math.floor(yoe / 100));
  const mp = Math.floor((5 * doy + 2) / 153);
  const d = doy - Math.floor((153 * mp + 2) / 5) + 1;
  const m = mp + (mp < 10 ? 3 : -9);
  return [m <= 2 ? y + 1 : y, m, d];
};

// DateTime<Utc> serde (visit_str -> DateTime::from_str over RFC3339).
// Returns the canonical serde emission: "YYYY-MM-DDTHH:MM:SS(.SSS|.SSSSSS|.SSSSSSSSS)Z".
const TS_FULL = /^([+-]?\d+)-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?([Zz]|([+-])(\d{2}):(\d{2}))$/;
export const tDateTime = (v) => {
  if (typeof v !== "string") invalidType(v, "an RFC 3339 formatted date and time string");
  const s = v.trim();
  const m = TS_FULL.exec(s);
  if (!m) {
    if (/^\s*[+-]?\d+-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}(\.\d+)?([Zz]|[+-]\d{2}:\d{2})\s*$/.test(s) ||
        /^[+-]?\d+-\d{2}-\d{2}([Tt ]\d{2}(:\d{2}(:\d{2}(\.\d*)?)?)?)?$/.test(s) ||
        /^\d{0,4}(-\d{0,2}){0,2}$/.test(s))
      fail(s.trim() === "" || s.length < 20 ? "premature end of input" : "input contains invalid characters");
    fail("input contains invalid characters");
  }
  const [, ys, mo, d, hh, mi, ss, frac, , tzsign, oh, om] = m;
  const y = parseInt(ys, 10);
  const yy = Math.abs(y);
  const leap = yy % 4 === 0 && (yy % 100 !== 0 || yy % 400 === 0);
  if (+mo < 1 || +mo > 12 || +d < 1 || +d > DIM[+mo - 1] + (mo === "02" && leap ? 1 : 0) ||
      +hh > 23 || +mi > 59 || +ss > 60 || (oh !== undefined && (+oh > 23 || +om > 59)))
    fail("input is out of range");
  const fracNs = frac ? BigInt(frac.slice(0, 9).padEnd(9, "0")) : 0n;
  const offNs = oh === undefined ? 0n
    : BigInt(+oh * 3600 + +om * 60) * 1_000_000_000n * (tzsign === "-" ? -1n : 1n);
  const days = BigInt(daysFromCivil(y, +mo, +d));
  const ns = days * 86_400_000_000_000n +
    BigInt(+hh * 3600 + +mi * 60 + +ss) * 1_000_000_000n + fracNs - offNs;
  const q = ns / 86_400_000_000_000n;
  const rem = ns - q * 86_400_000_000_000n;
  const day = Number(q), sub = rem;
  const [cy, cm, cd] = civilFromDays(day);
  const secOfDay = Number(sub / 1_000_000_000n);
  const nanos = Number(sub % 1_000_000_000n);
  const pad = (n, w) => String(n).padStart(w, "0");
  const yPart = cy < 0 ? `-${pad(-cy, 4)}` : cy > 9999 ? `+${cy}` : pad(cy, 4);
  const f9 = pad(nanos, 9);
  const f = nanos === 0 ? "" : nanos % 1000000 === 0 ? `.${f9.slice(0, 3)}`
    : nanos % 1000 === 0 ? `.${f9.slice(0, 6)}` : `.${f9}`;
  return `${yPart}-${pad(cm, 2)}-${pad(cd, 2)}T${pad(Math.floor(secOfDay / 3600), 2)}:` +
    `${pad(Math.floor(secOfDay / 60) % 60, 2)}:${pad(secOfDay % 60, 2)}${f}Z`;
};

// ------------------------------------------------- rust_decimal::Decimal
// from_str grammar + to_string emission, both oracle-verified.
const DEC_STR = /^([+-]?)(\d+(?:\.(\d*))?|\.(\d+))(?:[eE]([+-]?\d+))?$/;
const DEC_MAX = 79228162514264337593543950335n;
// Expand a JS number into plain decimal digits (no exponent) the way a
// shortest-round-trip decimal expansion reads: "1e+21" -> "1" + 21 zeros.
const expandNumber = (n) => {
  const s = String(n); // shortest repr; may contain 'e'
  const m = /^(-?)(\d+)(?:\.(\d+))?(?:e([+-]?\d+))?$/i.exec(s);
  const digits = m[2] + (m[3] || "");
  const point = m[2].length + (m[4] ? parseInt(m[4], 10) : 0);
  return { neg: m[1] === "-", digits, scale: digits.length - point };
};
// (neg, digits, scale) -> canonical Decimal::to_string() text.
const decRender = (neg, digits, scale) => {
  digits = digits.replace(/^0+/, "") || "0";
  if (scale < 0) { digits += "0".repeat(-scale); scale = 0; }
  if (digits === "0") neg = false; // -0 -> "0"
  let int, frac;
  if (digits.length > scale) { int = digits.slice(0, digits.length - scale); frac = digits.slice(digits.length - scale); }
  else { int = "0"; frac = "0".repeat(scale - digits.length) + digits; }
  return (neg ? "-" : "") + int + (scale > 0 ? "." + frac : "");
};
export const decCanon = (v) => {
  let neg, digits, scale;
  if (typeof v === "string") {
    const m = DEC_STR.exec(v);
    if (!m) fail(`invalid value: string ${rustStr(v)}, expected a Decimal type representing a fixed-point number`);
    neg = m[1] === "-";
    const int = m[2] !== undefined ? m[2].split(".")[0] : "";
    const frac = m[3] ?? m[4] ?? "";
    digits = int + frac;
    scale = frac.length - (m[5] ? parseInt(m[5], 10) : 0);
  } else if (typeof v === "number" || v instanceof F64) {
    const n = v instanceof F64 ? v.v : v;
    if (!Number.isFinite(n)) fail(`invalid value: floating point \`${serF64(n)}\`, expected a Decimal type representing a fixed-point number`);
    ({ neg, digits, scale } = expandNumber(n));
  } else if (typeof v === "bigint") {
    neg = v < 0n;
    digits = (neg ? -v : v).toString();
    scale = 0;
  } else {
    invalidType(v, "a Decimal type representing a fixed-point number");
  }
  // scale > 28: round half-up at the 28th fractional digit (oracle-verified).
  if (scale > 28) {
    const keep = digits.length - (scale - 28);
    const kept = digits.slice(0, Math.max(0, keep));
    const dropped = digits.slice(Math.max(0, keep));
    let round = false;
    if (keep >= 0 && dropped !== "") round = dropped[0] >= "5";
    digits = kept === "" ? "0" : kept;
    scale = 28;
    if (round) {
      let carry = 1, out = digits.split("");
      for (let i = out.length - 1; i >= 0 && carry; i--) {
        const c = out[i].charCodeAt(0) - 48 + 1;
        out[i] = String.fromCharCode(48 + (c % 10));
        carry = c >= 10 ? 1 : 0;
      }
      digits = (carry ? "1" : "") + out.join("");
    }
    if (scale > digits.length) scale = Math.max(scale, digits.length); // unreachable-ish guard
  }
  const mant = BigInt((digits || "0") + "0".repeat(Math.max(0, -scale)) || "0") === 0n ? 0n
    : BigInt(digits === "" ? "0" : digits + (scale < 0 ? "0".repeat(-scale) : ""));
  if (mant > DEC_MAX)
    fail(`invalid value: ${typeof v === "string" ? `string ${rustStr(v)}` : `floating point \`${serF64(v instanceof F64 ? v.v : v)}\``}, expected a Decimal type representing a fixed-point number`);
  return decRender(neg, digits, scale);
};
// Value equality — Decimal PartialEq is numeric ("1.230" == "1.23").
const decNorm = (canon) => {
  const neg = canon[0] === "-";
  const body = neg ? canon.slice(1) : canon;
  const dot = body.indexOf(".");
  let int = dot < 0 ? body : body.slice(0, dot);
  let frac = dot < 0 ? "" : body.slice(dot + 1);
  int = int.replace(/^0+/, "") || "0";
  frac = frac.replace(/0+$/, "");
  return { neg: neg && (int !== "0" || frac !== ""), int, frac };
};
export const decEq = (a, b) => {
  const x = decNorm(decCanon(a)), y = decNorm(decCanon(b));
  return x.neg === y.neg && x.int === y.int && x.frac === y.frac;
};
export const decCmp = (a, b) => {
  const x = decNorm(decCanon(a)), y = decNorm(decCanon(b));
  if (x.neg !== y.neg) return x.neg ? -1 : 1;
  const mag = x.int.length !== y.int.length ? x.int.length - y.int.length
    : x.int !== y.int ? (x.int < y.int ? -1 : 1)
    : x.frac !== y.frac ? (x.frac < y.frac ? -1 : 1) : 0;
  return x.neg ? -mag : mag;
};

// -------------------------------------------------- typed struct codecs
// serde_json::to_vec parity for the market_update surface: struct fields
// emit in declaration order; Option serializes null; skip_serializing_if
// omits. Dates -> Display string, DateTime -> RFC3339-Z, Decimal -> string.
export const J = JSON.stringify;

// MarketDataBatch (market_data.rs field order).
export const serBar = (b) =>
  `{"date":${J(b.date.s)},"open":${J(decCanon(b.open))},"high":${J(decCanon(b.high))},` +
  `"low":${J(decCanon(b.low))},"close":${J(decCanon(b.close))},"volume":${b.volume === null || b.volume === undefined ? "null" : J(decCanon(b.volume))},` +
  `"amount":${b.amount === null || b.amount === undefined ? "null" : J(decCanon(b.amount))},"r7_raw":${b.r7_raw == null ? "null" : String(b.r7_raw)}}`;
export const serEvidenceValue = (e) =>
  `{"status":${J(e.status)},"value":${e.value === null || e.value === undefined ? "null" : J(e.value)},"reason":${J(e.reason)}}`;
const serDerivation = (d) => d === null || d === undefined ? "null" :
  `{"base_daily_version_id":${J(d.base_daily_version_id)},"algorithm_version":${J(d.algorithm_version)},` +
  `"first_trading_date":${J(d.first_trading_date.s ?? d.first_trading_date)},"last_trading_date":${J(d.last_trading_date.s ?? d.last_trading_date)}}`;
const serSnapshotInput = (s) =>
  `{"role":${J(s.role)},"member":${J(s.member)},"archive_sha256":${J(s.archive_sha256)},"member_sha256":${J(s.member_sha256)}}`;
export const serProvenance = (p) =>
  `{"source":${J(p.source)},"source_period":${J(p.source_period)},"source_adjustment":${J(p.source_adjustment)},` +
  `"symbol":${J(p.symbol)},"acquired_at":${J(p.acquired_at)},"adapter_version":${J(p.adapter_version)},` +
  `"yearly_derivation":${serDerivation(p.yearly_derivation)},"snapshot_inputs":${p.snapshot_inputs == null ? "null" : `[${p.snapshot_inputs.map(serSnapshotInput).join(",")}]`}}`;
export const serCoverage = (c) => `{"status":${J(c.status)},"reason":${J(c.reason)}}`;
export const serUnitCurrency = (u) =>
  `{"price_currency":${serEvidenceValue(u.price_currency)},"price_unit":${serEvidenceValue(u.price_unit)},` +
  `"volume_unit":${serEvidenceValue(u.volume_unit)},"amount_currency":${serEvidenceValue(u.amount_currency)},"amount_unit":${serEvidenceValue(u.amount_unit)}}`;
export const serBatch = (b) =>
  `{"symbol":${J(b.symbol)},"period":${J(b.period)},"adjustment":${J(b.adjustment)},"bars":[${b.bars.map(serBar).join(",")}],` +
  `"provenance":${serProvenance(b.provenance)},"coverage":${serCoverage(b.coverage)},"unit_currency":${serUnitCurrency(b.unit_currency)},` +
  `"adjustment_evidence":${serEvidenceValue(b.adjustment_evidence)}}`;

// serde_json::to_value parity — plain JS objects, all keys emitted (they
// are written through serValue which sorts on emission). Decimals emit
// canonical strings; dates emit Display strings.
export const barValue = (b) => ({
  date: b.date.s, open: decCanon(b.open), high: decCanon(b.high), low: decCanon(b.low),
  close: decCanon(b.close),
  volume: b.volume == null ? null : decCanon(b.volume),
  amount: b.amount == null ? null : decCanon(b.amount),
  r7_raw: b.r7_raw ?? null });
const derivationValue = (d) => d == null ? null : ({
  base_daily_version_id: d.base_daily_version_id, algorithm_version: d.algorithm_version,
  first_trading_date: d.first_trading_date.s ?? d.first_trading_date,
  last_trading_date: d.last_trading_date.s ?? d.last_trading_date });
export const provenanceValue = (p) => ({
  source: p.source, source_period: p.source_period, source_adjustment: p.source_adjustment,
  symbol: p.symbol, acquired_at: p.acquired_at, adapter_version: p.adapter_version,
  yearly_derivation: derivationValue(p.yearly_derivation),
  snapshot_inputs: p.snapshot_inputs == null ? null : p.snapshot_inputs.map((s) => ({
    role: s.role, member: s.member, archive_sha256: s.archive_sha256, member_sha256: s.member_sha256 })) });
export const coverageValue = (c) => ({ status: c.status, reason: c.reason });
export const evidenceValueValue = (e) => ({ status: e.status, value: e.value ?? null, reason: e.reason });
export const unitCurrencyValue = (u) => ({
  price_currency: evidenceValueValue(u.price_currency), price_unit: evidenceValueValue(u.price_unit),
  volume_unit: evidenceValueValue(u.volume_unit), amount_currency: evidenceValueValue(u.amount_currency),
  amount_unit: evidenceValueValue(u.amount_unit) });
export const batchValue = (b) => ({
  symbol: b.symbol, period: b.period, adjustment: b.adjustment,
  bars: b.bars.map(barValue), provenance: provenanceValue(b.provenance),
  coverage: coverageValue(b.coverage), unit_currency: unitCurrencyValue(b.unit_currency),
  adjustment_evidence: evidenceValueValue(b.adjustment_evidence) });

// Evidence (market_update.rs) to_value — `continuity` is
// skip_serializing_if None, i.e. omitted entirely.
export const evidenceValue = (e) => {
  const out = {
    base_version_id: e.base_version_id, base_integrity_sha256: e.base_integrity_sha256,
    candidate_sha256: e.candidate_sha256,
    range: { start: e.range.start.s ?? e.range.start, end: e.range.end.s ?? e.range.end },
    same_qfq_basis_ref: e.same_qfq_basis_ref,
    revision_ref: e.revision_ref ?? null };
  if (e.continuity !== null && e.continuity !== undefined) out.continuity = e.continuity;
  return out;
};
