//! Applies sealed verification decisions in the same database as target tasks.
use crate::{
    sqlite_tasks::{SqliteTasks, read_bound_task, write_task},
    task_control::{Error, Status, TaskSnapshot},
    task_verification::decision::{Audit, Receipt, StopPlan, StopRepository, validate_audit},
};
use rusqlite::{Connection, OptionalExtension, params};
fn storage(e: impl ToString) -> Error {
    Error::StorageUnavailable(e.to_string())
}
fn integrity(e: impl ToString) -> Error {
    Error::NeedsVerification(e.to_string())
}
fn read_audit(c: &Connection, id: &str) -> Result<Option<Audit>, Error> {
    let row:Option<(String,String,String)>=c.query_row("SELECT target_task_id,audit_json,audit_sha256 FROM stopped_write_checks WHERE verification_task_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(storage)?;
    let Some((target, raw, digest)) = row else {
        return Ok(None);
    };
    if crate::hash::sha256(raw.as_bytes()) != digest {
        return Err(integrity("stopped check digest differs"));
    }
    let audit: Audit = serde_json::from_str(&raw).map_err(integrity)?;
    if audit.receipt.verification_task_id != id || audit.receipt.target_task_id != target {
        return Err(integrity("stopped check association differs"));
    }
    validate_audit(&audit).map_err(integrity)?;
    Ok(Some(audit))
}
pub(crate) fn validate_task_link(c: &Connection, s: &TaskSnapshot) -> Result<(), Error> {
    let Some(result) = &s.task.result else {
        return Ok(());
    };
    let id = if s.task.status == Status::Interrupted {
        Some(
            result["interruption"]["verification_task_id"]
                .as_str()
                .ok_or_else(|| integrity("interruption check link missing"))?,
        )
    } else if result.get("stopped_write_check").is_some() {
        Some(s.task.task_id.as_str())
    } else {
        None
    };
    if let Some(id) = id {
        let audit = read_audit(c, id)?.ok_or_else(|| integrity("stopped check audit missing"))?;
        let expected = if s.task.status == Status::Interrupted {
            audit
                .target_after
                .as_ref()
                .ok_or_else(|| integrity("target interruption absent from audit"))?
        } else {
            &audit.verification_after
        };
        if expected != s {
            return Err(integrity("task differs from stopped check audit"));
        }
    }
    Ok(())
}
pub(crate) fn read_receipt(c: &Connection, id: &str) -> Result<Option<(Audit, Receipt)>, Error> {
    let Some(audit) = read_audit(c, id)? else {
        // A committed check with a missing audit is not an unexecuted check.
        read_bound_task(c, id)?;
        return Ok(None);
    };
    let actual = read_bound_task(c, id)?.ok_or_else(|| integrity("verification task missing"))?;
    if actual != audit.verification_after {
        return Err(integrity("verification completion differs"));
    }
    if let Some(after) = &audit.target_after
        && read_bound_task(c, &audit.receipt.target_task_id)?.as_ref() != Some(after)
    {
        return Err(integrity("interrupted target differs"));
    }
    if audit.receipt.conclusion == crate::task_verification::decision::Conclusion::AlreadyVerified {
        let prior_id = audit
            .target_before
            .task
            .result
            .as_ref()
            .and_then(|r| r["interruption"]["verification_task_id"].as_str())
            .ok_or_else(|| integrity("prior check link missing"))?;
        let prior = read_audit(c, prior_id)?
            .ok_or_else(|| integrity("prior interruption audit missing"))?;
        if prior_id == id
            || prior.receipt.conclusion
                != crate::task_verification::decision::Conclusion::Interrupted
            || prior.receipt.target_task_id != audit.receipt.target_task_id
        {
            return Err(integrity("prior interruption association differs"));
        }
        read_receipt(c, prior_id)?
            .ok_or_else(|| integrity("prior interruption receipt missing"))?;
    }
    let receipt = audit.receipt.clone();
    Ok(Some((audit, receipt)))
}
impl StopRepository for SqliteTasks {
    fn commit_stop_check(&self, plan: &StopPlan) -> Result<Receipt, Error> {
        self.write(|c| {
            let audit = &plan.audit;
            let id = &audit.receipt.verification_task_id;
            if let Some((previous, receipt)) = read_receipt(c, id)? {
                if &previous != audit {
                    return Err(Error::RequestConflict);
                }
                return Ok(receipt);
            }
            let target =
                read_bound_task(c, &audit.receipt.target_task_id)?.ok_or(Error::TaskNotFound)?;
            let verification = read_bound_task(c, id)?.ok_or(Error::TaskNotFound)?;
            if target != audit.target_before || verification != audit.verification_before {
                return Err(Error::RevisionConflict);
            }
            if let Some(after) = &audit.target_after {
                write_task(c, after, target.task.state_revision)?;
            }
            write_task(
                c,
                &audit.verification_after,
                verification.task.state_revision,
            )?;
            let raw = serde_json::to_string(audit).map_err(integrity)?;
            c.execute(
                "INSERT INTO stopped_write_checks VALUES(?1,?2,?3,?4)",
                params![
                    id,
                    audit.receipt.target_task_id,
                    raw,
                    crate::hash::sha256(raw.as_bytes())
                ],
            )
            .map_err(storage)?;
            read_receipt(c, id)?
                .map(|(_, r)| r)
                .ok_or_else(|| integrity("inserted stopped check missing"))
        })
    }
    fn stop_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| read_receipt(c, id).map(|r| r.map(|(_, receipt)| receipt)))
    }
}

