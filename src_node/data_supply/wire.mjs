// serde_json::from_value-compatible decoders for data_supply wire types
// (src/data_supply/wire.rs + the registry-local CapabilitiesQuery/
// WorkerInput/WorkerOutput structs).
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

import { F64, serF64 } from "../mcp/serde_json.mjs";

export class Decode extends Error {}
const fail = (msg) => { throw new Decode(msg); };

const isObj = (v) => v !== null && typeof v === "object" && !Array.isArray(v) && !(v instanceof F64);
const isArr = Array.isArray;

const rustStr = (s) =>
  '"' + s.replace(/[\\"\n\r\t]|[\x00-\x1f\x7f]/g, (c) => {
    if (c === '"') return '\\"';
    if (c === "\\") return "\\\\";
    if (c === "\n") return "\\n";
    if (c === "\r") return "\\r";
    if (c === "\t") return "\\t";
    return `\\u{${c.codePointAt(0).toString(16)}}`;
  }) + '"';
// serde de::Unexpected Display for a parsed Value
const desc = (v) =>
  v === null ? "null" :
  typeof v === "boolean" ? `boolean \`${v}\`` :
  v instanceof F64 ? `floating point \`${serF64(v.v)}\`` :
  typeof v === "number" || typeof v === "bigint" ? `integer \`${v}\`` :
  typeof v === "string" ? `string ${rustStr(v)}` :
  isArr(v) ? "sequence" : "map";
const invalidType = (v, expected) =>
  fail(`invalid type: ${desc(v)}, expected ${expected}`);
const expectedFields = (xs) =>
  xs.length === 1 ? `\`${xs[0]}\`` :
  xs.length === 2 ? `\`${xs[0]}\` or \`${xs[1]}\`` :
  `one of ${xs.map((x) => `\`${x}\``).join(", ")}`;

// ----------------------------------------------------------------- scalars
const tString = (v) => (typeof v === "string" ? v : invalidType(v, "a string"));
const tBool = (v) => (typeof v === "boolean" ? v : invalidType(v, "a boolean"));
const intKind = (v) =>
  typeof v === "bigint" ? v : typeof v === "number" && Number.isInteger(v) ? BigInt(v) : null;
const I64_MIN = -(1n << 63n);
const U64_MAX = (1n << 64n) - 1n;
const tInt = (name, lo, hi) => (v) => {
  const i = intKind(v);
  if (i !== null) {
    if (i >= lo && i <= hi) return Number(i);
    if (i >= I64_MIN) fail(`invalid value: integer \`${i}\`, expected ${name}`);
    invalidType(v, name);
  }
  invalidType(v, name);
};
const tU64 = tInt("u64", 0n, U64_MAX);
const tUsize = tInt("usize", 0n, U64_MAX);
const tI32 = tInt("i32", -(2n ** 31n), 2n ** 31n - 1n);

