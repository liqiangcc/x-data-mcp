//! Narrow read tools (D-75, issue-525 PR-1, PR-1b). Each tool takes a small typed
//! input and becomes one existing v2 [`Query`]; `queries.query_v2` then does
//! all validation, integrity checks and projection. Nothing here selects,
//! checks or projects data, so every tool answers byte-for-byte like the
//! equivalent `query_data` call.
use super::wire::*;
use crate::index_registry::{INDEX_SERIES, lookup};
use crate::mcp::ApiError;
use chrono::NaiveDate;
use schemars::{JsonSchema, json_schema};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

/// Output schema of the narrow tools: the v2 columnar envelope. Its layout
/// depends on the dataset (`panel`, `cross_section`, `table`), so the schema
/// only promises an object; the shape is documented by the v2 contract.
#[derive(Debug, JsonSchema)]
pub struct ColumnarPage {}

/// Output schema for derived tools (D-82): a table with tool-specific columns,
/// `inputs`, `algorithm_versions`, `temporal` and `limitations`.
#[derive(Debug, JsonSchema)]
pub struct TableReply {}

/// Which narrow tool a registry entry routes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadTool {
    FindSecurities,
    DailyBars,
    YearlyBars,
    IndexBars,
    MarketSnapshot,
    TradingCalendar,
    PriceLimitRules,
    SecurityState,
    SecurityHistory,
    SecurityEvents,
    Tradability,
    MarketCap,
}

pub const FIND_SECURITIES_DESCRIPTION: &str = "Find securities by name or code. Default kind=stock: current SH/SZ A-shares with industry. kind=etf: ETFs, current and delisted; names are observed from the first list snapshot and may be null.";
pub const DAILY_BARS_DESCRIPTION: &str = "Daily bars for up to 100 securities. Stocks default to qfq; ETFs need adjustment=raw. Indices: use get_index_bars.";
pub const YEARLY_BARS_DESCRIPTION: &str = "Calendar-year bars derived from qfq daily data, A-shares only. ETFs and indices have none. The current year is marked year_unfinished.";
pub const INDEX_BARS_DESCRIPTION: &str = "Daily bars for the 9 registered index benchmarks.";
pub const MARKET_SNAPSHOT_DESCRIPTION: &str = "Whole-market daily bars for at most 5 trading days. For a longer window, use get_daily_bars. ETF universes need adjustment=raw.";
pub const TRADING_CALENDAR_DESCRIPTION: &str = "Trading days in a date range.";
pub const PRICE_LIMIT_RULES_DESCRIPTION: &str =
    "Price limit rules in effect on as_of. as_of defaults to the latest trading day.";

pub const SECURITY_STATE_DESCRIPTION: &str = "State of securities on one day (as_of, default latest trading day): one fact per call. known_at hides facts learned later. SH name history is empty (SZ only).";
pub const SECURITY_HISTORY_DESCRIPTION: &str = "All intervals of one fact that overlap start_date..end_date (names, risk warnings, suspensions, industry). known_at hides facts learned later. SH name history is empty (SZ only).";
pub const SECURITY_EVENTS_DESCRIPTION: &str = "Capital changes (capital) or ex-rights/dividend events (adjustment) of securities in a date range.";
pub const TRADABILITY_DESCRIPTION: &str = "Whether each A-share could trade on as_of, plus its price limits that day. state is tradable, not_tradable or unknown; unknown is never tradable. as_of defaults to the latest trading day.";
pub const MARKET_CAP_DESCRIPTION: &str = "Market cap of securities on as_of (default latest trading day). scope is required: float_a, company_total or a_share_total. company_total is A+B+H; a_share_total is always unknown.";

/// Tool names this module serves; the decode layer exempts them from the
/// `query_data`-only top-level `as_of` rule (`get_price_limit_rules` has one).
pub const NAMES: &[&str] = &[
    "find_securities",
    "get_daily_bars",
    "get_yearly_bars",
    "get_index_bars",
    "get_market_snapshot",
    "get_trading_calendar",
    "get_price_limit_rules",
    "get_security_state",
    "get_security_history",
    "get_security_events",
    "get_tradability",
    "get_market_cap",
];

const BAR_FIELDS: &[&str] = &["open", "high", "low", "close", "volume", "amount"];

