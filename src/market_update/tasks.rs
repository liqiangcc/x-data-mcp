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
        let ids = input
            .items
            .iter()
            .map(|i| i.base_version_id.clone())
            .collect::<Vec<_>>();
        // One repository snapshot verifies every explicit base before admission.
        let bases = self.repository.fixed_many(&ids).map_err(invalid)?;
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
