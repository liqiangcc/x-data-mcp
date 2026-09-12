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
