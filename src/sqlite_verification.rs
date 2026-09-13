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
fn read_receipt(c: &Connection, id: &str) -> Result<Option<(Audit, Receipt)>, Error> {
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