// chrono NaiveDate::from_str ("%Y-%m-%d") — sequential scanner matching
// chrono's parse order: optional sign + 1-4 digit year, '-', 1-2 digit
// month (range-checked immediately: "001" reads month 00 -> out of
// range), '-', 1-2 digit day; leftover -> "trailing input"; running out
// of text -> "premature end of input"; a non-digit/non-dash byte ->
// "input contains invalid characters".
const DIM = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const tDate = (v) => {
  if (typeof v !== "string") invalidType(v, "a formatted date string");
  const s = v;
  let i = 0;
  if (s === "") fail("premature end of input");
  if (s[i] === "+" || s[i] === "-") i++;
  const yStart = i;
  while (i < s.length && s[i] >= "0" && s[i] <= "9") i++;
  if (i === yStart) fail("input contains invalid characters");
  if (i - yStart > 4) fail("input contains invalid characters");
  const yearEnd = i;
  const yy = Math.abs(parseInt(s.slice(0, i), 10));
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
  const leap = yy % 4 === 0 && (yy % 100 !== 0 || yy % 400 === 0);
  if (dd < 1 || dd > DIM[mm - 1] + (mm === 2 && leap ? 1 : 0)) fail("input is out of range");
  if (i < s.length) fail("trailing input");
  // decode to the chrono NaiveDate Display form so downstream range/order
  // comparisons behave like date comparisons, not string comparisons:
  // "+2025-01-01" -> "2025-01-01", "999-01-01" -> "0999-01-01",
  // "-0001-01-01" -> "-0001-01-01" (negatives sort before year 0).
  const sign = s[0] === "-" ? "-" : "";
  const yNum = parseInt(s.slice(yStart, yearEnd), 10);
  const canon = `${sign}${String(yNum).padStart(4, "0")}-${String(mm).padStart(2, "0")}-${String(dd).padStart(2, "0")}`;
  return canon;
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
const tDateTime = (v) => {
  if (typeof v !== "string")
    invalidType(v, "an RFC 3339 formatted date and time string");
  const s = v;
  const m = TS_FULL.exec(s);
  if (m) {
    const [, y, mo, d, hh, mi, ss, frac, , tzsign, oh, om] = m;
    const yy = Math.abs(parseInt(y, 10));
    const leap = yy % 4 === 0 && (yy % 100 !== 0 || yy % 400 === 0);
    const dim = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if (+mo < 1 || +mo > 12 || +d < 1 || +d > dim[+mo - 1] + (mo === "02" && leap ? 1 : 0) ||
        +hh > 23 || +mi > 59 || +ss > 60 || (oh !== undefined && (+oh > 23 || +om > 59)))
      fail("input is out of range");
    // decode to the UTC instant (BigInt nanoseconds) — chrono compares
    // instants, not strings; leap :60.x is ordering-equivalent to
    // :59 + (1e9 + frac) ns, i.e. plain seconds arithmetic works.
    const fracNs = frac ? BigInt(frac.slice(0, 9).padEnd(9, "0")) : 0n;
    const offNs = oh === undefined ? 0n
      : BigInt(+oh * 3600 + +om * 60) * 1_000_000_000n * (tzsign === "-" ? -1n : 1n);
    const days = daysFromCivil(parseInt(y, 10), +mo, +d);
    return BigInt(days) * 86_400_000_000_000n +
      BigInt(+hh * 3600 + +mi * 60 + +ss) * 1_000_000_000n + fracNs - offNs;
  }
  const prefix = /^([+-]?\d{4,}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}:\d{2}))/.exec(s);
  if (prefix && s.slice(prefix[0].length).trim() !== "") fail("trailing input");
  if (/^[+-]?\d{4,}-\d{2}-\d{2}([Tt ]\d{2}(:\d{2}(:\d{2}(\.\d*)?)?)?)?$/.test(s) ||
      /^\d{4}(-\d{0,2}){0,2}$/.test(s))
    fail("premature end of input");
  fail("input contains invalid characters");
};

const tOpt = (inner) => (v) => (v === null || v === undefined ? null : inner(v));
const tVec = (inner) => (v) => {
  if (!isArr(v)) invalidType(v, "a sequence");
  return v.map(inner);
};

// fields: [{name, dec, opt?, default?}] — opt = Option<T> (map-missing ->
// null; NOT defaulted in seq form); default = #[serde(default = ..)]
// (applies in both forms).
const tStruct = (name, fields) => (v) => {
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
      fail(`invalid length ${i}, expected struct ${name} with ${fields.length} elements`);
    }
    if (v.length > i)
      fail(`invalid length ${v.length}, expected fewer elements in array`);
    return out;
  }
  invalidType(v, `struct ${name}`);
};

// Externally-tagged unit-only enum (Dataset, Exchange, ...). serde_json
// accepts "v" and {"v":null}; {"v":x} rejects non-unit payloads.
const tEnumUnit = (name, variants) => (v) => {
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

// Internally-tagged enum: variants = {wireName: {fields:[...]|null}} —
// null marks a unit variant (extra keys ignored — serde quirk).
const tTagged = (name, tag, variants) => (v) => {
  if (isArr(v)) fail(`missing field \`${tag}\``);
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
    out[k] = fields.find((f) => f.name === k).dec(v[k]);
  }
  for (const f of fields) {
    if (f.name in out) continue;
    if (f.opt) out[f.name] = null;
    else if (f.default !== undefined) out[f.name] = f.default();
    else fail(`missing field \`${f.name}\``);
  }
  return out;
};

