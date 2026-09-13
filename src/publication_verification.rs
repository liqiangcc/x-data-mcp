//! Evidence facts, not permission to republish or release a task's claims.
use crate::{
    market_storage::{Error, Receipt},
    task_control::TaskSnapshot,
};
#[derive(Debug, Clone)]
pub struct Facts {
    pub task: TaskSnapshot,
    pub items: Vec<ItemFact>,
}
#[derive(Debug, Clone)]
pub struct ItemFact {
    pub item_id: String,
    pub evidence: Evidence,
}
#[derive(Debug, Clone)]
pub enum Evidence {
    /// No committed evidence was found in this snapshot. NOT "not published".
    Missing,
    Committed {
        receipt: Receipt,
        reused_version: bool,
    },
}
pub trait EvidenceRepository {
    fn publication_facts(&self, task_id: &str) -> Result<Facts, Error>;
}
