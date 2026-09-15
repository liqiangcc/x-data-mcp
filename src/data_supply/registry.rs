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
    VerifyGap,
}
pub struct Entry {
    pub name: &'static str,
    pub handler: Handler,
    pub input: Value,
    pub output: Value,
    pub example: Value,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesQuery {
    pub dataset: Option<Dataset>,
    pub operation: Option<String>,
}
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub tool_name: String,
    pub description: String,
    pub dataset: Option<Dataset>,
    pub contract_version: ContractVersion,
    pub implemented: bool,
    pub readiness: String,
    pub blocking_reasons: Vec<String>,
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
    pub production_ready: bool,
}
pub const RANGE_DESCRIPTION: &str = "Explicit market.daily ranges or market.yearly natural-year sets, each bound to a published daily version; market.daily also accepts selection {\"kind\":\"default\"}, resolved and frozen from the acceptance snapshot (catalog + started securities + published heads + open proven gaps). Yearly publication mode and comparison are explicit and frozen; historical mode never advances head. Automatic gap discovery and catalog updates are not implemented. Production source remains blocked; the installed worker records SOURCE_NOT_VERIFIED for explicit ranges and fails closed on the default frozen shape until default execution lands.";
pub const WORKER_DESCRIPTION: &str = "Stage A development-only control: synchronously executes at most one locally owned pending task, mutating task outcomes and occupancy. Installed market executors record SOURCE_NOT_VERIFIED; stopping inspections may interrupt proven never-started tasks; started or unsupported executions retain their claims with insufficient evidence. Publication inspections record fixed durable evidence without changing original tasks. It never downloads or publishes market data or forcibly unlocks original tasks. Not a background scheduler or production pipeline.";
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
    example: Value,
) -> Entry {
    Entry {
        name,
        handler,
        input: serde_json::to_value(schema_for!(I)).unwrap(),
        output: serde_json::to_value(schema_for!(O)).unwrap(),
        example,
    }
}
pub fn entries() -> Vec<Entry> {
    let query = json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},"filter":{"kind":"daily","start_date":"2025-01-01","end_date":"2025-12-31"},"version":{"kind":"latest_per_entity"}});
    vec![
        #[cfg(target_os = "linux")]
        entry::<InspectVerification, RequestView>(
            "inspect_verification_task",
            Handler::InspectVerification,
            json!({"contract_version":"v1","request_id":"bounded-inspection-request","target_task_id":"verification-task-id"}),
        ),
        entry::<
            crate::publication_verification::wire::VerifyPublication,
            crate::task_verification::wire::VerificationReply,
        >(
            "verify_publication_result",
            Handler::VerifyPublication,
            json!({"contract_version":"v1","request_id":"publication-check-request","target_task_id":"original-task-id"}),
        ),
        entry::<
            crate::task_verification::wire::VerifyStopped,
            crate::task_verification::wire::VerificationReply,
        >(
            "verify_stopped_writes",
            Handler::VerifyStopped,
            json!({"contract_version":"v1","request_id":"verification-request","target_task_id":"original-task-id"}),
        ),
        entry::<crate::gap_verification::wire::VerifyDataGap, WriteAcceptance>(
            "verify_data_gap",
            Handler::VerifyGap,
            json!({"contract_version":"v1","request_id":"gap-check-request","target":{"symbol":"sh.600006","period":"day","start_date":"2025-01-01","end_date":"2025-01-10"}}),
        ),
        entry::<RangeWrite, WriteAcceptance>(
            "update_data",
            Handler::Range(crate::market_update::tasks::Operation::Update),
            json!({"request_id":"example-update","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"published-daily-id","start_date":"2025-01-01","end_date":"2025-12-31"}]}),
        ),
        entry::<RangeWrite, WriteAcceptance>(
            "backfill_data",
            Handler::Range(crate::market_update::tasks::Operation::Backfill),
            json!({"request_id":"example-backfill","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"published-daily-id","start_date":"2025-01-01","end_date":"2025-12-31"}]}),
        ),
        entry::<WorkerInput, WorkerOutput>("run_worker_once", Handler::Worker, json!({})),
        entry::<crate::metrics::wire::Compute, crate::metrics::wire::Computation>(
            "compute_metrics",
            Handler::Compute,
            json!({"contract_version":"v1","execution":"inline","inputs":[{"input_id":"prices","version_ref":{"dataset":"market.yearly","contract_version":"v1","entity":{"exchange":"sh","code":"600006"},"version_id":"published-yearly-id","integrity_sha256":"0000000000000000000000000000000000000000000000000000000000000000"}}],"range":{"start_year":2020,"end_year":2023},"metrics":[{"metric_id":"down","name":"consecutive_direction","algorithm_version":"local-consecutive-direction-v1","input_ids":["prices"],"parameters":{"direction":"down","min_periods":3}}]}),
        ),
        entry::<Collection, WriteAcceptance>(
            "start_collection",
            Handler::Collection,
            json!({"request_id":"example-daily","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600006"}]}}),
        ),
        entry::<RetryItems, WriteAcceptance>(
            "retry_task",
            Handler::Retry,
            json!({"contract_version":"v1","original_task_id":"returned-task-id","new_request_id":"retry-request","item_ids":["returned-item-id"]}),
        ),
        entry::<TaskV1, TaskSummary>(
            "get_task",
            Handler::Task,
            json!({"contract_version":"v1","task_id":"returned-task-id"}),
        ),
        entry::<RequestV1, RequestView>(
            "get_request",
            Handler::Request,
            json!({"contract_version":"v1","request_id":"returned-request-id"}),
        ),
        entry::<CancelV1, Cancellation>(
            "cancel_task",
            Handler::Cancel,
            json!({"contract_version":"v1","task_id":"returned-task-id","expected_revision":0}),
        ),
        entry::<Query, DataPage>("query_data", Handler::Query, query.clone()),
        entry::<VersionsQuery, VersionPage>(
            "list_data_versions",
            Handler::Versions,
            json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}}}),
        ),
        entry::<Query, AvailabilityPage>("get_data_availability", Handler::Availability, query),
        entry::<TasksQuery, TaskListPage>("list_tasks", Handler::Tasks, json!({})),
        entry::<ProgressQuery, ProgressPage>(
            "get_task_progress",
            Handler::Progress,
            json!({"task_id":"returned-task-id"}),
        ),
        entry::<CapabilitiesQuery, Capabilities>(
            "get_data_capabilities",
            Handler::Capabilities,
            json!({}),
        ),
    ]
}

pub const INSPECTION_DESCRIPTION: &str = "Owner-only bounded management of an abnormal verification task, not a recursive asynchronous task. Each reliably persisted result, including insufficient evidence, consumes request_id; the same ID replays its historical result without re-inspecting. A new ID is needed for fresh evidence. Checks registered local execution exclusion and durable finish separately; may close only the verification task, never the original business task or its claims. Missing/unregistered runtime proof retains protection. No force/evidence flags, waiting, implicit retry or production source approval.";
