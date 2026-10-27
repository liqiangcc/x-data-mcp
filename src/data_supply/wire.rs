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
    // M1 PR-5 benchmark series (issue-383 §6.3): a raw, 0.01-point bar
    // series kept apart from `market.daily` so an index never changes the
    // stock query's semantics (v8 #387 §4.2 constraint 3).
    #[serde(rename = "market.index")]
    Index,
    // M1 PR-4 parsed corporate-action facts (issue-383 §3.6): effective-day
    // rows, no versioned price series.
    #[serde(rename = "market.adjustment_events")]
    AdjustmentEvents,
    // M1 PR-6 derived trading calendar (issue-383 §3.8): market-level, no
    // entity and no security price.
    #[serde(rename = "market.calendar")]
    Calendar,
    // M2 parsed-fact datasets (issue-391 §12.3). They have no v1 row shape
    // both because they arrived after v7 and because their rows are intervals,
    // not price panels; a `v1` request is rejected rather than reshaped.
    #[serde(rename = "market.security_master")]
    SecurityMaster,
    #[serde(rename = "market.security_names")]
    SecurityNames,
    #[serde(rename = "market.capital_events")]
    CapitalEvents,
    #[serde(rename = "market.risk_warnings")]
    RiskWarnings,
    #[serde(rename = "market.suspensions")]
    Suspensions,
    #[serde(rename = "market.price_limit_rules")]
    PriceLimitRules,
}
impl Dataset {
    pub fn period(self) -> Option<Period> {
        match self {
            Self::Daily | Self::Index => Some(Period::Day),
            Self::Yearly => Some(Period::Year),
            Self::Catalog
            | Self::AdjustmentEvents
            | Self::Calendar
            | Self::SecurityMaster
            | Self::SecurityNames
            | Self::CapitalEvents
            | Self::RiskWarnings
            | Self::Suspensions
            | Self::PriceLimitRules => None,
        }
    }
    /// Wire spelling; used by v2 error messages (v8 PR-3, #387 §5).
    pub fn name(self) -> &'static str {
        match self {
            Self::Catalog => "securities.catalog",
            Self::Daily => "market.daily",
            Self::Yearly => "market.yearly",
            Self::Index => "market.index",
            Self::AdjustmentEvents => "market.adjustment_events",
            Self::Calendar => "market.calendar",
            Self::SecurityMaster => "market.security_master",
            Self::SecurityNames => "market.security_names",
            Self::CapitalEvents => "market.capital_events",
            Self::RiskWarnings => "market.risk_warnings",
            Self::Suspensions => "market.suspensions",
            Self::PriceLimitRules => "market.price_limit_rules",
        }
    }
    /// The datasets added after v7 have no v1 row shape: their requests
    /// require `contract_version: "v2"` (v8 PR-4, #387 §14; M2 in #391 §12.3).
    pub fn is_v2_only(self) -> bool {
        matches!(
            self,
            Self::Index
                | Self::AdjustmentEvents
                | Self::Calendar
                | Self::SecurityMaster
                | Self::SecurityNames
                | Self::CapitalEvents
                | Self::RiskWarnings
                | Self::Suspensions
                | Self::PriceLimitRules
        )
    }
    /// M2 facts served by the research read model (issue-391 §12.3). This is
    /// the one predicate the decode layer, capability declaration and storage
    /// branch share; adding an M2 dataset in one place cannot silently leave
    /// another accepting point-in-time parameters it cannot honour.
    pub fn is_m2(self) -> bool {
        matches!(
            self,
            Self::SecurityMaster
                | Self::SecurityNames
                | Self::CapitalEvents
                | Self::RiskWarnings
                | Self::Suspensions
                | Self::PriceLimitRules
        )
    }
    /// Whether rows carry an entity column (the interval/point facts). The
    /// whole-market `market.price_limit_rules` does not.
    pub fn is_entity_bound(self) -> bool {
        matches!(
            self,
            Self::SecurityMaster
                | Self::SecurityNames
                | Self::CapitalEvents
                | Self::RiskWarnings
                | Self::Suspensions
        )
    }
    /// Per-dataset `temporal` declaration published by
    /// `get_data_capabilities` (issue-391 §12.3). A dataset absent from this
    /// table declares neither parameter, so both are rejected with
    /// `unsupported_for_dataset`.
    pub fn temporal(self) -> TemporalSupport {
        match self {
            Self::SecurityMaster | Self::SecurityNames | Self::RiskWarnings | Self::Suspensions => {
                TemporalSupport::declared(TemporalCoverage::Partial)
            }
            Self::CapitalEvents | Self::PriceLimitRules => {
                TemporalSupport::as_of_only(TemporalCoverage::None)
            }
            _ => TemporalSupport::NONE,
        }
    }
    /// The read model's dataset name in `temporal.datasets_without_known_at`
    /// (issue-391 §12.2). `None` for datasets the read model never observes.
    pub fn temporal_name(self) -> Option<&'static str> {
        Some(match self {
            Self::SecurityMaster => "security_master",
            Self::SecurityNames => "security_names",
            Self::CapitalEvents => "capital_events",
            Self::RiskWarnings => "risk_warnings",
            Self::Suspensions => "suspensions",
            Self::PriceLimitRules => "price_limit_rules",
            _ => return None,
        })
    }
}
/// How much of a dataset carries a 知晓时间 (mirrors the read model's
/// `KnownAtCoverage`, issue-391 §12.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemporalCoverage {
    Full,
    Partial,
    None,
}
/// The per-dataset `temporal` declaration in `get_data_capabilities`
/// (issue-391 §12.3). `as_of`/`known_at` are accepted only when the named
/// field is `true`; otherwise the decode layer rejects them with
/// `invalid_arguments` + `unsupported_for_dataset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TemporalSupport {
    pub as_of: bool,
    pub known_at: bool,
    pub known_at_coverage: TemporalCoverage,
}
impl TemporalSupport {
    /// No point-in-time parameter is declared.
    pub const NONE: Self = Self {
        as_of: false,
        known_at: false,
        known_at_coverage: TemporalCoverage::None,
    };
    const fn declared(coverage: TemporalCoverage) -> Self {
        Self {
            as_of: true,
            known_at: true,
            known_at_coverage: coverage,
        }
    }
    const fn as_of_only(coverage: TemporalCoverage) -> Self {
        Self {
            as_of: true,
            known_at: false,
            known_at_coverage: coverage,
        }
    }
}
/// The `temporal` block every M2 read-model answer carries (issue-391 §12.2):
/// the point in time used, the information-set cutoff (if any), how many rows
/// lost their 知晓时间, and which datasets never had one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TemporalBlock {
    pub as_of: NaiveDate,
    pub known_at: Option<NaiveDate>,
    pub unknown_known_at_rows: usize,
    pub datasets_without_known_at: Vec<String>,
}
/// One cell of an M2 fact row. The v2 layouts project these by field name;
/// the tagged `Row` shape is never serialized for these datasets (they are
/// v2-only), so a closed value vocabulary is enough.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FactValue {
    Null,
    Bool(bool),
    Int(i64),
    Text(String),
}
impl FactValue {
    pub fn json(self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(value) => serde_json::json!(value),
            Self::Int(value) => serde_json::json!(value),
            Self::Text(value) => serde_json::json!(value),
        }
    }
    pub fn day(day: Option<NaiveDate>) -> Self {
        day.map_or(Self::Null, |day| Self::Text(day.to_string()))
    }
}
// Write surfaces accept `v1` only; read surfaces use `ReadContractVersion`,
// so a write tool's schema never advertises a version it rejects (v8 PR-3,
// #387 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ContractVersion {
    #[serde(rename = "v1")]
    V1,
}

