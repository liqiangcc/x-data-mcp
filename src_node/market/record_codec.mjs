// NM-050 — port of market_storage/compat.rs + publish.rs validators +
// market_data.rs typed surface: legacy_digest, check, EffectiveVersion/
// MarketDataBatch serde behavior, validate_and_deduplicate,
// validate_unit_currency_evidence, validate_adjustment_evidence.
import { createHash } from "node:crypto";

export const DIGEST_FORMAT = "legacy-published-record-v1";
export const MAX_RECORDS = 100_000;

export class MarketError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = "MarketError";
    this.kind = kind; // integrity | invalid | storage | conflict | not_found | outcome_unknown | basis_not_verified
    this.code = kind === "integrity" ? "inconsistent"
      : kind === "invalid" ? "invalid_input"
      : kind === "conflict" ? "conflict"
      : kind === "storage" ? "unavailable" : kind;
  }
}
const integrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const invalid = (m) => new MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);
// PublishError Display texts -> Integrity("<display>") at check()
const publishInvalid = (m) => `invalid market data record: ${m}`;
const publishDup = (m) => `conflicting duplicate market data key: ${m}`;
const publishUnit = (m) => `unit/currency evidence is not confirmed: ${m}`;
const publishAdj = (m) => `adjustment evidence is not confirmed: ${m}`;

// ---------------------------------------------------------------------
// serde_json::to_vec parity for arbitrary Values: object keys are a
// BTreeMap (sorted), strings escape only control/quote/backslash.
const sortSer = (v) => {
  if (v === null || v === undefined) return "null";
  if (typeof v !== "object") return JSON.stringify(v);
  if (Array.isArray(v)) return `[${v.map(sortSer).join(",")}]`;
  return `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${sortSer(v[k])}`).join(",")}}`;
};

export const legacyDigest = (raw) => {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    throw integrity("record must be object");
  }
  const copy = { ...raw };
  delete copy.integrity_sha256;
  return createHash("sha256").update(sortSer(copy), "utf8").digest("hex");
};

// ---------------------------------------------------------------------
// Serde-faithful typed parse (market_data.rs declarations verbatim;
// Option<T> fields default to null when absent, #[serde(default)] uses
// the declared default, unknown fields are ignored).
const serdeMissing = (f) => `missing field \`${f}\``;
const serdeVariant = (v, exp) =>
  `unknown variant \`${v}\`, expected ${exp.length === 2
    ? exp.map((x) => `\`${x}\``).join(" or ")
    : `one of ${exp.map((x) => `\`${x}\``).join(", ")}`}`;
const serdeType = (got, want) => `invalid type: ${got}, expected ${want}`;
const typeName = (v) =>
  v === null ? "null" : Array.isArray(v) ? "a sequence" :
    typeof v === "number" ? (Number.isInteger(v) ? `integer \`${v}\`` : `floating point \`${v}\``) :
    typeof v === "string" ? `a string` : `a map`;

const req = (o, f) => {
  if (!(f in o)) throw new TypeError(serdeMissing(f));
  return o[f];
};
const reqStr = (o, f) => {
  const v = req(o, f);
  if (typeof v !== "string") throw new TypeError(serdeType(typeName(v), "a string"));
  return v;
};
const optStr = (o, f) => {
  if (!(f in o) || o[f] === null) return null;
  if (typeof o[f] !== "string") throw new TypeError(serdeType(typeName(o[f]), "a string"));
  return o[f];
};
const reqEnum = (o, f, variants) => {
  const v = reqStr(o, f);
  if (!variants.includes(v)) throw new TypeError(serdeVariant(v, variants));
  return v;
};
const reqInt = (o, f) => {
  const v = req(o, f);
  if (typeof v !== "number" || !Number.isInteger(v) || v < 0) {
    throw new TypeError(serdeType(typeName(v), "u64"));
  }
  return v;
};
const reqBool = (o, f) => {
  const v = req(o, f);
  if (typeof v !== "boolean") throw new TypeError(serdeType(typeName(v), "a boolean"));
  return v;
};

