// NM-054 — compact_market.rs port: content digests, CompactBar
// conversion, canonical evidence verification (JCS), epoch/entity
// primitives. Error kind = "invalid" carrying the verbatim message;
// callers wrap with integrity() like super::integrity(compact errors).
import { createHash } from "node:crypto";
import { MarketError } from "./record_codec.mjs";
import { _dec } from "../algorithms/qfq_affine.mjs";

export const MAX_SEGMENT_BARS = 512;

const invalid = (m) => new MarketError("invalid", `compact market data invalid: ${m}`);
const sha = (b) => createHash("sha256").update(b).digest();
const digestHex = (d) => Buffer.from(d).toString("hex");
export { digestHex };

// ---- epoch/period/entity ----
// chrono num_days_from_ce -> epoch offset: 1970-01-01 = ce 719163.
const CE_EPOCH = 719163;
const daysFromCe = (y, mo, d) => {
  const a = Math.floor((14 - mo) / 12);
  const yy = y + 4800 - a;
  const mm = mo + 12 * a - 3;
  const jdn = d + Math.floor((153 * mm + 2) / 5) + 365 * yy + Math.floor(yy / 4) -
    Math.floor(yy / 100) + Math.floor(yy / 400) - 32045;
  return jdn - 1721425;
};
const ceToYmd = (ce) => {
  const jdn = ce + 1721425; // back to Julian day number
  const a = jdn + 32044;
  const b = Math.floor((4 * a + 3) / 146097);
  const c = a - Math.floor((146097 * b) / 4);
  const dd = Math.floor((4 * c + 3) / 1461);
  const e = c - Math.floor((1461 * dd) / 4);
  const m = Math.floor((5 * e + 2) / 153);
  const day = e - Math.floor((153 * m + 2) / 5) + 1;
  const month = m + 3 - 12 * Math.floor(m / 10);
  const year = 100 * b + dd - 4800 + Math.floor(m / 10);
  return `${String(year).padStart(4, "0")}-${String(month).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
};

export const epochDay = (ymd) => {
  const [y, mo, d] = (typeof ymd === "string" ? ymd : ymd.s).split("-").map(Number);
  return daysFromCe(y, mo, d) - CE_EPOCH;
};

export const epochDate = (day) => {
  const n = Number(day);
  if (!Number.isSafeInteger(n)) throw invalid("trade_day out of range");
  // chrono NaiveDate bounds: -262143-01-01 ..= +262143-12-31
  if (n < -96465292 || n > 95026601) throw invalid("trade_day out of date range");
  return ceToYmd(n + CE_EPOCH);
};

export const periodCode = (p) => (p === "Day" ? 1 : p === "Year" ? 2 : (() => { throw invalid(`unknown period`); })());
export const decodePeriod = (code) =>
  code === 1n || code === 1 ? "Day" : code === 2n || code === 2 ? "Year" : (() => { throw invalid("unknown period code"); })();

export const canonicalEntity = (entity) => {
  const ok = entity.length === 9 &&
    (entity.startsWith("sh.") || entity.startsWith("sz.")) &&
    /^[0-9]{6}$/.test(entity.slice(3));
  if (!ok) throw invalid(`non-canonical entity ${entity}`);
};

// ---- CompactBar ----
// shape: {trade_day:i64(BigInt), open_cents..close_cents:i64, volume:?i64,
//         amount:?string(canonical decimal), r7_raw:?u32}

// Dec {m,s} -> i64 cents: rust_decimal `value*100 -> to_i64 (trunc),
// then Decimal::from(cents)/100 == value` exactness check.
const cents = (v) => {
  const scaled = _dec.mul(v, { m: 100n, s: 0 });
  const div = 10n ** BigInt(scaled.s);
  const cents = scaled.m / div; // BigInt division truncates toward zero
  if (cents > 9223372036854775807n || cents < -9223372036854775808n) {
    throw invalid("price exceeds i64 cents range");
  }
  if (!decValueEq({ m: cents, s: 2 }, v)) {
    throw invalid("price is not exactly 2dp");
  }
  return cents;
};

// Dec numeric equality (value comparison, scale-insensitive)
const decValueEq = (a, b) => {
  const s = Math.max(a.s, b.s);
  return a.m * 10n ** BigInt(s - a.s) === b.m * 10n ** BigInt(s - b.s);
};

const volume = (v) => {
  // rust_decimal to_i64 (truncate) + Decimal::from(vol) == value + >=0
  const s = 10n ** BigInt(v.s);
  const vol = v.m / s;
  if (vol > 9223372036854775807n || vol < -9223372036854775808n) {
    throw invalid("volume exceeds i64 range");
  }
  if (v.m % s !== 0n) throw invalid("volume is not integral");
  if (vol < 0n) throw invalid("negative volume");
  return vol;
};

// rust_decimal normalize(): strip trailing zeros (reduce scale)
const decNormalize = (v) => {
  let { m, s } = v;
  while (s > 0 && m % 10n === 0n) { m /= 10n; s -= 1; }
  if (m === 0n) return { m: 0n, s: 0 };
  return { m, s };
};

const amount = (v) => {
  if (v.m < 0n) throw invalid("negative amount");
  const text = _dec.toString(decNormalize(v));
  let parsed;
  try { parsed = _dec.fromStr(text); } catch { parsed = null; }
  if (parsed === null || !decValueEq(parsed, v)) {
    throw invalid("canonical amount does not roundtrip");
  }
  return text;
};

export const compactBarFromBar = (bar, period) => {
  if (period === "Year" && bar.r7_raw !== null && bar.r7_raw !== undefined) {
    throw invalid("yearly bar must not carry r7_raw");
  }
  const c = {
    trade_day: BigInt(epochDay(typeof bar.date === "string" ? bar.date : bar.date.s)),
    open_cents: cents(bar.open), high_cents: cents(bar.high),
    low_cents: cents(bar.low), close_cents: cents(bar.close),
    volume: bar.volume == null ? null : volume(bar.volume),
    amount: bar.amount == null ? null : amount(bar.amount),
    r7_raw: bar.r7_raw ?? null,
  };
  compactBarCheck(c, period);
  return c;
};

export const compactBarCheck = (c, period) => {
  epochDate(Number(c.trade_day));
  if (c.high_cents < c.open_cents || c.high_cents < c.close_cents ||
      c.high_cents < c.low_cents ||
      c.low_cents > c.open_cents || c.low_cents > c.close_cents) {
    throw invalid("OHLC ordering violated");
  }
  if (c.volume !== null && c.volume !== undefined && c.volume < 0n) {
    throw invalid("negative volume");
  }
  if (c.amount !== null && c.amount !== undefined) {
    let parsed;
    try { parsed = _dec.fromStr(c.amount); } catch { throw invalid("stored amount is not a decimal"); }
    if (parsed.m < 0n) throw invalid("negative amount");
    if (!canonicalDecimal(c.amount)) throw invalid("stored amount is not canonical");
  }
  if (period === "Year" && c.r7_raw !== null && c.r7_raw !== undefined) {
    throw invalid("yearly bar must not carry r7_raw");
  }
};

export const compactBarToBar = (c, period) => {
  compactBarCheck(c, period);
  let amt = null;
  if (c.amount !== null && c.amount !== undefined) {
    try { amt = _dec.fromStr(c.amount); } catch { throw invalid("stored amount is not a decimal"); }
  }
  return {
    date: epochDate(Number(c.trade_day)),
    open: { m: c.open_cents, s: 2 }, high: { m: c.high_cents, s: 2 },
    low: { m: c.low_cents, s: 2 }, close: { m: c.close_cents, s: 2 },
    volume: c.volume == null ? null : { m: c.volume, s: 0 },
    amount: amt,
    r7_raw: c.r7_raw ?? null,
  };
};

// ---- encodings ----
const pushBytes = (out, bytes) => {
  const len = Buffer.alloc(4); len.writeUInt32BE(bytes.length);
  out.push(len, Buffer.from(bytes));
};
const pushText = (out, v) => pushBytes(out, Buffer.from(v, "utf8"));

export const barEncoding = (out, c) => {
  out.push(Buffer.from("x-data-bar-v1\0"));
  const td = Buffer.alloc(8); td.writeBigInt64BE(BigInt(c.trade_day)); out.push(td);
  for (const v of [c.open_cents, c.high_cents, c.low_cents, c.close_cents]) {
    const b = Buffer.alloc(8); b.writeBigInt64BE(BigInt(v)); out.push(b);
  }
  if (c.volume !== null && c.volume !== undefined) {
    out.push(Buffer.from([1]));
    const b = Buffer.alloc(8); b.writeBigInt64BE(BigInt(c.volume)); out.push(b);
  } else out.push(Buffer.from([0]));
  if (c.amount !== null && c.amount !== undefined) {
    out.push(Buffer.from([1])); pushText(out, c.amount);
  } else out.push(Buffer.from([0]));
  if (c.r7_raw !== null && c.r7_raw !== undefined) {
    out.push(Buffer.from([1]));
    const b = Buffer.alloc(4); b.writeUInt32BE(Number(c.r7_raw)); out.push(b);
  } else out.push(Buffer.from([0]));
};

const canonicalDecimal = (text) => {
  const body = text.startsWith("-") ? text.slice(1) : text;
  const dot = body.indexOf(".");
  const int = dot === -1 ? body : body.slice(0, dot);
  const frac = dot === -1 ? "" : body.slice(dot + 1);
  if (int === "" || !/^\d+$/.test(int) || (int.length > 1 && int.startsWith("0"))) return false;
  if (body.includes(".") && (frac === "" || !/^\d+$/.test(frac) || frac.endsWith("0"))) return false;
  if (text.startsWith("-") && /^[0.]+$/.test(body)) return false;
  return true;
};

export const segmentDigest = (entity, period, bars) => {
  canonicalEntity(entity);
  if (bars.length === 0 || bars.length > MAX_SEGMENT_BARS) {
    throw invalid("segment row_count out of bounds");
  }
  for (let i = 1; i < bars.length; i++) {
    if (bars[i].trade_day <= bars[i - 1].trade_day) {
      throw invalid("segment bars not strictly increasing");
    }
  }
  const out = [Buffer.from("x-data-segment-v1\0")];
  pushText(out, entity);
  out.push(Buffer.from([periodCode(period)]));
  const n = Buffer.alloc(4); n.writeUInt32BE(bars.length); out.push(n);
  for (const b of bars) { compactBarCheck(b, period); barEncoding(out, b); }
  return sha(Buffer.concat(out));
};

export const checkpointProjectionDigest = (entity, period, clips) => {
  canonicalEntity(entity);
  if (clips.length === 0) throw invalid("checkpoint requires at least one clip");
  let prevEnd = null;
  for (const clip of clips) {
    if (clip.clip_start_day > clip.clip_end_day) throw invalid("clip bounds inverted");
    if (clip.row_count === 0) throw invalid("clip row_count must be positive");
    if (prevEnd !== null && clip.clip_start_day <= prevEnd) {
      throw invalid("checkpoint clips overlap or are unordered");
    }
    prevEnd = clip.clip_end_day;
  }
  const out = [Buffer.from("x-data-projection-checkpoint-v1\0")];
  pushText(out, entity);
  out.push(Buffer.from([periodCode(period)]));
  const n = Buffer.alloc(4); n.writeUInt32BE(clips.length); out.push(n);
  for (const clip of clips) {
    out.push(Buffer.from(clip.segment_digest));
    const s = Buffer.alloc(8); s.writeBigInt64BE(BigInt(clip.clip_start_day)); out.push(s);
    const e = Buffer.alloc(8); e.writeBigInt64BE(BigInt(clip.clip_end_day)); out.push(e);
    const r = Buffer.alloc(4); r.writeUInt32BE(clip.row_count); out.push(r);
  }
  return sha(Buffer.concat(out));
};

const checkDeltaOps = (ops) => {
  if (ops.length === 0) throw invalid("delta requires at least one op");
  let prevEnd = null;
  for (const op of ops) {
    if (op.range_start_day > op.range_end_day) throw invalid("delta range bounds inverted");
    if (prevEnd !== null && op.range_start_day <= prevEnd) {
      throw invalid("delta ranges overlap or are unordered");
    }
    prevEnd = op.range_end_day;
    if (op.kind.kind === "upsert") {
      const clip = op.kind.clip;
      if (clip.clip_start_day > clip.clip_end_day) throw invalid("clip bounds inverted");
      if (clip.row_count === 0) throw invalid("clip row_count must be positive");
      if (clip.clip_start_day < op.range_start_day || clip.clip_end_day > op.range_end_day) {
        throw invalid("upsert clip outside op range");
      }
    }
  }
};

const deltaOpEncoding = (out, op) => {
  if (op.kind.kind === "upsert") {
    const clip = op.kind.clip;
    out.push(Buffer.from([1]));
    const s = Buffer.alloc(8); s.writeBigInt64BE(BigInt(op.range_start_day)); out.push(s);
    const e = Buffer.alloc(8); e.writeBigInt64BE(BigInt(op.range_end_day)); out.push(e);
    out.push(Buffer.from([1]), Buffer.from(clip.segment_digest), Buffer.from([1]));
    const cs = Buffer.alloc(8); cs.writeBigInt64BE(BigInt(clip.clip_start_day)); out.push(cs);
    out.push(Buffer.from([1]));
    const ce = Buffer.alloc(8); ce.writeBigInt64BE(BigInt(clip.clip_end_day)); out.push(ce);
    const r = Buffer.alloc(4); r.writeUInt32BE(clip.row_count); out.push(r);
  } else {
    out.push(Buffer.from([2]));
    const s = Buffer.alloc(8); s.writeBigInt64BE(BigInt(op.range_start_day)); out.push(s);
    const e = Buffer.alloc(8); e.writeBigInt64BE(BigInt(op.range_end_day)); out.push(e);
    out.push(Buffer.from([0, 0, 0]), Buffer.alloc(4));
  }
};

export const deltaDigest = (ops) => {
  checkDeltaOps(ops);
  const out = [Buffer.from("x-data-version-delta-v1\0")];
  const n = Buffer.alloc(4); n.writeUInt32BE(ops.length); out.push(n);
  for (const op of ops) deltaOpEncoding(out, op);
  return sha(Buffer.concat(out));
};

export const deltaProjectionDigest = (parent, ops) => {
  const delta = deltaDigest(ops);
  const out = [Buffer.from("x-data-projection-delta-v1\0"), Buffer.from(parent), delta];
  return [delta, sha(Buffer.concat(out))];
};

// ---- source binding / version identity digests ----
export const sourceBindingDigest = (b) => {
  if (b.generation_id === "") throw invalid("source binding generation_id is empty");
  const out = [Buffer.from("x-data-source-binding-v1\0")];
  pushText(out, b.generation_id);
  for (const d of [b.generation_manifest_sha256, b.day_member_sha256,
                   b.security_event_sha256, b.evidence_digest]) {
    out.push(Buffer.from(d));
  }
  return sha(Buffer.concat(out));
};

// chrono: parse RFC3339 first (Z/offsets ok), then require offset==0 and
// exact to_rfc3339() roundtrip — "+00:00", AutoSi subseconds (none/3/6/9).
const RFC3339_PARSE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$/;
const RFC3339_CANON = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(\.\d{3}|\.\d{6}|\.\d{9})?\+00:00$/;
export const canonicalRfc3339 = (s) => {
  if (!RFC3339_PARSE.test(s)) throw invalid("published_at is not RFC 3339");
  const m = RFC3339_CANON.exec(s);
  if (!m) throw invalid("published_at is not canonical UTC RFC 3339");
  const dt = new Date(Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]));
  if (Number.isNaN(dt.getTime()) || dt.getUTCFullYear() !== +m[1] ||
      dt.getUTCMonth() !== +m[2] - 1 || dt.getUTCDate() !== +m[3] ||
      dt.getUTCHours() !== +m[4] || dt.getUTCMinutes() !== +m[5] ||
      dt.getUTCSeconds() !== +m[6]) {
    throw invalid("published_at is not RFC 3339");
  }
  return s;
};

