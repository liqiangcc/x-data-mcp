// NM-082 — src/market_update.rs (crate-root mod.rs) port: DateRange,
// Evidence, ContinuityEvidence, Reason, Decision, PreparedRange,
// UpdateBaseFacts, UpdateBatchItem/UpdateBatchPrepared, candidate_digest,
// unit_drift, prepare. Pure decision logic, verbatim fail-closed ordering.
//
// Value conventions (record_codec.mjs-compatible): dates {y,mo,d,s},
// decimals canonical rust_decimal strings, timestamps serde-Z RFC3339.
import { createHash } from "node:crypto";
import { MarketError } from "../market/record_codec.mjs";
import * as RC from "../market/record_codec.mjs";
import {
  dateCmp, dateEq, decEq, decCanon, serBatch, serBar, tDate, tDateTime,
  tString, tStruct, tOpt, tVec, tEnumUnit, tU32, evidenceValue,
} from "./serde.mjs";

export { MarketError };

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
export const sha256Hex = sha256;

export const invalid = (m) => new MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);
export const integrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
export const storageErr = (m) => new MarketError("storage", `MARKET_STORAGE: ${m}`);
export const baseErr = (m) => new MarketError("base", `BASE_UNAVAILABLE: ${m}`);
export const conflict = (m) => new MarketError("conflict", `PUBLISH_PRECONDITION: ${m}`);
export const notFound = () => new MarketError("not_found", "MARKET_NOT_FOUND");
export const outcomeUnknown = (m) =>
  new MarketError("outcome_unknown", `PUBLISH_OUTCOME_UNKNOWN: ${m}; inspect the same publication_id`);
export const basisNotVerified = (m) =>
  new MarketError("basis_not_verified", `GAP_BASIS_NOT_VERIFIED: ${m}`);
// Error::Task(#[from]) — Display is the inner task_control error text.
export const taskErr = (te) =>
  Object.assign(new MarketError("task", te.message), { isTaskError: true, code: te.code });

// SourceError (market_data.rs) — provider-facing failures, Display
// verbatim; update_candidate maps them to Error::Invalid(e.to_string()).
export class SourceError extends Error {
  constructor(kind, detail) {
    super(kind === "not_verified" ? `SOURCE_NOT_VERIFIED: ${detail}`
      : kind === "unsupported" ? `unsupported request: ${detail}`
      : kind === "invocation" ? `source invocation failed: ${detail}`
      : kind === "timeout" ? `source invocation timed out after ${detail} ms`
      : `invalid source response: ${detail}`);
    this.kind = kind;
    this.detail = detail;
    this.isSourceError = true;
  }
}

// task_control::Error equivalent — verbatim Display text; `code` carries
// the machine tag used by the diff harnesses.
export const tcError = (code, display) =>
  Object.assign(new Error(display), { code, taskControl: true });