// chrono NaiveDate — strict %Y-%m-%d, real calendar day
const DATE_RE = /^(\d{4})-(\d{2})-(\d{2})$/;
const parseDate = (v) => {
  if (typeof v !== "string") throw new TypeError(serdeType(typeName(v), "a date string"));
  const m = DATE_RE.exec(v);
  if (!m) throw new TypeError("input contains invalid characters");
  const [_, y, mo, d] = m.map(Number);
  const dt = new Date(Date.UTC(y, mo - 1, d));
  if (dt.getUTCFullYear() !== y || dt.getUTCMonth() !== mo - 1 || dt.getUTCDate() !== d) {
    throw new TypeError("input is out of range");
  }
  return { y, mo, d, s: v };
};
// days from 0001-01-01 (chrono num_days_from_ce)
const daysFromCe = ({ y, mo, d }) => {
  const a = Math.floor((14 - mo) / 12);
  const yy = y + 4800 - a;
  const mm = mo + 12 * a - 3;
  const jdn = d + Math.floor((153 * mm + 2) / 5) + 365 * yy + Math.floor(yy / 4) -
    Math.floor(yy / 100) + Math.floor(yy / 400) - 32045;
  return jdn - 1721425; // JDN of 0001-01-01 is 1721426; ce = jdn-1721425
};
const RFC3339 = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$/;
const reqTimestamp = (o, f) => {
  const v = reqStr(o, f);
  if (!RFC3339.test(v)) throw new TypeError("input contains invalid characters");
  return v;
};

// rust_decimal serde (string or number); normalize to a canonical
// sign+int+frac string so Decimal PartialEq is value equality.
const DEC_RE = /^[+-]?\d+(\.\d+)?([eE][+-]?\d+)?$/;
const decNorm = (v) => {
  let s;
  if (typeof v === "string") s = v;
  else if (typeof v === "number" && Number.isFinite(v)) s = String(v);
  else throw new TypeError(serdeType(typeName(v), "a decimal"));
  if (!DEC_RE.test(s)) throw new TypeError("invalid decimal");
  let neg = s.startsWith("-");
  s = s.replace(/^[+-]/, "");
  let [int, frac = ""] = s.split(".");
  if (s.includes("e") || s.includes("E")) {
    // expand exponent into plain digits
    const e = parseInt(s.slice(s.search(/[eE]/) + 1), 10);
    const mant = s.slice(0, s.search(/[eE]/));
    const [mi, mf = ""] = mant.split(".");
    const digits = (mi + mf).replace(/^0+/, "") || "0";
    const point = mi.length + e;
    if (point <= 0) { int = "0"; frac = "0".repeat(-point) + digits; }
    else if (point >= digits.length) { int = digits + "0".repeat(point - digits.length); frac = ""; }
    else { int = digits.slice(0, point); frac = digits.slice(point); }
  }
  int = int.replace(/^0+/, "") || "0";
  frac = frac.replace(/0+$/, "");
  if (int === "0" && frac === "") neg = false;
  return { neg, int, frac };
};
const cmpDec = (a, b) => {
  const x = decNorm(a), y = decNorm(b);
  if (x.neg !== y.neg) return x.neg ? -1 : 1;
  const mag = x.int.length !== y.int.length ? x.int.length - y.int.length
    : x.int !== y.int ? (x.int < y.int ? -1 : 1)
    : x.frac !== y.frac ? (x.frac < y.frac ? -1 : 1) : 0;
  return x.neg ? -mag : mag;
};
const decEq = (a, b) => cmpDec(a, b) === 0;
const reqDec = (o, f) => {
  const v = req(o, f);
  decNorm(v); // validity check
  return v;
};
const optDec = (o, f) => {
  if (!(f in o) || o[f] === null) return null;
  decNorm(o[f]);
  return o[f];
};

const PERIODS = ["Day", "Year"];
const ADJUSTMENTS = ["Forward"];
const COVERAGE = ["Complete", "Incomplete", "Unknown"];
const EVIDENCE_STATUS = ["Confirmed", "Unknown"];

