//! One typed contract feeds decoding, schema discovery and handler registration.
use crate::market_data::Period;
use chrono::{DateTime, NaiveDate, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Dataset {
    #[serde(rename = "securities.catalog")]
    Catalog,
    #[serde(rename = "market.daily")]
    Daily,
    #[serde(rename = "market.yearly")]
    Yearly,
}
impl Dataset {
    pub fn period(self) -> Option<Period> {
        match self {
            Self::Daily => Some(Period::Day),
            Self::Yearly => Some(Period::Year),
            Self::Catalog => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ContractVersion {
    #[serde(rename = "v1")]
    V1,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Security {
    pub exchange: Exchange,
    pub code: String,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Exchange {
    Sh,
    Sz,
}
impl Security {
    pub fn symbol(&self) -> String {
        format!(
            "{}.{}",
            match self.exchange {
                Exchange::Sh => "sh",
                Exchange::Sz => "sz",
            },
            self.code
        )
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.code.len() == 6 && self.code.bytes().all(|b| b.is_ascii_digit()) {
            Ok(())
        } else {
            Err("six digit security code required".into())
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Selection {
    Single { entity: Security },
    Batch { entities: Vec<Security> },
    Catalog { scope: CatalogScope },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CatalogScope {
    #[serde(rename = "current_sh_sz_a")]
    CurrentShSzA,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Filter {
    Daily {
        start_date: NaiveDate,
        end_date: NaiveDate,
    },
    Yearly {
        start_year: i32,
        end_year: i32,
    },
    Catalog {
        code: Option<String>,
        name_query: Option<String>,
        industry_code: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionBinding {
    pub entity: Security,
    pub version_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VersionChoice {
    LatestPerEntity,
    ExplicitVersionMap { versions: Vec<VersionBinding> },
    CatalogVersion { version_id: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub dataset: Dataset,
    pub contract_version: ContractVersion,
    pub selection: Selection,
    pub filter: Filter,
    pub version: VersionChoice,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}
pub fn default_page() -> usize {
    100
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionRef {
    pub dataset: Dataset,
    pub contract_version: ContractVersion,
    pub entity: Option<Security>,
    pub version_id: String,
    pub integrity_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub entity: Option<Security>,
    pub version_ref: Option<VersionRef>,
    pub status: ManifestStatus,
    pub metadata: Option<Metadata>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManifestStatus {
    Present,
    Absent,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Industry {
    Assigned { code: String, name: String },
    Missing { reason: String },
    Conflicted { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Row {
    Market {
        entity: Security,
        version_ref: VersionRef,
        date: NaiveDate,
        first_trading_date: Option<NaiveDate>,
        last_trading_date: Option<NaiveDate>,
        year_unfinished: Option<bool>,
        open: String,
        high: String,
        low: String,
        close: String,
        volume: Option<String>,
        amount: Option<String>,
    },
    Catalog {
        entity: Security,
        version_ref: VersionRef,
        name: String,
        industry: Industry,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DataPage {
    pub dataset: Dataset,
    pub contract_version: ContractVersion,
    pub snapshot_id: String,
    pub query_time: DateTime<Utc>,
    pub version_manifest: Vec<ManifestEntry>,
    pub records: Vec<Row>,
    pub next_cursor: Option<String>,
    pub page_complete: bool,
    pub selection_complete: bool,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionsQuery {
    pub dataset: Dataset,
    pub contract_version: ContractVersion,
    pub selection: Selection,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionInfo {
    pub version_ref: VersionRef,
    pub published_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionPage {
    pub snapshot_id: String,
    pub snapshot_time: DateTime<Utc>,
    pub items: Vec<VersionInfo>,
    pub next_cursor: Option<String>,
    pub page_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub field: String,
    pub status: String,
    pub value: Option<String>,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lineage {
    pub base_daily_version_id: String,
    pub algorithm_version: String,
    pub first_trading_date: NaiveDate,
    pub last_trading_date: NaiveDate,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub source: String,
    pub published_at: DateTime<Utc>,
    pub coverage_status: String,
    pub coverage_reason: String,
    pub units: Vec<Evidence>,
    pub lineage: Option<Lineage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TasksQuery {
    pub dataset: Option<Dataset>,
    pub operation: Option<TaskOperation>,
    pub status: Option<Vec<TaskStatus>>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskOperation {
    StartCollection,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    CancelRequested,
    NeedsVerification,
    Succeeded,
    PartiallyFailed,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressQuery {
    pub task_id: String,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskSummary {
    pub task_id: String,
    pub request_id: String,
    pub dataset: String,
    pub operation: String,
    pub contract_version: String,
    pub status: TaskStatus,
    pub state_revision: u64,
    pub created_at: DateTime<Utc>,
    pub last_progress_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressItem {
    pub item_id: String,
    pub entity: Option<Security>,
    pub period: Option<String>,
    pub progress_unit: String,
    pub stage: String,
    pub success: Option<bool>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskListPage {
    pub snapshot_id: String,
    pub snapshot_time: DateTime<Utc>,
    pub items: Vec<TaskSummary>,
    pub next_cursor: Option<String>,
    pub page_complete: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressPage {
    pub snapshot_id: String,
    pub snapshot_time: DateTime<Utc>,
    pub task: TaskSummary,
    pub total: usize,
    pub finished: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub pending: usize,
    pub running: usize,
    pub execution_percent: String,
    pub unfinished: Vec<ProgressItem>,
    pub next_cursor: Option<String>,
    pub page_complete: bool,
    pub publication_versions: Vec<VersionRef>,
    pub frozen_daily_ranges: Vec<FrozenDailyRange>,
    pub range_results: Vec<RangeExecutionResult>,
    pub frozen_yearly_inputs: Vec<FrozenYearlyInput>,
    pub yearly_results: Vec<YearlyExecutionResult>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Absent,
    Unknown,
    Unavailable,
    Available,
    Partial,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityItem {
    pub entity: Option<Security>,
    pub version_ref: Option<VersionRef>,
    pub availability: Availability,
    pub integrity: String,
    pub confirmed_gaps: Vec<ConfirmedGap>,
    pub unresolved_ranges: Vec<Filter>,
    pub coverage_evidence: String,
    pub first_record: Option<NaiveDate>,
    pub last_record: Option<NaiveDate>,
    pub record_count: usize,
    pub freshness: String,
    pub related_tasks: Vec<TaskSummary>,
    pub limitations: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityPage {
    pub snapshot_id: String,
    pub snapshot_time: DateTime<Utc>,
    pub items: Vec<AvailabilityItem>,
    pub next_cursor: Option<String>,
    pub page_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WriteSelection {
    Explicit {
        securities: Vec<Security>,
    },
    Catalog {
        version_id: String,
        industry_code: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "dataset", deny_unknown_fields)]
pub enum Collection {
    #[serde(rename = "market.daily")]
    Daily {
        request_id: String,
        contract_version: ContractVersion,
        selection: WriteSelection,
    },
    #[serde(rename = "securities.catalog")]
    Catalog {
        request_id: String,
        contract_version: ContractVersion,
        selection: CatalogSelection,
    },
    #[serde(rename = "market.yearly")]
    Yearly {
        request_id: String,
        contract_version: ContractVersion,
        selection: WriteSelection,
        base_daily_versions: Vec<VersionBinding>,
        years: Vec<i32>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogSelection {
    pub scope: CatalogScope,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteAcceptance {
    pub request_id: String,
    pub task_id: String,
    pub dataset: String,
    pub contract_version: String,
    pub reused: bool,
    pub status: TaskStatus,
    pub item_ids: Vec<String>,
    pub readiness: String,
    pub blocking_reasons: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryItems {
    pub contract_version: ContractVersion,
    pub original_task_id: String,
    pub new_request_id: String,
    pub item_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskV1 {
    pub contract_version: ContractVersion,
    pub task_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestV1 {
    pub contract_version: ContractVersion,
    pub request_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestView {
    pub request_id: String,
    pub bound: bool,
    pub task: Option<TaskSummary>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelV1 {
    pub contract_version: ContractVersion,
    pub task_id: String,
    pub expected_revision: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cancellation {
    pub task_id: String,
    pub status: TaskStatus,
    pub state_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedGap {
    pub range: Filter,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RangeWriteItem {
    pub entity: Security,
    pub base_version_id: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "dataset", deny_unknown_fields)]
pub enum RangeWrite {
    #[serde(rename = "market.daily")]
    Daily {
        request_id: String,
        contract_version: ContractVersion,
        #[schemars(length(min = 1))]
        items: Vec<RangeWriteItem>,
    },
    #[serde(rename = "market.yearly")]
    Yearly {
        request_id: String,
        contract_version: ContractVersion,
        #[schemars(length(min = 1))]
        items: Vec<YearlyWriteItem>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum YearlyPublication {
    HistoricalFixedBase {
        comparison_version_id: Option<String>,
    },
    CurrentHead {
        expected_yearly_head: Option<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct YearlyWriteItem {
    pub entity: Security,
    pub base_daily_version_id: String,
    #[schemars(length(min = 1))]
    pub years: Vec<i32>,
    pub publication: YearlyPublication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrozenDailyRange {
    pub item_id: String,
    pub entity: Security,
    pub base_version_id: String,
    pub base_integrity_sha256: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RangeExecutionResult {
    pub result_id: String,
    pub item_id: String,
    pub base_version: VersionRef,
    pub result_version: VersionRef,
    pub no_change: bool,
    pub acquired_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonVersion {
    pub version_id: String,
    pub integrity_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrozenYearlyInput {
    pub item_id: String,
    pub entity: Security,
    pub base_daily_version_id: String,
    pub base_integrity_sha256: String,
    pub years: Vec<i32>,
    pub publication: YearlyPublication,
    pub comparison: Option<ComparisonVersion>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct YearlyExecutionResult {
    pub result_id: String,
    pub item_id: String,
    pub base_daily_version: VersionRef,
    pub result_version: VersionRef,
    pub no_change: bool,
    pub derived_at: DateTime<Utc>,
}