export const tcInvalid = (m) => tcError("invalid_input", `INVALID_COMMAND: ${m}`);
export const tcNotFound = () => tcError("task_not_found", "TASK_NOT_FOUND");
export const tcConflict = () => tcError("revision_conflict", "STATE_REVISION_CONFLICT");
export const tcNeedsVerification = (m) =>
  tcError("needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: ${m}`);
export const tcStorageUnavailable = (m) => tcError("unavailable", `STORAGE_UNAVAILABLE: ${m}`);
export const tcOutcomeUnknown = (m) =>
  tcError("outcome_unknown", `OUTCOME_UNKNOWN: ${m}; query the same request_id before retrying`);

// Recover a market_storage::Error variant's inner message from a
// MarketError carrying the full Display text.
const DISPLAY_PREFIX = {
  storage: "MARKET_STORAGE: ", integrity: "MARKET_INTEGRITY: ",
  base: "BASE_UNAVAILABLE: ", conflict: "PUBLISH_PRECONDITION: ",
  basis_not_verified: "GAP_BASIS_NOT_VERIFIED: ",
  invalid: "INVALID_MARKET_COMMAND: ",
};
export const innerMsg = (e) => {
  if (!(e instanceof MarketError)) return e.message;
  if (e.kind === "outcome_unknown")
    return e.message.slice("PUBLISH_OUTCOME_UNKNOWN: ".length)
      .replace(/; inspect the same publication_id$/, "");
  if (e.kind === "not_found") return "";
  const p = DISPLAY_PREFIX[e.kind];
  return p !== undefined && e.message.startsWith(p) ? e.message.slice(p.length) : e.message;
};
// PublishError -> Error::Invalid(e.to_string()): the publish validators
// throw integrity-kind MarketErrors; the inner text is the PublishError
// display.
export const asInvalid = (e) => invalid(innerMsg(e));

// ---------------------------------------------------------------- types
// DateRange { start, end } — inclusive [start, end].
export const rangeContains = (range, d) =>
  dateCmp(range.start, d) <= 0 && dateCmp(d, range.end) <= 0;
export const rangeEq = (a, b) => dateEq(a.start, b.start) && dateEq(a.end, b.end);
export const rangeValue = (r) => ({ start: r.start.s ?? r.start, end: r.end.s ?? r.end });

// Reason — tagged {kind, dates?}: coverage_unknown | basis_unknown |
// units_incompatible | missing_old_dates | known_value_lost |
// revision_unverified. Debug parity for "NEEDS_VERIFICATION: {:?}".
export const reasonDebug = (r) =>
  r.kind === "coverage_unknown" ? "CoverageUnknown" :
  r.kind === "basis_unknown" ? "BasisUnknown" :
  r.kind === "units_incompatible" ? "UnitsIncompatible" :
  `${{ missing_old_dates: "MissingOldDates", known_value_lost: "KnownValueLost",
      revision_unverified: "RevisionUnverified" }[r.kind]}([${r.dates.map((d) => d.s ?? d).join(", ")}])`;

// candidate_digest: sha256(serde_json::to_vec(batch)) — struct-order
// bytes (decimals canonical strings, timestamps RFC3339-Z AutoSi).
export const candidateDigest = (batch) => sha256(Buffer.from(serBatch(batch), "utf8"));

// --------------------------------------------------------- field equality
const optStrEq = (a, b) => (a ?? null) === (b ?? null);
export const evEq = (a, b) =>
  a.status === b.status && optStrEq(a.value, b.value) && a.reason === b.reason;
export const ucEq = (a, b) =>
  evEq(a.price_currency, b.price_currency) && evEq(a.price_unit, b.price_unit) &&
  evEq(a.volume_unit, b.volume_unit) && evEq(a.amount_currency, b.amount_currency) &&
  evEq(a.amount_unit, b.amount_unit);
export const covEq = (a, b) => a.status === b.status && a.reason === b.reason;
export const derivEq = (a, b) =>
  (a == null) === (b == null) &&
  (a == null || (a.base_daily_version_id === b.base_daily_version_id &&
    a.algorithm_version === b.algorithm_version &&
    dateEq(a.first_trading_date, b.first_trading_date) &&
    dateEq(a.last_trading_date, b.last_trading_date)));
// Bar PartialEq — decimals compare by VALUE, Option fields by presence
// then value.
export const barEq = (a, b) =>
  dateEq(a.date, b.date) && decEq(a.open, b.open) && decEq(a.high, b.high) &&
  decEq(a.low, b.low) && decEq(a.close, b.close) &&
  ((a.volume == null) === (b.volume == null)) &&
  (a.volume == null || decEq(a.volume, b.volume)) &&
  ((a.amount == null) === (b.amount == null)) &&
  (a.amount == null || decEq(a.amount, b.amount)) &&
  optStrEq(a.r7_raw, b.r7_raw);
export const barsEq = (a, b) => a.length === b.length && a.every((x, i) => barEq(x, b[i]));

const isConfirmed = (ev) =>
  ev.status === "Confirmed" && typeof ev.value === "string" && ev.value !== "";
const conflictingUnits = (a, b) =>
  isConfirmed(a) && isConfirmed(b) && a.value !== b.value;

// unit_drift — drift when units conflict OR an unconfirmed base unit
// gains a confirmed value.
export const unitDrift = (base, next) =>
  ["price_currency", "price_unit", "volume_unit", "amount_currency", "amount_unit"]
    .some((u) => conflictingUnits(base[u], next[u]) || (!isConfirmed(base[u]) && isConfirmed(next[u])));

// ------------------------------------------------------------------ prepare
// Verbatim src/market_update.rs:170-324. `existing` is the RC.check()
// output {raw,version,batch}; `batch` is a parsed batch; `evidence` is
// the provider-constructed Evidence or null.
export const prepare = (existing, batch, range, evidence) => {
  const old = existing.version;
  if (dateCmp(range.start, range.end) > 0 || range.start.y <= 0 || range.end.y > 9999) {
    throw invalid("daily update range");
  }
  if (old.period !== "Day" || batch.period !== "Day") {
    throw invalid("yearly must derive from one fixed daily version");
  }
  if (batch.symbol !== old.symbol || batch.provenance.symbol !== old.symbol ||
      batch.adjustment !== old.adjustment ||
      batch.provenance.source !== old.source ||
      batch.provenance.source_period !== "day" ||
      batch.provenance.source_adjustment !== "qfq" ||
      batch.provenance.yearly_derivation != null) {
    throw invalid("update identity/source/adjustment mismatch");
  }
  if (batch.bars.some((b) => !rangeContains(range, b.date))) {
    throw invalid("candidate outside explicit range");
  }
  const incoming = new Set(batch.bars.map((b) => b.date.s));
  const missing = old.records
    .filter((b) => rangeContains(range, b.date) && !incoming.has(b.date.s))
    .map((b) => b.date);
  if (missing.length > 0) {
    return { kind: "needs_verification", reason: { kind: "missing_old_dates", dates: missing } };
  }
  let bars;
  try { bars = RC.validateAndDeduplicate(batch).bars; } catch (e) { throw asInvalid(e); }
  try { RC.validateUnitCurrencyEvidence(batch); } catch (e) { throw asInvalid(e); }
  try { RC.validateAdjustmentEvidence(batch); } catch (e) { throw asInvalid(e); }
  if (batch.coverage.status !== "Complete") {
    return { kind: "needs_verification", reason: { kind: "coverage_unknown" } };
  }
  const a = old.unit_currency, b = batch.unit_currency;
  if (["price_currency", "price_unit", "volume_unit", "amount_currency", "amount_unit"]
      .some((u) => conflictingUnits(a[u], b[u]))) {
    return { kind: "needs_verification", reason: { kind: "units_incompatible" } };
  }
  if (evidence == null) {
    return { kind: "needs_verification", reason: { kind: "basis_unknown" } };
  }
  const proof = evidence;
  if (proof.base_version_id !== old.version_id ||
      proof.base_integrity_sha256 !== existing.raw["integrity_sha256"] ||
      proof.candidate_sha256 !== candidateDigest(batch) ||
      !rangeEq(proof.range, range) ||
      proof.same_qfq_basis_ref.trim() === "") {
    return { kind: "needs_verification", reason: { kind: "basis_unknown" } };
  }

  // BTreeMap<NaiveDate, Bar> merge — old records first; iterate the
  // deduplicated (date-sorted) candidate bars, insert after checks.
  const combined = new Map(old.records.map((bar) => [bar.date.s, bar]));
  const changed = [];
  const revised = [];
  const lost = [];
  for (const bar of bars) {
    const previous = combined.get(bar.date.s);
    if (previous !== undefined) {
      if ((previous.volume != null && bar.volume == null) ||
          (previous.amount != null && bar.amount == null) ||
          (previous.r7_raw != null && bar.r7_raw == null)) {
        lost.push(bar.date);
      }
      // #4 AC-71: filling an unknown optional field is not a correction.
      if (!decEq(previous.open, bar.open) || !decEq(previous.high, bar.high) ||
          !decEq(previous.low, bar.low) || !decEq(previous.close, bar.close) ||
          (previous.volume != null && bar.volume != null && !decEq(previous.volume, bar.volume)) ||
          (previous.amount != null && bar.amount != null && !decEq(previous.amount, bar.amount)) ||
          (previous.r7_raw != null && bar.r7_raw != null && previous.r7_raw !== bar.r7_raw)) {
        revised.push(bar.date);
      }
    }
    if (previous === undefined || !barEq(previous, bar)) changed.push(bar.date);
    combined.set(bar.date.s, bar);
  }
  if (lost.length > 0) {
    return { kind: "needs_verification", reason: { kind: "known_value_lost", dates: lost } };
  }
  if (revised.length > 0 && (proof.revision_ref == null || proof.revision_ref.trim() === "")) {
    return { kind: "needs_verification", reason: { kind: "revision_unverified", dates: revised } };
  }
  // #4 AC-84/87: unchanged prices alone do not prove unchanged data.
  const unitsEnriched = ["price_currency", "price_unit", "volume_unit", "amount_currency", "amount_unit"]
    .some((u) => !isConfirmed(a[u]) && isConfirmed(b[u]));
  const metadataChanged = !covEq(batch.coverage, existing.batch.coverage) || unitsEnriched;
  if (changed.length === 0 && !metadataChanged) {
    return { kind: "no_change", version_id: old.version_id };
  }
  const records = [...combined.values()].sort((x, y) => dateCmp(x.date, y.date));
  const years = new Set(metadataChanged
    ? records.map((bar) => bar.date.y)
    : changed.map((d) => d.y));
  return {
    kind: "candidate",
    affected_years: [...years].sort((x, y) => x - y),
    records,
    changed_dates: changed,
    metadata_changed: metadataChanged,
    base_version_id: old.version_id,
    base_integrity_sha256: proof.base_integrity_sha256,
    candidate_sha256: proof.candidate_sha256,
    requested_range: { start: range.start, end: range.end },
    same_qfq_basis_ref: proof.same_qfq_basis_ref,
    revision_ref: proof.revision_ref ?? null,
  };
};

// ------------------------------------------------------------- serde batch
// serde_json::from_value::<MarketDataBatch> — permissive structs (no
// deny_unknown_fields in market_data.rs) with #[serde(default)] on
// coverage/unit_currency/adjustment_evidence/r7_raw/yearly_derivation/
// snapshot_inputs.
const evDefault = (reason) => () => ({ status: "Unknown", value: null, reason });
const EVIDENCE_STATUS = ["Confirmed", "Unknown"];
const tEvidenceValue = tStruct("EvidenceValue", [
  { name: "status", dec: tEnumUnit("EvidenceStatus", EVIDENCE_STATUS) },
  { name: "value", dec: tOpt(tString), opt: true },
  { name: "reason", dec: tString },
]);
const UC_DEFAULT_REASON = "unit/currency evidence was not supplied";
const tUnitCurrency = tStruct("UnitCurrencyEvidence", [
  { name: "price_currency", dec: tEvidenceValue },
  { name: "price_unit", dec: tEvidenceValue },
  { name: "volume_unit", dec: tEvidenceValue },
  { name: "amount_currency", dec: tEvidenceValue },
  { name: "amount_unit", dec: tEvidenceValue },
]);
const ucDefault = () => ({
  price_currency: { status: "Unknown", value: null, reason: UC_DEFAULT_REASON },
  price_unit: { status: "Unknown", value: null, reason: UC_DEFAULT_REASON },
  volume_unit: { status: "Unknown", value: null, reason: UC_DEFAULT_REASON },
  amount_currency: { status: "Unknown", value: null, reason: UC_DEFAULT_REASON },
  amount_unit: { status: "Unknown", value: null, reason: UC_DEFAULT_REASON },
});
const tYearlyDerivation = tStruct("YearlyDerivation", [
  { name: "base_daily_version_id", dec: tString },
  { name: "algorithm_version", dec: tString },
  { name: "first_trading_date", dec: tDate },
  { name: "last_trading_date", dec: tDate },
]);
const tSnapshotInput = tStruct("SnapshotInputBinding", [
  { name: "role", dec: tString },
  { name: "member", dec: tString },
  { name: "archive_sha256", dec: tString },
  { name: "member_sha256", dec: tString },
]);
const tBar = tStruct("Bar", [
  { name: "date", dec: tDate },
  { name: "open", dec: decCanon },
  { name: "high", dec: decCanon },
  { name: "low", dec: decCanon },
  { name: "close", dec: decCanon },
  { name: "volume", dec: tOpt(decCanon), opt: true },
  { name: "amount", dec: tOpt(decCanon), opt: true },
  { name: "r7_raw", dec: tOpt(tU32), default: () => null },
]);
const tProvenance = tStruct("Provenance", [
  { name: "source", dec: tString },
  { name: "source_period", dec: tString },
  { name: "source_adjustment", dec: tString },
  { name: "symbol", dec: tString },
  { name: "acquired_at", dec: tDateTime },
  { name: "adapter_version", dec: tString },
  { name: "yearly_derivation", dec: tOpt(tYearlyDerivation), default: () => null },
  { name: "snapshot_inputs", dec: tOpt(tVec(tSnapshotInput)), default: () => null },
]);
const tCoverage = tStruct("CoverageEvidence", [
  { name: "status", dec: tEnumUnit("CoverageStatus", ["Complete", "Incomplete", "Unknown"]) },
  { name: "reason", dec: tString },
]);
export const parseBatch = tStruct("MarketDataBatch", [
  { name: "symbol", dec: tString },
  { name: "period", dec: tEnumUnit("Period", ["Day", "Year"]) },
  { name: "adjustment", dec: tEnumUnit("Adjustment", ["Forward"]) },
  { name: "bars", dec: tVec(tBar) },
  { name: "provenance", dec: tProvenance },
  { name: "coverage", dec: tCoverage, default: evDefault("coverage evidence was not supplied") },
  { name: "unit_currency", dec: tUnitCurrency, default: ucDefault },
  { name: "adjustment_evidence", dec: tEvidenceValue, default: evDefault("adjustment evidence was not supplied") },
]);

// market_update::DateRange — permissive struct decode for probe inputs.
export const tDateRange = tStruct("DateRange", [
  { name: "start", dec: tDate },
  { name: "end", dec: tDate },
]);

// Evidence/ContinuityEvidence are Serialize-only in Rust (provider-
// constructed). normalizeEvidence accepts JSON probe inputs leniently.
const toDate = (d) => (typeof d === "string" ? tDate(d) : d);
export const normalizeEvidence = (e) => e == null ? null : ({
  base_version_id: e.base_version_id,
  base_integrity_sha256: e.base_integrity_sha256,
  candidate_sha256: e.candidate_sha256,
  range: { start: toDate(e.range.start), end: toDate(e.range.end) },
  same_qfq_basis_ref: e.same_qfq_basis_ref,
  revision_ref: e.revision_ref ?? null,
  continuity: e.continuity ?? null,
});

export { evidenceValue, serBar, tBar, tCoverage, tUnitCurrency, tEvidenceValue };
