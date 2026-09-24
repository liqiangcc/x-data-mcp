// data_supply/wire.rs type tables on the shared serde from_value
// machinery (src_node/mcp/serde_value.mjs — NM-070 consolidation).
// Decode semantics: see serde_value.mjs header — verified against the
// mcp_data oracle.
//
// Decoded value conventions:
//   NaiveDate   -> canonical Display string ("0999-01-01")
//   DateTime<Utc> -> BigInt nanosecond instant
//   internally-tagged enums -> { <tag>: variant, ...fields }
//   Option      -> null | value
//   #[serde(default)] -> the default value
//
// Request-side only types whose output siblings live cross-module are
// carried verbatim here (verification/gap descriptors).

import {
  Decode, decode, tBool, tDate, tDateTime, tEnumUnit, tI32, tOpt,
  tString, tStruct, tTagged, tUsize, tU64, tVec, anyValue,
} from "../mcp/serde_value.mjs";

export { Decode, decode };

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
  single: { rust: "Single", fields: [{ name: "entity", dec: Security }] },
  batch: { rust: "Batch", fields: [{ name: "entities", dec: tVec(Security) }] },
  catalog: { rust: "Catalog", fields: [{ name: "scope", dec: CatalogScope }] },
});
export const Filter = tTagged("Filter", "kind", {
  daily: { rust: "Daily", fields: [{ name: "start_date", dec: tDate }, { name: "end_date", dec: tDate }] },
  yearly: { rust: "Yearly", fields: [{ name: "start_year", dec: tI32 }, { name: "end_year", dec: tI32 }] },
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
  latest_per_entity: { rust: "LatestPerEntity", fields: null },
  explicit_version_map: { rust: "ExplicitVersionMap", fields: [{ name: "versions", dec: tVec(VersionBinding) }] },
  catalog_version: { rust: "CatalogVersion", fields: [{ name: "version_id", dec: tString }] },
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
  explicit: { rust: "Explicit", fields: [{ name: "securities", dec: tVec(Security) }] },
  catalog: { fields: [
    { name: "version_id", dec: tString },
    { name: "industry_code", dec: tOpt(tString), opt: true },
  ] },
  default: { rust: "Default", fields: null },
});
export const CatalogSelection = tStruct("CatalogSelection", [
  { name: "scope", dec: CatalogScope },
]);
export const YearlyPublication = tTagged("YearlyPublication", "mode", {
  historical_fixed_base: { rust: "HistoricalFixedBase", fields: [
    { name: "comparison_version_id", dec: tOpt(tString), opt: true },
  ] },
  current_head: { rust: "CurrentHead", fields: [
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
  "market.daily": { rust: "Daily", fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "selection", dec: WriteSelection },
  ] },
  "securities.catalog": { rust: "Catalog", fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "selection", dec: CatalogSelection },
  ] },
  "market.yearly": { rust: "Yearly", fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "selection", dec: WriteSelection },
    { name: "base_daily_versions", dec: tVec(VersionBinding) },
    { name: "years", dec: tVec(tI32) },
  ] },
});
export const RangeWrite = tTagged("RangeWrite", "dataset", {
  "market.daily": { rust: "Daily", fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "items", dec: tOpt(tVec(RangeWriteItem)), opt: true },
    { name: "selection", dec: tOpt(RangeWriteSelection), opt: true },
  ] },
  "market.yearly": { rust: "Yearly", fields: [
    { name: "request_id", dec: tString },
    { name: "contract_version", dec: ContractVersion },
    { name: "items", dec: tVec(YearlyWriteItem) },
  ] },
});
export const RepublishWrite = tTagged("RepublishWrite", "dataset", {
  "market.daily": { rust: "Daily", fields: [
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
  assigned: { rust: "Assigned", fields: [{ name: "code", dec: tString }, { name: "name", dec: tString }] },
  missing: { rust: "Missing", fields: [{ name: "reason", dec: tString }] },
  conflicted: { rust: "Conflicted", fields: [{ name: "reason", dec: tString }] },
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
  market: { rust: "Market", fields: [
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
  catalog: { rust: "Catalog", fields: [
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
