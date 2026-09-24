// serde_json::from_value-compatible typed decoders — the consolidated
// machinery shared by the data_supply/metrics wire tables (NM-063/070).
//
// `from_value` errors carry no line/column — messages are bare serde
// texts. Verified against the `mcp_data` oracle:
//   - structs: deny_unknown_fields ("unknown field `k`, expected ..."),
//     missing field, "invalid type: X, expected struct Y", struct-from-seq
//     ("invalid length N, expected struct X with M elements" short,
//     "invalid length N, expected fewer elements in array" long).
//     `#[serde(default)]` applies in seq form; Option<T> fields do NOT
//     auto-default in seq form (map form only).
//   - externally-tagged unit enums: string, or {"variant":null} map;
//     non-string/map -> "expected string or map"; non-null payload ->
//     "expected unit".
//   - internally-tagged enums (tag=kind/dataset/status/mode): seq ->
//     "missing field `tag`"; non-string tag -> "expected variant
//     identifier"; unknown variant list uses the variant's wire names;
//     unknown field expected list = the VARIANT's fields only; UNIT
//     variants ignore extra keys (serde quirk the RangeWriteSelection
//     doc-comment relies on).
//   - NaiveDate: [+-]?1-4 digit year, 1-2 digit month/day; field ranges
//     -> "input is out of range"; non-string -> "a formatted date string".
//   - DateTime<Utc>: chrono RFC3339 classes (same as consistency_check).
//   - usize/u64: "invalid value: integer `-1`, expected u64";
//     u64-overflow lexemes arrive as f64 -> "floating point `...`".
//
// The same text primitives (rustStr/desc/expectedFields/chrono scanners)
// are exported for the positional `from_slice` decoder in
// src_node/bin/_consistency_input.mjs — one serde surface, two entry
// points.

import { F64, serF64 } from "./serde_json.mjs";

export class Decode extends Error {}
export const fail = (msg) => { throw new Decode(msg); };

export const isObj = (v) =>
  v !== null && typeof v === "object" && !Array.isArray(v) && !(v instanceof F64);
export const isArr = Array.isArray;

export const rustStr = (s) =>
  '"' + s.replace(/[\\"\n\r\t]|[\x00-\x1f\x7f]/g, (c) => {
    if (c === '"') return '\\"';
    if (c === "\\") return "\\\\";
    if (c === "\n") return "\\n";
    if (c === "\r") return "\\r";
    if (c === "\t") return "\\t";
    return `\\u{${c.codePointAt(0).toString(16)}}`;
  }) + '"';
// serde de::Unexpected Display for a parsed Value
export const desc = (v) =>
  v === null ? "null" :
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

// ----------------------------------------------------------------- scalars
export const tString = (v) => (typeof v === "string" ? v : invalidType(v, "a string"));
export const tBool = (v) => (typeof v === "boolean" ? v : invalidType(v, "a boolean"));
const intKind = (v) =>
  typeof v === "bigint" ? v : typeof v === "number" && Number.isInteger(v) ? BigInt(v) : null;
export const I64_MIN = -(1n << 63n);
export const U64_MAX = (1n << 64n) - 1n;
export const tInt = (name, lo, hi) => (v) => {
  const i = intKind(v);
  if (i !== null) {
    if (i >= lo && i <= hi) return Number(i);
    if (i >= I64_MIN) fail(`invalid value: integer \`${i}\`, expected ${name}`);
    invalidType(v, name);
  }
  invalidType(v, name);
};
export const tU64 = tInt("u64", 0n, U64_MAX);
export const tUsize = tInt("usize", 0n, U64_MAX);
export const tI32 = tInt("i32", -(2n ** 31n), 2n ** 31n - 1n);

