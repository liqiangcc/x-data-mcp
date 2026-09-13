//! Read all task publication facts from one verified SQLite snapshot.
use super::*;
use crate::publication_verification::{Evidence, EvidenceRepository, Facts, ItemFact};
use crate::task_control::{Contract, Error as TaskError, Stage, legacy::LegacyCollection};
impl EvidenceRepository for SqliteMarket {
    fn publication_facts(&self, task_id: &str) -> Result<Facts, Error> {
        self.read(|c| {
            let task = crate::sqlite_tasks::read_bound_task(c, task_id)?.ok_or(Error::Task(TaskError::TaskNotFound))?;
            let key = &task.task.key;
            let daily = key.dataset == "market.daily" && key.contract_version == crate::market_update::tasks::CONTRACT_VERSION;
            let yearly = key.dataset == "market.yearly" && key.contract_version == crate::yearly_publication::tasks::CONTRACT_VERSION;
            let legacy = key == &LegacyCollection.key() || (key.operation == "start_collection" && key.contract_version == "v1" && matches!(key.dataset.as_str(), "market.daily" | "market.yearly"));
            if !legacy && (!matches!(key.operation.as_str(), "update_data" | "backfill_data") || !(daily || yearly)) {
                return Err(Error::Task(TaskError::UnsupportedContract));
            }
            let mut stmt = c.prepare("SELECT 0,result_id,item_id FROM market_update_results WHERE task_id=?1 UNION ALL SELECT 1,result_id,item_id FROM yearly_execution_results WHERE task_id=?1").map_err(storage)?;
            let rows = stmt.query_map([task_id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(storage)?;
            let mut found = std::collections::BTreeMap::new();
            let mut published = 0_i64;
            for row in rows {
                let (kind,id,item) = row.map_err(storage)?;
                if legacy || (kind == 0) != daily || !task.items.iter().any(|i|i.spec.item_id==item) || found.contains_key(&item) {
                    return Err(integrity("publication result contract/item association differs"));
                }
                let (audit,receipt) = if daily { super::update::read_result(c,&id)? } else { super::yearly_results::read_result(c,&id)? }.ok_or_else(||integrity("publication result disappeared inside snapshot"))?;
                let reused_version = audit["no_change"].as_bool().ok_or_else(||integrity("reuse fact missing"))?;
                if !reused_version { published += 1; }
                found.insert(item, Evidence::Committed { receipt, reused_version });
            }
            if legacy {
                let mut stmt = c.prepare("SELECT publication_id,version_id,item_id,command_json,receipt_json FROM publication_batches WHERE task_id=?1").map_err(storage)?;
                let rows = stmt.query_map([task_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?))).map_err(storage)?;
                for row in rows {
                    let (id,version,item,command,raw_receipt) = row.map_err(storage)?;
                    let command: Publication = serde_json::from_str(&command).map_err(integrity)?;
                    let receipt: Receipt = serde_json::from_str(&raw_receipt).map_err(integrity)?;
                    let record = read_record(c, &version, true, true)?;
                    if receipt.publication_id != id || receipt.version_id != version || receipt.task_id != task_id || command.task_id != task_id || command.item_id != item || command.publication_id != id || command.version_id != version || receipt.task_revision <= command.expected_revision || receipt.task_revision > task.task.state_revision || found.contains_key(&item) || !task.items.iter().any(|i|i.spec.item_id == item && i.stage == Stage::Finished && i.outcome.as_ref().is_some_and(|o|o.success)) {
                        return Err(integrity("legacy publication/task receipt association differs"));
                    }
                    // Force verified fixed-record evaluation; no latest lookup.
                    if record.version.version_id != version { return Err(integrity("legacy fixed version differs")); }
                    published += 1;
                    found.insert(item, Evidence::Committed { receipt, reused_version: false });
                }
            }
            // Orphan/extra batches cannot be relabelled as absence. Reuse has no
            // new batch; its fixed version and receipt were verified above.
            for table in ["publication_batches", "task_publications"] {
                let count: i64 = c.query_row(&format!("SELECT count(*) FROM {table} WHERE task_id=?1"), [task_id], |r|r.get(0)).map_err(storage)?;
                if count != published { return Err(integrity("task publication associations do not match execution receipts")); }
            }
            let mut items = Vec::new();
            for item in &task.items {
                let evidence = found.remove(&item.spec.item_id).unwrap_or(Evidence::Missing);
                if matches!(evidence, Evidence::Missing) && (item.stage==Stage::Finished && item.outcome.as_ref().is_some_and(|o|o.success)) {
                    return Err(integrity("successful item lacks publication execution evidence"));
                }
                items.push(ItemFact { item_id:item.spec.item_id.clone(), evidence });
            }
            Ok(Facts { task, items })
        })
    }
}
