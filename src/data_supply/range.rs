//! Public explicit-range DTO mapping; decisions remain in RangeContract.
use super::wire::RangeWrite;
use crate::market_update::tasks::{EntityRange, ExplicitRanges};
impl RangeWrite {
    pub fn payload(&self) -> serde_json::Value {
        serde_json::to_value(ExplicitRanges {
            items: self
                .items
                .iter()
                .map(|item| EntityRange {
                    symbol: item.entity.symbol(),
                    base_version_id: item.base_version_id.clone(),
                    start_date: item.start_date,
                    end_date: item.end_date,
                })
                .collect(),
        })
        .unwrap()
    }
}
