//! Application qualification for a verification claim with a real runtime guard.
use crate::{
    task_control::{self, Change, CommandKey, Error, Status, TaskSnapshot},
    verification_runtime::{GuardDescriptor, PROTOCOL},
};
use chrono::{DateTime, Utc};
pub fn supported(key: &CommandKey) -> bool {
    *key == crate::task_verification::command_key()
        || *key == crate::publication_verification::admission::command_key()
}
pub struct ClaimPlan {
    pub(crate) before: TaskSnapshot,
    pub(crate) after: TaskSnapshot,
    pub(crate) descriptor: GuardDescriptor,
}
impl ClaimPlan {
    pub fn claimed(&self) -> &TaskSnapshot {
        &self.after
    }
}
pub fn prepare_claim(
    task: TaskSnapshot,
    descriptor: GuardDescriptor,
    now: DateTime<Utc>,
) -> Result<ClaimPlan, Error> {
    if !supported(&task.task.key) {
        return Err(Error::UnsupportedContract);
    }
    if descriptor.protocol != PROTOCOL
        || descriptor.task_id != task.task.task_id
        || descriptor.run_id != task.task.accepted_run_id
        || task.task.status != Status::Pending
        || task.task.owner_run_id.is_some()
    {
        return Err(Error::RevisionConflict);
    }
    let mut after = task.clone();
    task_control::decide(&mut after, &descriptor.run_id, Change::Claim, now)?;
    Ok(ClaimPlan {
        before: task,
        after,
        descriptor,
    })
}
