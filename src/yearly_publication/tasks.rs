//! Internal explicit yearly update/backfill contract. No public evidence flags.
use super::YearlyCandidate;
use crate::{
    market_data::Period,
    market_storage::{Error as MarketError, MarketRepository, Publication, PublishIntent},
    market_update::tasks::Operation,
    task_control::{CommandKey, Contract, Error, FrozenWork, ItemSpec, TaskSnapshot},
};
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const CONTRACT_VERSION: &str = "explicit-yearly-task-v1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct YearlyItem {
    pub symbol: String,
    pub base_daily_version_id: String,
    pub years: BTreeSet<i32>,
    pub intent: PublishIntent,
    pub comparison_version_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplicitYears {
    pub items: Vec<YearlyItem>,
}
pub struct YearlyContract<R> {
    pub operation: Operation,
    pub repository: R,
    /// Server Shanghai date, never an input protocol override.
    pub today: fn() -> NaiveDate,
}
fn invalid(message: impl ToString) -> Error {
    Error::InvalidCommand(message.to_string())
}
impl<R: MarketRepository> Contract for YearlyContract<R> {
    fn key(&self) -> CommandKey {
        let mut key = self.operation.key();
        key.dataset = "market.yearly".into();
        key.contract_version = CONTRACT_VERSION.into();
        key
    }
    fn normalize(&self, raw: Value) -> Result<Value, Error> {
        let mut input: ExplicitYears = serde_json::from_value(raw).map_err(invalid)?;
        for item in &input.items {
            let valid_symbol = item.symbol.split_once('.').is_some_and(|(exchange, code)| {
                matches!(exchange, "sh" | "sz")
                    && code.len() == 6
                    && code.bytes().all(|b| b.is_ascii_digit())
            });
            if item
                .comparison_version_id
                .as_ref()
                .is_some_and(|id| id.trim().is_empty())
            {
                return Err(invalid("empty comparison version"));
            }
            let valid_intent = match &item.intent {
                PublishIntent::HistoricalFixedBase => true,
                PublishIntent::CurrentHead {
                    expected_daily_head,
                    expected_yearly_head,
                } => {
                    expected_daily_head == &item.base_daily_version_id
                        && item
                            .comparison_version_id
                            .as_ref()
                            .is_none_or(|id| Some(id) == expected_yearly_head.as_ref())
                        && expected_yearly_head
                            .as_ref()
                            .is_none_or(|v| !v.trim().is_empty())
                }
                _ => false,
            };
            if !valid_symbol
                || item.base_daily_version_id.trim().is_empty()
                || item.years.is_empty()
                || item.years.iter().any(|y| !(1..=9999).contains(y))
                || !valid_intent
            {
                return Err(invalid("explicit yearly identity/years/intent"));
            }
        }
        input.items.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        input.items.dedup();
        if input.items.is_empty() || input.items.len() > 100 {
            return Err(invalid("explicit yearly budget 1..100"));
        }
        if input.items.windows(2).any(|w| w[0].symbol == w[1].symbol) {
            return Err(invalid("conflicting yearly bindings for same entity"));
        }
        serde_json::to_value(input).map_err(invalid)
    }
    fn resolve(&self, raw: &Value) -> Result<FrozenWork, Error> {
        let input: ExplicitYears = serde_json::from_value(raw.clone()).map_err(invalid)?;
        let future: Vec<_> = input
            .items
            .iter()
            .filter(|i| i.years.iter().any(|year| *year > (self.today)().year()))
            .map(|i| i.symbol.clone())
            .collect();
        if !future.is_empty() {
            return Err(invalid(format!("future yearly write rejected: {future:?}")));
        }
        let ids = input
            .items
            .iter()
            .map(|i| i.base_daily_version_id.clone())
            .collect::<Vec<_>>();
        let comparisons: Vec<_> = input
            .items
            .iter()
            .map(|i| match &i.intent {
                PublishIntent::CurrentHead {
                    expected_yearly_head,
                    ..
                } => expected_yearly_head.clone(),
                _ => i.comparison_version_id.clone(),
            })
            .collect();
        let mut all_ids = ids.clone();
        all_ids.extend(comparisons.iter().flatten().cloned());
        let all = self
            .repository
            .fixed_many(&all_ids)
            .map_err(repository_error)?;
        if all.len() != all_ids.len() {
            return Err(Error::NeedsVerification("incomplete version lookup".into()));
        }
        let mut prior_records = all.iter().skip(ids.len());
        let bases = all.iter().take(ids.len());
        if ids.len() != input.items.len() {
            return Err(Error::NeedsVerification(
                "incomplete fixed base lookup".into(),
            ));
        }
        let mut scopes = BTreeSet::new();
        let mut items = vec![];
        for ((item, base), comparison_id) in input.items.into_iter().zip(bases).zip(comparisons) {
            let comparison = comparison_id.map(|_| prior_records.next().unwrap());
            if comparison.is_some_and(|p| {
                p.version().symbol != item.symbol || p.version().period != Period::Year
            }) {
                return Err(invalid("comparison yearly identity/period mismatch"));
            }
            if base.version().symbol != item.symbol
                || base.version().period != Period::Day
                || base.version().version_id != item.base_daily_version_id
            {
                return Err(invalid("fixed daily identity/period mismatch"));
            }
            let available: BTreeSet<_> = base
                .version()
                .records
                .iter()
                .map(|r| r.date.year())
                .collect();
            if !item.years.is_subset(&available) {
                return Err(Error::NeedsVerification(
                    "selected year absent from fixed daily".into(),
                ));
            }
            let scope = crate::task_control::legacy::scope_id(&item.symbol, "year");
            scopes.insert(scope.clone());
            items.push(ItemSpec {
                item_id: scope.clone(),
                scope_keys: [scope].into(),
                target: json!({"symbol":item.symbol,"period":"year"}),
                progress_unit: "security_period".into(),
                frozen_inputs: {
                    let mut value = frozen(&item.base_daily_version_id,base.raw()["integrity_sha256"].as_str().unwrap(),&item.years,&item.intent);
                    value["comparison"] = comparison.map(|p|json!({"version_id":p.version().version_id,"integrity_sha256":p.raw()["integrity_sha256"]})).unwrap_or(Value::Null);
                    value
                },
            });
        }
        Ok(FrozenWork { items, scopes })
    }
}
pub(crate) fn frozen(
    base: &str,
    digest: &str,
    years: &BTreeSet<i32>,
    intent: &PublishIntent,
) -> Value {
    json!({"base_daily_version_id":base,"base_integrity_sha256":digest,"years":years,"intent":intent})
}
pub(super) fn validate_binding(
    task: &TaskSnapshot,
    command: &Publication,
    candidate: &YearlyCandidate,
) -> Result<(), MarketError> {
    if !matches!(
        task.task.key.operation.as_str(),
        "update_data" | "backfill_data"
    ) {
        return Ok(());
    }
    let years: BTreeSet<i32> = serde_json::from_value(
        candidate.raw["local_yearly_derivation_v1"]["evidence"]["years"].clone(),
    )
    .map_err(|e| MarketError::Integrity(e.to_string()))?;
    let mut expected = frozen(
        &candidate.base_daily_version_id,
        &candidate.base_integrity_sha256,
        &years,
        &command.intent,
    );
    expected["comparison"] = task
        .items
        .iter()
        .find(|i| i.spec.item_id == command.item_id)
        .map(|i| i.spec.frozen_inputs["comparison"].clone())
        .unwrap_or(Value::Null);
    if task.task.key.contract_version != CONTRACT_VERSION
        || task.task.key.dataset != "market.yearly"
        || !task
            .items
            .iter()
            .any(|i| i.spec.item_id == command.item_id && i.spec.frozen_inputs == expected)
    {
        return Err(MarketError::Conflict(
            "yearly publication differs from accepted frozen inputs".into(),
        ));
    }
    Ok(())
}
fn repository_error(error: MarketError) -> Error {
    match error {
        MarketError::Task(e) => e,
        MarketError::Storage(e) => Error::StorageUnavailable(e),
        MarketError::OutcomeUnknown(e) => Error::OutcomeUnknown(e),
        MarketError::Invalid(e) => Error::InvalidCommand(e),
        other => Error::NeedsVerification(other.to_string()),
    }
}
