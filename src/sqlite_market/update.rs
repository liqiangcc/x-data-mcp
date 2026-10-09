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
                    "SELECT EXISTS(SELECT 1 FROM publication_batches WHERE publication_id=?1 UNION ALL SELECT 1 FROM yearly_execution_results WHERE result_id=?1)",
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
                let mut binding = crate::sqlite_tasks::read_item_binding(
                    c,
                    &command.task_id,
                    &command.item_id,
                )?
                .ok_or(Error::Task(crate::task_control::Error::TaskNotFound))?;
                plan.publication.finish_task(&mut binding)?;
                crate::sqlite_tasks::persist_decided(
                    c,
                    &binding.head,
                    Some(&binding.item),
                    command.expected_revision,
                )?;
                Receipt {
                    publication_id: command.publication_id.clone(),
                    version_id: command.version_id.clone(),
                    task_id: command.task_id.clone(),
                    task_revision: binding.head.state_revision,
                }
            } else {
                commit_publication(c, &plan.publication)?
            };
            crate::sqlite_market::envelope::write_payloads(c, &plan.audit_payloads)?;
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
    fn item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| {
            let id: Option<String> = c
                .query_row(
                    "SELECT result_id FROM market_update_results WHERE task_id=?1 AND item_id=?2",
                    params![task, item],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            id.map(|id| {
                read_result(c, &id)?
                    .map(|(_, r)| r)
                    .ok_or_else(|| integrity("range item receipt missing"))
            })
            .transpose()
        })
    }
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
    fn range_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| read_result(c, id).map(|r| r.map(|(_, receipt)| receipt)))
    }
    fn cover_gap(&self, covering: &crate::market_storage::GapCovering) -> Result<bool, Error> {
        SqliteMarket::cover_gap(self, covering)
    }
    fn gap_coverings(
        &self,
        gap_id: &str,
    ) -> Result<Vec<crate::market_storage::GapCovering>, Error> {
        SqliteMarket::gap_coverings(self, gap_id)
    }
}
pub(crate) fn read_result(c: &Connection, id: &str) -> Result<Option<(Value, Receipt)>, Error> {
    read_result_ctx(c, &mut super::compact_versions::VerifyCtx::default(), id)
}

