//! Schema and routing share one descriptor; no advertised handler without a contract.
use super::wire::*;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Copy)]
pub enum Handler {
    Query,
    Versions,
    Availability,
    Tasks,
    Progress,
    Capabilities,
    Collection,
    Retry,
    Task,
    Request,
    Cancel,
    Compute,
    Worker,
    VerifyStopped,
    VerifyPublication,
    InspectVerification,
    Range(crate::market_update::tasks::Operation),
    Republish,
    VerifyGap,
}
/// One tool descriptor: tools/list, get_data_capabilities, dispatch and
/// golden examples are all generated from this single source.
pub struct Entry {
    pub name: &'static str,
    pub handler: Handler,
    pub description: &'static str,
    pub input: Value,
    pub output: Value,
    pub example: Value,
    /// Operator/debug controls exist for humans and service wiring; routine
    /// AI synchronization must not orchestrate them.
    pub operator_only: bool,
}
impl Entry {
    /// Write/verification controls require the local-owner context; read
    /// discovery tools stay available to restricted access.
    pub fn requires_local(&self) -> bool {
        matches!(
            self.handler,
            Handler::Collection
                | Handler::Range(_)
                | Handler::Republish
                | Handler::Retry
                | Handler::Task
                | Handler::Request
                | Handler::Cancel
                | Handler::Worker
                | Handler::VerifyStopped
                | Handler::VerifyPublication
                | Handler::InspectVerification
                | Handler::VerifyGap
        )
    }
    /// Datasets this tool advertises in get_data_capabilities. `None` is a
    /// dataset-independent tool; the input schema itself pins the exact
    /// (tool, dataset, contract_version) shape for dataset-bound tools.
    pub fn datasets(&self, access: &super::Access) -> Vec<Option<Dataset>> {
        match self.handler {
            Handler::Range(_) => [Dataset::Daily, Dataset::Yearly]
                .into_iter()
                .filter(|d| access.allows(*d, None))
                .map(Some)
                .collect(),
            Handler::Republish => access
                .allows(Dataset::Daily, None)
                .then_some(Some(Dataset::Daily))
                .into_iter()
                .collect(),
            Handler::Query | Handler::Versions | Handler::Availability | Handler::Collection => {
                access.datasets.iter().copied().map(Some).collect()
            }
            _ => vec![None],
        }
    }
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesQuery {
    pub dataset: Option<Dataset>,
    pub operation: Option<String>,
}
/// Capability fact for one (tool, dataset) pair: what is implemented, what
/// is executable in this runtime, and why not. Facts only — a capability
/// never names other tools, next actions, or Skill content.
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub tool_name: String,
    pub description: String,
    pub dataset: Option<Dataset>,
    pub contract_version: ContractVersion,
    pub implemented: bool,
    pub operator_only: bool,
    pub runtime_readiness: Readiness,
    pub governance_readiness: Readiness,
    pub blocking_reasons: Vec<BlockingReason>,
    pub declared_scope: String,
    /// Set once immutable generations exist (#260 phase B); until then
    /// write tools bind the installed inspected snapshot at execution.
    pub active_generation_ref: Option<String>,
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    pub example: Option<Value>,
    pub schema_digest: String,
}
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub capability_revision: String,
    pub items: Vec<Capability>,
    pub max_batch_entities: usize,
    pub max_page_size: usize,
    pub max_snapshot_rows: usize,
    pub max_snapshot_bytes: usize,
    pub snapshots_per_category: usize,
    pub declared_scope: String,
    pub production_ready: bool,
    pub production_blocking_reasons: Vec<BlockingReason>,
}
pub const UPDATE_DESCRIPTION: &str = "Routine daily refresh. market.daily accepts explicit items each bound to a published base version, or selection {\"kind\":\"default\"} resolved and frozen from the acceptance snapshot (catalog + started securities + published heads + open proven gaps). market.yearly items publish explicit natural-year sets bound to published daily versions with an explicit frozen publication mode; historical mode never advances head. Daily items execute only against the verified active generation (or the installed inspected snapshot in legacy mode); unchanged inputs are proven by member/event digests and land no_change, appended tails commit as new versions, and historical revisions fail closed without advancing heads. Per-item outcomes are published / no_change / needs_verification. Automatic gap discovery and catalog updates are not implemented.";
pub const BACKFILL_DESCRIPTION: &str = "Bounded backfill, not the routine update path. market.daily explicit items fill a named range on top of a published base version; selection {\"kind\":\"default\"} resolves to frozen proven gaps only — never a full-market refresh. market.yearly items publish explicit natural-year sets bound to published daily versions with an explicit frozen publication mode. Daily items execute only against the verified active generation (or the installed inspected snapshot in legacy mode); revisions fail closed without advancing heads.";
pub const REPUBLISH_DESCRIPTION: &str = "Local-owner explicit market.daily republish (#247): republishes a fixed base version as a new version through the same verified pipeline, authorized by this command itself — not by frozen proven gaps. Only pure enrichment (filling previously unpromoted optional fields such as volume/amount/r7_raw) or new dates may differ from the base; any revised or lost known value fails closed at both the provider merge and market_update::prepare. The republished version advances the daily head; the base stays queryable verbatim. market.daily only; explicit items only — no selection and no other dataset.";
pub const WORKER_DESCRIPTION: &str = "Operator/debug single-step execution of at most one locally owned pending task. The data_service background worker executes pending tasks automatically, so routine synchronization flows must not invoke this tool; while the service worker is busy it fails with worker_busy and never runs a second execution. It never downloads market data, never approves sources, and never unlocks or repairs tasks.";
pub const COLLECTION_DESCRIPTION: &str = "First establishment only: creates the initial publication for entities with no published head — explicit entity list, catalog version, or the snapshot-resolved default selection, frozen at acceptance. Daily and catalog items execute only against the verified active generation (or the installed inspected snapshot in legacy mode); yearly items derive from published daily bases. This is not the routine update path; entities with published heads update through their own write tasks. Replaying the same request_id returns the original acceptance.";
pub const INSPECT_DESCRIPTION: &str = "Owner-only bounded management of an abnormal verification task, not a recursive asynchronous task. Each reliably persisted result, including insufficient evidence, consumes request_id; the same ID replays its historical result without re-inspecting. A new ID is needed for fresh evidence. Checks registered local execution exclusion and durable finish separately; may close only the verification task, never the original business task or its claims. Missing/unregistered runtime proof retains protection. No force/evidence flags, waiting, implicit retry or production source approval.";
pub const QUERY_DESCRIPTION: &str = "Read integrity-checked published rows for one dataset/selection with an explicit filter and version choice. Paged and snapshot-scoped; never triggers collection, repair or source access.";
pub const VERSIONS_DESCRIPTION: &str =
    "List published version references for one dataset/selection. Paged; read-only.";
