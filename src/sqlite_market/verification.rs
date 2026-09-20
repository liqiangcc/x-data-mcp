//! Read all task publication facts from one verified SQLite snapshot.
use super::*;
use crate::publication_verification::{Evidence, EvidenceRepository, Facts, ItemFact};
use crate::task_control::{Error as TaskError, Stage};
impl EvidenceRepository for SqliteMarket {
    fn publication_facts(&self, task_id: &str) -> Result<Facts, Error> {
        self.read(|c| read_facts(c, task_id))
    }
}
pub(crate) fn read_facts(c: &Connection, task_id: &str) -> Result<Facts, Error> {
    let task = crate::sqlite_tasks::read_bound_task(c, task_id)?
        .ok_or(Error::Task(TaskError::TaskNotFound))?;
    use crate::publication_verification::{FactContract, contract_kind};
    let kind = contract_kind(&task.task.key).ok_or(Error::Task(TaskError::UnsupportedContract))?;
    let daily = matches!(kind, FactContract::Daily);
    let legacy = matches!(kind, FactContract::Legacy);
    let mut stmt = c.prepare("SELECT 0,result_id,item_id FROM market_update_results WHERE task_id=?1 UNION ALL SELECT 1,result_id,item_id FROM yearly_execution_results WHERE task_id=?1").map_err(storage)?;
    let rows = stmt
        .query_map([task_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(storage)?;
    let mut found = std::collections::BTreeMap::new();
    let mut published = 0_i64;
    for row in rows {
        let (kind, id, item) = row.map_err(storage)?;
        if legacy
            || (kind == 0) != daily
            || !task.items.iter().any(|i| i.spec.item_id == item)
            || found.contains_key(&item)
        {
            return Err(integrity(
                "publication result contract/item association differs",
            ));
        }
        let (audit, receipt) = if daily {
            super::update::read_result(c, &id)?
        } else {
            super::yearly_results::read_result(c, &id)?
        }
        .ok_or_else(|| integrity("publication result disappeared inside snapshot"))?;
        let reused_version = audit["no_change"]
            .as_bool()
            .ok_or_else(|| integrity("reuse fact missing"))?;
        if !reused_version {
            published += 1;
        }
        found.insert(
            item,
            Evidence::Committed {
                evidence_sha256: crate::hash::sha256(
                    serde_json::json!([audit, receipt]).to_string().as_bytes(),
                ),
                receipt,
                reused_version,
            },
        );
    }
    if legacy {
        let mut stmt = c.prepare("SELECT publication_id,version_id,item_id,command_json,receipt_json FROM publication_batches WHERE task_id=?1").map_err(storage)?;
        let rows = stmt
            .query_map([task_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(storage)?;
        for row in rows {
            let (id, version, item, command, raw_receipt) = row.map_err(storage)?;
            let command_value: Value = serde_json::from_str(&command).map_err(integrity)?;
            let command: Publication =
                serde_json::from_value(command_value.clone()).map_err(integrity)?;
            let receipt: Receipt = serde_json::from_str(&raw_receipt).map_err(integrity)?;
            let record = read_record(c, &version, true, true)?;
            if receipt.publication_id != id
                || receipt.version_id != version
                || receipt.task_id != task_id
                || command.task_id != task_id
                || command.item_id != item
                || command.publication_id != id
                || command.version_id != version
                || task.task.owner_run_id.as_deref() != Some(&command.run_id)
                || !matches!((&command.intent, record.version.period),
                    (PublishIntent::Daily { .. }, Period::Day)
                    | (PublishIntent::HistoricalFixedBase | PublishIntent::CurrentHead { .. }, Period::Year))
                || receipt.task_revision <= command.expected_revision
                || receipt.task_revision > task.task.state_revision
                || found.contains_key(&item)
                || !task.items.iter().any(|i| {
                    i.spec.item_id == item
                        && i.stage == Stage::Finished
                        && i.outcome.as_ref().is_some_and(|o| o.success)
                        && i.spec.target == serde_json::json!({"symbol":record.version.symbol,"period":period(record.version.period)})
                        && i.spec.scope_keys.contains(&Scope::of(&record.version).occupancy_key())
                })
            {
                return Err(integrity(
                    "legacy publication/task receipt association differs",
                ));
            }
            // Force verified fixed-record evaluation; no latest lookup.
            if record.version.version_id != version {
                return Err(integrity("legacy fixed version differs"));
            }
            published += 1;
            found.insert(
                item,
                Evidence::Committed {
                    evidence_sha256: crate::hash::sha256(
                        serde_json::json!([record.raw, command_value, receipt])
                            .to_string()
                            .as_bytes(),
                    ),
                    receipt,
                    reused_version: false,
                },
            );
        }
    }
    // Orphan/extra batches cannot be relabelled as absence. Reuse has no
    // new batch; its fixed version and receipt were verified above.
    for table in ["publication_batches", "task_publications"] {
        let count: i64 = c
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE task_id=?1"),
                [task_id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if count != published {
            return Err(integrity(
                "task publication associations do not match execution receipts",
            ));
        }
    }
    let mut items = Vec::new();
    for item in &task.items {
        let evidence = found
            .remove(&item.spec.item_id)
            .unwrap_or(Evidence::Missing);
        if matches!(evidence, Evidence::Missing)
            && (item.stage == Stage::Finished && item.outcome.as_ref().is_some_and(|o| o.success))
        {
            return Err(integrity(
                "successful item lacks publication execution evidence",
            ));
        }
        items.push(ItemFact {
            item_id: item.spec.item_id.clone(),
            evidence,
        });
    }
    Ok(Facts { task, items })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_fact_snapshot_does_not_mix_a_concurrent_commit_into_old_task_state() {
        let (_dir, db, plan) = super::super::tests::setup();
        let id = plan.command.task_id.clone();
        let facts = db
            .read(|c| {
                let before = crate::sqlite_tasks::read_bound_task(c, &id)?.unwrap();
                // A real independent write transaction commits after the read view
                // is established but before its evidence enumeration.
                let writer = SqliteMarket::new(SqliteTasks::new(db.tasks.path()));
                writer.commit(&plan)?;
                let facts = read_facts(c, &id)?;
                assert_eq!(facts.task, before);
                assert!(matches!(facts.items[0].evidence, Evidence::Missing));
                Ok(facts)
            })
            .unwrap();
        assert!(!facts.task.claims.is_empty());
        let later = db.publication_facts(&id).unwrap();
        assert!(matches!(
            later.items[0].evidence,
            Evidence::Committed { .. }
        ));
        assert!(later.task.claims.is_empty());
    }
    #[test]
    fn publication_fact_snapshot_returns_verified_old_evidence_then_rejects_later_damage() {
        let (_dir, db, plan) = super::super::tests::setup();
        let receipt = db.commit(&plan).unwrap();
        let facts = db
            .read(|c| {
                crate::sqlite_tasks::read_bound_task(c, &receipt.task_id)?.unwrap();
                let writer = Connection::open(db.tasks.path()).unwrap();
                writer
                    .execute("UPDATE segment_bars SET close='999'", [])
                    .unwrap();
                read_facts(c, &receipt.task_id)
            })
            .unwrap();
        let Evidence::Committed {
            receipt: actual, ..
        } = &facts.items[0].evidence
        else {
            panic!("old snapshot receipt required")
        };
        assert_eq!(actual, &receipt);
        assert!(db.publication_facts(&receipt.task_id).is_err());
    }
    #[test]
    fn legacy_publication_evidence_rejects_changed_execution_owner_or_period_intent() {
        for wrong_owner in [false, true] {
            let (_dir, db, plan) = super::super::tests::setup();
            db.commit(&plan).unwrap();
            let mut command = plan.command.clone();
            if wrong_owner {
                command.run_id = "unrelated-writer".into();
            } else {
                command.intent = PublishIntent::HistoricalFixedBase;
            }
            let c = Connection::open(db.tasks.path()).unwrap();
            c.execute(
                "UPDATE publication_batches SET command_json=?1 WHERE publication_id=?2",
                params![
                    serde_json::to_string(&command).unwrap(),
                    command.publication_id
                ],
            )
            .unwrap();
            assert!(db.publication_facts(&command.task_id).is_err());
        }
    }
}
