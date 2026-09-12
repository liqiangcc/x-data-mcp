//! SQLite conditions for an application-approved yearly execution result.
use super::*;
use crate::yearly_publication::results::{YearlyRepository, YearlyResultPlan, equivalent};
impl YearlyRepository for SqliteMarket {
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<crate::task_control::TaskSnapshot, Error> {
        use crate::task_control::TaskRepository;
        Ok(self.tasks.transition(task, revision, run, change)?)
    }
    fn commit_yearly(&self, plan: &YearlyResultPlan) -> Result<Receipt, Error> {
        self.write_transaction(true, |c| {
            let command = plan.command();
            if let Some((audit, receipt)) = read_result(c, &command.publication_id)? {
                if audit != plan.audit {return Err(Error::Conflict("yearly result id reused with different inputs".into()));}
                return Ok(receipt);
            }
            let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM publication_batches WHERE publication_id=?1 UNION ALL SELECT 1 FROM market_update_results WHERE result_id=?1)",[&command.publication_id],|r|r.get(0)).map_err(storage)?;
            if exists {return Err(Error::Conflict("result id already used".into()));}
            let base = read_record(c, &plan.base.version.version_id, true, true)?;
            if base.raw != plan.base.raw {return Err(integrity("yearly base changed after approval"));}
            if let Some(prior) = &plan.prior
                && read_record(c,&prior.version.version_id,true,true)?.raw != prior.raw {
                    return Err(integrity("prior yearly changed after approval"));
                }
            let receipt = if plan.no_change {
                for (scope, expected) in &plan.publication.required_heads {
                    if head(c,scope)? != *expected {return Err(Error::Conflict("effective head changed".into()));}
                }
                let task=crate::sqlite_tasks::read_bound_task(c,&command.task_id)?.ok_or(Error::Task(crate::task_control::Error::TaskNotFound))?;
                let next=plan.publication.finish_task(task)?;
                crate::sqlite_tasks::write_task(c,&next,command.expected_revision)?;
                Receipt {publication_id:command.publication_id.clone(),version_id:command.version_id.clone(),task_id:command.task_id.clone(),task_revision:next.task.state_revision}
            }else{commit_publication(c,&plan.publication)?};
            let audit=text(&plan.audit)?;let receipt_json=text(&receipt)?;
            c.execute("INSERT INTO yearly_execution_results VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![command.publication_id,command.task_id,command.item_id,base.version.version_id,command.version_id,plan.no_change,audit,digest(&audit,&receipt_json),receipt_json]).map_err(storage)?;
            read_result(c,&command.publication_id)?.map(|(_,r)|r).ok_or_else(||integrity("yearly result missing after insert"))
        })
    }
    fn yearly_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| read_result(c, id).map(|r| r.map(|(_, receipt)| receipt)))
    }
    fn yearly_item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c|{
            let id:Option<String>=c.query_row("SELECT result_id FROM yearly_execution_results WHERE task_id=?1 AND item_id=?2",params![task,item],|r|r.get(0)).optional().map_err(storage)?;
            id.map(|id|read_result(c,&id)?.map(|(_,r)|r).ok_or_else(||integrity("yearly item result missing"))).transpose()
        })
    }
}
pub(crate) fn read_result(c: &Connection, id: &str) -> Result<Option<(Value, Receipt)>, Error> {
    let mut stmt=c.prepare("SELECT task_id,item_id,base_daily_version_id,result_version_id,no_change,audit_json,result_sha256,receipt_json FROM yearly_execution_results WHERE result_id=?1").map_err(storage)?;
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
    let stored_digest: String = row.get(6).map_err(storage)?;
    let receipt_raw: String = row.get(7).map_err(storage)?;
    if digest(&raw, &receipt_raw) != stored_digest {
        return Err(integrity("yearly result digest mismatch"));
    }
    let audit: Value = serde_json::from_str(&raw).map_err(integrity)?;
    let command: Publication =
        serde_json::from_value(audit["command"].clone()).map_err(integrity)?;
    let receipt: Receipt = serde_json::from_str(&receipt_raw).map_err(integrity)?;
    if audit["format"] != "local-yearly-result-v1"
        || audit["no_change"] != no_change
        || audit["base_daily_version_id"] != base_id
        || command.publication_id != id
        || command.task_id != task_id
        || command.item_id != item_id
        || command.version_id != version_id
        || receipt.publication_id != id
        || receipt.version_id != version_id
        || receipt.task_id != task_id
    {
        return Err(integrity("yearly result association mismatch"));
    }
    let base = read_record(c, &base_id, true, true)?;
    let result = read_record(c, &version_id, true, true)?;
    let candidate = compat::check(&audit["candidate"])?;
    validate_base(&candidate, Some(&base))?;
    if candidate.version.period != Period::Year
        || audit["base_integrity_sha256"] != base.raw["integrity_sha256"]
        || audit["result_integrity_sha256"] != result.raw["integrity_sha256"]
    {
        return Err(integrity(
            "yearly result fixed version digest/period mismatch",
        ));
    }
    if let Some(prior_id) = audit["prior_version_id"].as_str() {
        let prior = read_record(c, prior_id, true, true)?;
        if audit["prior_integrity_sha256"] != prior.raw["integrity_sha256"] {
            return Err(integrity("yearly prior digest mismatch"));
        }
    }
    if no_change {
        if audit["prior_version_id"] != version_id || !equivalent(&result, &candidate) {
            return Err(integrity("yearly reuse lacks equal fixed facts"));
        }
    } else if result.raw != candidate.raw {
        return Err(integrity("yearly published candidate mismatch"));
    }
    let task = crate::sqlite_tasks::read_bound_task(c, &task_id)?
        .ok_or_else(|| integrity("yearly result task missing"))?;
    if task.task.state_revision < receipt.task_revision
        || receipt.task_revision <= command.expected_revision
        || !task.items.iter().any(|i| {
            i.spec.item_id == item_id
                && i.stage == crate::task_control::Stage::Finished
                && i.outcome.as_ref().is_some_and(|o| o.success)
        })
    {
        return Err(integrity("yearly result lacks successful task item"));
    }
    if matches!(
        task.task.key.operation.as_str(),
        "update_data" | "backfill_data"
    ) {
        let years = candidate
            .version
            .records
            .iter()
            .map(|r| r.date.year())
            .collect();
        let mut expected = crate::yearly_publication::tasks::frozen(
            &base_id,
            base.raw["integrity_sha256"].as_str().unwrap(),
            &years,
            &command.intent,
        );
        expected["comparison"] = if audit["prior_version_id"].is_null() {
            Value::Null
        } else {
            serde_json::json!({"version_id":audit["prior_version_id"],"integrity_sha256":audit["prior_integrity_sha256"]})
        };
        if task.task.key.contract_version != crate::yearly_publication::tasks::CONTRACT_VERSION
            || task.task.key.dataset != "market.yearly"
            || !task
                .items
                .iter()
                .any(|i| i.spec.item_id == item_id && i.spec.frozen_inputs == expected)
        {
            return Err(integrity("yearly result differs from frozen task"));
        }
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
        let published: Receipt = serde_json::from_str(
            &published.ok_or_else(|| integrity("yearly publication missing"))?,
        )
        .map_err(integrity)?;
        if published != receipt {
            return Err(integrity("yearly publication receipt mismatch"));
        }
    }
    Ok(Some((audit, receipt)))
}
fn digest(audit: &str, receipt: &str) -> String {
    crate::hash::sha256(serde_json::json!([audit, receipt]).to_string().as_bytes())
}

#[cfg(test)]
mod tests;
