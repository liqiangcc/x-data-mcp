// src/data_supply/registry.rs — the descriptor table: tools/list,
// get_data_capabilities and dispatch all read this single source.
//
// input/output are the schemars-generated JSON schemas — byte-captured
// from the Rust oracle (regenerate: scripts/node_migration_p4/
// gen_registry_descriptors.mjs). Descriptions and examples are the
// registry.rs literals. Handler metadata (requires_local / datasets)
// is the ported Rust logic.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { DATASETS, DATASET_CATALOG, DATASET_DAILY, DATASET_YEARLY } from "./wire.mjs";

// registry.rs:7 — dispatch routing keys
export const Handler = {
  Query: "query", Versions: "versions", Availability: "availability",
  Tasks: "tasks", Progress: "progress", Capabilities: "capabilities",
  Collection: "collection", Retry: "retry", Task: "task",
  Request: "request", Cancel: "cancel", Compute: "compute",
  Worker: "worker", VerifyStopped: "verify_stopped",
  VerifyPublication: "verify_publication",
  InspectVerification: "inspect_verification",
  RangeUpdate: "range:update", RangeBackfill: "range:backfill",
  Republish: "republish", VerifyGap: "verify_gap",
};

const UPDATE_DESCRIPTION = "Routine daily refresh. market.daily accepts explicit items each bound to a published base version, or selection {\"kind\":\"default\"} resolved and frozen from the acceptance snapshot (catalog + started securities + published heads + open proven gaps). market.yearly items publish explicit natural-year sets bound to published daily versions with an explicit frozen publication mode; historical mode never advances head. Daily items execute only against the verified active generation (or the installed inspected snapshot in legacy mode); unchanged inputs are proven by member/event digests and land no_change, appended tails commit as new versions, and historical revisions fail closed without advancing heads. Per-item outcomes are published / no_change / needs_verification. Automatic gap discovery and catalog updates are not implemented.";
const BACKFILL_DESCRIPTION = "Bounded backfill, not the routine update path. market.daily explicit items fill a named range on top of a published base version; selection {\"kind\":\"default\"} resolves to frozen proven gaps only — never a full-market refresh. market.yearly items publish explicit natural-year sets bound to published daily versions with an explicit frozen publication mode. Daily items execute only against the verified active generation (or the installed inspected snapshot in legacy mode); revisions fail closed without advancing heads.";
const REPUBLISH_DESCRIPTION = "Local-owner explicit market.daily republish (#247): republishes a fixed base version as a new version through the same verified pipeline, authorized by this command itself — not by frozen proven gaps. Only pure enrichment (filling previously unpromoted optional fields such as volume/amount/r7_raw) or new dates may differ from the base; any revised or lost known value fails closed at both the provider merge and market_update::prepare. The republished version advances the daily head; the base stays queryable verbatim. market.daily only; explicit items only — no selection and no other dataset.";
const WORKER_DESCRIPTION = "Operator/debug single-step execution of at most one locally owned pending task. The data_service background worker executes pending tasks automatically, so routine synchronization flows must not invoke this tool; while the service worker is busy it fails with worker_busy and never runs a second execution. It never downloads market data, never approves sources, and never unlocks or repairs tasks.";
const COLLECTION_DESCRIPTION = "First establishment only: creates the initial publication for entities with no published head — explicit entity list, catalog version, or the snapshot-resolved default selection, frozen at acceptance. Daily and catalog items execute only against the verified active generation (or the installed inspected snapshot in legacy mode); yearly items derive from published daily bases. This is not the routine update path; entities with published heads update through their own write tasks. Replaying the same request_id returns the original acceptance.";
const INSPECT_DESCRIPTION = "Owner-only bounded management of an abnormal verification task, not a recursive asynchronous task. Each reliably persisted result, including insufficient evidence, consumes request_id; the same ID replays its historical result without re-inspecting. A new ID is needed for fresh evidence. Checks registered local execution exclusion and durable finish separately; may close only the verification task, never the original business task or its claims. Missing/unregistered runtime proof retains protection. No force/evidence flags, waiting, implicit retry or production source approval.";
const QUERY_DESCRIPTION = "Read integrity-checked published rows for one dataset/selection with an explicit filter and version choice. Paged and snapshot-scoped; never triggers collection, repair or source access.";
const VERSIONS_DESCRIPTION =
  "List published version references for one dataset/selection. Paged; read-only.";
const AVAILABILITY_DESCRIPTION = "Report published coverage, freshness, confirmed gaps and unresolved ranges per entity, with related task references. Paged; read-only.";
const TASKS_DESCRIPTION =
  "Page task summaries filtered by dataset, operation, status and creation range. Read-only.";
