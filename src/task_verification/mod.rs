//! Explicit verification use cases; accepting an inspection never stops a writer.
mod admission;
pub use admission::{StopVerification, verification_scope};
pub const DATASET: &str = "task.verification";
pub const CONTRACT_VERSION: &str = "stopped-writes-task-v1";
