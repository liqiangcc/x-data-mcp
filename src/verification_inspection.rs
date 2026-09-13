//! Bounded management of verification executions; never a recursive task.
use crate::task_control::{Command, CommandKey, Contract, Error, FrozenWork};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub mod decision;
pub mod service;
pub fn command_key() -> CommandKey {
    CommandKey {
        operation: "inspect_verification_task".into(),
        dataset: crate::task_verification::DATASET.into(),
        contract_version: "v1".into(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    target_task_id: String,
}
/// Registers normalization for historical get_request. Resolving async work is forbidden.
pub struct InspectionContract;
impl Contract for InspectionContract {
    fn key(&self) -> CommandKey {
        command_key()
    }
    fn normalize(&self, value: Value) -> Result<Value, Error> {
        let target: Target =
            serde_json::from_value(value).map_err(|e| Error::InvalidCommand(e.to_string()))?;
        if target.target_task_id.trim().is_empty() {
            return Err(Error::InvalidCommand("empty target_task_id".into()));
        }
        Ok(json!({"target_task_id":target.target_task_id}))
    }
    fn resolve(&self, _: &Value) -> Result<FrozenWork, Error> {
        Err(Error::UnsupportedContract)
    }
}
pub fn command(target: &str) -> Result<Command, Error> {
    Ok(Command {
        key: command_key(),
        intent: InspectionContract.normalize(json!({"target_task_id":target}))?,
    })
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Conclusion {
    AlreadyFinished,
    Interrupted,
    InsufficientEvidence,
    NotApplicable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub request_id: String,
    pub target_task_id: String,
    pub conclusion: Conclusion,
    pub target_revision: u64,
    pub target_status: String,
    pub target_result: Option<Value>,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalResult {
    VerificationInspection { receipt: Receipt },
}
impl TerminalResult {
    pub fn receipt(&self) -> &Receipt {
        match self {
            Self::VerificationInspection { receipt } => receipt,
        }
    }
}