const PROGRESS_DESCRIPTION = "Page one task's item progress, execution results, publication versions and verification receipts. Read-only.";
const CAPABILITIES_DESCRIPTION = "Report this server's capability matrix: implemented tools, per-dataset schemas, runtime and governance readiness, structured blocking reasons and production gate facts. Facts only — no workflow ordering or cross-server instructions.";
const TASK_DESCRIPTION = "Read one task's status, frozen scope and per-item results.";
const REQUEST_DESCRIPTION = "Read the durable request_id binding and its bound task or terminal result without accepting work.";
const CANCEL_DESCRIPTION = "Request cancellation of a task under an expected_revision precondition; returns the post-request visible status and state revision.";
const RETRY_DESCRIPTION = "Re-accept explicitly selected unfinished items of an existing task as a new linked task. Item ids only; outcomes and finished items cannot be silently re-run.";
const COMPUTE_DESCRIPTION = "Run a named bounded metric algorithm over explicit published version inputs and return the computation or a structured not_computable outcome. Read-only.";
// task_verification::wire::DESCRIPTION / publication_verification::wire::
// DESCRIPTION / gap_verification::wire::DESCRIPTION — cross-module
// literals carried verbatim (the same constants the Rust registry reads).
const VERIFY_STOPPED_DESCRIPTION = "Explicit asynchronous stopping-evidence inspection of a task awaiting verification. Admission preserves original claims. The installed verifier can prove an audited revision-zero task never started using verified SQLite facts and conditional exclusion of a later claim. Started or unsupported executions remain STOP_EVIDENCE_NOT_VERIFIED; no caller assertion, PID absence or old run identity can unlock a task. Inspect the separate check conclusion via get_task_progress.";
const VERIFY_PUBLICATION_DESCRIPTION = "Explicit asynchronous inspection of durable publication evidence for an unresolved market task. Reads fixed SQLite publication versions, receipts and projections; missing receipt is insufficient evidence, not proof of failure. The separate check never publishes, changes heads or releases the original task claims. A prior all-recorded conclusion can bind a new request without a new inspection. Inspect publication_check via get_task_progress. Corrupt evidence and interrupted inspections retain protection; no caller approval or force flags. This ready interface does not make the official data source production-ready.";
const VERIFY_GAP_DESCRIPTION = "Explicit asynchronous verification of a proven local coverage gap in one published QFQ daily version. The caller names only symbol/day/[start,end]; the subject head is frozen at acceptance and re-checked at decision commit. Decisions come only from the installed trusted source-gap verifier; source limitation or unavailability never produces a proven gap, and a decision never triggers update/backfill by itself. Proving a gap requires the verified active generation (or the installed inspected snapshot in legacy mode); without one runtime readiness is blocked and no gap can be proven.";

const QUERY_EXAMPLE = { dataset: "market.daily", contract_version: "v1", selection: { kind: "single", entity: { exchange: "sh", code: "600006" } }, filter: { kind: "daily", start_date: "2025-01-01", end_date: "2025-12-31" }, version: { kind: "latest_per_entity" } };
const COMPUTE_EXAMPLE = { contract_version: "v1", execution: "inline", inputs: [{ input_id: "prices", version_ref: { dataset: "market.yearly", contract_version: "v1", entity: { exchange: "sh", code: "600006" }, version_id: "published-yearly-id", integrity_sha256: "0000000000000000000000000000000000000000000000000000000000000000" } }], range: { start_year: 2020, end_year: 2023 }, metrics: [{ metric_id: "down", name: "consecutive_direction", algorithm_version: "local-consecutive-direction-v1", input_ids: ["prices"], parameters: { direction: "down", min_periods: 3 } }] };

const descriptors = JSON.parse(readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "registry_descriptors.json"), "utf8"));
const schemaOf = (name) => descriptors.find((d) => d.name === name);