// chrono NaiveDate::from_str ("%Y-%m-%d") — sequential scanner matching
// chrono's parse order: optional sign + 1-4 digit year, '-', 1-2 digit
// month (range-checked immediately: "001" reads month 00 -> out of
// range), '-', 1-2 digit day; leftover -> "trailing input"; running out
// of text -> "premature end of input"; a non-digit/non-dash byte ->
// "input contains invalid characters".
const DIM = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
// Returns { canon } on success (chrono NaiveDate Display form so
// downstream ordering compares like dates) or { msg } on failure.
export const parseNaiveDate = (s) => {
  let i = 0;
  const bad = (msg) => ({ msg });
  if (s === "") return bad("premature end of input");
  if (s[i] === "+" || s[i] === "-") i++;
  const yStart = i;
  while (i < s.length && s[i] >= "0" && s[i] <= "9") i++;
  if (i === yStart) return bad("input contains invalid characters");
  if (i - yStart > 4) return bad("input contains invalid characters");
  const yearEnd = i;
  const yy = Math.abs(parseInt(s.slice(0, i), 10));
  if (i >= s.length) return bad("premature end of input");
  if (s[i] !== "-") return bad("input contains invalid characters");
  i++;
  const mStart = i;
  while (i < s.length && s[i] >= "0" && s[i] <= "9" && i - mStart < 2) i++;
  if (i === mStart) return bad(i >= s.length ? "premature end of input" : "input contains invalid characters");
  const mm = +s.slice(mStart, i);
  if (mm < 1 || mm > 12) return bad("input is out of range");
  if (i >= s.length) return bad("premature end of input");
  if (s[i] !== "-") return bad("input contains invalid characters");
  i++;
  const dStart = i;
  while (i < s.length && s[i] >= "0" && s[i] <= "9" && i - dStart < 2) i++;
  if (i === dStart) return bad(i >= s.length ? "premature end of input" : "input contains invalid characters");
  const dd = +s.slice(dStart, i);
  const leap = yy % 4 === 0 && (yy % 100 !== 0 || yy % 400 === 0);
  if (dd < 1 || dd > DIM[mm - 1] + (mm === 2 && leap ? 1 : 0)) return bad("input is out of range");
  if (i < s.length) return bad("trailing input");
  // chrono NaiveDate Display: "+2025-01-01" -> "2025-01-01",
  // "999-01-01" -> "0999-01-01", "-0001-01-01" -> "-0001-01-01"
  // (negatives sort before year 0 — keeps range checks correct).
  const sign = s[0] === "-" ? "-" : "";
  const yNum = parseInt(s.slice(yStart, yearEnd), 10);
  return { canon: `${sign}${String(yNum).padStart(4, "0")}-${String(mm).padStart(2, "0")}-${String(dd).padStart(2, "0")}` };
};
export const tDate = (v) => {
  if (typeof v !== "string") invalidType(v, "a formatted date string");
  const r = parseNaiveDate(v);
  if (r.msg) fail(r.msg);
  return r.canon;
};

const TS_FULL = /^([+-]?\d{4,})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?([Zz]|([+-])(\d{2}):(\d{2}))$/;
// Howard Hinnant days_from_civil — proleptic Gregorian.
const daysFromCivil = (y, m, d) => {
  y -= m <= 2;
  const era = Math.floor((y >= 0 ? y : y - 399) / 400);
  const yoe = y - era * 400;
  const doy = Math.floor((153 * (m > 2 ? m - 3 : m + 9) + 2) / 5) + d - 1;
  return era * 146097 + yoe * 365 + Math.floor(yoe / 4) - Math.floor(yoe / 100) + doy - 719468;
};
// chrono DateTime<Utc>::from_str (RFC 3339) — classify the failure text
// the way chrono does: "input is out of range" | "premature end of
// input" | "trailing input" | "input contains invalid characters".
// Returns { instant } (BigInt ns — chrono compares instants, not
// strings; leap :60.x == :59 + (1e9+frac) ns) or { msg }.
export const parseDateTime = (s) => {
  const m = TS_FULL.exec(s);
  if (m) {
    const [, y, mo, d, hh, mi, ss, frac, , tzsign, oh, om] = m;
    const yy = Math.abs(parseInt(y, 10));
    const leap = yy % 4 === 0 && (yy % 100 !== 0 || yy % 400 === 0);
    if (+mo < 1 || +mo > 12 || +d < 1 || +d > DIM[+mo - 1] + (mo === "02" && leap ? 1 : 0) ||
        +hh > 23 || +mi > 59 || +ss > 60 || (oh !== undefined && (+oh > 23 || +om > 59)))
      return { msg: "input is out of range" };
    const fracNs = frac ? BigInt(frac.slice(0, 9).padEnd(9, "0")) : 0n;
    const offNs = oh === undefined ? 0n
      : BigInt(+oh * 3600 + +om * 60) * 1_000_000_000n * (tzsign === "-" ? -1n : 1n);
    const days = daysFromCivil(parseInt(y, 10), +mo, +d);
    return { instant: BigInt(days) * 86_400_000_000_000n +
      BigInt(+hh * 3600 + +mi * 60 + +ss) * 1_000_000_000n + fracNs - offNs };
  }
  const prefix = /^([+-]?\d{4,}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}:\d{2}))/.exec(s);
  if (prefix && s.slice(prefix[0].length).trim() !== "") return { msg: "trailing input" };
  if (/^[+-]?\d{4,}-\d{2}-\d{2}([Tt ]\d{2}(:\d{2}(:\d{2}(\.\d*)?)?)?)?$/.test(s) ||
      /^\d{4}(-\d{0,2}){0,2}$/.test(s))
    return { msg: "premature end of input" };
  return { msg: "input contains invalid characters" };
};
export const tDateTime = (v) => {
  if (typeof v !== "string")
    invalidType(v, "an RFC 3339 formatted date and time string");
  const r = parseDateTime(v);
  if (r.msg) fail(r.msg);
  return r.instant;
};

// ------------------------------------------------------------ combinators
export const tOpt = (inner) => (v) => (v === null || v === undefined ? null : inner(v));
export const tVec = (inner) => (v) => {
  if (!isArr(v)) invalidType(v, "a sequence");
  return v.map(inner);
};
export const tMap = (inner) => (v) => {
  if (!isObj(v)) invalidType(v, "a map");
  const out = {};
  for (const k of Object.keys(v)) out[k] = inner(v[k]);
  return out;
};
// Pass-through Value decoder — for shapes owned by a later slice.
export const anyValue = (v) => v;