// DateTime::parse_from_rfc3339().with_timezone(&Utc).to_rfc3339() —
// any RFC3339 offset accepted, emitted as canonical "+00:00" with AutoSi
// fractional digits (none / 3 / 6 / 9).
export const parseRfc3339Utc = (s) => {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(Z|([+-])(\d{2}):(\d{2}))$/.exec(s);
  if (!m || m[7] !== undefined && m[7].length > 9) {
    throw invalid(`invalid timestamp ${JSON.stringify(s)}`);
  }
  const [, y, mo, d, h, mi, sec, frac, , sign, oh, om] = m;
  if (+mo < 1 || +mo > 12 || +d < 1 || +d > 31 || +h > 23 || +mi > 59 ||
      +sec > 60 || (oh !== undefined && (+oh > 23 || +om > 59))) {
    throw invalid(`invalid timestamp ${JSON.stringify(s)}`);
  }
  let nanos = frac ? Number(frac.padEnd(9, "0").slice(0, 9)) : 0;
  // calendar-date validity (chrono rejects e.g. Feb 30 — JS would roll)
  const check = new Date(Date.UTC(+y, +mo - 1, +d, +h, +mi, Math.min(+sec, 59)));
  if (check.getUTCFullYear() !== +y || check.getUTCMonth() !== +mo - 1 ||
      check.getUTCDate() !== +d || check.getUTCHours() !== +h ||
      check.getUTCMinutes() !== +mi) {
    throw invalid(`invalid timestamp ${JSON.stringify(s)}`);
  }
  const offMs = oh !== undefined ? (+oh * 3600 + +om * 60) * 1000 : 0;
  const ms = check.getTime() + (sign === "-" ? offMs : -offMs);
  const dt = new Date(ms);
  if (Number.isNaN(dt.getTime())) throw invalid(`invalid timestamp ${JSON.stringify(s)}`);
  const pad = (n, w) => String(n).padStart(w, "0");
  const base = `${pad(dt.getUTCFullYear(), 4)}-${pad(dt.getUTCMonth() + 1, 2)}-${pad(dt.getUTCDate(), 2)}T` +
    `${pad(dt.getUTCHours(), 2)}:${pad(dt.getUTCMinutes(), 2)}:${pad(dt.getUTCSeconds(), 2)}`;
  // AutoSi: 0 -> none, %1e6 -> 3 digits, %1e3 -> 6, else 9
  const f9 = String(nanos).padStart(9, "0");
  const f = nanos === 0 ? "" : nanos % 1000000 === 0 ? `.${f9.slice(0, 3)}`
    : nanos % 1000 === 0 ? `.${f9.slice(0, 6)}` : `.${f9}`;
  return `${base}${f}+00:00`;
};