#[cfg(test)]
mod tests;

use crate::task_control::{Acceptance, Command, RequestOutcome, RequestRecord};
use crate::task_verification::existing::{ExistingPlan, ExistingStopRepository, TerminalResult};
pub(crate) fn validate_terminal(
    c: &Connection,
    command: &Command,
    value: &serde_json::Value,
) -> Result<(), Error> {
    if command.key != crate::task_verification::command_key() {
        return Ok(());
    }
    let result: TerminalResult = serde_json::from_value(value.clone()).map_err(integrity)?;
    let expected = result.receipt();
    if command.intent != serde_json::json!({"target_task_id":expected.target_task_id}) {
        return Err(integrity("known stop request intent differs"));
    }
    if read_receipt(c, &expected.verification_task_id)?
        .map(|(_, r)| r)
        .as_ref()
        != Some(expected)
    {
        return Err(integrity("known stop receipt missing or changed"));
    }
    if expected.conclusion != crate::task_verification::decision::Conclusion::Interrupted {
        return Err(integrity("known stop request has no interruption"));
    }
    Ok(())
}
impl ExistingStopRepository for SqliteTasks {
    fn bind_existing_stop(&self, plan: &ExistingPlan) -> Result<Acceptance, Error> {
        self.write(|c| {
            if let Some(record) = crate::sqlite_tasks::read_request(c, &plan.request_id)? {
                if record.command != plan.command {
                    return Err(Error::RequestConflict);
                }
                return Ok(Acceptance {
                    record,
                    reused: true,
                });
            }
            if read_bound_task(c, &plan.target.task.task_id)?.as_ref() != Some(&plan.target) {
                return Err(Error::RevisionConflict);
            }
            let value = serde_json::to_value(&plan.result).map_err(integrity)?;
            validate_terminal(c, &plan.command, &value)?;
            let command = serde_json::to_string(&plan.command).map_err(integrity)?;
            let result = serde_json::to_string(&value).map_err(integrity)?;
            c.execute(
                "INSERT INTO requests VALUES(?1,?2,?3,'terminal',NULL,?4)",
                params![
                    plan.request_id,
                    command,
                    crate::hash::sha256(command.as_bytes()),
                    result
                ],
            )
            .map_err(storage)?;
            Ok(Acceptance {
                record: RequestRecord {
                    request_id: plan.request_id.clone(),
                    command: plan.command.clone(),
                    outcome: RequestOutcome::Terminal(value),
                },
                reused: false,
            })
        })
    }
}
