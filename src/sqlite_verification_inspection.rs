//! Snapshot reads and conditional persistence for a sealed bounded inspection.
use crate::{
    sqlite_tasks::{SqliteTasks, read_bound_task, read_request, write_task},
    task_control::{Acceptance, Command, Error, RequestOutcome, Status, TaskSnapshot},
    verification_inspection::{
        TerminalResult, command_key,
        decision::{Audit, Facts, Observation, Plan},
        service::{Probe, Repository},
    },
};
use rusqlite::{Connection, OptionalExtension, params};
fn storage(e: impl ToString) -> Error {
    Error::StorageUnavailable(e.to_string())
}
fn integrity(e: impl ToString) -> Error {
    Error::NeedsVerification(e.to_string())
}
fn market_error(e: crate::market_storage::Error) -> Error {
    match e {
        crate::market_storage::Error::Task(e) => e,
        crate::market_storage::Error::Storage(s) => Error::StorageUnavailable(s),
        other => integrity(other),
    }
}
fn read_audit(c: &Connection, id: &str) -> Result<Option<Audit>, Error> {
    let row: Option<(String,String,String)> = c.query_row("SELECT target_task_id,audit_json,audit_sha256 FROM verification_inspections WHERE request_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(storage)?;
    let Some((target, raw, digest)) = row else {
        return Ok(None);
    };
    if crate::hash::sha256(raw.as_bytes()) != digest {
        return Err(integrity("inspection audit digest differs"));
    }
    let audit: Audit = serde_json::from_str(&raw).map_err(integrity)?;
    if audit.request_id != id || audit.facts.target.task.task_id != target {
        return Err(integrity("inspection association differs"));
    }
    crate::verification_inspection::decision::validate(&audit)?;
    Ok(Some(audit))
}
pub(crate) fn validate_absent_request(c: &Connection, id: &str) -> Result<(), Error> {
    let exists: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM verification_inspections WHERE request_id=?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if exists {
        return Err(integrity("inspection request binding missing"));
    }
    Ok(())
}
pub(crate) fn validate_terminal(
    c: &Connection,
    id: &str,
    command: &Command,
    value: &serde_json::Value,
) -> Result<(), Error> {
    if command.key != command_key() {
        // An inspection audit may not hide behind another registered contract.
        return validate_absent_request(c, id);
    }
    let result: TerminalResult = serde_json::from_value(value.clone()).map_err(integrity)?;
    let audit = read_audit(c, id)?.ok_or_else(|| integrity("inspection audit missing"))?;
    if audit.command != *command || audit.result != result {
        return Err(integrity(
            "inspection request differs from historical audit",
        ));
    }
    // Historical replay deliberately does not re-inspect the target's current state.
    Ok(())
}
pub(crate) fn validate_task_link(c: &Connection, task: &TaskSnapshot) -> Result<bool, Error> {
    let Some(id) = task
        .task
        .result
        .as_ref()
        .and_then(|r| r["interruption"]["inspection_request_id"].as_str())
    else {
        return Ok(false);
    };
    if task.task.status != Status::Interrupted
        || !crate::verification_execution::supported(&task.task.key)
    {
        return Err(integrity(
            "inspection interruption on unsupported task/state",
        ));
    }
    let audit = read_audit(c, id)?.ok_or_else(|| integrity("inspection finish audit missing"))?;
    if audit.after.as_ref() != Some(task) {
        return Err(integrity("inspection finish task projection differs"));
    }
    let record =
        read_request(c, id)?.ok_or_else(|| integrity("inspection finish request missing"))?;
    if record.command != audit.command {
        return Err(integrity("inspection finish request differs"));
    }
    Ok(true)
}
fn read_facts(c: &Connection, id: &str) -> Result<Facts, Error> {
    let target = read_bound_task(c, id)?.ok_or(Error::TaskNotFound)?;
    if !crate::verification_execution::supported(&target.task.key) {
        return Ok(Facts {
            target,
            original: None,
            registration: None,
            finished: false,
            has_business_effects: false,
            completion_digest: None,
        });
    }
    let original_id = target
        .items
        .first()
        .and_then(|i| i.spec.frozen_inputs["target_task_id"].as_str())
        .ok_or_else(|| integrity("verification original target missing"))?;
    if original_id == id {
        return Err(integrity("recursive verification target"));
    }
    let original = read_bound_task(c, original_id)?
        .ok_or_else(|| integrity("verification original task missing"))?;
    if original.task.key.dataset == crate::task_verification::DATASET {
        return Err(integrity("verification targets another verification task"));
    }
    let stop = crate::sqlite_verification::read_receipt(c, id)?;
    let publication =
        crate::sqlite_market::publication_checks::read_receipt(c, id).map_err(market_error)?;
    if (stop.is_some() && target.task.key != crate::task_verification::command_key())
        || (publication.is_some()
            && target.task.key != crate::publication_verification::admission::command_key())
    {
        return Err(integrity("wrong verification receipt kind"));
    }
    let completion_digest = if let Some((audit, _)) = &stop {
        Some(crate::hash::sha256(
            &serde_json::to_vec(audit).map_err(integrity)?,
        ))
    } else if let Some((audit, _)) = &publication {
        Some(crate::hash::sha256(
            &serde_json::to_vec(audit).map_err(integrity)?,
        ))
    } else {
        None
    };
    if target.task.status == Status::Succeeded && completion_digest.is_none() {
        return Err(integrity("successful verification lacks its receipt"));
    }
    let has_business_effects: bool = c
        .query_row(
            if crate::sqlite_market::is_latest_only_schema(c).map_err(storage)? {
                "SELECT EXISTS(SELECT 1 FROM market_events WHERE task_id=?1)"
            } else if crate::sqlite_market::is_compact_schema(c).map_err(storage)? {
                "SELECT EXISTS(SELECT 1 FROM task_publications WHERE task_id=?1 \
                 UNION ALL SELECT 1 FROM market_update_results WHERE task_id=?1 \
                 UNION ALL SELECT 1 FROM publication_results WHERE task_id=?1)"
            } else {
                "SELECT EXISTS(SELECT 1 FROM publication_batches WHERE task_id=?1 \
                 UNION ALL SELECT 1 FROM task_publications WHERE task_id=?1 \
                 UNION ALL SELECT 1 FROM market_update_results WHERE task_id=?1 \
                 UNION ALL SELECT 1 FROM yearly_execution_results WHERE task_id=?1)"
            },
            [id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    let registration = crate::sqlite_verification_runtime::read_registration(c, &target)?;
    let finished = target.task.status.terminal();
    Ok(Facts {
        target,
        original: Some(original),
        registration,
        finished,
        has_business_effects,
        completion_digest,
    })
}
impl Repository for SqliteTasks {
    fn inspection_facts(&self, id: &str) -> Result<Facts, Error> {
        self.read(|c| read_facts(c, id))
    }
    fn probe_execution(&self, facts: &Facts) -> Result<Probe, Error> {
        let registration = facts
            .registration
            .as_ref()
            .ok_or_else(|| integrity("runtime registration missing"))?;
        match crate::verification_runtime::try_acquire(self.path(), &registration.descriptor) {
            Ok(guard) => Ok(Probe {
                observation: Observation::Excluded,
                guard: Some(guard),
            }),
            Err(crate::verification_runtime::Error::Busy) => Ok(Probe {
                observation: Observation::Busy,
                guard: None,
            }),
            Err(error) => Ok(Probe {
                observation: Observation::Unverified(format!(
                    "runtime exclusion not verified: {error:?}"
                )),
                guard: None,
            }),
        }
    }
    fn commit_inspection(&self, plan: &Plan, probe: &Probe) -> Result<Acceptance, Error> {
        if plan.audit.observation != probe.observation {
            return Err(Error::RevisionConflict);
        }
        self.write(|c| {
            let a = &plan.audit;
            if let Some(record) = read_request(c, &a.request_id)? {
                if record.command != a.command {
                    return Err(Error::RequestConflict);
                }
                return Ok(Acceptance {
                    record,
                    reused: true,
                });
            }
            if read_facts(c, &a.facts.target.task.task_id)? != a.facts {
                return Err(Error::RevisionConflict);
            }
            if matches!(a.observation, Observation::Excluded) {
                let guard = probe
                    .guard
                    .as_ref()
                    .ok_or_else(|| integrity("actual runtime guard absent"))?;
                guard
                    .verify_binding(self.path())
                    .map_err(crate::sqlite_verification_runtime::runtime_error)?;
                if a.facts.registration.as_ref().map(|r| &r.descriptor) != Some(guard.descriptor())
                {
                    return Err(Error::RevisionConflict);
                }
            }
            let command = serde_json::to_string(&a.command).map_err(integrity)?;
            let value = serde_json::to_value(&a.result).map_err(integrity)?;
            let terminal = serde_json::to_string(&value).map_err(integrity)?;
            c.execute(
                "INSERT INTO requests VALUES(?1,?2,?3,'terminal',NULL,?4)",
                params![
                    a.request_id,
                    command,
                    crate::hash::sha256(command.as_bytes()),
                    terminal
                ],
            )
            .map_err(storage)?;
            let raw = serde_json::to_string(a).map_err(integrity)?;
            c.execute(
                "INSERT INTO verification_inspections VALUES(?1,?2,?3,?4)",
                params![
                    a.request_id,
                    a.facts.target.task.task_id,
                    raw,
                    crate::hash::sha256(raw.as_bytes())
                ],
            )
            .map_err(storage)?;
            if let Some(after) = &a.after {
                write_task(c, after, a.facts.target.task.state_revision)?;
            }
            if let Some(guard) = &probe.guard {
                guard
                    .verify_binding(self.path())
                    .map_err(crate::sqlite_verification_runtime::runtime_error)?;
            }
            let actual =
                read_bound_task(c, &a.facts.target.task.task_id)?.ok_or(Error::TaskNotFound)?;
            if &actual != a.after.as_ref().unwrap_or(&a.facts.target) {
                return Err(integrity("inspection target postcondition differs"));
            }
            let record = read_request(c, &a.request_id)?
                .ok_or_else(|| integrity("inspection request missing after insert"))?;
            if !matches!(&record.outcome,RequestOutcome::Terminal(v) if v==&value) {
                return Err(integrity("inspection terminal differs"));
            }
            Ok(Acceptance {
                record,
                reused: false,
            })
        })
    }
}
#[cfg(test)]
mod tests;

pub(crate) fn validate_task_request(
    c: &Connection,
    id: &str,
    command: &Command,
) -> Result<(), Error> {
    if command.key == command_key() {
        return Err(integrity(
            "bounded inspection must not bind an asynchronous task",
        ));
    }
    validate_absent_request(c, id)
}
