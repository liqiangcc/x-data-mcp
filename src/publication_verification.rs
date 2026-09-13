//! Evidence facts, not permission to republish or release a task's claims.
use crate::{
    market_storage::{Error, Receipt},
    task_control::TaskSnapshot,
};
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Facts {
    pub task: TaskSnapshot,
    pub items: Vec<ItemFact>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemFact {
    pub item_id: String,
    pub evidence: Evidence,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Evidence {
    /// No committed evidence was found in this snapshot. NOT "not published".
    Missing,
    Committed {
        receipt: Receipt,
        reused_version: bool,
        evidence_sha256: String,
    },
}
pub trait EvidenceRepository {
    fn publication_facts(&self, task_id: &str) -> Result<Facts, Error>;
}

pub mod admission;
#[derive(Clone, Copy)]
pub(crate) enum FactContract {
    Legacy,
    Daily,
    Yearly,
}
pub(crate) fn contract_kind(key: &crate::task_control::CommandKey) -> Option<FactContract> {
    use crate::task_control::{Contract, legacy::LegacyCollection};
    if key == &LegacyCollection.key()
        || (key.operation == "start_collection"
            && key.contract_version == "v1"
            && matches!(key.dataset.as_str(), "market.daily" | "market.yearly"))
    {
        return Some(FactContract::Legacy);
    }
    if !matches!(key.operation.as_str(), "update_data" | "backfill_data") {
        return None;
    }
    if key.dataset == "market.daily"
        && key.contract_version == crate::market_update::tasks::CONTRACT_VERSION
    {
        Some(FactContract::Daily)
    } else if key.dataset == "market.yearly"
        && key.contract_version == crate::yearly_publication::tasks::CONTRACT_VERSION
    {
        Some(FactContract::Yearly)
    } else {
        None
    }
}
pub mod decision;
