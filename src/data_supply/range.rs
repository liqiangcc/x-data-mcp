//! Protocol mapping only; selection, intent and eligibility live in task/application contracts.
use super::wire::{RangeWrite, YearlyPublication};
use crate::{
    market_storage::PublishIntent,
    market_update::tasks::{EntityRange, ExplicitRanges, Operation},
    task_control::Error,
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
        match self {
            Self::Yearly { .. } => {
                key.dataset = "market.yearly".into();
                key.contract_version = crate::yearly_publication::tasks::CONTRACT_VERSION.into();
            }
            Self::Daily {
                selection: Some(_), ..
            } => {
                key.contract_version = crate::market_update::tasks::DEFAULT_CONTRACT_VERSION.into();
            }
            _ => {}
        }
        key
    }
    pub fn payload(&self) -> Result<serde_json::Value, Error> {
        fn invalid(message: &str) -> Error {
            Error::InvalidCommand(message.into())
        }
        match self {
            Self::Daily {
                items, selection, ..
            } => match (items, selection) {
                (Some(items), None) => Ok(serde_json::to_value(ExplicitRanges {
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
                .unwrap()),
                (None, Some(_)) => Ok(serde_json::json!({"selection":{"kind":"default"}})),
                (Some(_), Some(_)) => Err(invalid("items and selection are mutually exclusive")),
                (None, None) => Err(invalid("items or selection required")),
            },
            Self::Yearly { items, .. } => Ok(serde_json::to_value(ExplicitYears {
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
            .unwrap()),
        }
    }
}
