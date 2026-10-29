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
    YearPresence,
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
    PlanMarketUpdate,
    RunMarketUpdate,
    Facets,
}
/// Which socket serves a tool (v8 design §3.1). The three deployed entries are
/// static allowlists; `All` is the stdio/dev surface used by `mcp_data` and
/// in-process tests, and `data_service` never accepts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Research,
    Ops,
    Maint,
    All,
}
/// Research entry (v8 §3.2): read-only discovery and queries only.
pub const RESEARCH_TOOLS: &[&str] = &[
    "get_data_capabilities",
    "query_data",
    "get_data_availability",
    "list_data_versions",
    "list_catalog_facets",
];
/// Ops entry: exactly the daily-sync Skill's data-side allowlist.
pub const OPS_TOOLS: &[&str] = &[
    "get_data_capabilities",
    "plan_market_update",
    "run_market_update",
    "start_collection",
    "update_data",
    "get_task",
    "get_task_progress",
    "list_tasks",
    "get_request",
    "get_data_availability",
    "query_data",
    "list_data_versions",
    "get_yearly_base_years",
    "verify_publication_result",
    "verify_stopped_writes",
];
/// Maintenance/forensics entry: operator-only retries, cancellation and
/// evidence checks stay off the research and ops sockets.
pub const MAINT_TOOLS: &[&str] = &[
    "get_data_capabilities",
    "backfill_data",
    "republish_data",
    "retry_task",
    "cancel_task",
    "inspect_verification_task",
    "verify_data_gap",
    "run_worker_once",
    "compute_metrics",
];
impl EntryKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "research" => Some(Self::Research),
            "ops" => Some(Self::Ops),
            "maint" => Some(Self::Maint),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Research => "research",
            Self::Ops => "ops",
            Self::Maint => "maint",
            Self::All => "all",
        }
    }
    /// The static allowlist for a deployed entry; `None` means the dev/test
    /// surface serves every registered tool.
    pub fn allowlist(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Research => Some(RESEARCH_TOOLS),
            Self::Ops => Some(OPS_TOOLS),
            Self::Maint => Some(MAINT_TOOLS),
            Self::All => None,
        }
    }
    pub fn allows(self, name: &str) -> bool {
        self.allowlist().is_none_or(|names| names.contains(&name))
    }
    /// Startup closure check: every allowlisted name must be registered and
    /// appear once. A mismatch refuses to start instead of silently serving a
    /// wider or narrower surface.
    pub fn validate(self) -> Result<(), String> {
        match self.allowlist() {
            Some(names) => self.validate_names(names),
            None => Ok(()),
        }
    }
    /// The closure check itself, over an arbitrary list: every name is
    /// registered exactly once, and the tunnel-exposed research entry never
    /// lists a writing tool (v8 phase 2 §2.2(c)). Exposed so the startup guard
    /// is testable without a second static allowlist.
    pub fn validate_names(self, names: &[&str]) -> Result<(), String> {
        let registered = entries();
        for (index, name) in names.iter().enumerate() {
            if names[..index].contains(name) {
                return Err(format!("entry {} lists {name} twice", self.name()));
            }
            let Some(tool) = registered.iter().find(|entry| entry.name == *name) else {
                return Err(format!(
                    "entry {} lists unregistered tool {name}",
                    self.name()
                ));
            };
            // Every write commits through the one process write coordinator
            // under the single database lease; a writing tool reachable from
            // the only tunnel-exposed entry would step outside that boundary.
            if self == Self::Research && tool.writes() {
                return Err(format!("research entry lists write tool {name}"));
            }
        }
        Ok(())
    }
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
    pub fn served_in(&self, entry: Option<EntryKind>) -> bool {
        entry.is_none_or(|entry| entry.allows(self.name))
    }
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
                | Handler::RunMarketUpdate
        )
    }
    /// State-changing tools: task acceptance/retry/cancellation, bounded data
    /// writes and the single worker turn. All of them commit through the one
    /// process `WriteCoordinator` under the single `DatabaseLease` (v8 phase 2
    /// §2.2(c)), and the worker turn additionally holds the shared
    /// `WorkerGate`. Reads, plans and forensics never appear here.
    pub fn writes(&self) -> bool {
        matches!(
            self.handler,
            Handler::Collection
                | Handler::Range(_)
                | Handler::Republish
                | Handler::Retry
                | Handler::Cancel
                | Handler::Worker
                | Handler::VerifyStopped
                | Handler::VerifyPublication
                | Handler::VerifyGap
                | Handler::RunMarketUpdate
        )
    }
    /// Datasets this tool advertises in get_data_capabilities. `None` is a
    /// dataset-independent tool; the input schema itself pins the exact
    /// (tool, dataset, contract_version) shape for dataset-bound tools.
    pub fn datasets(&self, access: &super::Access) -> Vec<Option<Dataset>> {
        match self.handler {
            Handler::Range(_) | Handler::PlanMarketUpdate | Handler::RunMarketUpdate => {
                [Dataset::Daily, Dataset::Yearly]
                    .into_iter()
                    .filter(|d| access.allows(*d, None))
                    .map(Some)
                    .collect()
            }
            Handler::Republish => access
                .allows(Dataset::Daily, None)
                .then_some(Some(Dataset::Daily))
                .into_iter()
                .collect(),
            Handler::Facets => access
                .allows(Dataset::Catalog, None)
                .then_some(Some(Dataset::Catalog))
                .into_iter()
                .collect(),
            Handler::Collection => [Dataset::Catalog, Dataset::Daily, Dataset::Yearly]
                .into_iter()
                .filter(|d| access.allows(*d, None))
                .map(Some)
                .collect(),
            Handler::Query | Handler::Versions | Handler::Availability => {
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
    /// Detailed-only fields are omitted from the compact catalog (v8 §6.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset: Option<Dataset>,
    /// Per-dataset point-in-time declaration (issue-391 §12.3). Present on
    /// every dataset-bound detailed item; both booleans are always serialized
    /// so a reader sees `false` rather than inferring "not declared".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalSupport>,
    pub contract_version: ContractVersion,
    pub implemented: bool,
    pub operator_only: bool,
    /// Entry serving this catalog item (v8 §3.3); "all" is the stdio surface.
    pub entry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_readiness: Option<Readiness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub governance_readiness: Option<Readiness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_reasons: Option<Vec<BlockingReason>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_scope: Option<String>,
    /// Set once immutable generations exist (#260 phase B); until then
    /// write tools bind the installed inspected snapshot at execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_generation_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_digest: Option<String>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogFacetsQuery {
    /// Only `securities.catalog` is served today; omitted means the catalog.
    pub dataset: Option<Dataset>,
    /// A frozen catalog version id; omitted or "latest" resolves the head.
    pub catalog_version: Option<String>,
}
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogFacets {
    pub dataset: Dataset,
    pub catalog_version: String,
    pub facets: Facets,
}
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Facets {
    pub industries: Vec<IndustryFacet>,
}
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IndustryFacet {
    pub code: String,
    pub name: String,
    pub members: u64,
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
pub const UPDATE_DESCRIPTION: &str = "Routine daily refresh. market.daily explicit items bind a published base version; the default selection resolves frozen heads and proven gaps. Unchanged inputs land no_change; revisions fail closed. Not for first establishment.";
pub const BACKFILL_DESCRIPTION: &str = "Bounded backfill, not the routine path: market.daily explicit items fill a named range over a published base; the default selection resolves only frozen proven gaps. Revisions fail closed; never a full-market refresh.";
pub const REPUBLISH_DESCRIPTION: &str = "Local-owner explicit-range source refresh: re-acquires a fixed start/end window over a pinned market.daily head through the same commit path as update_data. A superseded base needs_verification; revisions fail closed.";
pub const WORKER_DESCRIPTION: &str = "Operator/debug single step of at most one locally owned pending task. The background worker already runs tasks, so routine synchronization must not call this; it never downloads market data.";
pub const COLLECTION_DESCRIPTION: &str = "First establishment only: initial publication for entities with no published head. v1 takes explicit entities, catalog version, or default; v2 scope sh_sz_a_listed_ever resolves delisted A shares from the security master (operator-only).";
pub const INSPECT_DESCRIPTION: &str = "Owner-only bounded inspection of an abnormal verification task. Each request_id is consumed by one persisted result; a new id is needed for fresh evidence. Never closes the business task.";
pub const QUERY_DESCRIPTION: &str = "Read integrity-checked published rows for one dataset and selection. filter is dataset-specific; version defaults to latest_per_entity. Paged and snapshot-scoped; read-only, never collects or repairs.";
pub const VERSIONS_DESCRIPTION: &str = "List published version references for one dataset and selection; market datasets report at most one current head per entity. Paged; read-only.";
pub const AVAILABILITY_DESCRIPTION: &str = "Report published coverage, freshness, confirmed gaps and unresolved ranges per entity, with related task refs. Paged; read-only.";
pub const TASKS_DESCRIPTION: &str =
    "Page task summaries filtered by dataset, operation, status and creation range. Read-only.";
pub const PROGRESS_DESCRIPTION: &str = "Page one task's item progress, results, versions and verification receipts. Optional wait_seconds (max 50) with since_revision holds until terminal or advanced. Read-only.";
pub const PRESENCE_DESCRIPTION: &str = "Resolve the natural years present in pinned market.daily bases: 1..100 entity/base items in one bounded snapshot with verified integrity digests. Read-only; no cursor.";
pub const CAPABILITIES_DESCRIPTION: &str = "Report capability revision and the tool catalog. No filter: compact catalog. dataset plus operation: detailed schema, example and readiness. Facts only; no workflow ordering.";
pub const FACETS_DESCRIPTION: &str = "List catalog facets: industry code, name and member count for a frozen catalog version (default current). Read-only; pair with query_data filter.industry_code.";
pub const TASK_DESCRIPTION: &str = "Read one task's status, frozen scope and per-item results.";
pub const REQUEST_DESCRIPTION: &str = "Read the durable request_id binding and its bound task or terminal result without accepting work.";
pub const CANCEL_DESCRIPTION: &str = "Request cancellation of a task. expected_revision is optional: omitted reads the current revision, an explicit value keeps the CAS precondition. Returns visible status and state_revision.";
pub const RETRY_DESCRIPTION: &str = "Re-accept explicitly selected unfinished items of an existing task as a new linked task. Item ids only; finished items cannot be silently re-run.";
pub const COMPUTE_DESCRIPTION: &str = "Run a named bounded metric algorithm over explicit published version inputs; returns the computation or a structured not_computable outcome. Read-only.";
pub const PLAN_DESCRIPTION: &str = "Whole-catalog update plan: resolves the catalog against published heads and returns counts plus a deterministic plan_id binding the active generation and target day. Read-only; never lists entities.";
pub const RUN_DESCRIPTION: &str = "Execute a previously returned plan. plan_id must match a re-resolved plan exactly, else plan_stale and no task. One task covers every eligible entity; per-item outcomes published/no_change/needs_verification.";
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
    let mut input = schema_for!(I);
    // Read schemas drop the redundant `additionalProperties:false` keyword to
    // stay inside the entry byte budget; runtime decoding still denies it.
    // Write schemas keep the keyword: their closed-shape tests rely on it.
    if !matches!(
        handler,
        Handler::Collection
            | Handler::Range(_)
            | Handler::Republish
            | Handler::Retry
            | Handler::Cancel
            | Handler::Worker
            | Handler::PlanMarketUpdate
            | Handler::RunMarketUpdate
    ) {
        super::wire::strip_additional_properties(&mut input);
    }
    Entry {
        name,
        handler,
        description,
        input: serde_json::to_value(input).unwrap(),
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
        entry::<TaskQuery, TaskSummary>(
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
        entry::<CatalogFacetsQuery, CatalogFacets>(
            "list_catalog_facets",
            Handler::Facets,
            FACETS_DESCRIPTION,
            json!({"dataset":"securities.catalog"}),
        ),
        entry::<YearPresenceQuery, YearPresenceReply>(
            "get_yearly_base_years",
            Handler::YearPresence,
            PRESENCE_DESCRIPTION,
            json!({"contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":"published-daily-id","start_year":2020,"end_year":2026}]}),
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
        entry::<PlanMarketUpdate, PlanView>(
            "plan_market_update",
            Handler::PlanMarketUpdate,
            PLAN_DESCRIPTION,
            json!({"dataset":"market.daily","contract_version":"v1"}),
        ),
        entry::<RunMarketUpdate, PlanRunAcceptance>(
            "run_market_update",
            Handler::RunMarketUpdate,
            RUN_DESCRIPTION,
            json!({"dataset":"market.daily","contract_version":"v1","request_id":"example-plan-run","target_day":"2026-01-30","plan_id":"0000000000000000000000000000000000000000000000000000000000000000"}),
        ),
    ];
    for e in &mut entries {
        e.operator_only = matches!(e.handler, Handler::Worker | Handler::InspectVerification);
    }
    entries
}