const parseEvidence = (v, f) => {
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    throw new TypeError(serdeType(typeName(v), `struct ${f}`));
  }
  return {
    status: reqEnum(v, "status", EVIDENCE_STATUS),
    value: optStr(v, "value"),
    reason: reqStr(v, "reason"),
  };
};
const isConfirmed = (e) => e.status === "Confirmed" && typeof e.value === "string" && e.value !== "";
const EVIDENCE_DEFAULT = { status: "Unknown", value: null, reason: "unit/currency evidence was not supplied" };

const parseUnitCurrency = (v, f) => {
  if (v === undefined) {
    const d = () => ({ ...EVIDENCE_DEFAULT });
    return { price_currency: d(), price_unit: d(), volume_unit: d(), amount_currency: d(), amount_unit: d() };
  }
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    throw new TypeError(serdeType(typeName(v), `struct ${f}`));
  }
  return {
    price_currency: parseEvidence(req(v, "price_currency"), "EvidenceValue"),
    price_unit: parseEvidence(req(v, "price_unit"), "EvidenceValue"),
    volume_unit: parseEvidence(req(v, "volume_unit"), "EvidenceValue"),
    amount_currency: parseEvidence(req(v, "amount_currency"), "EvidenceValue"),
    amount_unit: parseEvidence(req(v, "amount_unit"), "EvidenceValue"),
  };
};
const parseAdjustmentEvidence = (v) => {
  if (v === undefined) {
    return { status: "Unknown", value: null, reason: "adjustment evidence was not supplied" };
  }
  return parseEvidence(v, "AdjustmentEvidence");
};
const parseCoverage = (v) => {
  if (v === undefined) {
    return { status: "Unknown", reason: "coverage evidence was not supplied" };
  }
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    throw new TypeError(serdeType(typeName(v), "struct CoverageEvidence"));
  }
  return { status: reqEnum(v, "status", COVERAGE), reason: reqStr(v, "reason") };
};
const parseBar = (v) => {
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    throw new TypeError(serdeType(typeName(v), "struct Bar"));
  }
  const r7 = v.r7_raw;
  if (r7 !== undefined && r7 !== null &&
      (typeof r7 !== "number" || !Number.isInteger(r7) || r7 < 0 || r7 > 4294967295)) {
    throw new TypeError(serdeType(typeName(r7), "u32"));
  }
  return {
    date: parseDate(req(v, "date")),
    open: reqDec(v, "open"), high: reqDec(v, "high"),
    low: reqDec(v, "low"), close: reqDec(v, "close"),
    volume: optDec(v, "volume"), amount: optDec(v, "amount"),
    r7_raw: r7 ?? null,
  };
};
const parseYearlyDerivation = (v) => {
  if (v === undefined || v === null) return null;
  return {
    base_daily_version_id: reqStr(v, "base_daily_version_id"),
    algorithm_version: reqStr(v, "algorithm_version"),
    first_trading_date: parseDate(req(v, "first_trading_date")),
    last_trading_date: parseDate(req(v, "last_trading_date")),
  };
};
const parseProvenance = (v) => {
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    throw new TypeError(serdeType(typeName(v), "struct Provenance"));
  }
  return {
    source: reqStr(v, "source"), source_period: reqStr(v, "source_period"),
    source_adjustment: reqStr(v, "source_adjustment"), symbol: reqStr(v, "symbol"),
    acquired_at: reqTimestamp(v, "acquired_at"),
    adapter_version: reqStr(v, "adapter_version"),
    yearly_derivation: parseYearlyDerivation(v.yearly_derivation),
    snapshot_inputs: v.snapshot_inputs === undefined || v.snapshot_inputs === null ? null :
      v.snapshot_inputs.map((x) => ({
        role: reqStr(x, "role"), member: reqStr(x, "member"),
        archive_sha256: reqStr(x, "archive_sha256"), member_sha256: reqStr(x, "member_sha256"),
      })),
  };
};
const parseBatch = (v) => {
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    throw new TypeError(serdeType(typeName(v), "struct MarketDataBatch"));
  }
  const bars = req(v, "bars");
  if (!Array.isArray(bars)) throw new TypeError(serdeType(typeName(bars), "a sequence"));
  return {
    symbol: reqStr(v, "symbol"),
    period: reqEnum(v, "period", PERIODS),
    adjustment: reqEnum(v, "adjustment", ADJUSTMENTS),
    bars: bars.map(parseBar),
    provenance: parseProvenance(req(v, "provenance")),
    coverage: parseCoverage(v.coverage),
    unit_currency: parseUnitCurrency(v.unit_currency, "UnitCurrencyEvidence"),
    adjustment_evidence: parseAdjustmentEvidence(v.adjustment_evidence),
  };
};
const parseVersion = (v) => {
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    throw new TypeError(serdeType(typeName(v), "struct EffectiveVersion"));
  }
  const records = req(v, "records");
  if (!Array.isArray(records)) throw new TypeError(serdeType(typeName(records), "a sequence"));
  const val = req(v, "validation");
  return {
    version_id: reqStr(v, "version_id"), symbol: reqStr(v, "symbol"),
    period: reqEnum(v, "period", PERIODS),
    adjustment: reqEnum(v, "adjustment", ADJUSTMENTS),
    source: reqStr(v, "source"),
    records: records.map(parseBar),
    published_at: reqTimestamp(v, "published_at"),
    validation: {
      record_count: reqInt(val, "record_count"),
      unique_record_count: reqInt(val, "unique_record_count"),
      duplicate_count: reqInt(val, "duplicate_count"),
    },
    unit_currency: parseUnitCurrency(v.unit_currency, "UnitCurrencyEvidence"),
    adjustment_evidence: parseAdjustmentEvidence(v.adjustment_evidence),
    yearly_derivation: parseYearlyDerivation(v.yearly_derivation),
  };
};

