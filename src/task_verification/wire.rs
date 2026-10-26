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
pub const DESCRIPTION: &str = "Verify that writes to a task stopped before takeover. Read-only; local owner only. Replaying request_id returns the stored receipt.";

#[derive(Serialize, JsonSchema)]
#[serde(untagged)]
pub enum VerificationReply {
    Accepted(crate::data_supply::wire::WriteAcceptance),
    Existing(Box<crate::data_supply::wire::RequestView>),
}