// (name, handler, description, example) in entries() order — the schemas
// are looked up by name from the captured table.
const TABLE = [
  ["inspect_verification_task", Handler.InspectVerification, INSPECT_DESCRIPTION,
    { contract_version: "v1", request_id: "bounded-inspection-request", target_task_id: "verification-task-id" }],
  ["verify_publication_result", Handler.VerifyPublication, VERIFY_PUBLICATION_DESCRIPTION,
    { contract_version: "v1", request_id: "publication-check-request", target_task_id: "original-task-id" }],
  ["verify_stopped_writes", Handler.VerifyStopped, VERIFY_STOPPED_DESCRIPTION,
    { contract_version: "v1", request_id: "verification-request", target_task_id: "original-task-id" }],
  ["verify_data_gap", Handler.VerifyGap, VERIFY_GAP_DESCRIPTION,
    { contract_version: "v1", request_id: "gap-check-request", target: { symbol: "sh.600006", period: "day", start_date: "2025-01-01", end_date: "2025-01-10" } }],
  ["update_data", Handler.RangeUpdate, UPDATE_DESCRIPTION,
    { request_id: "example-update", dataset: "market.daily", contract_version: "v1", items: [{ entity: { exchange: "sh", code: "600006" }, base_version_id: "published-daily-id", start_date: "2025-01-01", end_date: "2025-12-31" }] }],
  ["backfill_data", Handler.RangeBackfill, BACKFILL_DESCRIPTION,
    { request_id: "example-backfill", dataset: "market.daily", contract_version: "v1", items: [{ entity: { exchange: "sh", code: "600006" }, base_version_id: "published-daily-id", start_date: "2025-01-01", end_date: "2025-12-31" }] }],
  ["republish_data", Handler.Republish, REPUBLISH_DESCRIPTION,
    { request_id: "example-republish", dataset: "market.daily", contract_version: "v1", items: [{ entity: { exchange: "sh", code: "600006" }, base_version_id: "published-daily-id", start_date: "1990-01-01", end_date: "2025-12-31" }] }],
  ["run_worker_once", Handler.Worker, WORKER_DESCRIPTION, {}],
  ["compute_metrics", Handler.Compute, COMPUTE_DESCRIPTION, COMPUTE_EXAMPLE],
  ["start_collection", Handler.Collection, COLLECTION_DESCRIPTION,
    { request_id: "example-daily", dataset: "market.daily", contract_version: "v1", selection: { kind: "explicit", securities: [{ exchange: "sh", code: "600006" }] } }],
  ["retry_task", Handler.Retry, RETRY_DESCRIPTION,
    { contract_version: "v1", original_task_id: "returned-task-id", new_request_id: "retry-request", item_ids: ["returned-item-id"] }],
  ["get_task", Handler.Task, TASK_DESCRIPTION,
    { contract_version: "v1", task_id: "returned-task-id" }],
  ["get_request", Handler.Request, REQUEST_DESCRIPTION,
    { contract_version: "v1", request_id: "returned-request-id" }],
  ["cancel_task", Handler.Cancel, CANCEL_DESCRIPTION,
    { contract_version: "v1", task_id: "returned-task-id", expected_revision: 0 }],
  ["query_data", Handler.Query, QUERY_DESCRIPTION, QUERY_EXAMPLE],
  ["list_data_versions", Handler.Versions, VERSIONS_DESCRIPTION,
    { dataset: "market.daily", contract_version: "v1", selection: { kind: "single", entity: { exchange: "sh", code: "600006" } } }],
  ["get_data_availability", Handler.Availability, AVAILABILITY_DESCRIPTION, QUERY_EXAMPLE],
  ["list_tasks", Handler.Tasks, TASKS_DESCRIPTION, {}],
  ["get_task_progress", Handler.Progress, PROGRESS_DESCRIPTION,
    { task_id: "returned-task-id" }],
  ["get_data_capabilities", Handler.Capabilities, CAPABILITIES_DESCRIPTION, {}],
];

// registry.rs:44 — requires_local per Handler.
const REQUIRES_LOCAL = new Set([
  Handler.Collection, Handler.RangeUpdate, Handler.RangeBackfill,
  Handler.Republish, Handler.Retry, Handler.Task, Handler.Request,
  Handler.Cancel, Handler.Worker, Handler.VerifyStopped,
  Handler.VerifyPublication, Handler.InspectVerification, Handler.VerifyGap,
]);

// Entry.datasets(access) — registry.rs:64.
const entryDatasets = (e, access) => {
  if (e.handler === Handler.RangeUpdate || e.handler === Handler.RangeBackfill)
    return [DATASET_DAILY, DATASET_YEARLY]
      .filter((d) => access.allows(d, null));
  if (e.handler === Handler.Republish)
    return access.allows(DATASET_DAILY, null) ? [DATASET_DAILY] : [];
  if ([Handler.Query, Handler.Versions, Handler.Availability,
       Handler.Collection].includes(e.handler))
    // BTreeSet<Dataset> iterates in enum declaration order
    return DATASETS.filter((d) => access.datasets.has(d));
  return [null];
};

export function entries() {
  return TABLE.map(([name, handler, description, example]) => ({
    name, handler, description,
    input: schemaOf(name).input,
    output: schemaOf(name).output,
    example: JSON.parse(JSON.stringify(example)),
    operator_only: handler === Handler.Worker || handler === Handler.InspectVerification,
    requires_local: REQUIRES_LOCAL.has(handler),
    datasets: (access) => entryDatasets({ handler }, access),
  }));
}

// mcp.rs capability_example — mutates the entry example per dataset.
export function capabilityExample(e, dataset) {
  const v = JSON.parse(JSON.stringify(e.example));
  if (dataset === null || dataset === undefined) return v;
  v.dataset = dataset;
  if (dataset === DATASET_CATALOG) {
    v.selection = e.handler === Handler.Collection
      ? { scope: "current_sh_sz_a" }
      : { kind: "catalog", scope: "current_sh_sz_a" };
    if ("filter" in v) v.filter = { kind: "catalog" };
  } else if (dataset === DATASET_YEARLY) {
    if (e.handler === Handler.RangeUpdate || e.handler === Handler.RangeBackfill)
      v.items = [{ entity: { exchange: "sh", code: "600006" }, base_daily_version_id: "published-daily-id", years: [2025], publication: { mode: "historical_fixed_base", comparison_version_id: null } }];
    if ("filter" in v)
      v.filter = { kind: "yearly", start_year: 2025, end_year: 2025 };
    if (e.handler === Handler.Collection) {
      v.base_daily_versions = [{ entity: { exchange: "sh", code: "600006" }, version_id: "published-daily-id" }];
      v.years = [2025];
    }
  }
  return v;
}