/// `read_result` sharing the caller's transaction-scoped verified-facts
/// context so repeated immutable-version verification deduplicates.
pub(crate) fn read_result_ctx(
    c: &Connection,
    ctx: &mut super::compact_versions::VerifyCtx,
    id: &str,
) -> Result<Option<(Value, Receipt)>, Error> {
    if super::is_compact_schema(c)? {
        return super::compact_results::read_update_audit_ctx(c, ctx, id);
    }
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
    crate::sqlite_market::envelope::require_refs(c, &audit["segment"])?;
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
    let binding = crate::sqlite_tasks::read_item_binding(c, &task_id, &item_id)?
        .ok_or_else(|| integrity("range result task missing"))?;
    if binding.head.state_revision < receipt.task_revision
        || receipt.task_revision <= command.expected_revision
        || binding.item.stage != crate::task_control::Stage::Finished
        || !binding
            .item
            .outcome
            .as_ref()
            .is_some_and(|outcome| outcome.success)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        market_data::MarketDataBatch,
        market_update::{
            DateRange, Evidence,
            publication::{CombinedEvidence, Preparation},
        },
        task_control::{
            Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
            service::TaskControl,
        },
    };
    fn approve(db: &SqliteMarket, command: Publication, changed: bool) -> Box<RangePlan> {
        let base = db.fixed("legacy-daily").unwrap();
        let mut batch: MarketDataBatch = serde_json::from_value(base.raw["batch"].clone()).unwrap();
        batch.bars = vec![batch.bars[1].clone()];
        if changed {
            batch.bars[0].close = "10.06".parse().unwrap();
        }
        let range = DateRange {
            start: batch.bars[0].date,
            end: batch.bars[0].date,
        };
        let evidence = Evidence {
            base_version_id: "legacy-daily".into(),
            base_integrity_sha256: base.raw["integrity_sha256"].as_str().unwrap().into(),
            candidate_sha256: crate::market_update::candidate_digest(&batch).unwrap(),
            range: range.clone(),
            same_qfq_basis_ref: "fixture:crash-basis".into(),
            revision_ref: Some("fixture:crash-revision".into()),
            continuity: None,
        };
        let whole = changed.then(|| crate::market_update::runner::CombinedCandidate {
            record: db.candidate(&command.version_id).unwrap().raw,
            evidence: CombinedEvidence {
                base_integrity_sha256: evidence.base_integrity_sha256.clone(),
                segment_sha256: evidence.candidate_sha256.clone(),
                combined_integrity_sha256:
                    db.candidate(&command.version_id).unwrap().raw["integrity_sha256"]
                        .as_str()
                        .unwrap()
                        .into(),
                complete_coverage_ref: "fixture:crash-whole".into(),
            },
        });
        let Preparation::Ready(plan) = (Publisher {
            repository: db.clone(),
        })
        .prepare_range(command, &batch, &range, &evidence, whole.as_ref())
        .unwrap() else {
            panic!()
        };
        plan
    }
    #[test]
    fn range_crash_child() {
        let Ok(path) = std::env::var("X_DATA_MCP_TEST_RANGE_PATH") else {
            return;
        };
        let command =
            serde_json::from_str(&std::env::var("X_DATA_MCP_TEST_RANGE_COMMAND").unwrap()).unwrap();
        let changed = std::env::var("X_DATA_MCP_TEST_RANGE_CHANGED").unwrap() == "true";
        let db = SqliteMarket::new(SqliteTasks::new(path));
        let plan = approve(&db, command, changed);
        db.commit_range(&plan).unwrap();
        panic!("crash not reached");
    }
    #[test]
    fn range_real_exit_before_commit_and_lost_response_after_commit() {
        for changed in [false, true] {
            for point in ["before_commit", "after_commit"] {
                let (_dir, db, first) = super::super::tests::setup();
                db.commit(&first).unwrap();
                let version = if changed {
                    "range-changed"
                } else {
                    "legacy-daily"
                };
                if changed {
                    let mut raw = db.fixed("legacy-daily").unwrap().raw;
                    raw["version_id"] = serde_json::json!(version);
                    raw["records"][1]["close"] = serde_json::json!("10.06");
                    raw["batch"]["bars"][1]["close"] = serde_json::json!("10.06");
                    raw["integrity_sha256"] =
                        serde_json::json!(compat::legacy_digest(&raw).unwrap());
                    db.stage(&raw).unwrap();
                }
                let ctl = TaskControl::new(
                    db.tasks.clone(),
                    "run".into(),
                    vec![Box::new(LegacyCollection)],
                )
                .unwrap();
                let request = ctl
                    .accept(
                        "range",
                        LegacyCollection.key(),
                        serde_json::json!({"symbols":["sh.600006"],"periods":["day"]}),
                    )
                    .unwrap();
                let RequestOutcome::Task(task) = request.record.outcome else {
                    panic!()
                };
                let task = db
                    .tasks
                    .transition(&task.task.task_id, 0, "run", Change::Claim)
                    .unwrap();
                let task = db
                    .tasks
                    .transition(
                        &task.task.task_id,
                        task.task.state_revision,
                        "run",
                        Change::StartItem(task.items[0].spec.item_id.clone()),
                    )
                    .unwrap();
                let command = Publication {
                    publication_id: "range-result".into(),
                    version_id: version.into(),
                    task_id: task.task.task_id.clone(),
                    item_id: task.items[0].spec.item_id.clone(),
                    run_id: "run".into(),
                    expected_revision: task.task.state_revision,
                    intent: PublishIntent::Daily {
                        expected_head: Some("legacy-daily".into()),
                    },
                };
                let plan = approve(&db, command.clone(), changed);
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "sqlite_market::update::tests::range_crash_child",
                        "--nocapture",
                    ])
                    .env("X_DATA_MCP_TEST_RANGE_PATH", db.tasks.path())
                    .env("X_DATA_MCP_TEST_RANGE_COMMAND", text(&command).unwrap())
                    .env("X_DATA_MCP_TEST_RANGE_CHANGED", changed.to_string())
                    .env("X_DATA_MCP_TEST_MARKET_CRASH", point)
                    .status()
                    .unwrap();
                assert_eq!(status.code(), Some(79));
                let committed = point == "after_commit";
                assert_eq!(
                    db.range_receipt("range-result").unwrap().is_some(),
                    committed
                );
                let connection = Connection::open(db.tasks.path()).unwrap();
                let publications: i64 = connection
                    .query_row("SELECT count(*) FROM publication_batches", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(publications, 1 + i64::from(changed && committed));
                let results: i64 = connection
                    .query_row("SELECT count(*) FROM market_update_results", [], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert_eq!(results, i64::from(committed));
                if !committed {
                    assert_eq!(db.task(&task.task.task_id).unwrap(), task);
                }
                let receipt = db.commit_range(&plan).unwrap();
                assert_eq!(db.commit_range(&plan).unwrap(), receipt);
                assert!(db.task(&task.task.task_id).unwrap().claims.is_empty());
                let versions: i64 = connection
                    .query_row("SELECT count(*) FROM data_versions", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(versions, if changed { 2 } else { 1 });
            }
        }
    }
}
