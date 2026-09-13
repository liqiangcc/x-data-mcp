//! Closed publication-inspection protocol; callers supply identity, not evidence.
use crate::data_supply::wire::ContractVersion;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyPublication {
    pub contract_version: ContractVersion,
    pub request_id: String,
    pub target_task_id: String,
}
pub const DESCRIPTION: &str = "Explicit asynchronous inspection of durable publication evidence for an unresolved market task. Reads fixed SQLite publication versions, receipts and projections; missing receipt is insufficient evidence, not proof of failure. The separate check never publishes, changes heads or releases the original task claims. A prior all-recorded conclusion can bind a new request without a new inspection. Inspect publication_check via get_task_progress. Corrupt evidence and interrupted inspections retain protection; no caller approval or force flags. This ready interface does not make the official data source production-ready.";
