//! Protocol mapping only; selection, intent and eligibility live in task/application contracts.
use super::wire::{RangeWrite, YearlyPublication};
use crate::{
    market_storage::PublishIntent,
    market_update::tasks::{EntityRange, ExplicitRanges, Operation},
    yearly_publication::tasks::{ExplicitYears, YearlyItem},
};
impl YearlyPublication {
    pub(crate) fn intent(&self, base: &str) -> PublishIntent {
        match self {
            Self::HistoricalFixedBase { .. } => PublishIntent::HistoricalFixedBase,
            Self::CurrentHead {
                expected_yearly_head,
            } => PublishIntent::CurrentHead {
                expected_daily_head: base.into(),
                expected_yearly_head: expected_yearly_head.clone(),
            },
        }
    }
}
impl RangeWrite {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Daily { request_id, .. } | Self::Yearly { request_id, .. } => request_id,
        }
    }
    pub fn key(&self, operation: Operation) -> crate::task_control::CommandKey {
        let mut key = operation.key();
        if matches!(self, Self::Yearly { .. }) {
            key.dataset = "market.yearly".into();
            key.contract_version = crate::yearly_publication::tasks::CONTRACT_VERSION.into();
        }
        key
    }
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Self::Daily { items, .. } => serde_json::to_value(ExplicitRanges {
                items: items
                    .iter()
                    .map(|i| EntityRange {
                        symbol: i.entity.symbol(),
                        base_version_id: i.base_version_id.clone(),
                        start_date: i.start_date,
                        end_date: i.end_date,
                    })
                    .collect(),
            })
            .unwrap(),
            Self::Yearly { items, .. } => serde_json::to_value(ExplicitYears {
                items: items
                    .iter()
                    .map(|i| YearlyItem {
                        symbol: i.entity.symbol(),
                        base_daily_version_id: i.base_daily_version_id.clone(),
                        years: i.years.iter().copied().collect(),
                        intent: i.publication.intent(&i.base_daily_version_id),
                        comparison_version_id: match &i.publication {
                            YearlyPublication::HistoricalFixedBase {
                                comparison_version_id,
                            } => comparison_version_id.clone(),
                            _ => None,
                        },
                    })
                    .collect(),
            })
            .unwrap(),
        }
    }
}