// --------------------------------------------------------------- wire types
// Unit enums (serde rename/rename_all applied — wire names verbatim).
export const Dataset = tEnumUnit("Dataset", ["securities.catalog", "market.daily", "market.yearly"]);
export const ContractVersion = tEnumUnit("ContractVersion", ["v1"]);
export const Exchange = tEnumUnit("Exchange", ["sh", "sz"]);
export const CatalogScope = tEnumUnit("CatalogScope", ["current_sh_sz_a"]);
export const ManifestStatus = tEnumUnit("ManifestStatus", ["present", "absent"]);
export const TaskOperation = tEnumUnit("TaskOperation",
  ["start_collection", "verify_stopped_writes", "verify_publication_result"]);
export const TaskStatus = tEnumUnit("TaskStatus",
  ["pending", "running", "cancel_requested", "needs_verification",
   "succeeded", "partially_failed", "failed", "cancelled", "interrupted"]);
export const Availability = tEnumUnit("Availability",
  ["absent", "unknown", "unavailable", "available", "partial"]);
export const Readiness = tEnumUnit("Readiness", ["ready", "blocked"]);
export const RangeSelectionKind = tEnumUnit("RangeSelectionKind", ["default"]);

export const DATASETS = ["securities.catalog", "market.daily", "market.yearly"];
export const DATASET_DAILY = "market.daily";
export const DATASET_YEARLY = "market.yearly";
export const DATASET_CATALOG = "securities.catalog";
export const datasetPeriod = (d) =>
  d === DATASET_DAILY ? "day" : d === DATASET_YEARLY ? "year" : null;

export const Security = tStruct("Security", [
  { name: "exchange", dec: Exchange },
  { name: "code", dec: tString },
]);
// Security::symbol + Security::validate (wire.rs)
export const securitySymbol = (s) => `${s.exchange}.${s.code}`;
export const securityValidate = (s) =>
  s.code.length === 6 && [...s.code].every((b) => b >= "0" && b <= "9")
    ? null
    : "six digit security code required";
// Security Ord: derive(PartialOrd, Ord) — field order exchange then code;
// Exchange discriminants sh < sz (declaration order).
export const securityCmp = (a, b) =>
  (a.exchange === b.exchange ? 0 : a.exchange === "sh" ? -1 : 1) ||
  (a.code < b.code ? -1 : a.code > b.code ? 1 : 0);

