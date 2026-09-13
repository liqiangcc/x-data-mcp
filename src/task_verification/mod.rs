//! Explicit verification use cases; accepting an inspection never stops a writer.
mod admission;
pub use admission::{StopVerification, verification_scope};
pub const DATASET: &str = "task.verification";
pub const CONTRACT_VERSION: &str = "stopped-writes-task-v1";
pub mod decision;
pub mod runner;
pub mod wire;
pub fn command_key() -> crate::task_control::CommandKey {
    crate::task_control::CommandKey {
        operation: "verify_stopped_writes".into(),
        dataset: DATASET.into(),
        contract_version: CONTRACT_VERSION.into(),
    }
}
pub mod existing;
