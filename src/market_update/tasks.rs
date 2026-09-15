//! Explicit daily task input. Public MCP/default selection is a separate adapter.
use super::DateRange;
use crate::{
    market_data::Period,
    market_storage::MarketRepository,
    task_control::{CommandKey, Contract, Error, FrozenWork, ItemSpec},
};
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const CONTRACT_VERSION: &str = "explicit-range-task-v1";
/// Snapshot-resolved default daily contract. Parameter semantics differ
/// from explicit ranges, so it persists a distinct contract version and
/// never shares command identity with `CONTRACT_VERSION`.
pub const DEFAULT_CONTRACT_VERSION: &str = "default-range-task-v1";
#[derive(Debug, Clone, Copy)]
pub enum Operation {
    Update,
    Backfill,
}
impl Operation {
    pub fn key(self) -> CommandKey {
        CommandKey {
            operation: match self {
                Self::Update => "update_data",
                Self::Backfill => "backfill_data",
            }
            .into(),
            dataset: "market.daily".into(),
            contract_version: CONTRACT_VERSION.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityRange {
    pub symbol: String,
    pub base_version_id: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplicitRanges {
    pub items: Vec<EntityRange>,
}
fn invalid(message: impl ToString) -> Error {
    Error::InvalidCommand(message.to_string())
}
pub struct RangeContract<R> {
    pub operation: Operation,
    pub repository: R,
    /// Server clock, sampled once per new request resolution; not a wire field.
    pub today: fn() -> NaiveDate,
}
impl<R: MarketRepository> Contract for RangeContract<R> {
    fn key(&self) -> CommandKey {
        self.operation.key()
    }
    fn normalize(&self, raw: Value) -> Result<Value, Error> {
        let mut input: ExplicitRanges = serde_json::from_value(raw).map_err(invalid)?;
        for item in &input.items {
            let valid_symbol = item.symbol.split_once('.').is_some_and(|(exchange, code)| {
                matches!(exchange, "sh" | "sz")
                    && code.len() == 6
                    && code.bytes().all(|b| b.is_ascii_digit())
            });
            if !valid_symbol
                || item.base_version_id.trim().is_empty()
                || item.start_date > item.end_date
                || item.start_date.year() < 1
                || item.end_date.year() > 9999
            {
                return Err(invalid("invalid daily entity/base/date range"));
            }
        }
        input.items.sort();
        input.items.dedup();
        if input.items.is_empty() || input.items.len() > 100 {
            return Err(invalid("explicit range budget 1..100"));
        }
        if input.items.windows(2).any(|w| w[0].symbol == w[1].symbol) {
            return Err(invalid(
                "conflicting ranges or base versions for the same entity",
            ));
        }
        serde_json::to_value(input).map_err(invalid)
    }
    fn resolve(&self, raw: &Value) -> Result<FrozenWork, Error> {
        let input: ExplicitRanges = serde_json::from_value(raw.clone()).map_err(invalid)?;
        let today = (self.today)();
        let future: Vec<_> = input
            .items
            .iter()
            .filter(|i| i.end_date > today)
            .map(|i| json!({"symbol":i.symbol,"start_date":i.start_date,"end_date":i.end_date}))
            .collect();
        if !future.is_empty() {
            return Err(invalid(format!(
                "future daily write rejected on {today}: {}",
                json!(future)
            )));
        }
        let ids = input
            .items
            .iter()
            .map(|i| i.base_version_id.clone())
            .collect::<Vec<_>>();
        // One repository snapshot verifies every explicit base before admission.
        let bases = self.repository.fixed_many(&ids).map_err(repository_error)?;
        if bases.len() != input.items.len() {
            return Err(invalid("incomplete fixed base lookup"));
        }
        let mut items = vec![];
        let mut scopes = BTreeSet::new();
        for (item, base) in input.items.into_iter().zip(bases) {
            if base.version().symbol != item.symbol
                || base.version().period != Period::Day
                || base.version().version_id != item.base_version_id
            {
                return Err(invalid("fixed daily base identity mismatch"));
            }
            let scope = crate::task_control::legacy::scope_id(&item.symbol, "day");
            scopes.insert(scope.clone());
            items.push(ItemSpec {
                item_id: scope.clone(),
                scope_keys: BTreeSet::from([scope]),
                target: json!({"symbol":item.symbol,"period":"day"}),
                progress_unit: "security_period".into(),
                frozen_inputs: frozen(
                    &item.base_version_id,
                    base.raw()["integrity_sha256"].as_str().unwrap(),
                    &DateRange {
                        start: item.start_date,
                        end: item.end_date,
                    },
                ),
            });
        }
        Ok(FrozenWork { items, scopes })
    }
}
pub(crate) fn frozen(id: &str, digest: &str, range: &DateRange) -> Value {
    json!({"base_version_id":id,"base_integrity_sha256":digest,"range":range})
}
/// Runner-side reader of the `default-range-task-v1` frozen item shape
/// written by the default-range contract. Strict decode is the execution
/// integrity gate: unknown or missing fields fail closed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DefaultFrozen {
    /// Presence/shape is the integrity gate; the runner never reads the
    /// provenance content itself.
    #[allow(dead_code)]
    pub selection_evidence: DefaultFrozenEvidence,
    pub base_version_id: String,
    pub base_integrity_sha256: String,
    /// `update` only; the key is absent on `backfill` items.
    #[serde(default)]
    pub refresh_range: Option<FrozenDayRange>,
    pub proven_gap_ranges: Vec<FrozenGapRange>,
}
/// Selection/catalog/execution-start provenance, opaque to the runner but
/// required to be present and well-formed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct DefaultFrozenEvidence {
    pub catalog_version_id: String,
    pub catalog_digest: String,
    pub securities: Vec<String>,
    pub starts: std::collections::BTreeMap<String, Value>,
    pub evidence_digest: String,
}
/// One closed `[start_date, end_date]` in the frozen shape.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenDayRange {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
impl FrozenDayRange {
    pub(crate) fn range(&self) -> Result<DateRange, crate::market_storage::Error> {
        if self.start_date > self.end_date {
            return Err(crate::market_storage::Error::Integrity(
                "frozen range inverted".into(),
            ));
        }
        Ok(DateRange {
            start: self.start_date,
            end: self.end_date,
        })
    }
}
/// One frozen open gap with its evidence identity preserved verbatim.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenGapRange {
    pub gap_id: String,
    pub range: FrozenDayRange,
    /// Preserved verbatim; covering bindings reference the gap row itself.
    #[allow(dead_code)]
    pub evidence_digest: String,
}
impl DefaultFrozen {
    /// Required ranges: `update` is refresh plus every gap range;
    /// `backfill` is gap ranges only.
    pub(crate) fn required_ranges(&self) -> Result<Vec<DateRange>, crate::market_storage::Error> {
        let mut ranges = vec![];
        if let Some(refresh) = &self.refresh_range {
            ranges.push(refresh.range()?);
        }
        for gap in &self.proven_gap_ranges {
            ranges.push(gap.range.range()?);
        }
        Ok(ranges)
    }
    /// Single fetch range satisfying every required range (the fetch-union
    /// allowed by #147). Never loses per-gap provenance: the frozen gap
    /// list stays authoritative for covering write-back.
    pub(crate) fn union(&self) -> Result<DateRange, crate::market_storage::Error> {
        self.required_ranges()?
            .into_iter()
            .reduce(|a, b| DateRange {
                start: a.start.min(b.start),
                end: a.end.max(b.end),
            })
            .ok_or_else(|| {
                crate::market_storage::Error::Integrity("default item has no required range".into())
            })
    }
}
/// Strictly decode the default frozen shape; malformed input is a storage
/// integrity failure, never a silent skip.
pub(crate) fn parse_default_frozen(
    frozen: &Value,
) -> Result<DefaultFrozen, crate::market_storage::Error> {
    serde_json::from_value(frozen.clone())
        .map_err(|e| crate::market_storage::Error::Integrity(format!("default frozen inputs: {e}")))
}

pub(crate) fn repository_error(error: crate::market_storage::Error) -> Error {
    use crate::market_storage::Error as MarketError;
    match error {
        MarketError::Task(error) => error,
        MarketError::Storage(message) => Error::StorageUnavailable(message),
        MarketError::Integrity(message)
        | MarketError::Base(message)
        | MarketError::Conflict(message) => Error::NeedsVerification(message),
        MarketError::OutcomeUnknown(message) => Error::OutcomeUnknown(message),
        MarketError::BasisNotVerified(message) => {
            Error::NeedsVerification(format!("GAP_BASIS_NOT_VERIFIED: {message}"))
        }
        MarketError::Invalid(message) => Error::InvalidCommand(message),
        MarketError::NotFound => {
            Error::InvalidCommand("explicit published daily base unavailable".into())
        }
    }
}
