//! Narrow read tools (D-75, issue-525 PR-1). Each tool takes a small typed
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
}

pub const FIND_SECURITIES_DESCRIPTION: &str = "Find current SH/SZ A-share securities by name, code or industry. Returns code, name and industry. ETFs and delisted codes are not listed.";
pub const DAILY_BARS_DESCRIPTION: &str = "Daily bars for up to 100 securities. Stocks default to qfq; ETFs need adjustment=raw. Indices: use get_index_bars.";
pub const YEARLY_BARS_DESCRIPTION: &str = "Calendar-year bars derived from qfq daily data, A-shares only. ETFs and indices have none. The current year is marked year_unfinished.";
pub const INDEX_BARS_DESCRIPTION: &str = "Daily bars for the 9 registered index benchmarks.";
pub const MARKET_SNAPSHOT_DESCRIPTION: &str = "Whole-market daily bars for at most 5 trading days. For a longer window, use get_daily_bars. ETF universes need adjustment=raw.";
pub const TRADING_CALENDAR_DESCRIPTION: &str = "Trading days in a date range.";
pub const PRICE_LIMIT_RULES_DESCRIPTION: &str =
    "Price limit rules in effect on as_of. as_of defaults to the latest trading day.";

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

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindSecuritiesInput {
    #[schemars(extend("description" = "name substring"))]
    pub query: Option<String>,
    pub code: Option<String>,
    pub industry_code: Option<String>,
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

fn decode<T: DeserializeOwned>(input: Value) -> Result<T, ApiError> {
    serde_json::from_value(input).map_err(|e| ApiError::new("invalid_arguments", e.to_string()))
}

fn invalid(message: String) -> ApiError {
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
fn reject_indices(entities: &[Security]) -> Result<(), ApiError> {
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
                    default_page(),
                    None,
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
        }
    }
}
