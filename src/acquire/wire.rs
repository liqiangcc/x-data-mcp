//! Typed wire contract for x-data-acquire. Single-action tools only:
//! outputs are state/outcome/refs/reason codes — never next actions,
//! other tool names, or orchestration hints.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ContractVersion {
    #[serde(rename = "v1")]
    V1,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckOfficialUpdates {
    pub contract_version: ContractVersion,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcquireGeneration {
    pub contract_version: ContractVersion,
    pub request_id: String,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetAcquisitionTask {
    pub contract_version: ContractVersion,
    pub task_id: String,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetAcquisitionRequest {
    pub contract_version: ContractVersion,
    pub request_id: String,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyGeneration {
    pub contract_version: ContractVersion,
    pub request_id: String,
    pub generation_id: String,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivateGeneration {
    pub contract_version: ContractVersion,
    pub request_id: String,
    pub generation_id: String,
    /// Expected currently-active generation id; `null` means "none active"
    /// (first activation). Any mismatch fails closed (D5).
    pub expected_previous: Option<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncStatus {
    pub contract_version: ContractVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Changed,
    Unchanged,
    Unknown,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteFile {
    pub role: String,
    pub url: String,
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_length: Option<u64>,
    pub change: Change,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateCheck {
    pub checked_at: String,
    pub files: Vec<RemoteFile>,
    pub changed: Vec<String>,
    pub unknown: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionAcceptance {
    pub request_id: String,
    pub task_id: String,
    pub state: String,
    pub reused: bool,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionTaskView {
    pub task_id: String,
    pub request_id: String,
    pub kind: String,
    pub state: String,
    pub generation_id: Option<String>,
    pub stage: Option<String>,
    pub bytes_done: u64,
    pub error: Option<String>,
    pub detail: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionRequestView {
    pub request_id: String,
    pub bound: bool,
    pub task: Option<AcquisitionTaskView>,
    pub terminal_result: Option<serde_json::Value>,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationView {
    pub generation_id: String,
    pub state: String,
    pub manifest_sha256: String,
    pub consistency_status: Option<String>,
    pub consistency_reasons: Option<Vec<String>>,
    pub decision_sha256: Option<String>,
    pub acquisition_request_id: Option<String>,
    pub installed_at: String,
}
/// Terminal verify reply. `state` is the generation lifecycle state after
/// this call: `consistency_verified`, `not_verified`, `conflict`, or
/// `refused` (precondition failed before evaluation). Refusals are
/// durable outcomes replayed by request identity, not transient errors.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyReply {
    pub request_id: String,
    pub generation_id: String,
    pub state: String,
    pub consistency_status: Option<String>,
    pub consistency_reasons: Vec<String>,
    pub decision_sha256: Option<String>,
    pub reason_code: Option<String>,
    pub reused: bool,
}
/// Terminal activate reply. `state` is `active` on a committed pointer
/// swap, otherwise `refused` with a stable `reason_code`
/// (`generation_not_installed`, `generation_not_consistency_verified`,
/// `generation_bytes_unverified`, `activation_conflict`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivateReply {
    pub request_id: String,
    pub generation_id: String,
    pub state: String,
    pub reason_code: Option<String>,
    pub detail: Option<String>,
    pub manifest_sha256: Option<String>,
    pub activated_at: Option<String>,
    pub previous_generation_id: Option<String>,
    pub reused: bool,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusView {
    pub active: Option<crate::generation::ActivePointer>,
    pub generations: Vec<GenerationView>,
    pub inflight_tasks: Vec<AcquisitionTaskView>,
    pub generation_contract_version: String,
    pub consistency_contract_version: String,
}
