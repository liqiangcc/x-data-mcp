//! Atomic mapping for an application-approved range result.
use super::*;
use crate::market_update::publication::{RangePlan, RangeRepository};

impl RangeRepository for SqliteMarket {
    fn commit_range(&self, plan: &RangePlan) -> Result<Receipt, Error> {
        self.write_transaction(true, |c| {
            let command = plan.command();
            if let Some((audit, receipt)) = read_result(c, &command.publication_id)? {
                if audit != plan.audit {
                    return Err(Error::Conflict(
                        "range result id reused with different inputs".into(),
                    ));
                }
                return Ok(receipt);
            }
            // Do not attach a range result retroactively to another publication.
            let exists: bool = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM publication_batches WHERE publication_id=?1)",
                    [&command.publication_id],
                    |r| r.get(0),
                )
                .map_err(storage)?;
            if exists {
                return Err(Error::Conflict("publication id already used".into()));
            }
            let base = read_record(c, &plan.base.version.version_id, true, true)?;
            if base.raw != plan.base.raw {
                return Err(integrity("update base changed after range approval"));
            }
            let receipt = if plan.no_change {
                for (scope, expected) in &plan.publication.required_heads {
                    if head(c, scope)? != *expected {
                        return Err(Error::Conflict("effective head changed".into()));
                    }
                }
                let task = crate::sqlite_tasks::read_bound_task(c, &command.task_id)?
                    .ok_or(Error::Task(crate::task_control::Error::TaskNotFound))?;
                let next = plan.publication.finish_task(task)?;
                crate::sqlite_tasks::write_task(c, &next, command.expected_revision)?;
                Receipt {
                    publication_id: command.publication_id.clone(),
                    version_id: command.version_id.clone(),
                    task_id: command.task_id.clone(),
                    task_revision: next.task.state_revision,
                }
            } else {
                commit_publication(c, &plan.publication)?
            };
            let audit = text(&plan.audit)?;
            let receipt_json = text(&receipt)?;
            c.execute(
                "INSERT INTO market_update_results VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    command.publication_id,
                    command.task_id,
                    command.item_id,
                    base.version.version_id,
                    command.version_id,
                    plan.no_change,
                    audit,
                    result_digest(&audit, &receipt_json),
                    receipt_json
                ],
            )
            .map_err(storage)?;
            read_result(c, &command.publication_id)?
                .map(|(_, r)| r)
                .ok_or_else(|| integrity("range receipt missing after insert"))
        })
    }
    fn range_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| read_result(c, id).map(|r| r.map(|(_, receipt)| receipt)))
    }
}
fn read_result(c: &Connection, id: &str) -> Result<Option<(Value, Receipt)>, Error> {
    let mut stmt = c.prepare(
        "SELECT task_id,item_id,base_version_id,result_version_id,no_change,audit_json,result_sha256,receipt_json FROM market_update_results WHERE result_id=?1",
    ).map_err(storage)?;
    let mut rows = stmt.query([id]).map_err(storage)?;
    let Some(row) = rows.next().map_err(storage)? else {
        return Ok(None);
    };
    let task_id: String = row.get(0).map_err(storage)?;
    let item_id: String = row.get(1).map_err(storage)?;
    let base_id: String = row.get(2).map_err(storage)?;
    let version_id: String = row.get(3).map_err(storage)?;
    let no_change: bool = row.get(4).map_err(storage)?;
    let raw: String = row.get(5).map_err(storage)?;
    let digest: String = row.get(6).map_err(storage)?;
    let receipt: String = row.get(7).map_err(storage)?;
    if result_digest(&raw, &receipt) != digest {
        return Err(integrity("range audit digest mismatch"));
    }
    let audit: Value = serde_json::from_str(&raw).map_err(integrity)?;
    let command: Publication =
        serde_json::from_value(audit["command"].clone()).map_err(integrity)?;
    let receipt: Receipt = serde_json::from_str(&receipt).map_err(integrity)?;
    if command.publication_id != id
        || command.task_id != task_id
        || command.item_id != item_id
        || command.version_id != version_id
        || receipt.publication_id != id
        || receipt.task_id != task_id
        || receipt.version_id != version_id
        || audit["evidence"]["base_version_id"] != base_id
        || audit["no_change"] != no_change
        || command.intent
            != (PublishIntent::Daily {
                expected_head: Some(base_id.clone()),
            })
        || no_change != (base_id == version_id)
        || audit["algorithm_version"] != "local-range-update-v1"
    {
        return Err(integrity("range result association mismatch"));
    }
    let base = read_record(c, &base_id, true, true)?;
    let result = read_record(c, &version_id, true, true)?;
    if audit["base_integrity_sha256"] != base.raw["integrity_sha256"]
        || audit["result_integrity_sha256"] != result.raw["integrity_sha256"]
    {
        return Err(integrity("range result version digest mismatch"));
    }
    let task = crate::sqlite_tasks::read_bound_task(c, &task_id)?
        .ok_or_else(|| integrity("range result task missing"))?;
    if task.task.state_revision < receipt.task_revision
        || receipt.task_revision <= command.expected_revision
        || !task.items.iter().any(|item| {
            item.spec.item_id == item_id
                && item.stage == crate::task_control::Stage::Finished
                && item.outcome.as_ref().is_some_and(|outcome| outcome.success)
        })
    {
        return Err(integrity("range result lacks successful task item"));
    }
    if !no_change {
        let published: Option<String> = c
            .query_row(
                "SELECT receipt_json FROM publication_batches WHERE publication_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        let published: Receipt =
            serde_json::from_str(&published.ok_or_else(|| integrity("range publication missing"))?)
                .map_err(integrity)?;
        if published != receipt {
            return Err(integrity("range publication receipt mismatch"));
        }
    }
    Ok(Some((audit, receipt)))
}

fn result_digest(audit: &str, receipt: &str) -> String {
    // JSON array framing prevents ambiguous concatenation; both persisted values are covered.
    crate::hash::sha256(serde_json::json!([audit, receipt]).to_string().as_bytes())
}
