//! Atomic task Claim + runtime identity registration. No stopping decision here.
use crate::{
    sqlite_tasks::{SqliteTasks, read_bound_task, write_task},
    task_control::{Error, TaskSnapshot},
    verification_execution::ClaimPlan,
    verification_runtime::{ExecutionGuard, GuardDescriptor, PROTOCOL},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub claim_revision: u64,
    pub descriptor: GuardDescriptor,
}
fn storage(error: impl ToString) -> Error {
    Error::StorageUnavailable(error.to_string())
}
fn integrity(error: impl ToString) -> Error {
    Error::NeedsVerification(error.to_string())
}
pub(crate) fn runtime_error(error: crate::verification_runtime::Error) -> Error {
    match error {
        crate::verification_runtime::Error::Io(error) => storage(error),
        error => integrity(error),
    }
}
pub(crate) fn read_registration(
    c: &Connection,
    task: &TaskSnapshot,
) -> Result<Option<Registration>, Error> {
    let row: Option<(i64,String,String)> = c.query_row(
        "SELECT claim_revision,descriptor_json,descriptor_sha256 FROM verification_runtime_guards WHERE task_id=?1",
        [&task.task.task_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional().map_err(storage)?;
    let Some((revision, raw, digest)) = row else {
        return Ok(None);
    };
    if crate::hash::sha256(raw.as_bytes()) != digest {
        return Err(integrity("runtime registration digest differs"));
    }
    let descriptor: GuardDescriptor = serde_json::from_str(&raw).map_err(integrity)?;
    let revision = u64::try_from(revision).map_err(integrity)?;
    if revision == 0
        || revision > task.task.state_revision
        || descriptor.protocol != PROTOCOL
        || descriptor.task_id != task.task.task_id
        || task.task.owner_run_id.as_deref() != Some(&descriptor.run_id)
        || task.task.accepted_run_id != descriptor.run_id
        || !crate::verification_execution::supported(&task.task.key)
    {
        return Err(integrity(
            "runtime registration does not bind the claimed task",
        ));
    }
    Ok(Some(Registration {
        claim_revision: revision,
        descriptor,
    }))
}
impl SqliteTasks {
    pub fn runtime_registration(&self, id: &str) -> Result<Option<Registration>, Error> {
        self.read(|c| {
            let task = read_bound_task(c, id)?;
            if let Some(task) = task {
                return read_registration(c, &task);
            }
            let exists: bool = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM verification_runtime_guards WHERE task_id=?1)",
                    [id],
                    |r| r.get(0),
                )
                .map_err(storage)?;
            if exists {
                return Err(integrity("orphan runtime registration"));
            }
            Err(Error::TaskNotFound)
        })
    }
    pub fn claim_with_runtime_guard(
        &self,
        plan: &ClaimPlan,
        guard: &ExecutionGuard,
    ) -> Result<TaskSnapshot, Error> {
        guard.verify_binding(self.path()).map_err(runtime_error)?;
        if guard.descriptor() != &plan.descriptor {
            return Err(Error::RevisionConflict);
        }
        self.write(|c| {
            let current =
                read_bound_task(c, &plan.before.task.task_id)?.ok_or(Error::TaskNotFound)?;
            if current != plan.before {
                return Err(Error::RevisionConflict);
            }
            if read_registration(c, &current)?.is_some() {
                return Err(integrity("task already has a runtime claim"));
            }
            guard.verify_binding(self.path()).map_err(runtime_error)?;
            write_task(c, &plan.after, plan.before.task.state_revision)?;
            let raw = serde_json::to_string(&plan.descriptor).map_err(integrity)?;
            let revision = i64::try_from(plan.after.task.state_revision).map_err(integrity)?;
            c.execute(
                "INSERT INTO verification_runtime_guards VALUES(?1,?2,?3,?4)",
                params![
                    plan.after.task.task_id,
                    revision,
                    raw,
                    crate::hash::sha256(raw.as_bytes())
                ],
            )
            .map_err(storage)?;
            let stored =
                read_bound_task(c, &plan.after.task.task_id)?.ok_or(Error::TaskNotFound)?;
            if stored != plan.after {
                return Err(integrity("claimed runtime task projection differs"));
            }
            read_registration(c, &stored)?
                .ok_or_else(|| integrity("runtime registration missing"))?;
            guard.verify_binding(self.path()).map_err(runtime_error)?;
            Ok(stored)
        })
    }
}
#[cfg(test)]
mod tests;