fn securities_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({
        "type": "array",
        "items": {"type": "string", "pattern": "^(sh|sz)\\.[0-9]{6}$"},
        "minItems": 1,
        "maxItems": 100,
        "description": "e.g. sh.600000"
    })
}

fn indices_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let members: Vec<&str> = INDEX_SERIES.iter().map(|series| series.entity).collect();
    json_schema!({"type": "array", "items": {"enum": members}, "minItems": 1})
}

fn fields_of(names: &[&str]) -> schemars::Schema {
    json_schema!({"type": "array", "items": {"enum": names}})
}

fn daily_fields(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut names = BAR_FIELDS.to_vec();
    names.push("no_trade");
    fields_of(&names)
}

fn yearly_fields(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut names = BAR_FIELDS.to_vec();
    names.extend(["first_trading_date", "last_trading_date", "year_unfinished"]);
    fields_of(&names)
}

fn index_fields(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    fields_of(BAR_FIELDS)
}

fn adjustment_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({"enum": ["qfq", "raw"]})
}

fn universe_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({"enum": ["a_current", "a_listed_ever", "etf_current", "etf_listed_ever"], "default": "a_current"})
}

/// What `find_securities` looks for. `stock` (the default) is the A-share
/// catalog and answers exactly as it did before `kind` existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityKind {
    Stock,
    Etf,
}

fn security_kind_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({"enum": ["stock", "etf"], "default": "stock"})
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindSecuritiesInput {
    #[schemars(extend("description" = "name substring"))]
    pub query: Option<String>,
    pub code: Option<String>,
    pub industry_code: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "security_kind_schema")]
    pub kind: Option<SecurityKind>,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DailyBarsInput {
    #[schemars(schema_with = "securities_schema")]
    pub securities: Vec<Security>,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    #[serde(default)]
    #[schemars(schema_with = "adjustment_schema")]
    pub adjustment: Option<AdjustmentChoice>,
    #[serde(default)]
    #[schemars(schema_with = "daily_fields")]
    pub fields: Option<Vec<String>>,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct YearlyBarsInput {
    #[schemars(schema_with = "securities_schema")]
    pub securities: Vec<Security>,
    pub start_year: i32,
    pub end_year: i32,
    #[serde(default)]
    #[schemars(schema_with = "yearly_fields")]
    pub fields: Option<Vec<String>>,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IndexBarsInput {
    #[schemars(schema_with = "indices_schema")]
    pub indices: Vec<String>,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    #[serde(default)]
    #[schemars(schema_with = "index_fields")]
    pub fields: Option<Vec<String>>,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarketSnapshotInput {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    #[serde(default)]
    #[schemars(schema_with = "universe_schema")]
    pub universe: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "adjustment_schema")]
    pub adjustment: Option<AdjustmentChoice>,
    #[serde(default)]
    #[schemars(schema_with = "daily_fields")]
    pub fields: Option<Vec<String>>,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TradingCalendarInput {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}

/// The fact `get_security_state` reads; every value takes the same arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateFact {
    SecurityMaster,
    SecurityNames,
    RiskWarnings,
    Suspensions,
    Industry,
}

impl StateFact {
    fn dataset(self) -> Dataset {
        match self {
            Self::SecurityMaster => Dataset::SecurityMaster,
            Self::SecurityNames => Dataset::SecurityNames,
            Self::RiskWarnings => Dataset::RiskWarnings,
            Self::Suspensions => Dataset::Suspensions,
            Self::Industry => Dataset::IndustryAssignments,
        }
    }
}

fn fact_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({"enum": ["security_master", "security_names", "risk_warnings", "suspensions", "industry"]})
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecurityStateInput {
    #[schemars(schema_with = "securities_schema")]
    pub securities: Vec<Security>,
    #[schemars(schema_with = "fact_schema")]
    pub fact: StateFact,
    pub as_of: Option<NaiveDate>,
    pub known_at: Option<NaiveDate>,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}

/// The fact `get_security_history` reads: the interval-valued subset of
/// [`StateFact`]. `security_master` is left out; its rows are one lifecycle
/// record per security, not a history of states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryFact {
    SecurityNames,
    RiskWarnings,
    Suspensions,
    Industry,
}

impl HistoryFact {
    fn dataset(self) -> Dataset {
        match self {
            Self::SecurityNames => Dataset::SecurityNames,
            Self::RiskWarnings => Dataset::RiskWarnings,
            Self::Suspensions => Dataset::Suspensions,
            Self::Industry => Dataset::IndustryAssignments,
        }
    }
}

