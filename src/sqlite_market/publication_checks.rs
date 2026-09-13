//! Applies sealed publication-check decisions; never mutates the target task.
use super::*;
use crate::publication_verification::decision::{
    Audit, AuditStore, Conclusion, Plan, Receipt as CheckReceipt, validate_audit,
};
fn read_audit(c: &Connection, id: &str) -> Result<Option<Audit>, Error> {
    let row:Option<(String,String,String)>=c.query_row("SELECT target_task_id,audit_json,audit_sha256 FROM publication_checks WHERE verification_task_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(storage)?;
    let Some((target, raw, digest)) = row else {
        return Ok(None);
    };
    if crate::hash::sha256(raw.as_bytes()) != digest {
        return Err(integrity("publication check audit digest differs"));
    }
    let audit: Audit = serde_json::from_str(&raw).map_err(integrity)?;
    if audit.receipt.verification_task_id != id || audit.receipt.target_task_id != target {
        return Err(integrity("publication check association differs"));
    }
    validate_audit(&audit)?;
    Ok(Some(audit))
}
pub(crate) fn validate_task_link(
    c: &Connection,
    task: &crate::task_control::TaskSnapshot,
) -> Result<(), crate::task_control::Error> {
    if task.task.key != crate::publication_verification::admission::command_key()
        || task.task.status != crate::task_control::Status::Succeeded
    {
        return Ok(());
    }
    let audit = read_audit(c, &task.task.task_id)
        .map_err(|e| match e {
            Error::Task(e) => e,
            Error::Storage(s) => crate::task_control::Error::StorageUnavailable(s),
            other => crate::task_control::Error::NeedsVerification(other.to_string()),
        })?
        .ok_or_else(|| {
            crate::task_control::Error::NeedsVerification("publication check audit missing".into())
        })?;
    if audit.verification_after != *task {
        return Err(crate::task_control::Error::NeedsVerification(
            "publication check task differs from audit".into(),
        ));
    }
    Ok(())
}
fn read_receipt(c: &Connection, id: &str) -> Result<Option<(Audit, CheckReceipt)>, Error> {
    let Some(audit) = read_audit(c, id)? else {
        crate::sqlite_tasks::read_bound_task(c, id)?;
        return Ok(None);
    };
    let task = crate::sqlite_tasks::read_bound_task(c, id)?
        .ok_or_else(|| integrity("publication check task missing"))?;
    if task != audit.verification_after {
        return Err(integrity("publication check completion differs"));
    }
    if audit.receipt.conclusion == Conclusion::AllRecorded {
        let current = super::verification::read_facts(c, &audit.receipt.target_task_id)?;
        if current.items != audit.facts.items {
            return Err(integrity("confirmed publication evidence changed"));
        }
    }
    let receipt = audit.receipt.clone();
    Ok(Some((audit, receipt)))
}
impl AuditStore for SqliteMarket {
    fn commit_check(&self, plan: &Plan) -> Result<CheckReceipt, Error> {
        self.write_transaction(true, |c| {
            let audit = &plan.audit;
            let id = &audit.receipt.verification_task_id;
            if let Some((previous, receipt)) = read_receipt(c, id)? {
                if previous != *audit {
                    return Err(Error::Conflict("publication check replay differs".into()));
                }
                return Ok(receipt);
            }
            let check = crate::sqlite_tasks::read_bound_task(c, id)?
                .ok_or(Error::Task(crate::task_control::Error::TaskNotFound))?;
            let facts = super::verification::read_facts(c, &audit.receipt.target_task_id)?;
            if check != audit.verification_before || facts != audit.facts {
                return Err(Error::Conflict(
                    "publication check revision or evidence changed".into(),
                ));
            }
            crate::sqlite_tasks::write_task(
                c,
                &audit.verification_after,
                check.task.state_revision,
            )?;
            let raw = serde_json::to_string(audit).map_err(integrity)?;
            c.execute(
                "INSERT INTO publication_checks VALUES(?1,?2,?3,?4)",
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
                .ok_or_else(|| integrity("publication check receipt missing after insert"))
        })
    }
    fn check_receipt(&self, id: &str) -> Result<Option<CheckReceipt>, Error> {
        self.read(|c| read_receipt(c, id).map(|r| r.map(|(_, receipt)| receipt)))
    }
}