// serde content buffering: values inside an internally-tagged enum
// decode through serde's buffered Content deserializer, whose seq
// over-length message is "expected N element(s) in sequence" — the
// direct Value deserializer says "expected fewer elements in array".
// Depth counter so nested structs inherit the buffered phrasing.
let bufferedDepth = 0;
const inBuffered = () => bufferedDepth > 0;
const decBuffered = (dec, x) => {
  bufferedDepth++;
  try { return dec(x); } finally { bufferedDepth--; }
};

const plural = (n) => (n === 1 ? "element" : "elements");

// fields: [{name, dec, opt?, default?}] — opt = Option<T> (map-missing ->
// null; NOT defaulted in seq form); default = #[serde(default = ..)]
// (applies in both forms).
export const tStruct = (name, fields) => (v) => {
  if (isObj(v)) {
    const out = {};
    for (const k of Object.keys(v)) {
      const f = fields.find((x) => x.name === k);
      if (!f)
        fail(fields.length === 0
          ? `unknown field \`${k}\`, there are no fields`
          : `unknown field \`${k}\`, expected ${expectedFields(fields.map((x) => x.name))}`);
      out[f.name] = f.dec(v[k]);
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
      fail(`invalid length ${i}, expected struct ${name} with ${fields.length} ${plural(fields.length)}`);
    }
    if (v.length > i)
      fail(inBuffered()
        ? `invalid length ${v.length}, expected ${fields.length} ${plural(fields.length)} in sequence`
        : `invalid length ${v.length}, expected fewer elements in array`);
    return out;
  }
  invalidType(v, `struct ${name}`);
};

// Externally-tagged unit-only enum (Dataset, Exchange, ...). serde_json
// accepts "v" and {"v":null}; {"v":x} rejects non-unit payloads.
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

// Internally-tagged enum: variants = {wireName: {fields:[...]|null,
// rust: RustVariantName}} — null fields marks a unit variant (extra map
// keys ignored — serde quirk). Seq form `[tag, ...fields]`: element 0 is
// the variant identifier, the rest bind positionally — under-length ->
// "invalid length N, expected struct variant E::V with M elements";
// over -> "invalid length N, expected M element(s) in sequence"
// (remaining count); unit-variant extras -> "invalid length N, expected
// 1 element in sequence" (total count — enum-level arity check).
export const tTagged = (name, tag, variants) => (v) => {
  if (isArr(v)) {
    const tagVal = v[0];
    if (tagVal === undefined) fail(`missing field \`${tag}\``);
    if (typeof tagVal !== "string") invalidType(tagVal, "variant identifier");
    const names = Object.keys(variants);
    if (!names.includes(tagVal))
      fail(`unknown variant \`${tagVal}\`, expected ${expectedFields(names)}`);
    const varDef = variants[tagVal];
    if (varDef.fields === null) {
      if (v.length > 1)
        fail(`invalid length ${v.length - 1}, expected 0 elements in sequence`);
      return { [tag]: tagVal };
    }
    const fields = varDef.fields;
    const rest = v.slice(1);
    if (rest.length > fields.length)
      fail(`invalid length ${rest.length}, expected ${fields.length} ${plural(fields.length)} in sequence`);
    const out = { [tag]: tagVal };
    for (let i = 0; i < fields.length; i++) {
      const f = fields[i];
      if (i < rest.length) out[f.name] = decBuffered(f.dec, rest[i]);
      else if (f.default !== undefined) out[f.name] = f.default();
      else fail(`invalid length ${i}, expected struct variant ${name}::${varDef.rust} with ${fields.length} ${plural(fields.length)}`);
    }
    return out;
  }
  if (!isObj(v)) invalidType(v, `internally tagged enum ${name}`);
  const tagVal = v[tag];
  if (tagVal === undefined) fail(`missing field \`${tag}\``);
  if (typeof tagVal !== "string") invalidType(tagVal, "variant identifier");
  const names = Object.keys(variants);
  if (!names.includes(tagVal))
    fail(`unknown variant \`${tagVal}\`, expected ${expectedFields(names)}`);
  const fields = variants[tagVal].fields;
  if (fields === null) return { [tag]: tagVal }; // unit variant: extras ignored
  const out = { [tag]: tagVal };
  const allowed = new Set([tag, ...fields.map((f) => f.name)]);
  for (const k of Object.keys(v)) {
    if (k === tag) continue;
    if (!allowed.has(k))
      fail(`unknown field \`${k}\`, expected ${expectedFields(fields.map((f) => f.name))}`);
    out[k] = decBuffered(fields.find((f) => f.name === k).dec, v[k]);
  }
  for (const f of fields) {
    if (f.name in out) continue;
    if (f.opt) out[f.name] = null;
    else if (f.default !== undefined) out[f.name] = f.default();
    else fail(`missing field \`${f.name}\``);
  }
  return out;
};

// Entry point parity with serde_json::from_value::<T>(v) — Decode errors
// are the serde Display text verbatim.
export const decode = (ty, v) => ty(v);