// serde DateTime<Utc> serialization: canonical RFC3339 with "Z"
export const serdeDateTime = (s) => s.replace(/\+00:00$/, "Z");

export const versionIntegrityDigest = (id) => {
  if (id.version_id === "") throw invalid("version_id is empty");
  canonicalEntity(id.entity);
  if (id.record_count === 0n || id.record_count === 0) throw invalid("record_count must be positive");
  if (id.first_day > id.last_day) throw invalid("version day bounds inverted");
  epochDate(Number(id.first_day)); epochDate(Number(id.last_day));
  canonicalRfc3339(id.published_at);
  if (id.manifest_mode === "checkpoint") {
    if (id.parent_version_id !== null && id.parent_version_id !== undefined ||
        Number(id.delta_depth) !== 0) {
      throw invalid("checkpoint version carries parent/depth");
    }
  } else {
    const p = id.parent_version_id;
    if (p === null || p === undefined || p === "" ||
        !(Number(id.delta_depth) >= 1 && Number(id.delta_depth) <= 256)) {
      throw invalid("delta version lacks parent or depth 1..=256");
    }
  }
  if (id.period === "Day" && (id.yearly_lineage !== null && id.yearly_lineage !== undefined)) {
    throw invalid("period/lineage mismatch");
  }
  if (id.period === "Year") {
    const l = id.yearly_lineage;
    if (l === null || l === undefined) throw invalid("period/lineage mismatch");
    if (l.base_daily_version_id === "" || l.algorithm_version === "") {
      throw invalid("yearly lineage identity is empty");
    }
    if (l.first_trade_day > l.last_trade_day || l.last_trade_day > l.as_of_day) {
      throw invalid("yearly lineage bounds inverted");
    }
    epochDate(Number(l.first_trade_day)); epochDate(Number(l.last_trade_day));
    epochDate(Number(l.as_of_day));
  }
  const out = [Buffer.from("x-data-market-version-v1\0")];
  pushText(out, id.version_id);
  pushText(out, id.entity);
  out.push(Buffer.from([periodCode(id.period), 1]));
  out.push(Buffer.from(id.source_binding_digest));
  if (id.parent_version_id !== null && id.parent_version_id !== undefined) {
    out.push(Buffer.from([1])); pushText(out, id.parent_version_id);
  } else out.push(Buffer.from([0]));
  out.push(Buffer.from([id.manifest_mode === "checkpoint" ? 1 : 2]));
  const dd = Buffer.alloc(4); dd.writeUInt32BE(Number(id.delta_depth)); out.push(dd);
  out.push(Buffer.from(id.projection_digest));
  const rc = Buffer.alloc(8); rc.writeBigUInt64BE(BigInt(id.record_count)); out.push(rc);
  const fd = Buffer.alloc(8); fd.writeBigInt64BE(BigInt(id.first_day)); out.push(fd);
  const ld = Buffer.alloc(8); ld.writeBigInt64BE(BigInt(id.last_day)); out.push(ld);
  out.push(Buffer.from([id.coverage_status === "Complete" ? 1 : id.coverage_status === "Incomplete" ? 2 : 3]));
  out.push(Buffer.from(id.evidence_digest));
  pushText(out, id.published_at);
  if (id.yearly_lineage !== null && id.yearly_lineage !== undefined) {
    const l = id.yearly_lineage;
    out.push(Buffer.from([1]));
    pushText(out, l.base_daily_version_id); pushText(out, l.algorithm_version);
    for (const v of [l.first_trade_day, l.last_trade_day, l.as_of_day]) {
      const b = Buffer.alloc(8); b.writeBigInt64BE(BigInt(v)); out.push(b);
    }
  } else out.push(Buffer.from([0]));
  return sha(Buffer.concat(out));
};