pub const AVAILABILITY_DESCRIPTION: &str = "Report published coverage, freshness, confirmed gaps and unresolved ranges per entity, with related task references. Paged; read-only.";
pub const TASKS_DESCRIPTION: &str =
    "Page task summaries filtered by dataset, operation, status and creation range. Read-only.";
pub const PROGRESS_DESCRIPTION: &str = "Page one task's item progress, execution results, publication versions and verification receipts. Read-only.";
pub const CAPABILITIES_DESCRIPTION: &str = "Report this server's capability matrix: implemented tools, per-dataset schemas, runtime and governance readiness, structured blocking reasons and production gate facts. Facts only — no workflow ordering or cross-server instructions.";
pub const TASK_DESCRIPTION: &str = "Read one task's status, frozen scope and per-item results.";
pub const REQUEST_DESCRIPTION: &str = "Read the durable request_id binding and its bound task or terminal result without accepting work.";
pub const CANCEL_DESCRIPTION: &str = "Request cancellation of a task under an expected_revision precondition; returns the post-request visible status and state revision.";
pub const RETRY_DESCRIPTION: &str = "Re-accept explicitly selected unfinished items of an existing task as a new linked task. Item ids only; outcomes and finished items cannot be silently re-run.";
pub const COMPUTE_DESCRIPTION: &str = "Run a named bounded metric algorithm over explicit published version inputs and return the computation or a structured not_computable outcome. Read-only.";
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerInput {}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerOutput {
    pub idle: bool,
    pub task_id: Option<String>,
    pub status: Option<TaskStatus>,
}
fn entry<I: JsonSchema, O: JsonSchema>(
    name: &'static str,
    handler: Handler,
    description: &'static str,
    example: Value,
) -> Entry {
    Entry {
        name,
        handler,
        description,
        input: serde_json::to_value(schema_for!(I)).unwrap(),
        output: serde_json::to_value(schema_for!(O)).unwrap(),
        example,
        operator_only: false,
    }
}
pub fn entries() -> Vec<Entry> {
    let query = json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},"filter":{"kind":"daily","start_date":"2025-01-01","end_date":"2025-12-31"},"version":{"kind":"latest_per_entity"}});
    let mut entries = vec![
        #[cfg(target_os = "linux")]
        entry::<InspectVerification, RequestView>(
            "inspect_verification_task",
            Handler::InspectVerification,
            INSPECT_DESCRIPTION,
            json!({"contract_version":"v1","request_id":"bounded-inspection-request","target_task_id":"verification-task-id"}),
        ),
        entry::<
            crate::publication_verification::wire::VerifyPublication,
            crate::task_verification::wire::VerificationReply,
        >(
            "verify_publication_result",
            Handler::VerifyPublication,
            crate::publication_verification::wire::DESCRIPTION,
            json!({"contract_version":"v1","request_id":"publication-check-request","target_task_id":"original-task-id"}),
        ),
        entry::<
            crate::task_verification::wire::VerifyStopped,
            crate::task_verification::wire::VerificationReply,
        >(
            "verify_stopped_writes",
            Handler::VerifyStopped,
            crate::task_verification::wire::DESCRIPTION,
            json!({"contract_version":"v1","request_id":"verification-request","target_task_id":"original-task-id"}),
        ),
        entry::<crate::gap_verification::wire::VerifyDataGap, WriteAcceptance>(
            "verify_data_gap",
            Handler::VerifyGap,
            crate::gap_verification::wire::DESCRIPTION,
            json!({"contract_version":"v1","request_id":"gap-check-request","target":{"symbol":"sh.600006","period":"day","start_date":"2025-01-01","end_date":"2025-01-10"}}),
        ),
        entry::<RangeWrite, WriteAcceptance>(
            "update_data",
            Handler::Range(crate::market_update::tasks::Operation::Update),
            UPDATE_DESCRIPTION,
            json!({"request_id":"example-update","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"published-daily-id","start_date":"2025-01-01","end_date":"2025-12-31"}]}),
        ),
        entry::<RangeWrite, WriteAcceptance>(
            "backfill_data",
            Handler::Range(crate::market_update::tasks::Operation::Backfill),
            BACKFILL_DESCRIPTION,
            json!({"request_id":"example-backfill","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"published-daily-id","start_date":"2025-01-01","end_date":"2025-12-31"}]}),
        ),
        entry::<RepublishWrite, WriteAcceptance>(
            "republish_data",
            Handler::Republish,
            REPUBLISH_DESCRIPTION,
            json!({"request_id":"example-republish","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"published-daily-id","start_date":"1990-01-01","end_date":"2025-12-31"}]}),
        ),
        entry::<WorkerInput, WorkerOutput>(
            "run_worker_once",
            Handler::Worker,
            WORKER_DESCRIPTION,
            json!({}),
        ),
        entry::<crate::metrics::wire::Compute, crate::metrics::wire::Computation>(
            "compute_metrics",
            Handler::Compute,
            COMPUTE_DESCRIPTION,
            json!({"contract_version":"v1","execution":"inline","inputs":[{"input_id":"prices","version_ref":{"dataset":"market.yearly","contract_version":"v1","entity":{"exchange":"sh","code":"600006"},"version_id":"published-yearly-id","integrity_sha256":"0000000000000000000000000000000000000000000000000000000000000000"}}],"range":{"start_year":2020,"end_year":2023},"metrics":[{"metric_id":"down","name":"consecutive_direction","algorithm_version":"local-consecutive-direction-v1","input_ids":["prices"],"parameters":{"direction":"down","min_periods":3}}]}),
        ),
        entry::<Collection, WriteAcceptance>(
            "start_collection",
            Handler::Collection,
            COLLECTION_DESCRIPTION,
            json!({"request_id":"example-daily","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600006"}]}}),
        ),
        entry::<RetryItems, WriteAcceptance>(
            "retry_task",
            Handler::Retry,
            RETRY_DESCRIPTION,
            json!({"contract_version":"v1","original_task_id":"returned-task-id","new_request_id":"retry-request","item_ids":["returned-item-id"]}),
        ),
        entry::<TaskV1, TaskSummary>(
            "get_task",
            Handler::Task,
            TASK_DESCRIPTION,
            json!({"contract_version":"v1","task_id":"returned-task-id"}),
        ),
        entry::<RequestV1, RequestView>(
            "get_request",
            Handler::Request,
            REQUEST_DESCRIPTION,
            json!({"contract_version":"v1","request_id":"returned-request-id"}),
        ),
        entry::<CancelV1, Cancellation>(
            "cancel_task",
            Handler::Cancel,
            CANCEL_DESCRIPTION,
            json!({"contract_version":"v1","task_id":"returned-task-id","expected_revision":0}),
        ),
        entry::<Query, DataPage>(
            "query_data",
            Handler::Query,
            QUERY_DESCRIPTION,
            query.clone(),
        ),
        entry::<VersionsQuery, VersionPage>(
            "list_data_versions",
            Handler::Versions,
            VERSIONS_DESCRIPTION,
            json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}}}),
        ),
        entry::<Query, AvailabilityPage>(
            "get_data_availability",
            Handler::Availability,
            AVAILABILITY_DESCRIPTION,
            query,
        ),
        entry::<TasksQuery, TaskListPage>(
            "list_tasks",
            Handler::Tasks,
            TASKS_DESCRIPTION,
            json!({}),
        ),
        entry::<ProgressQuery, ProgressPage>(
            "get_task_progress",
            Handler::Progress,
            PROGRESS_DESCRIPTION,
            json!({"task_id":"returned-task-id"}),
        ),
        entry::<CapabilitiesQuery, Capabilities>(
            "get_data_capabilities",
            Handler::Capabilities,
            CAPABILITIES_DESCRIPTION,
            json!({}),
        ),
    ];
    for e in &mut entries {
        e.operator_only = matches!(e.handler, Handler::Worker | Handler::InspectVerification);
    }
    entries
}