fn history_fact_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({"enum": ["security_names", "risk_warnings", "suspensions", "industry"]})
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecurityHistoryInput {
    #[schemars(schema_with = "securities_schema")]
    pub securities: Vec<Security>,
    #[schemars(schema_with = "history_fact_schema")]
    pub fact: HistoryFact,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub known_at: Option<NaiveDate>,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}

/// The event kind `get_security_events` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Capital,
    Adjustment,
}

fn event_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({"enum": ["capital", "adjustment"]})
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecurityEventsInput {
    #[schemars(schema_with = "securities_schema")]
    pub securities: Vec<Security>,
    #[schemars(schema_with = "event_schema")]
    pub event: EventKind,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceLimitRulesInput {
    pub as_of: Option<NaiveDate>,
    #[serde(default = "default_page")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TradabilityInput {
    #[schemars(schema_with = "securities_schema")]
    pub securities: Vec<Security>,
    pub as_of: Option<NaiveDate>,
}

/// The scope `get_market_cap` calculates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketCapScope {
    FloatA,
    CompanyTotal,
    AShareTotal,
}

fn scope_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({"enum": ["float_a", "company_total", "a_share_total"]})
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarketCapInput {
    #[schemars(schema_with = "securities_schema")]
    pub securities: Vec<Security>,
    #[schemars(schema_with = "scope_schema")]
    pub scope: MarketCapScope,
    pub as_of: Option<NaiveDate>,
}