export const Selection = tTagged("Selection", "kind", {
  single: { fields: [{ name: "entity", dec: Security }] },
  batch: { fields: [{ name: "entities", dec: tVec(Security) }] },
  catalog: { fields: [{ name: "scope", dec: CatalogScope }] },
});
export const Filter = tTagged("Filter", "kind", {
  daily: { fields: [{ name: "start_date", dec: tDate }, { name: "end_date", dec: tDate }] },
  yearly: { fields: [{ name: "start_year", dec: tI32 }, { name: "end_year", dec: tI32 }] },
  catalog: { fields: [
    { name: "code", dec: tOpt(tString), opt: true },
    { name: "name_query", dec: tOpt(tString), opt: true },
    { name: "industry_code", dec: tOpt(tString), opt: true },
  ] },
});
export const VersionBinding = tStruct("VersionBinding", [
  { name: "entity", dec: Security },
  { name: "version_id", dec: tString },
]);
export const VersionChoice = tTagged("VersionChoice", "kind", {
  latest_per_entity: { fields: null },
  explicit_version_map: { fields: [{ name: "versions", dec: tVec(VersionBinding) }] },
  catalog_version: { fields: [{ name: "version_id", dec: tString }] },
});
const defaultPage = () => 100;
export const Query = tStruct("Query", [
  { name: "dataset", dec: Dataset },
  { name: "contract_version", dec: ContractVersion },
  { name: "selection", dec: Selection },
  { name: "filter", dec: Filter },
  { name: "version", dec: VersionChoice },
  { name: "page_size", dec: tUsize, default: defaultPage },
  { name: "cursor", dec: tOpt(tString), opt: true },
]);
export const VersionsQuery = tStruct("VersionsQuery", [
  { name: "dataset", dec: Dataset },
  { name: "contract_version", dec: ContractVersion },
  { name: "selection", dec: Selection },
  { name: "page_size", dec: tUsize, default: defaultPage },
  { name: "cursor", dec: tOpt(tString), opt: true },
]);
export const CapabilitiesQuery = tStruct("CapabilitiesQuery", [
  { name: "dataset", dec: tOpt(Dataset), opt: true },
  { name: "operation", dec: tOpt(tString), opt: true },
]);
export const TasksQuery = tStruct("TasksQuery", [
  { name: "dataset", dec: tOpt(Dataset), opt: true },
  { name: "operation", dec: tOpt(TaskOperation), opt: true },
  { name: "status", dec: tOpt(tVec(TaskStatus)), opt: true },
  { name: "created_from", dec: tOpt(tDateTime), opt: true },
  { name: "created_to", dec: tOpt(tDateTime), opt: true },
  { name: "page_size", dec: tUsize, default: defaultPage },
  { name: "cursor", dec: tOpt(tString), opt: true },
]);
export const ProgressQuery = tStruct("ProgressQuery", [
  { name: "task_id", dec: tString },
  { name: "page_size", dec: tUsize, default: defaultPage },
  { name: "cursor", dec: tOpt(tString), opt: true },
]);
export const TaskV1 = tStruct("TaskV1", [
  { name: "contract_version", dec: ContractVersion },
  { name: "task_id", dec: tString },
]);
export const RequestV1 = tStruct("RequestV1", [
  { name: "contract_version", dec: ContractVersion },
  { name: "request_id", dec: tString },
]);
export const CancelV1 = tStruct("CancelV1", [
  { name: "contract_version", dec: ContractVersion },
  { name: "task_id", dec: tString },
  { name: "expected_revision", dec: tU64 },
]);
export const RetryItems = tStruct("RetryItems", [
  { name: "contract_version", dec: ContractVersion },
  { name: "original_task_id", dec: tString },
  { name: "new_request_id", dec: tString },
  { name: "item_ids", dec: tVec(tString) },
]);
export const WorkerInput = tStruct("WorkerInput", []);
export const InspectVerification = tStruct("InspectVerification", [
  { name: "contract_version", dec: ContractVersion },
  { name: "request_id", dec: tString },
  { name: "target_task_id", dec: tString },
]);

// Write-side request types
export const WriteSelection = tTagged("WriteSelection", "kind", {
  explicit: { fields: [{ name: "securities", dec: tVec(Security) }] },
  catalog: { fields: [
    { name: "version_id", dec: tString },
    { name: "industry_code", dec: tOpt(tString), opt: true },
  ] },
  default: { fields: null },
});
export const CatalogSelection = tStruct("CatalogSelection", [
  { name: "scope", dec: CatalogScope },
]);
export const YearlyPublication = tTagged("YearlyPublication", "mode", {
  historical_fixed_base: { fields: [
    { name: "comparison_version_id", dec: tOpt(tString), opt: true },
  ] },
  current_head: { fields: [
    { name: "expected_yearly_head", dec: tOpt(tString), opt: true },
  ] },
});
export const RangeWriteItem = tStruct("RangeWriteItem", [
  { name: "entity", dec: Security },
  { name: "base_version_id", dec: tString },
  { name: "start_date", dec: tDate },
  { name: "end_date", dec: tDate },
]);
export const YearlyWriteItem = tStruct("YearlyWriteItem", [
  { name: "entity", dec: Security },
  { name: "base_daily_version_id", dec: tString },
  { name: "years", dec: tVec(tI32) },
  { name: "publication", dec: YearlyPublication },
]);
export const RangeWriteSelection = tStruct("RangeWriteSelection", [
  { name: "kind", dec: RangeSelectionKind },
]);
export const Collection = tTagged("Collection", "dataset", {
  "market.daily": { fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "selection", dec: WriteSelection },
  ] },
  "securities.catalog": { fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "selection", dec: CatalogSelection },
  ] },
  "market.yearly": { fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "selection", dec: WriteSelection },
    { name: "base_daily_versions", dec: tVec(VersionBinding) },
    { name: "years", dec: tVec(tI32) },
  ] },
});
export const RangeWrite = tTagged("RangeWrite", "dataset", {
  "market.daily": { fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "items", dec: tOpt(tVec(RangeWriteItem)), opt: true },
    { name: "selection", dec: tOpt(RangeWriteSelection), opt: true },
  ] },
  "market.yearly": { fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "items", dec: tVec(YearlyWriteItem) },
  ] },
});
export const RepublishWrite = tTagged("RepublishWrite", "dataset", {
  "market.daily": { fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "items", dec: tVec(RangeWriteItem) },
  ] },
});

