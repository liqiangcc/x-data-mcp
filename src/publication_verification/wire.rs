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
pub const DESCRIPTION: &str = "Re-check publication of one task's items without trusting the publisher. Read-only; local owner only. Replaying request_id returns the stored receipt.";