// ---------------------------------------------------------------------
// publish.rs validators — PublishError Display text verbatim.
const barEq = (a, b) =>
  a.date.s === b.date.s && decEq(a.open, b.open) && decEq(a.high, b.high) &&
  decEq(a.low, b.low) && decEq(a.close, b.close) &&
  (a.volume === null ? b.volume === null : b.volume !== null && decEq(a.volume, b.volume)) &&
  (a.amount === null ? b.amount === null : b.amount !== null && decEq(a.amount, b.amount)) &&
  a.r7_raw === b.r7_raw;
const recordKey = (period, bar) => period === "Day" ? bar.date.s : String(bar.date.y);

export const validateAndDeduplicate = (batch) => {
  if (batch.bars.length === 0) {
    throw integrity(publishInvalid("batch has no records"));
  }
  const records = [];
  const seen = new Map();
  let duplicate_count = 0;
  for (const bar of batch.bars) {
    if (bar.date.y <= 0) throw integrity(publishInvalid("record has no natural year/date"));
    if (cmpDec(bar.high, bar.low) < 0) throw integrity(publishInvalid("high is below low"));
    if (cmpDec(bar.open, bar.low) < 0 || cmpDec(bar.open, bar.high) > 0) {
      throw integrity(publishInvalid("open is outside low/high"));
    }
    if (cmpDec(bar.close, bar.low) < 0 || cmpDec(bar.close, bar.high) > 0) {
      throw integrity(publishInvalid("close is outside low/high"));
    }
    if (bar.volume !== null && cmpDec(bar.volume, "0") < 0) {
      throw integrity(publishInvalid("volume is negative"));
    }
    if (bar.amount !== null && cmpDec(bar.amount, "0") < 0) {
      throw integrity(publishInvalid("amount is negative"));
    }
    const key = batch.period === "Day" ? daysFromCe(bar.date) : bar.date.y;
    const idx = seen.get(key);
    if (idx !== undefined) {
      if (!barEq(records[idx], bar)) {
        throw integrity(publishDup(recordKey(batch.period, bar)));
      }
      duplicate_count++;
    } else {
      seen.set(key, records.length);
      records.push(bar);
    }
  }
  records.sort((a, b2) => {
    const ka = recordKey(batch.period, a), kb = recordKey(batch.period, b2);
    return ka < kb ? -1 : ka > kb ? 1 : 0;
  });
  return {
    bars: records,
    validation: {
      record_count: batch.bars.length,
      unique_record_count: records.length,
      duplicate_count,
    },
  };
};

