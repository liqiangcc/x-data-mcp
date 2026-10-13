//! Returning a prior verified conclusion is a persisted request outcome, not a new inspection.
use super::{
    StopVerification, command_key,
    decision::{Conclusion, Receipt, StopRepository},
};
use crate::task_control::{
    Acceptance, Command, Contract, Error, Status, TaskRepository, TaskSnapshot,
    service::TaskControl,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalResult {
    AlreadyVerified { receipt: Receipt },
}
impl TerminalResult {
    pub fn receipt(&self) -> &Receipt {
        match self {
            Self::AlreadyVerified { receipt } => receipt,
        }
    }
}
pub struct ExistingPlan {
    pub(crate) request_id: String,
    pub(crate) command: Command,
    pub(crate) target: TaskSnapshot,
    pub(crate) result: TerminalResult,
}
pub trait ExistingStopRepository {
    fn bind_existing_stop(&self, plan: &ExistingPlan) -> Result<Acceptance, Error>;
}
fn existing<R: TaskRepository + StopRepository + ExistingStopRepository>(
    repo: &R,
    id: &str,
    target: TaskSnapshot,
) -> Result<Acceptance, Error> {
    let check = target
        .task
        .result
        .as_ref()
        .and_then(|r| r["interruption"]["verification_task_id"].as_str())
        .ok_or_else(|| Error::NeedsVerification("interruption evidence link missing".into()))?;
    let receipt = repo
        .stop_receipt(check)?
        .ok_or_else(|| Error::NeedsVerification("interruption receipt missing".into()))?;
    if target.task.status != Status::Interrupted
        || receipt.conclusion != Conclusion::Interrupted
        || receipt.target_task_id != target.task.task_id
        || receipt.target_revision != target.task.state_revision
    {
        return Err(Error::NeedsVerification(
            "prior stopping conclusion does not bind target".into(),
        ));
    }
    let plan = ExistingPlan {
        request_id: id.into(),
        command: Command {
            key: command_key(),
            intent: json!({"target_task_id":target.task.task_id}),
        },
        target,
        result: TerminalResult::AlreadyVerified { receipt },
    };
    repo.bind_existing_stop(&plan)
}
pub fn accept<R: TaskRepository + StopRepository + ExistingStopRepository + Clone>(
    control: &TaskControl<R>,
    id: &str,
    target_id: &str,
) -> Result<Acceptance, Error> {
    if id.trim().is_empty() {
        return Err(Error::InvalidCommand("empty request_id".into()));
    }
    let intent = StopVerification::new(control.repository.clone(), control.run_id.clone())?
        .normalize(json!({"target_task_id":target_id}))?;
    if control.repository.read_request(id)?.is_some() {
        return control.accept(id, command_key(), intent);
    }
    let target = control
        .repository
        .read_task(target_id)?
        .ok_or(Error::TaskNotFound)?;
    if verified_stopped(&target) {
        return existing(&control.repository, id, target);
    }
    match control.accept(id, command_key(), intent) {
        Err(error @ Error::InvalidCommand(_)) => {
            // Bounded recheck if the original finished between resolution reads.
            let current = control
                .repository
                .read_task(target_id)?
                .ok_or(Error::TaskNotFound)?;
            if verified_stopped(&current) {
                existing(&control.repository, id, current)
            } else {
                Err(error)
            }
        }
        result => result,
    }
}

/// A verified-stop interruption binds the receipt that closed it. An
/// `interrupted` task without that link (e.g. a plan-run orphan closed by
/// the startup reconcile) is already terminal — the fresh verification
/// admission reports it as not applicable rather than needing
/// verification, and never invents a prior conclusion.
fn verified_stopped(target: &TaskSnapshot) -> bool {
    target.task.status == Status::Interrupted
        && target
            .task
            .result
            .as_ref()
            .and_then(|r| r["interruption"]["verification_task_id"].as_str())
            .is_some()
}