// Output-side types — decoders ported for completeness (deny_unknown_fields
// parity is symmetric), though the skeleton never deserializes them.
export const VersionRef = tStruct("VersionRef", [
  { name: "dataset", dec: Dataset },
  { name: "contract_version", dec: ContractVersion },
  { name: "entity", dec: tOpt(Security), opt: true },
  { name: "version_id", dec: tString },
  { name: "integrity_sha256", dec: tString },
]);
export const Industry = tTagged("Industry", "status", {
  assigned: { fields: [{ name: "code", dec: tString }, { name: "name", dec: tString }] },
  missing: { fields: [{ name: "reason", dec: tString }] },
  conflicted: { fields: [{ name: "reason", dec: tString }] },
});
export const Evidence = tStruct("Evidence", [
  { name: "field", dec: tString },
  { name: "status", dec: tString },
  { name: "value", dec: tOpt(tString), opt: true },
  { name: "reason", dec: tString },
]);
export const Lineage = tStruct("Lineage", [
  { name: "base_daily_version_id", dec: tString },
  { name: "algorithm_version", dec: tString },
  { name: "first_trading_date", dec: tDate },
  { name: "last_trading_date", dec: tDate },
]);
export const Metadata = tStruct("Metadata", [
  { name: "source", dec: tString },
  { name: "published_at", dec: tDateTime },
  { name: "coverage_status", dec: tString },
  { name: "coverage_reason", dec: tString },
  { name: "units", dec: tVec(Evidence) },
  { name: "lineage", dec: tOpt(Lineage), opt: true },
]);
export const ManifestEntry = tStruct("ManifestEntry", [
  { name: "entity", dec: tOpt(Security), opt: true },
  { name: "version_ref", dec: tOpt(VersionRef), opt: true },
  { name: "status", dec: ManifestStatus },
  { name: "metadata", dec: tOpt(Metadata), opt: true },
]);
export const Row = tTagged("Row", "kind", {
  market: { fields: [
    { name: "entity", dec: Security },
    { name: "version_ref", dec: VersionRef },
    { name: "date", dec: tDate },
    { name: "first_trading_date", dec: tOpt(tDate), opt: true },
    { name: "last_trading_date", dec: tOpt(tDate), opt: true },
    { name: "year_unfinished", dec: tOpt(tBool), opt: true },
    { name: "open", dec: tString },
    { name: "high", dec: tString },
    { name: "low", dec: tString },
    { name: "close", dec: tString },
    { name: "volume", dec: tOpt(tString), opt: true },
    { name: "amount", dec: tOpt(tString), opt: true },
  ] },
  catalog: { fields: [
    { name: "entity", dec: Security },
    { name: "version_ref", dec: VersionRef },
    { name: "name", dec: tString },
    { name: "industry", dec: Industry },
  ] },
});
export const DataPage = tStruct("DataPage", [
  { name: "dataset", dec: Dataset },
  { name: "contract_version", dec: ContractVersion },
  { name: "snapshot_id", dec: tString },
  { name: "query_time", dec: tDateTime },
  { name: "version_manifest", dec: tVec(ManifestEntry) },
  { name: "records", dec: tVec(Row) },
  { name: "next_cursor", dec: tOpt(tString), opt: true },
  { name: "page_complete", dec: tBool },
  { name: "selection_complete", dec: tBool },
  { name: "limitations", dec: tVec(tString) },
]);
export const VersionInfo = tStruct("VersionInfo", [
  { name: "version_ref", dec: VersionRef },
  { name: "published_at", dec: tDateTime },
]);
export const VersionPage = tStruct("VersionPage", [
  { name: "snapshot_id", dec: tString },
  { name: "snapshot_time", dec: tDateTime },
  { name: "items", dec: tVec(VersionInfo) },
  { name: "next_cursor", dec: tOpt(tString), opt: true },
  { name: "page_complete", dec: tBool },
]);
export const TaskSummary = tStruct("TaskSummary", [
  { name: "task_id", dec: tString },
  { name: "request_id", dec: tString },
  { name: "dataset", dec: tString },
  { name: "operation", dec: tString },
  { name: "contract_version", dec: tString },
  { name: "status", dec: TaskStatus },
  { name: "state_revision", dec: tU64 },
  { name: "created_at", dec: tDateTime },
  { name: "last_progress_at", dec: tDateTime },
]);
export const ProgressItem = tStruct("ProgressItem", [
  { name: "item_id", dec: tString },
  { name: "entity", dec: tOpt(Security), opt: true },
  { name: "period", dec: tOpt(tString), opt: true },
  { name: "progress_unit", dec: tString },
  { name: "stage", dec: tString },
  { name: "success", dec: tOpt(tBool), opt: true },
  { name: "error", dec: tOpt(tString), opt: true },
]);
export const TaskListPage = tStruct("TaskListPage", [
  { name: "snapshot_id", dec: tString },
  { name: "snapshot_time", dec: tDateTime },
  { name: "items", dec: tVec(TaskSummary) },
  { name: "next_cursor", dec: tOpt(tString), opt: true },
  { name: "page_complete", dec: tBool },
]);
export const BlockingReason = tStruct("BlockingReason", [
  { name: "code", dec: tString },
  { name: "detail", dec: tString },
  { name: "evidence_ref", dec: tOpt(tString), opt: true },
]);
export const WriteAcceptance = tStruct("WriteAcceptance", [
  { name: "request_id", dec: tString },
  { name: "task_id", dec: tString },
  { name: "dataset", dec: tString },
  { name: "contract_version", dec: tString },
  { name: "reused", dec: tBool },
  { name: "status", dec: TaskStatus },
  { name: "item_ids", dec: tVec(tString) },
  { name: "runtime_readiness", dec: Readiness },
  { name: "blocking_reasons", dec: tVec(BlockingReason) },
]);
export const ConfirmedGap = tStruct("ConfirmedGap", [
  { name: "range", dec: Filter },
  { name: "evidence", dec: tString },
]);
export const ComparisonVersion = tStruct("ComparisonVersion", [
  { name: "version_id", dec: tString },
  { name: "integrity_sha256", dec: tString },
]);
export const FrozenDailyRange = tStruct("FrozenDailyRange", [
  { name: "item_id", dec: tString },
  { name: "entity", dec: Security },
  { name: "base_version_id", dec: tString },
  { name: "base_integrity_sha256", dec: tString },
  { name: "start_date", dec: tDate },
  { name: "end_date", dec: tDate },
]);
export const FrozenYearlyInput = tStruct("FrozenYearlyInput", [
  { name: "item_id", dec: tString },
  { name: "entity", dec: Security },
  { name: "base_daily_version_id", dec: tString },
  { name: "base_integrity_sha256", dec: tString },
  { name: "years", dec: tVec(tI32) },
  { name: "publication", dec: YearlyPublication },
  { name: "comparison", dec: tOpt(ComparisonVersion), opt: true },
]);
export const RangeExecutionResult = tStruct("RangeExecutionResult", [
  { name: "result_id", dec: tString },
  { name: "item_id", dec: tString },
  { name: "base_version", dec: VersionRef },
  { name: "result_version", dec: VersionRef },
  { name: "no_change", dec: tBool },
  { name: "acquired_at", dec: tDateTime },
]);
export const YearlyExecutionResult = tStruct("YearlyExecutionResult", [
  { name: "result_id", dec: tString },
  { name: "item_id", dec: tString },
  { name: "base_daily_version", dec: VersionRef },
  { name: "result_version", dec: VersionRef },
  { name: "no_change", dec: tBool },
  { name: "derived_at", dec: tDateTime },
]);
export const RequestView = tStruct("RequestView", [
  { name: "request_id", dec: tString },
  { name: "bound", dec: tBool },
  { name: "task", dec: tOpt(TaskSummary), opt: true },
  { name: "terminal_result", dec: tOpt((v) => v), opt: true }, // untagged — see note
]);
export const Cancellation = tStruct("Cancellation", [
  { name: "task_id", dec: tString },
  { name: "status", dec: TaskStatus },
  { name: "state_revision", dec: tU64 },
]);
export const AvailabilityItem = tStruct("AvailabilityItem", [
  { name: "entity", dec: tOpt(Security), opt: true },
  { name: "version_ref", dec: tOpt(VersionRef), opt: true },
  { name: "availability", dec: Availability },
  { name: "integrity", dec: tString },
  { name: "confirmed_gaps", dec: tVec(ConfirmedGap) },
  { name: "unresolved_ranges", dec: tVec(Filter) },
  { name: "coverage_evidence", dec: tString },
  { name: "first_record", dec: tOpt(tDate), opt: true },
  { name: "last_record", dec: tOpt(tDate), opt: true },
  { name: "record_count", dec: tUsize },
  { name: "freshness", dec: tString },
  { name: "related_tasks", dec: tVec(TaskSummary) },
  { name: "limitations", dec: tVec(tString) },
]);
export const AvailabilityPage = tStruct("AvailabilityPage", [
  { name: "snapshot_id", dec: tString },
  { name: "snapshot_time", dec: tDateTime },
  { name: "items", dec: tVec(AvailabilityItem) },
  { name: "next_cursor", dec: tOpt(tString), opt: true },
  { name: "page_complete", dec: tBool },
]);
export const WorkerOutput = tStruct("WorkerOutput", [
  { name: "idle", dec: tBool },
  { name: "task_id", dec: tOpt(tString), opt: true },
  { name: "status", dec: tOpt(TaskStatus), opt: true },
]);
// ProgressPage.stopped_write_check / publication_check are cross-module
// Receipt types (task_verification::decision / publication_verification::
// decision) — not ported here; decode as any-value placeholders only to
// keep the deny_unknown_fields surface faithful at the top level.
const anyValue = (v) => v;
export const ProgressPage = tStruct("ProgressPage", [
  { name: "snapshot_id", dec: tString },
  { name: "snapshot_time", dec: tDateTime },
  { name: "task", dec: TaskSummary },
  { name: "total", dec: tUsize },
  { name: "finished", dec: tUsize },
  { name: "succeeded", dec: tUsize },
  { name: "failed", dec: tUsize },
  { name: "pending", dec: tUsize },
  { name: "running", dec: tUsize },
  { name: "execution_percent", dec: tString },
  { name: "unfinished", dec: tVec(ProgressItem) },
  { name: "next_cursor", dec: tOpt(tString), opt: true },
  { name: "page_complete", dec: tBool },
  { name: "publication_versions", dec: tVec(VersionRef) },
  { name: "frozen_daily_ranges", dec: tVec(FrozenDailyRange) },
  { name: "range_results", dec: tVec(RangeExecutionResult) },
  { name: "frozen_yearly_inputs", dec: tVec(FrozenYearlyInput) },
  { name: "yearly_results", dec: tVec(YearlyExecutionResult) },
  { name: "stopped_write_check", dec: tOpt(anyValue), opt: true },
  { name: "publication_check", dec: tOpt(anyValue), opt: true },
]);

