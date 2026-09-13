//! Bind a new global request to a prior verified conclusion, without inspection.
use super::{
    admission::{PublicationVerification, command_key},
    decision::{Conclusion, Receipt},
};
use crate::task_control::{
    Acceptance, Command, Contract, Error, TaskRepository, TaskSnapshot, service::TaskControl,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct KnownPlan {
    pub(crate) request_id: String,
    pub(crate) command: Command,
    pub(crate) target: TaskSnapshot,
    pub(crate) result: TerminalResult,
}
pub trait KnownRepository {
    fn known_publication(&self, target: &str) -> Result<Option<Receipt>, Error>;
    fn bind_known_publication(&self, plan: &KnownPlan) -> Result<Acceptance, Error>;
}
fn known<R: TaskRepository + KnownRepository>(
    repository: &R,
    id: &str,
    target_id: &str,
) -> Result<Option<Acceptance>, Error> {
    let target = repository
        .read_task(target_id)?
        .ok_or(Error::TaskNotFound)?;
    if super::contract_kind(&target.task.key).is_none() {
        return Err(Error::UnsupportedContract);
    }
    let Some(receipt) = repository.known_publication(target_id)? else {
        return Ok(None);
    };
    if receipt.conclusion != Conclusion::AllRecorded || receipt.target_task_id != target_id {
        return Err(Error::NeedsVerification(
            "known publication conclusion does not bind target".into(),
        ));
    }
    repository
        .bind_known_publication(&KnownPlan {
            request_id: id.into(),
            command: Command {
                key: command_key(),
                intent: json!({"target_task_id":target_id}),
            },
            target,
            result: TerminalResult::AlreadyVerified { receipt },
        })
        .map(Some)
}
pub fn accept<R: TaskRepository + KnownRepository + Clone>(
    control: &TaskControl<R>,
    id: &str,
    target: &str,
) -> Result<Acceptance, Error> {
    if id.trim().is_empty() {
        return Err(Error::InvalidCommand("empty request_id".into()));
    }
    let intent = PublicationVerification::new(control.repository.clone(), control.run_id.clone())?
        .normalize(json!({"target_task_id":target}))?;
    if control.repository.read_request(id)?.is_some() {
        return control.accept(id, command_key(), intent);
    }
    if let Some(result) = known(&control.repository, id, target)? {
        return Ok(result);
    }
    match control.accept(id, command_key(), intent) {
        Err(error @ Error::InvalidCommand(_)) => {
            Ok(known(&control.repository, id, target)?.ok_or(error)?)
        }
        result => result,
    }
}