// `v1` keeps the row/task shape, `v2` selects the columnar read shape (PR-3)
// or the task diagnosis shape (PR-5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ReadContractVersion {
    #[serde(rename = "v1")]
    #[default]
    V1,
    #[serde(rename = "v2")]
    V2,
}
impl ReadContractVersion {
    /// Cursor keys and v1 responses omit the default version, so the v1
    /// bytes stay exactly what they were before v2 existed.
    pub fn is_v1(&self) -> bool {
        *self == Self::V1
    }
}
/// Security identity (`{exchange, code}`; `"sh.600000"` is also accepted).
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
    /// Parse the canonical identity text `"sh.600000"` (the spelling used by
    /// tasks, scopes and logs). Six-digit validation stays in `validate`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let Some((exchange, code)) = text.split_once('.') else {
            return Err(format!(
                "expected \"sh.600000\" or {{\"exchange\":\"sh\",\"code\":\"600000\"}}, got {text:?}"
            ));
        };
        let exchange = match exchange.to_ascii_lowercase().as_str() {
            "sh" => Exchange::Sh,
            "sz" => Exchange::Sz,
            other => return Err(format!("exchange must be sh or sz, got {other:?}")),
        };
        Ok(Self {
            exchange,
            code: code.to_owned(),
        })
    }
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
            Err(format!(
                "code must be six digits, got {:?}; pass the whole identity as entity: \"sh.600000\" or {{\"exchange\":\"sh\",\"code\":\"600000\"}}",
                self.code
            ))
        }
    }
}
/// `deny_unknown_fields` already rejects extras at runtime; dropping the
/// redundant schema keyword keeps `tools/list` inside its byte budget
/// (v8 #387 §6.3). Applies to published input schemas, not to validation.
pub(crate) fn strip_additional_properties(schema: &mut schemars::Schema) {
    fn walk(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                map.remove("additionalProperties");
                for value in map.values_mut() {
                    walk(value);
                }
            }
            serde_json::Value::Array(items) => {
                for value in items {
                    walk(value);
                }
            }
            _ => {}
        }
    }
    if let Some(map) = schema.as_object_mut() {
        map.remove("additionalProperties");
        for value in map.values_mut() {
            walk(value);
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Selection {
    Single {
        entity: Security,
    },
    Batch {
        entities: Vec<Security>,
    },
    Catalog {
        scope: CatalogScope,
    },
    // A frozen catalog version's whole member set (v8 #387 §4.3). The
    // server resolves `"latest"` and echoes the concrete version id.
    // `market.index` resolves its members from the static benchmark
    // registry instead, so it omits `catalog_version`.
    Universe {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        catalog_version: Option<String>,
    },
    // A market-level dataset with no security entity (`market.calendar`).
    // Struct variant, not a unit: `deny_unknown_fields` ignores extra keys on
    // internally-tagged unit variants (see default_range.rs).
    Market {},
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
        /// M2 only: the interval/point must be effective at this day. It is
        /// rejected (not ignored) for every dataset whose `temporal.as_of` is
        /// false (issue-391 §12.2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        as_of: Option<NaiveDate>,
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
    // At most five trading days (v8 #387 §4.3): a cross-section is not a
    // panel. The repository resolves the trading days from the calendar.
    CrossSection {
        start_date: NaiveDate,
        end_date: NaiveDate,
    },
}
impl Filter {
    /// The M2 point-in-time value lives inside `filter` (issue-387 §4.6,
    /// issue-391 §12.2): it constrains the record set, not the selection.
    pub fn as_of(&self) -> Option<NaiveDate> {
        match self {
            Self::Daily { as_of, .. } => *as_of,
            _ => None,
        }
    }
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
    LatestPerEntity {},
    ExplicitVersionMap { versions: Vec<VersionBinding> },
    CatalogVersion { version_id: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub dataset: Dataset,
    pub contract_version: ReadContractVersion,
    pub selection: Selection,
    pub filter: Filter,
    pub version: VersionChoice,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
    // v2 only: column projection (omitted = the dataset's core fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<String>>,
    /// M2 only: restrict the whole information set to rows known by this day
    /// (issue-391 §12.2). Rejected for datasets whose `temporal.known_at` is
    /// false, and only accepted by the query tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_at: Option<NaiveDate>,
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
    /// Set on present entries whose stored content trails the live source:
    /// `"behind"` — the row serves data derived from an older daily base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<String>,
    /// Companion to `stale`: the daily base day the row is correct as of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_day: Option<NaiveDate>,
    /// Set on absent entries rejected for a classified reason (yearly rows
    /// whose daily base was superseded or whose value semantics moved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<AbsentReason>,
}
/// Fixed `reason` enum for absent manifest entries (SPEC/RULINGS A1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AbsentReason {
    DailyReplacedSinceDerivation,
    ValueSemanticsChanged,
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
    /// One derived calendar day (`market.calendar`): market-level, no entity.
    Calendar { trade_day: NaiveDate },
    /// One parsed factor event (`market.adjustment_events`). `known_at` is
    /// deliberately absent: M1 stores NULL and the design forbids inventing a
    /// point-in-time value (v8 #387 §4.6, separation point 3).
    AdjustmentEvent {
        entity: Security,
        effective_day: NaiveDate,
        category: u8,
        c1: String,
        c2: String,
        c3: String,
        c4: String,
    },
    /// One M2 parsed-fact row (issue-391 §12.3). `day` is the row's ordering
    /// key and the availability window bound; `cells` follows the dataset's
    /// column set. Never serialized on the wire: these datasets are v2-only.
    Fact {
        entity: Option<Security>,
        day: NaiveDate,
        cells: std::collections::BTreeMap<String, FactValue>,
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
    /// M2 point-in-time facts (issue-391 §12.2); absent for datasets that
    /// declare neither parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionsQuery {
    pub dataset: Dataset,
    pub contract_version: ReadContractVersion,
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
    pub adjustment: Option<Evidence>,
    pub lineage: Option<Lineage>,
    /// Per-index unit/aggregation facts (issue-383 §3.9). Present only for
    /// `market.index`; an index's volume unit is 手, not 股.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<IndexMetadata>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IndexMetadata {
    pub aggregate_scope: String,
    pub member_count: Option<i64>,
    pub price_unit: String,
    pub volume_unit: String,
    pub amount_unit: String,
    pub aliases: String,
    /// Bars dropped from this index series because their own OHLC ordering was
    /// impossible (#383 owner ruling 2026-10-27). Always 0 for a clean member.
    pub excluded_bar_count: usize,
}

fn presence_min_year() -> i32 {
    1
}
fn presence_max_year() -> i32 {
    9999
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct YearPresenceItem {
    pub entity: Security,
    pub base_daily_version_id: String,
    #[serde(default = "presence_min_year")]
    pub start_year: i32,
    #[serde(default = "presence_max_year")]
    pub end_year: i32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct YearPresenceQuery {
    pub contract_version: ContractVersion,
    #[schemars(length(min = 1, max = 100))]
    pub items: Vec<YearPresenceItem>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum YearPresenceStatus {
    Resolved,
    Unavailable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum YearPresenceReason {
    BaseUnavailable,
    /// The pinned daily id was committed but is superseded by a newer head
    /// — ordinary expiry, not corruption (OWNER-RULING-PAR-1A R2').
    BaseSuperseded,
    IntegrityFailure,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct YearPresenceResult {
    pub entity: Security,
    pub base_daily_version_id: String,
    pub status: YearPresenceStatus,
    pub version_ref: Option<VersionRef>,
    pub present_years: Option<Vec<i32>>,
    pub reason_code: Option<YearPresenceReason>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct YearPresenceReply {
    pub snapshot_id: String,
    pub snapshot_time: DateTime<Utc>,
    pub items: Vec<YearPresenceResult>,
    pub page_complete: bool,
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
    /// v8 PR-5 (#387 §7.2 row 11): both task reads default to v1.
    #[serde(default, skip_serializing_if = "ReadContractVersion::is_v1")]
    pub contract_version: ReadContractVersion,
    /// v2 only: keep tasks with at least one failed item of this class
    /// (`REVISION_UNVERIFIED` / `PUBLISH_PRECONDITION` / `NO_BARS_IN_RANGE` /
    /// `OTHER`). A v1 request carrying it is rejected, never ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskOperation {
    StartCollection,
    /// `update_data` also covers tasks admitted through `run_market_update`
    /// (the plan-run contract writes this operation).
    UpdateData,
    BackfillData,
    RepublishData,
    RetryTask,
    VerifyStoppedWrites,
    VerifyPublicationResult,
    VerifyDataGap,
    InspectVerificationTask,
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
    Interrupted,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressQuery {
    pub task_id: String,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
    /// v8 PR-5 (#387 §7.2 row 11): both task reads default to v1.
    #[serde(default, skip_serializing_if = "ReadContractVersion::is_v1")]
    pub contract_version: ReadContractVersion,
    /// Optional server-side wait (long-poll), in seconds. Absent or 0
    /// returns immediately (the pre-existing behavior). A positive value
    /// lets the server hold the call until the task reaches a terminal
    /// status or its `state_revision` moves past `since_revision`, capped
    /// at 50 seconds (larger values are treated as 50, never rejected).
    /// Applies to fresh calls; cursor pages always return immediately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_seconds: Option<u32>,
    /// Optional: the `task.state_revision` the caller last observed.
    /// Waiting ends early once the task advances past it. When absent, the
    /// revision read at wait entry is used as the baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_revision: Option<u64>,
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
    /// Set on tasks admitted through `run_market_update`: the verified
    /// plan digest this task's frozen work was rebuilt from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    /// Companion to `plan_id`: the plan's per-class counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_counts: Option<PlanCounts>,
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
    /// Stable machine-readable class of `error` (bounded large-task pages
    /// only; see `observe::reason_code`). Absent on small tasks, which keep
    /// their original shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
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
/// Bounded progress summary of the plan that produced a large task
/// (`contract_version == market-plan-run-task-v1`): the plan-level facts
/// a poller needs when per-item arrays are withheld by the bounded-read
/// path — `task.plan_id`/`plan_counts` carry the digest and counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanProgress {
    pub plan_id: String,
    pub generation_id: Option<String>,
    pub target_day: Option<chrono::NaiveDate>,
    /// Failed items by `reason_code`, counted when the first page of the
    /// progress snapshot is built. Lets a caller route a failure class
    /// (for example `REVISION_UNVERIFIED` to an explicit `update_data`
    /// forward window) without paging every failed item.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub failed_by_reason: std::collections::BTreeMap<String, usize>,
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
    /// Set when the task exceeded the bounded-read threshold
    /// (`observe::LARGE_TASK_ITEMS`): per-item arrays below are empty by
    /// design and this object carries the plan summary instead. Absent
    /// on smaller tasks, which keep the full per-item shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanProgress>,
    pub publication_versions: Vec<VersionRef>,
    pub frozen_daily_ranges: Vec<FrozenDailyRange>,
    pub range_results: Vec<RangeExecutionResult>,
    pub frozen_yearly_inputs: Vec<FrozenYearlyInput>,
    pub yearly_results: Vec<YearlyExecutionResult>,
    pub stopped_write_check: Option<crate::task_verification::decision::Receipt>,
    pub publication_check: Option<crate::publication_verification::decision::Receipt>,
}

// ---------------------------------------------------------------------------
// Task surface v2 (v8 PR-5, #387 §7.2 rows 9/14, §14 PR-5)
// ---------------------------------------------------------------------------

/// Structured task-item identity. A security item is the three components the
/// stored `item_id` string already carries (`["market","sh.600000","day"]`);
/// a server-internal derived item keeps its whole id in `dataset`
/// (`derived:calendar`, `derived:index:sh.000001`) because the two derived
/// items must stay distinguishable and neither has an entity or a period.
/// `task_v2::item_id`/`ItemId::text` round-trip to the string `retry_task`
/// still accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemId {
    pub dataset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<String>,
}
impl ItemId {
    /// The stored `item_id` text (`retry_task`'s accepted spelling).
    pub fn text(&self) -> String {
        match (&self.entity, &self.period) {
            (Some(entity), Some(period)) => {
                serde_json::to_string(&[self.dataset.as_str(), entity.as_str(), period.as_str()])
                    .expect("three strings always serialize")
            }
            _ => self.dataset.clone(),
        }
    }
}

/// One failed item in the bounded diagnosis sample: the class replaces the
/// full error text (which stays in the task record) beyond
/// `task_v2::BOUNDED_ERROR_CHARS`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemError {
    pub item_id: ItemId,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// v2 `get_task` counters: the same six numbers `get_task_progress` already
/// reports, so a diagnosis does not need a second call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskCounts {
    pub total: usize,
    pub pending: usize,
    pub running: usize,
    pub finished: usize,
    pub succeeded: usize,
    pub failed: usize,
}

/// v2 progress item: identity, stage and outcome. `entity`/`period` live in
/// `item_id` only, and a failed item always carries its class plus a bounded
/// error (v1 only did that for bounded large-task pages).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressItemV2 {
    pub item_id: ItemId,
    pub progress_unit: String,
    pub stage: String,
    pub success: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}
/// v2 frozen-range receipt: `item_id` carries the entity (no second copy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrozenDailyRangeV2 {
    pub item_id: ItemId,
    pub base_version_id: String,
    pub base_integrity_sha256: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
/// v2 range receipt: `item_id` carries the entity (no second copy).
#[serde(deny_unknown_fields)]
pub struct RangeExecutionResultV2 {
    pub result_id: String,
    pub item_id: ItemId,
    pub base_version: VersionRef,
    pub result_version: VersionRef,
    pub no_change: bool,
    pub acquired_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
/// v2 frozen yearly input: `item_id` carries the entity (no second copy).
#[serde(deny_unknown_fields)]
pub struct FrozenYearlyInputV2 {
    pub item_id: ItemId,
    pub base_daily_version_id: String,
    pub base_integrity_sha256: String,
    pub years: Vec<i32>,
    pub publication: YearlyPublication,
    pub comparison: Option<ComparisonVersion>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
/// v2 yearly receipt: `item_id` carries the entity (no second copy).
#[serde(deny_unknown_fields)]
pub struct YearlyExecutionResultV2 {
    pub result_id: String,
    pub item_id: ItemId,
    pub base_daily_version: VersionRef,
    pub result_version: VersionRef,
    pub no_change: bool,
    pub derived_at: DateTime<Utc>,
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
    /// In-flight tasks plus the newest finished ones (generic-data-v5);
    /// see `related_tasks_total` / `related_tasks_truncated`.
    pub related_tasks: Vec<TaskSummary>,
    pub related_tasks_total: usize,
    pub related_tasks_truncated: bool,
    /// One aggregate row replacing the per-entity rows for
    /// `selection.kind=universe` (v8 #387 §4.1/§8.2 T3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universe: Option<UniverseSummary>,
    /// Yearly rows trailing the live daily base: `"behind"` + the daily
    /// base day the row is correct as of (RULINGS A1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_day: Option<NaiveDate>,
    /// Classified reason for an absent yearly row (daily base superseded
    /// or value semantics moved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<AbsentReason>,
    pub limitations: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UniverseSummary {
    pub entities: usize,
    pub present: usize,
    pub absent: usize,
    pub stale: usize,
    pub unavailable: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityPage {
    pub snapshot_id: String,
    pub snapshot_time: DateTime<Utc>,
    pub items: Vec<AvailabilityItem>,
    pub next_cursor: Option<String>,
    pub page_complete: bool,
    /// M2 point-in-time state of this availability read (issue-391 §12.2);
    /// absent for datasets that declare neither parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalBlock>,
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
    /// Snapshot-resolved selection, implemented only for market.daily
    /// update/backfill acceptance. Carries no caller-provided base, range
    /// or gap overrides.
    Default {},
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
/// Machine-checkable readiness fact; no workflow hints or next actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Readiness {
    Ready,
    Blocked,
}
/// Structured blocker: stable code plus human-readable detail and an
/// optional evidence reference. Never a free-form instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlockingReason {
    pub code: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
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
    pub runtime_readiness: Readiness,
    pub blocking_reasons: Vec<BlockingReason>,
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
pub struct TaskQuery {
    /// `v2` adds the diagnosis projection (v8 PR-5, #387 §7.2 row 9/14).
    pub contract_version: ReadContractVersion,
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
    pub terminal_result: Option<RequestTerminal>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelV1 {
    pub contract_version: ContractVersion,
    pub task_id: String,
    /// Optional CAS token (v8 PR-1, #387 §7.2 row 10): absent means the
    /// current revision is read first; an explicit value keeps the strict
    /// precondition (terminal tasks enforce it, live tasks apply the intent
    /// at the revision the write reads).
    #[serde(default)]
    pub expected_revision: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cancellation {
    pub task_id: String,
    pub status: TaskStatus,
    pub state_revision: u64,
}

/// `plan_market_update` input (generic-data-v7): a dataset-tagged request
/// for a compact, deterministic whole-catalog update plan. `end_date`
/// defaults to the Asia/Shanghai acceptance date and may only be earlier —
/// a future target is rejected before any work is described.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "dataset", deny_unknown_fields)]
pub enum PlanMarketUpdate {
    #[serde(rename = "market.daily")]
    Daily {
        contract_version: ContractVersion,
        end_date: Option<NaiveDate>,
    },
    #[serde(rename = "market.yearly")]
    Yearly {
        contract_version: ContractVersion,
        end_date: Option<NaiveDate>,
    },
}
impl PlanMarketUpdate {
    pub fn dataset(&self) -> Dataset {
        match self {
            Self::Daily { .. } => Dataset::Daily,
            Self::Yearly { .. } => Dataset::Yearly,
        }
    }
    pub fn end_date(&self) -> Option<NaiveDate> {
        match self {
            Self::Daily { end_date, .. } | Self::Yearly { end_date, .. } => *end_date,
        }
    }
}
/// Per-class plan outcome counts; `eligible` is the only executable class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanCounts {
    pub eligible: u64,
    pub already_current: u64,
    pub absent: u64,
    pub ineligible: u64,
}
/// Count of non-eligible entities grouped by a fixed reason string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanReasonCount {
    pub reason: String,
    pub count: u64,
}
/// One non-eligible entity named in a plan view, with its fixed reason
/// string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanIneligibleEntity {
    pub entity: String,
    pub reason: String,
}
/// `plan_market_update` output: a compact digest-bound summary. `plan_id`
/// is the sha256 of the deterministic canonical plan — the same state and
/// the same `end_date` always produce the same id, and any catalog, head,
/// gap or generation change produces a different one. The plan carries no
/// eligible entity list; it names only the ineligible entities that need
/// caller routing, bounded by `PLAN_INELIGIBLE_ENTITY_LIMIT`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanView {
    pub plan_id: String,
    pub dataset: Dataset,
    pub contract_version: ContractVersion,
    pub generation_ref: Option<String>,
    pub target_day: NaiveDate,
    /// How `target_day` was chosen: `explicit` (the caller passed
    /// `end_date`), `generation_latest` (the active generation's latest
    /// trade day, never later than today) or `today_fallback` (no readable
    /// generation sample). Informational: it is not part of the canonical
    /// plan, so it never affects `plan_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_day_source: Option<String>,
    pub counts: PlanCounts,
    pub ineligible_reasons: Vec<PlanReasonCount>,
    /// Ineligible entities that need caller routing, sorted by (entity,
    /// reason) and capped at `PLAN_INELIGIBLE_ENTITY_LIMIT`. The same
    /// classes as `ineligible_reasons`: `already_current`/`absent`/
    /// `no_daily_base` are excluded. Diagnostic only: never part of the
    /// canonical plan, so it does not affect `plan_id`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ineligible_entities: Vec<PlanIneligibleEntity>,
    /// `true` when more ineligible entities exist than the limit admitted:
    /// `ineligible_entities` holds the first `PLAN_INELIGIBLE_ENTITY_LIMIT`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ineligible_entities_truncated: bool,
    pub snapshot_time: DateTime<Utc>,
}
/// `run_market_update` input: binds the request to a plan digest the
/// server re-derives under one read snapshot — a digest mismatch is a
/// `plan_stale` refusal, never a partial execution.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "dataset", deny_unknown_fields)]
pub enum RunMarketUpdate {
    #[serde(rename = "market.daily")]
    Daily {
        contract_version: ContractVersion,
        request_id: String,
        target_day: NaiveDate,
        plan_id: String,
    },
    #[serde(rename = "market.yearly")]
    Yearly {
        contract_version: ContractVersion,
        request_id: String,
        target_day: NaiveDate,
        plan_id: String,
    },
}
impl RunMarketUpdate {
    pub fn dataset(&self) -> Dataset {
        match self {
            Self::Daily { .. } => Dataset::Daily,
            Self::Yearly { .. } => Dataset::Yearly,
        }
    }
    pub fn request_id(&self) -> &str {
        match self {
            Self::Daily { request_id, .. } | Self::Yearly { request_id, .. } => request_id,
        }
    }
    pub fn target_day(&self) -> NaiveDate {
        match self {
            Self::Daily { target_day, .. } | Self::Yearly { target_day, .. } => *target_day,
        }
    }
    pub fn plan_id(&self) -> &str {
        match self {
            Self::Daily { plan_id, .. } | Self::Yearly { plan_id, .. } => plan_id,
        }
    }
}
/// `run_market_update` acceptance: the admitted task identity plus the
/// plan the request bound. The eligible set can cover the whole catalog,
/// so the view returns counts, not item ids — progress and per-item
/// outcomes are paged through `get_task_progress`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanRunAcceptance {
    pub request_id: String,
    pub task_id: String,
    pub dataset: String,
    pub contract_version: String,
    pub plan_id: String,
    pub reused: bool,
    pub status: TaskStatus,
    pub item_count: u64,
    pub plan_counts: PlanCounts,
    pub runtime_readiness: Readiness,
    pub blocking_reasons: Vec<BlockingReason>,
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
/// Closed `selection` object for range writes. A struct (not an
/// internally-tagged enum) because serde `deny_unknown_fields` does not
/// reject extra keys on unit enum variants — caller overrides like
/// `base_version_id`/`start_date`/`gap_ids` must fail decode, not be
/// silently dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RangeWriteSelection {
    pub kind: RangeSelectionKind,
}
/// Only the snapshot-resolved default selection is implemented; explicit
/// and catalog selections stay unavailable for update/backfill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RangeSelectionKind {
    Default,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "dataset", deny_unknown_fields)]
pub enum RangeWrite {
    #[serde(rename = "market.daily")]
    Daily {
        request_id: String,
        contract_version: ContractVersion,
        /// Explicit item list; mutually exclusive with `selection`
        #[schemars(length(min = 1))]
        items: Option<Vec<RangeWriteItem>>,
        /// Snapshot-resolved selection; only `{"kind":"default"}` is
        /// implemented for update/backfill
        selection: Option<RangeWriteSelection>,
    },
    #[serde(rename = "market.yearly")]
    Yearly {
        request_id: String,
        contract_version: ContractVersion,
        #[schemars(length(min = 1))]
        items: Vec<YearlyWriteItem>,
    },
}
/// Republish accepts only `market.daily`: the single-variant tagged enum
/// rejects every other dataset at schema/decode time, before any side
/// effect can reach task acceptance.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "dataset", deny_unknown_fields)]
pub enum RepublishWrite {
    #[serde(rename = "market.daily")]
    Daily {
        request_id: String,
        contract_version: ContractVersion,
        #[schemars(length(min = 1))]
        items: Vec<RangeWriteItem>,
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

/// Untagged wrapper preserves the existing stopped-result wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RequestTerminal {
    Stopped(crate::task_verification::existing::TerminalResult),
    Publication(crate::publication_verification::existing::TerminalResult),
    #[cfg(target_os = "linux")]
    Inspection(crate::verification_inspection::TerminalResult),
}

/// No caller-supplied proof, force flag, actor or clock.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InspectVerification {
    pub contract_version: ContractVersion,
    pub request_id: String,
    pub target_task_id: String,
}

mod normalize;
pub use normalize::normalize;