// Cross-module request types decoded by DataApi dispatch (their owning
// slices land later; the decode shape is pinned by the oracle corpus).
// task_verification::wire::VerifyStopped / publication_verification::
// wire::VerifyPublication share {contract_version, request_id,
// target_task_id}; gap_verification::wire::VerifyDataGap adds `target`.
export const VerifyV1 = tStruct("VerifyV1", [
  { name: "contract_version", dec: ContractVersion },
  { name: "request_id", dec: tString },
  { name: "target_task_id", dec: tString },
]);
export const VerifyGapPeriod = tEnumUnit("VerifyGapPeriod", ["day"]);
export const VerifyDataGap = tStruct("VerifyDataGap", [
  { name: "contract_version", dec: ContractVersion },
  { name: "request_id", dec: tString },
  { name: "target", dec: tStruct("VerifyGapTarget", [
    { name: "symbol", dec: tString },
    { name: "period", dec: VerifyGapPeriod },
    { name: "start_date", dec: tDate },
    { name: "end_date", dec: tDate },
  ]) },
]);
// metrics::wire request types — decoded by compute_metrics dispatch
// (the compute execution itself is a later slice; the decode shape is
// pinned by the oracle corpus).
export const Execution = tEnumUnit("Execution", ["inline"]);
export const MetricName = tEnumUnit("MetricName", ["consecutive_direction"]);
export const Algorithm = tEnumUnit("Algorithm", ["local-consecutive-direction-v1"]);
export const Direction = tEnumUnit("Direction", ["up", "down"]);
export const PriceField = tEnumUnit("PriceField", ["close"]);
export const EqualPolicy = tEnumUnit("EqualPolicy", ["break"]);
export const GapPolicy = tEnumUnit("GapPolicy", ["reject"]);
export const YearlyDataset = tEnumUnit("YearlyDataset", ["market.yearly"]);
export const YearlyVersionRef = tStruct("YearlyVersionRef", [
  { name: "dataset", dec: YearlyDataset },
  { name: "contract_version", dec: ContractVersion },
  { name: "entity", dec: Security },
  { name: "version_id", dec: tString },
  { name: "integrity_sha256", dec: tString },
]);
export const ComputeInput = tStruct("Input", [
  { name: "input_id", dec: tString },
  { name: "version_ref", dec: YearlyVersionRef },
]);
export const Years = tStruct("Years", [
  { name: "start_year", dec: tI32 },
  { name: "end_year", dec: tI32 },
]);
export const Parameters = tStruct("Parameters", [
  { name: "direction", dec: Direction },
  { name: "min_periods", dec: tUsize },
  { name: "price_field", dec: PriceField, default: () => "close" },
  { name: "completed_only", dec: tBool, default: () => true },
  { name: "equal_policy", dec: EqualPolicy, default: () => "break" },
  { name: "gap_policy", dec: GapPolicy, default: () => "reject" },
]);
export const Metric = tStruct("Metric", [
  { name: "metric_id", dec: tString },
  { name: "name", dec: MetricName },
  { name: "algorithm_version", dec: Algorithm },
  { name: "input_ids", dec: tVec(tString) },
  { name: "parameters", dec: Parameters },
]);
export const Compute = tStruct("Compute", [
  { name: "contract_version", dec: ContractVersion },
  { name: "inputs", dec: tVec(ComputeInput) },
  { name: "range", dec: Years },
  { name: "metrics", dec: tVec(Metric) },
  { name: "execution", dec: Execution },
]);
// Output-side metrics types (decode parity; never emitted by the seam).
export const Point = tStruct("Point", [
  { name: "year", dec: tI32 },
  { name: "first_trading_date", dec: tDate },
  { name: "last_trading_date", dec: tDate },
  { name: "close", dec: tString },
]);
export const ObservedDirection = tEnumUnit("ObservedDirection", ["up", "down", "flat"]);
export const RunKind = tEnumUnit("RunKind", ["exact", "lower_bound"]);
export const Reason = tEnumUnit("Reason",
  ["window_too_short", "missing_year", "unknown_completion",
   "incomplete_period", "non_positive_price"]);