// ---- canonical evidence (JCS RFC-8785) ----
const jcs = (v) => {
  if (v === null) return "null";
  if (typeof v === "boolean") return v ? "true" : "false";
  if (typeof v === "number") {
    if (!Number.isFinite(v)) throw invalid("evidence is not canonicalizable JSON");
    return String(v);
  }
  if (typeof v === "bigint") return String(v);
  if (typeof v === "string") return JSON.stringify(v);
  if (Array.isArray(v)) return `[${v.map(jcs).join(",")}]`;
  const keys = Object.keys(v).sort((a, b) => {
    // UTF-16 code-unit order matches UTF-8 byte order for BMP
    for (let i = 0; i < Math.min(a.length, b.length); i++) {
      if (a.charCodeAt(i) !== b.charCodeAt(i)) return a.charCodeAt(i) - b.charCodeAt(i);
    }
    return a.length - b.length;
  });
  return `{${keys.map((k) => `${JSON.stringify(k)}:${jcs(v[k])}`).join(",")}}`;
};

export const EVIDENCE_KINDS = ["generation", "batch_selection", "version", "continuity", "catalog", "yearly"];
const byteLimit = (kind) => ({
  continuity: 4096, version: 16384, yearly: 16384,
  generation: 262144, catalog: 262144, batch_selection: 4194304,
})[kind];