/// The batch shape the derived tools accept, mirroring the v2 path's
/// `1..=100`, six-digit, no-duplicate rules: those tools do not go through
/// `QueryPlan`, so nothing else would enforce them.
pub fn validate_securities(entities: &[Security]) -> Result<(), ApiError> {
    if entities.is_empty() || entities.len() > 100 {
        return Err(ApiError::new(
            "invalid_arguments",
            format!(
                "securities must contain 1..100 entities, got {}",
                entities.len()
            ),
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for entity in entities {
        entity.validate().map_err(|message| {
            ApiError::new(
                "invalid_arguments",
                format!("invalid security {}: {message}", entity.symbol()),
            )
        })?;
        if !seen.insert(entity.clone()) {
            return Err(ApiError::new(
                "invalid_arguments",
                format!("duplicate entity {}", entity.symbol()),
            ));
        }
    }
    Ok(())
}

/// `get_tradability`/`get_market_cap` do not take `known_at`: none of their
/// inputs carries a usable A-share knowledge time (design §4), so the parameter
/// is refused with the reason and a pointer to the tool that does support it,
/// rather than being ignored or rejected by `deny_unknown_fields`.
pub fn reject_known_at(input: &Value) -> Result<(), ApiError> {
    match input.get("known_at") {
        None | Some(Value::Null) => Ok(()),
        Some(_) => Err(ApiError::new(
            "invalid_arguments",
            "known_at is not accepted: none of this tool's inputs carries a usable \
             A-share knowledge time, so a known_at filter would exclude every row \
             instead of bounding the information set. Use get_security_state for \
             facts that have known_at (risk warnings, suspensions, industry).",
        )),
    }
}

pub fn decode<T: DeserializeOwned>(input: Value) -> Result<T, ApiError> {
    serde_json::from_value(input).map_err(|e| ApiError::new("invalid_arguments", e.to_string()))
}

pub fn invalid(message: String) -> ApiError {
    ApiError::new("invalid_arguments", message)
}

fn selection(mut entities: Vec<Security>) -> Selection {
    if entities.len() == 1 {
        Selection::Single {
            entity: entities.remove(0),
        }
    } else {
        Selection::Batch { entities }
    }
}

/// The v2 query every tool shares: latest head per entity, no `known_at`.
fn query(
    dataset: Dataset,
    selection: Selection,
    filter: Filter,
    fields: Option<Vec<String>>,
    page_size: usize,
    cursor: Option<String>,
) -> Query {
    Query {
        dataset,
        contract_version: ReadContractVersion::V2,
        selection,
        filter,
        version: VersionChoice::LatestPerEntity {},
        adjustment: None,
        page_size,
        cursor,
        fields,
        known_at: None,
    }
}

/// An index code (or its alias) is stored under `market.index`; reading it as
/// a stock series would only return `absent`, so name the right tool instead.
pub fn reject_indices(entities: &[Security]) -> Result<(), ApiError> {
    let hits: Vec<String> = entities
        .iter()
        .map(Security::symbol)
        .filter(|symbol| {
            lookup(symbol).is_some()
                || INDEX_SERIES
                    .iter()
                    .any(|series| series.aliases.contains(&symbol.as_str()))
        })
        .collect();
    if hits.is_empty() {
        return Ok(());
    }
    Err(invalid(format!(
        "{} is a registered index, not a stock or ETF; use get_index_bars",
        hits.join(", ")
    )))
}

fn canonical_indices(indices: Vec<String>) -> Result<Vec<Security>, ApiError> {
    indices
        .iter()
        .map(|text| {
            if lookup(text).is_none() {
                let allowed: Vec<&str> = INDEX_SERIES.iter().map(|s| s.entity).collect();
                return Err(invalid(format!(
                    "{text:?} is not a registered index; indices must be one of {}",
                    allowed.join(", ")
                )));
            }
            Security::parse(text).map_err(invalid)
        })
        .collect()
}

fn scope_of(universe: Option<&str>) -> Result<Option<CatalogScope>, ApiError> {
    match universe {
        None | Some("a_current") => Ok(None),
        Some("a_listed_ever") => Ok(Some(CatalogScope::ShSzAListedEver)),
        Some("etf_current") => Ok(Some(CatalogScope::ShSzEtfCurrent)),
        Some("etf_listed_ever") => Ok(Some(CatalogScope::ShSzEtfListedEver)),
        Some(other) => Err(invalid(format!(
            "universe must be a_current, a_listed_ever, etf_current or etf_listed_ever, got {other:?}"
        ))),
    }
}

impl ReadTool {
    /// Turn one call's (already normalized) arguments into the v2 query.
    /// `last_trading_day` resolves `get_price_limit_rules`' default `as_of`.
    pub fn to_query(
        self,
        input: Value,
        last_trading_day: impl FnOnce() -> Result<NaiveDate, ApiError>,
    ) -> Result<Query, ApiError> {
        match self {
            Self::FindSecurities => {
                let a: FindSecuritiesInput = decode(input)?;
                if a.kind == Some(SecurityKind::Etf) {
                    // An ETF has no industry: refuse the filter instead of
                    // answering a different question than the one asked.
                    if a.industry_code.is_some() {
                        return Err(invalid(
                            "industry_code applies to kind=stock only; an ETF has no industry"
                                .into(),
                        ));
                    }
                    return Ok(query(
                        Dataset::SecurityNames,
                        Selection::Market {},
                        Filter::FindEtf {
                            code: a.code,
                            name_query: a.query,
                        },
                        None,
                        a.page_size,
                        a.cursor,
                    ));
                }
                Ok(query(
                    Dataset::Catalog,
                    Selection::Catalog {
                        scope: CatalogScope::CurrentShSzA,
                    },
                    Filter::Catalog {
                        code: a.code,
                        name_query: a.query,
                        industry_code: a.industry_code,
                    },
                    None,
                    a.page_size,
                    a.cursor,
                ))
            }
            Self::DailyBars => {
                let a: DailyBarsInput = decode(input)?;
                reject_indices(&a.securities)?;
                let mut q = query(
                    Dataset::Daily,
                    selection(a.securities),
                    Filter::Daily {
                        start_date: a.start_date,
                        end_date: a.end_date,
                        as_of: None,
                    },
                    a.fields,
                    a.page_size,
                    a.cursor,
                );
                q.adjustment = a.adjustment;
                Ok(q)
            }
            Self::YearlyBars => {
                let a: YearlyBarsInput = decode(input)?;
                Ok(query(
                    Dataset::Yearly,
                    selection(a.securities),
                    Filter::Yearly {
                        start_year: a.start_year,
                        end_year: a.end_year,
                    },
                    a.fields,
                    a.page_size,
                    a.cursor,
                ))
            }
            Self::IndexBars => {
                let a: IndexBarsInput = decode(input)?;
                Ok(query(
                    Dataset::Index,
                    selection(canonical_indices(a.indices)?),
                    Filter::Daily {
                        start_date: a.start_date,
                        end_date: a.end_date,
                        as_of: None,
                    },
                    a.fields,
                    a.page_size,
                    a.cursor,
                ))
            }
            Self::MarketSnapshot => {
                let a: MarketSnapshotInput = decode(input)?;
                let mut q = query(
                    Dataset::Daily,
                    Selection::Universe {
                        catalog_version: None,
                        scope: scope_of(a.universe.as_deref())?,
                    },
                    Filter::CrossSection {
                        start_date: a.start_date,
                        end_date: a.end_date,
                    },
                    a.fields,
                    a.page_size,
                    a.cursor,
                );
                q.adjustment = a.adjustment;
                Ok(q)
            }
            Self::TradingCalendar => {
                let a: TradingCalendarInput = decode(input)?;
                Ok(query(
                    Dataset::Calendar,
                    Selection::Market {},
                    Filter::Daily {
                        start_date: a.start_date,
                        end_date: a.end_date,
                        as_of: None,
                    },
                    None,
                    a.page_size,
                    a.cursor,
                ))
            }
            Self::PriceLimitRules => {
                let a: PriceLimitRulesInput = decode(input)?;
                let day = match a.as_of {
                    Some(day) => day,
                    None => last_trading_day()?,
                };
                Ok(query(
                    Dataset::PriceLimitRules,
                    Selection::Market {},
                    Filter::Daily {
                        start_date: day,
                        end_date: day,
                        as_of: Some(day),
                    },
                    None,
                    a.page_size,
                    a.cursor,
                ))
            }
            Self::SecurityState => {
                let a: SecurityStateInput = decode(input)?;
                // The tool's `as_of` is one day: the range collapses to it, so
                // the day is always written into both ends of the filter.
                let day = match a.as_of {
                    Some(day) => day,
                    None => last_trading_day()?,
                };
                let mut q = query(
                    a.fact.dataset(),
                    selection(a.securities),
                    Filter::Daily {
                        start_date: day,
                        end_date: day,
                        as_of: Some(day),
                    },
                    None,
                    a.page_size,
                    a.cursor,
                );
                q.known_at = a.known_at;
                Ok(q)
            }
            Self::SecurityHistory => {
                let a: SecurityHistoryInput = decode(input)?;
                let mut q = query(
                    a.fact.dataset(),
                    selection(a.securities),
                    Filter::History {
                        start_date: a.start_date,
                        end_date: a.end_date,
                    },
                    None,
                    a.page_size,
                    a.cursor,
                );
                q.known_at = a.known_at;
                Ok(q)
            }
            Self::SecurityEvents => {
                let a: SecurityEventsInput = decode(input)?;
                let dataset = match a.event {
                    EventKind::Capital => Dataset::CapitalEvents,
                    EventKind::Adjustment => Dataset::AdjustmentEvents,
                };
                Ok(query(
                    dataset,
                    selection(a.securities),
                    Filter::Daily {
                        start_date: a.start_date,
                        end_date: a.end_date,
                        as_of: None,
                    },
                    None,
                    a.page_size,
                    a.cursor,
                ))
            }
            Self::Tradability | Self::MarketCap => {
                // These tools bypass the Query path and are handled directly in
                // the dispatch layer (Handler::Derived).
                unreachable!("Tradability and MarketCap should use Handler::Derived")
            }
        }
    }
}

const SH_NAMES_LIMITATION: &str = "name history covers SZ only (D-61): 0 rows for an SH security means the history is not available, not that it has no name";

/// Limitations a tool adds to the answer of `query`. `get_security_state` and
/// `get_security_history` have one: an SH security asked for `security_names`.
pub fn extra_limitations(query: &Query) -> Vec<&'static str> {
    let sh = match &query.selection {
        Selection::Single { entity } => entity.exchange == Exchange::Sh,
        Selection::Batch { entities } => entities.iter().any(|e| e.exchange == Exchange::Sh),
        _ => false,
    };
    if query.dataset == Dataset::SecurityNames && sh {
        vec![SH_NAMES_LIMITATION]
    } else {
        Vec::new()
    }
}

/// Append `notes` to the answer's `limitations` list.
pub fn add_limitations(answer: &mut Value, notes: Vec<&'static str>) {
    if notes.is_empty() {
        return;
    }
    let Some(object) = answer.as_object_mut() else {
        return;
    };
    let list = object
        .entry("limitations")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(list) = list.as_array_mut() {
        list.extend(notes.into_iter().map(Value::from));
    }
}