export const Precision = tStruct("Precision", [
  { name: "scale", dec: tInt("u32", 0n, (1n << 32n) - 1n) },
  { name: "rounding", dec: tString },
  { name: "unit", dec: tString },
]);
export const Outcome = tTagged("Outcome", "outcome", {
  computed: { fields: [
    { name: "observed_direction", dec: ObservedDirection },
    { name: "run_length", dec: tUsize },
    { name: "run_length_kind", dec: RunKind },
    { name: "matches", dec: tOpt(tBool), opt: true },
    { name: "anchor", dec: tOpt(Point), opt: true },
    { name: "first_change", dec: tOpt(Point), opt: true },
    { name: "end", dec: Point },
    { name: "cumulative_change", dec: tOpt(tString), opt: true },
    { name: "precision", dec: Precision },
    { name: "observations_used", dec: tUsize },
    { name: "continuity_evidence", dec: tString },
    { name: "actual_input_range", dec: Years },
    { name: "excluded_tail", dec: tVec(tI32) },
    { name: "provisional", dec: tBool },
    { name: "limitations", dec: tVec(tString) },
  ] },
  not_computable: { fields: [
    { name: "reason", dec: Reason },
    { name: "excluded_tail", dec: tVec(tI32) },
    { name: "limitations", dec: tVec(tString) },
  ] },
});
export const Computation = tStruct("Computation", [
  { name: "contract_version", dec: ContractVersion },
  { name: "input", dec: ComputeInput },
  { name: "metric", dec: Metric },
  { name: "requested_range", dec: Years },
  { name: "result", dec: Outcome },
]);
