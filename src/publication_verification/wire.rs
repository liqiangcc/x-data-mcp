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
pub const DESCRIPTION: &str = "Explicit asynchronous inspection of durable publication evidence for an unresolved market task. In latest-only storage each committed item result is one market_events row: an event still naming the current series is fully verified (event row exists, new_content_digest equals the current series digest, evidence bundle replays); an event already overwritten by later updates is verified only for existence plus closure of the entity digest chain to the current series digest — the old bar set no longer exists to re-verify. Missing receipt is insufficient evidence, not proof of failure; conclusions are all_recorded / insufficient_evidence / not_applicable, bound to the target task revision. The separate check never publishes, changes heads or releases the original task claims. A prior all-recorded conclusion can bind a new request without a new inspection. Inspect publication_check via get_task_progress. Corrupt evidence and interrupted inspections retain protection; no caller approval or force flags. This ready interface does not make the official data source production-ready.";
