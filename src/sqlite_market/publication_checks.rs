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

use crate::publication_verification::existing::{KnownPlan, KnownRepository, TerminalResult};
fn task_error(error: Error) -> crate::task_control::Error {
    match error {
        Error::Task(e) => e,
        Error::Storage(s) => crate::task_control::Error::StorageUnavailable(s),
        Error::OutcomeUnknown(s) => crate::task_control::Error::OutcomeUnknown(s),
        other => crate::task_control::Error::NeedsVerification(other.to_string()),
    }
}
pub(crate) fn validate_terminal(
    c: &Connection,
    command: &crate::task_control::Command,
    value: &Value,
) -> Result<(), crate::task_control::Error> {
    if command.key != crate::publication_verification::admission::command_key() {
        return Ok(());
    }
    let result: TerminalResult =
        serde_json::from_value(value.clone()).map_err(|e| task_error(integrity(e)))?;
    let receipt = result.receipt();
    if receipt.conclusion != Conclusion::AllRecorded
        || command.intent != serde_json::json!({"target_task_id":receipt.target_task_id})
    {
        return Err(task_error(integrity(
            "known publication request intent/conclusion differs",
        )));
    }
    if read_receipt(c, &receipt.verification_task_id)
        .map_err(task_error)?
        .map(|(_, r)| r)
        .as_ref()
        != Some(receipt)
    {
        return Err(task_error(integrity(
            "known publication receipt missing or changed",
        )));
    }
    Ok(())
}
impl KnownRepository for SqliteTasks {
    fn known_publication(
        &self,
        target: &str,
    ) -> Result<Option<CheckReceipt>, crate::task_control::Error> {
        self.read(|c| {
            let mut stmt=c.prepare("SELECT verification_task_id AS id FROM publication_checks WHERE target_task_id=?1 UNION SELECT t.task_id FROM tasks t JOIN task_items i ON i.task_id=t.task_id WHERE t.status='succeeded' AND json_extract(t.command_key_json,'$.operation')='verify_publication_result' AND json_extract(i.target_json,'$.task_id')=?1 ORDER BY id").map_err(|e|task_error(storage(e)))?;
            let ids=stmt.query_map([target],|r|r.get::<_,String>(0)).map_err(|e|task_error(storage(e)))?;
            for id in ids {
                let id=id.map_err(|e|task_error(storage(e)))?;
                if let Some((_,receipt))=read_receipt(c,&id).map_err(task_error)? {
                    if receipt.target_task_id!=target {return Err(task_error(integrity("known check targets another task")));}
                    if receipt.conclusion==Conclusion::AllRecorded {return Ok(Some(receipt));}
                }
            }
            Ok(None)
        })
    }
    fn bind_known_publication(
        &self,
        plan: &KnownPlan,
    ) -> Result<crate::task_control::Acceptance, crate::task_control::Error> {
        use crate::task_control::{Acceptance, Error as TaskError, RequestOutcome, RequestRecord};
        self.write(|c| {
            if let Some(record) = crate::sqlite_tasks::read_request(c, &plan.request_id)? {
                if record.command != plan.command {
                    return Err(TaskError::RequestConflict);
                }
                return Ok(Acceptance {
                    record,
                    reused: true,
                });
            }
            if crate::sqlite_tasks::read_bound_task(c, &plan.target.task.task_id)?.as_ref()
                != Some(&plan.target)
            {
                return Err(TaskError::RevisionConflict);
            }
            let value = serde_json::to_value(&plan.result).map_err(|e| task_error(integrity(e)))?;
            validate_terminal(c, &plan.command, &value)?;
            let command =
                serde_json::to_string(&plan.command).map_err(|e| task_error(integrity(e)))?;
            let raw = serde_json::to_string(&value).map_err(|e| task_error(integrity(e)))?;
            c.execute(
                "INSERT INTO requests VALUES(?1,?2,?3,'terminal',NULL,?4)",
                params![
                    plan.request_id,
                    command,
                    crate::hash::sha256(command.as_bytes()),
                    raw
                ],
            )
            .map_err(|e| task_error(storage(e)))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publication_verification::{
        EvidenceRepository,
        admission::{PublicationVerification, command_key},
        decision::prepare,
    };
    use crate::task_control::{Change, RequestOutcome, TaskRepository, service::TaskControl};
    #[test]
    fn crash_child() {
        let Ok(path) = std::env::var("X_DATA_MCP_TEST_CHECK_PATH") else {
            return;
        };
        let audit: Audit =
            serde_json::from_str(&std::env::var("X_DATA_MCP_TEST_CHECK_AUDIT").unwrap()).unwrap();
        validate_audit(&audit).unwrap();
        SqliteMarket::new(SqliteTasks::new(path))
            .commit_check(&Plan { audit })
            .unwrap();
        panic!("expected real process exit");
    }
    #[test]
    fn publication_check_real_exit_keeps_audit_and_completion_atomic_without_target_mutation() {
        for published in [false, true] {
            for point in ["before_commit", "after_commit"] {
                let (_dir, db, publication) = super::super::tests::setup();
                let id = publication.command.task_id.clone();
                let control = TaskControl::new(
                    db.tasks.clone(),
                    "check-run".into(),
                    vec![Box::new(
                        PublicationVerification::new(db.tasks.clone(), "check-run".into()).unwrap(),
                    )],
                )
                .unwrap();
                let RequestOutcome::Task(check) = control
                    .accept(
                        "inspect",
                        command_key(),
                        serde_json::json!({"target_task_id":id}),
                    )
                    .unwrap()
                    .record
                    .outcome
                else {
                    panic!()
                };
                let check = db
                    .tasks
                    .transition(&check.task.task_id, 0, "check-run", Change::Claim)
                    .unwrap();
                let check = db
                    .tasks
                    .transition(
                        &check.task.task_id,
                        check.task.state_revision,
                        "check-run",
                        Change::StartItem("publication-results".into()),
                    )
                    .unwrap();
                if published {
                    db.commit(&publication).unwrap();
                }
                let original = db.task(&id).unwrap();
                let plan = prepare(
                    check.clone(),
                    db.publication_facts(&id).unwrap(),
                    "check-run",
                    "test-owner",
                    chrono::Utc::now(),
                )
                .unwrap();
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "sqlite_market::publication_checks::tests::crash_child",
                        "--nocapture",
                    ])
                    .env("X_DATA_MCP_TEST_CHECK_PATH", db.tasks.path())
                    .env(
                        "X_DATA_MCP_TEST_CHECK_AUDIT",
                        serde_json::to_string(&plan.audit).unwrap(),
                    )
                    .env("X_DATA_MCP_TEST_MARKET_CRASH", point)
                    .status()
                    .unwrap();
                assert_eq!(status.code(), Some(79));
                let reopened = SqliteMarket::new(SqliteTasks::new(db.tasks.path()));
                let committed = point == "after_commit";
                assert_eq!(
                    reopened
                        .check_receipt(&check.task.task_id)
                        .unwrap()
                        .is_some(),
                    committed
                );
                assert_eq!(reopened.task(&id).unwrap(), original);
                assert_eq!(
                    reopened
                        .tasks
                        .read_task(&check.task.task_id)
                        .unwrap()
                        .unwrap(),
                    if committed {
                        plan.finished_check().clone()
                    } else {
                        check.clone()
                    }
                );
                assert_eq!(reopened.commit_check(&plan).unwrap(), *plan.receipt());
                assert_eq!(reopened.commit_check(&plan).unwrap(), *plan.receipt());
                assert_eq!(reopened.task(&id).unwrap(), original);
            }
        }
    }
    #[test]
    fn known_crash_child() {
        let Ok(path) = std::env::var("X_DATA_MCP_TEST_KNOWN_CHECK_PATH") else {
            return;
        };
        let target = std::env::var("X_DATA_MCP_TEST_KNOWN_CHECK_TARGET").unwrap();
        let db = SqliteTasks::new(path);
        let control = TaskControl::new(
            db.clone(),
            "check-run".into(),
            vec![Box::new(
                PublicationVerification::new(db, "check-run".into()).unwrap(),
            )],
        )
        .unwrap();
        crate::publication_verification::existing::accept(&control, "known-exit-request", &target)
            .unwrap();
        panic!("expected request commit process exit");
    }
    #[test]
    fn known_publication_real_exit_binds_only_after_commit_and_replays_without_new_check() {
        for point in ["before_commit", "after_commit"] {
            let (_dir, db, publication) = super::super::tests::setup();
            let target = publication.command.task_id.clone();
            let control = TaskControl::new(
                db.tasks.clone(),
                "check-run".into(),
                vec![Box::new(
                    PublicationVerification::new(db.tasks.clone(), "check-run".into()).unwrap(),
                )],
            )
            .unwrap();
            let RequestOutcome::Task(check) = control
                .accept(
                    "check-before-publish",
                    command_key(),
                    serde_json::json!({"target_task_id":target}),
                )
                .unwrap()
                .record
                .outcome
            else {
                panic!()
            };
            db.commit(&publication).unwrap();
            crate::publication_verification::runner::Runner {
                tasks: db.tasks.clone(),
                evidence: db.clone(),
                run_id: "check-run".into(),
                actor: "test-owner".into(),
            }
            .run(&check.task.task_id)
            .unwrap();
            let before = db.task(&target).unwrap();
            let receipt = db.check_receipt(&check.task.task_id).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sqlite_market::publication_checks::tests::known_crash_child",
                    "--nocapture",
                ])
                .env("X_DATA_MCP_TEST_KNOWN_CHECK_PATH", db.tasks.path())
                .env("X_DATA_MCP_TEST_KNOWN_CHECK_TARGET", &target)
                .env("X_DATA_MCP_TEST_SQLITE_CRASH", point)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(78));
            let reopened = SqliteTasks::new(db.tasks.path());
            assert_eq!(
                reopened
                    .read_request("known-exit-request")
                    .unwrap()
                    .is_some(),
                point == "after_commit"
            );
            let actual = crate::publication_verification::existing::accept(
                &control,
                "known-exit-request",
                &target,
            )
            .unwrap();
            let replay = crate::publication_verification::existing::accept(
                &control,
                "known-exit-request",
                &target,
            )
            .unwrap();
            assert_eq!(actual.record, replay.record);
            assert!(replay.reused);
            assert_eq!(db.task(&target).unwrap(), before);
            assert_eq!(db.check_receipt(&check.task.task_id).unwrap(), receipt);
            let c = Connection::open(db.tasks.path()).unwrap();
            assert_eq!(
                c.query_row("SELECT count(*) FROM publication_checks", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(c.query_row("SELECT count(*) FROM tasks WHERE json_extract(command_key_json,'$.operation')='verify_publication_result'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        }
    }
}