const evidenceDigest = (kind, canonicalJson) => {
  const out = [Buffer.from("x-data-evidence-v1\0")];
  pushText(out, kind);
  pushBytes(out, Buffer.from(canonicalJson, "utf8"));
  return sha(Buffer.concat(out));
};

const rejectBarArrays = (v) => {
  if (v !== null && typeof v === "object") {
    if (Array.isArray(v)) { for (const x of v) rejectBarArrays(x); return; }
    for (const [k, item] of Object.entries(v)) {
      if ((k === "bars" || k === "records") && Array.isArray(item)) {
        throw invalid("evidence embeds a bars/records array");
      }
      rejectBarArrays(item);
    }
  }
};

const checkEvidenceFields = (f) => {
  if (f.reason === "") throw invalid("evidence reason is empty");
  if (f.status === "Confirmed") {
    if (typeof f.value !== "string" || f.value === "") {
      throw invalid("confirmed evidence lacks a value");
    }
  } else if (f.status === "Unknown") {
    if (f.value !== null && f.value !== undefined) {
      throw invalid("unknown evidence carries a value");
    }
  }
};

const checkVersionEvidence = (value) => {
  const need = (o, f) => {
    if (o === null || typeof o !== "object" || !(f in o) ||
        typeof o[f] !== "object" || o[f] === null) {
      throw invalid("version evidence shape mismatch");
    }
    const e = o[f];
    if (typeof e.status !== "string" || !["Confirmed", "Unknown"].includes(e.status) ||
        (e.value !== null && e.value !== undefined && typeof e.value !== "string") ||
        typeof e.reason !== "string") {
      throw invalid("version evidence shape mismatch");
    }
    return e;
  };
  if (typeof value.source !== "string" || typeof value.coverage_reason !== "string" ||
      typeof value.unit_currency !== "object" || value.unit_currency === null ||
      typeof value.adjustment_evidence !== "object" || value.adjustment_evidence === null) {
    throw invalid("version evidence shape mismatch");
  }
  if (value.source === "" || value.coverage_reason === "") {
    throw invalid("version evidence source/coverage_reason empty");
  }
  const unit = value.unit_currency;
  for (const f of ["price_currency", "price_unit", "volume_unit", "amount_currency", "amount_unit"]) {
    checkEvidenceFields(need(unit, f));
  }
  const adj = need(value, "adjustment_evidence");
  checkEvidenceFields(adj);
  if (adj.status !== "Confirmed" || adj.value !== "qfq") {
    throw invalid("adjustment evidence is not confirmed qfq");
  }
};

const checkEvidenceSemantics = (kind, value) => {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw invalid("evidence is not a JSON object");
  }
  rejectBarArrays(value);
  if (kind === "version") checkVersionEvidence(value);
};

export const verifyEvidence = (kind, canonicalJson, expected) => {
  let value;
  try { value = JSON.parse(canonicalJson); } catch { throw invalid("evidence is not JSON"); }
  const recanonicalized = jcs(value);
  if (recanonicalized !== canonicalJson) {
    throw invalid("stored evidence is not canonical JCS");
  }
  if (Buffer.compare(evidenceDigest(kind, canonicalJson), Buffer.from(expected)) !== 0) {
    throw invalid("evidence digest mismatch");
  }
  checkEvidenceSemantics(kind, value);
  if (Buffer.byteLength(canonicalJson, "utf8") > byteLimit(kind)) {
    throw invalid("evidence exceeds kind byte limit");
  }
  return value;
};

export const parseEvidenceKind = (raw) =>
  EVIDENCE_KINDS.includes(raw) ? raw : (() => { throw invalid("unknown evidence kind"); })();

export const digestLen = (raw) =>
  Buffer.from(raw).length === 32 ? Buffer.from(raw) : null;