export const validateUnitCurrencyEvidence = (batch) => {
  const need = (field, e) => {
    if (!isConfirmed(e)) throw integrity(publishUnit(`${field}: ${e.reason}`));
  };
  need("price currency", batch.unit_currency.price_currency);
  need("price unit", batch.unit_currency.price_unit);
  if (batch.bars.some((b) => b.volume !== null)) {
    need("volume unit", batch.unit_currency.volume_unit);
  }
  if (batch.bars.some((b) => b.amount !== null)) {
    need("amount currency", batch.unit_currency.amount_currency);
    need("amount unit", batch.unit_currency.amount_unit);
  }
};

export const validateAdjustmentEvidence = (batch) => {
  const e = batch.adjustment_evidence;
  if (!isConfirmed(e)) throw integrity(publishAdj(e.reason));
  if (e.value !== "qfq") throw integrity(publishAdj("confirmed value must be qfq"));
};

const dateEq = (a, b) => a === null || a === undefined ? b === null : b && a.s === b.s;
const derivEq = (a, b) => {
  if (a === null) return b === null;
  if (b === null) return false;
  return a.base_daily_version_id === b.base_daily_version_id &&
    a.algorithm_version === b.algorithm_version &&
    a.first_trading_date.s === b.first_trading_date.s &&
    a.last_trading_date.s === b.last_trading_date.s;
};
const evEq = (a, b) =>
  a.status === b.status && a.value === b.value && a.reason === b.reason;
const ucEq = (a, b) =>
  ["price_currency", "price_unit", "volume_unit", "amount_currency", "amount_unit"]
    .every((k) => evEq(a[k], b[k]));

/** compat::check — verbatim order: digest -> version parse -> batch parse
 *  -> budget -> dedup -> unit/currency -> adjustment -> identity. */
export const check = (raw) => {
  const stored = raw !== null && typeof raw === "object" && !Array.isArray(raw)
    ? raw.integrity_sha256 : undefined;
  if (typeof stored !== "string" || stored !== legacyDigest(raw)) {
    throw integrity("missing/mismatched legacy digest");
  }
  let version;
  try { version = parseVersion(raw); }
  catch (e) { throw integrity(e.message); }
  let batch;
  try { batch = parseBatch(raw.batch === undefined ? null : raw.batch); }
  catch (e) { throw integrity(`publication evidence missing: ${e.message}`); }
  if (version.records.length > MAX_RECORDS || batch.bars.length > MAX_RECORDS) {
    throw invalid("version exceeds 100000 row verification budget");
  }
  const { bars, validation } = validateAndDeduplicate(batch);
  validateUnitCurrencyEvidence(batch);
  validateAdjustmentEvidence(batch);
  const versionRecordsOk = version.records.length === bars.length &&
    version.records.every((r, i) => barEq(r, bars[i]));
  if (version.version_id.trim() === "" ||
      version.symbol.trim() === "" ||
      version.source.trim() === "" ||
      batch.coverage.status !== "Complete" ||
      version.symbol !== batch.symbol ||
      version.period !== batch.period ||
      version.adjustment !== batch.adjustment ||
      version.source !== batch.provenance.source ||
      batch.provenance.symbol !== version.symbol ||
      batch.provenance.source_adjustment !== "qfq" ||
      batch.provenance.source_period !== "day" ||
      !versionRecordsOk ||
      !(version.validation.record_count === validation.record_count &&
        version.validation.unique_record_count === validation.unique_record_count &&
        version.validation.duplicate_count === validation.duplicate_count) ||
      !ucEq(version.unit_currency, batch.unit_currency) ||
      !evEq(version.adjustment_evidence, batch.adjustment_evidence) ||
      !derivEq(version.yearly_derivation, batch.provenance.yearly_derivation) ||
      (version.period === "Day" && version.yearly_derivation !== null) ||
      (version.period === "Year" &&
        (version.yearly_derivation === null || batch.provenance.source_period !== "day"))) {
    throw integrity("record/batch publication evidence mismatch");
  }
  return { raw, version, batch };
};
