//! Closed protocol shapes; they carry target identity, never stopping evidence.
use crate::data_supply::wire::ContractVersion;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyStopped {
    pub contract_version: ContractVersion,
    pub request_id: String,
    pub target_task_id: String,
}
pub const DESCRIPTION: &str = "Explicit asynchronous stopping-evidence inspection of a task awaiting verification. Admission preserves original claims. The installed verifier reports STOP_EVIDENCE_NOT_VERIFIED; no caller assertion, PID absence or old run identity can unlock a task. Inspect the separate check conclusion via get_task_progress.";
pub const BLOCKER: &str = "STOP_EVIDENCE_NOT_VERIFIED";
