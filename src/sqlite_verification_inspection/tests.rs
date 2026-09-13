use super::*;
use crate::{
    task_control::{
        Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
    task_verification::StopVerification,
    verification_inspection::{Conclusion, decision::prepare, service::inspect},
};
use serde_json::json;
fn fixture() -> (tempfile::TempDir, SqliteTasks, TaskSnapshot, TaskSnapshot) {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("inspect.sqlite"));
    db.initialize().unwrap();
    let original_control =
        TaskControl::new(db.clone(), "old".into(), vec![Box::new(LegacyCollection)]).unwrap();
    let RequestOutcome::Task(original) = original_control
        .accept(
            "original",
            LegacyCollection.key(),
            json!({"symbols":["sh.600006"],"periods":["day"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let control = TaskControl::new(
        db.clone(),
        "check-run".into(),
        vec![Box::new(
            StopVerification::new(db.clone(), "check-run".into()).unwrap(),
        )],
    )
    .unwrap();
    let RequestOutcome::Task(check) = control
        .accept(
            "check",
            crate::task_verification::command_key(),
            json!({"target_task_id":original.task.task_id}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    (dir, db, *original, *check)
}
fn receipt(a: &Acceptance) -> crate::verification_inspection::Receipt {
    let RequestOutcome::Terminal(value) = &a.record.outcome else {
        panic!("inspection must not create a task")
    };
    serde_json::from_value::<TerminalResult>(value.clone())
        .unwrap()
        .receipt()
        .clone()
}
fn count(db: &SqliteTasks, table: &str) -> i64 {
    Connection::open(db.path())
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn claimed(
    db: &SqliteTasks,
    check: &TaskSnapshot,
) -> crate::sqlite_verification_runtime::guarded::GuardedTasks {
    let tasks = crate::sqlite_verification_runtime::guarded::GuardedTasks::open(
        db.clone(),
        &check.task.task_id,
        "check-run",
    )
    .unwrap();
    tasks
        .transition(&check.task.task_id, 0, "check-run", Change::Claim)
        .unwrap();
    tasks
        .transition(
            &check.task.task_id,
            1,
            "check-run",
            Change::StartItem("stopped-writes".into()),
        )
        .unwrap();
    tasks
}
#[test]
fn never_started_inspection_is_terminal_request_and_only_releases_check_protection() {
    let (_dir, db, original, check) = fixture();
    let result = inspect(&db, "inspect", &check.task.task_id, "manager", "owner").unwrap();
    assert_eq!(receipt(&result).conclusion, Conclusion::Interrupted);
    assert_eq!(count(&db, "tasks"), 2);
    assert_eq!(count(&db, "requests"), 3);
    assert_eq!(count(&db, "verification_inspections"), 1);
    let ended = db.read_task(&check.task.task_id).unwrap().unwrap();
    assert_eq!(ended.task.status, Status::Interrupted);
    assert!(ended.claims.is_empty());
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
    assert!(
        db.transition(&check.task.task_id, 0, "check-run", Change::Claim)
            .is_err()
    );
    let replay = inspect(&db, "inspect", &check.task.task_id, "new-manager", "owner").unwrap();
    assert!(replay.reused);
    assert_eq!(replay.record, result.record);
    let finished = inspect(
        &db,
        "another-inspect",
        &check.task.task_id,
        "manager",
        "owner",
    )
    .unwrap();
    assert_eq!(receipt(&finished).conclusion, Conclusion::AlreadyFinished);
    assert_eq!(db.read_task(&check.task.task_id).unwrap(), Some(ended));
}
#[test]
fn active_registered_execution_is_not_stopped_and_insufficient_history_is_immutable() {
    let (_dir, db, original, check) = fixture();
    let tasks = claimed(&db, &check);
    let normal = inspect(&db, "normal", &check.task.task_id, "check-run", "owner").unwrap();
    assert_eq!(receipt(&normal).conclusion, Conclusion::NotApplicable);
    let insufficient =
        inspect(&db, "old-manager", &check.task.task_id, "manager", "owner").unwrap();
    assert_eq!(
        receipt(&insufficient).conclusion,
        Conclusion::InsufficientEvidence
    );
    let protected = db.read_task(&check.task.task_id).unwrap().unwrap();
    assert_eq!(protected.task.status, Status::NeedsVerification);
    assert!(!protected.claims.is_empty());
    drop(tasks);
    let replay = inspect(&db, "old-manager", &check.task.task_id, "manager2", "owner").unwrap();
    assert_eq!(replay.record, insufficient.record);
    assert_eq!(db.read_task(&check.task.task_id).unwrap(), Some(protected));
    let fresh = inspect(&db, "fresh", &check.task.task_id, "manager2", "owner").unwrap();
    assert_eq!(receipt(&fresh).conclusion, Conclusion::Interrupted);
    assert_eq!(
        inspect(&db, "old-manager", &check.task.task_id, "manager3", "owner")
            .unwrap()
            .record,
        insufficient.record
    );
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
}
#[test]
fn unregistered_started_execution_retains_protection_and_does_not_backfill_guard() {
    let (_dir, db, original, check) = fixture();
    db.transition(&check.task.task_id, 0, "check-run", Change::Claim)
        .unwrap();
    let result = inspect(&db, "inspect", &check.task.task_id, "manager", "owner").unwrap();
    assert_eq!(
        receipt(&result).conclusion,
        Conclusion::InsufficientEvidence
    );
    assert_eq!(count(&db, "verification_runtime_guards"), 0);
    assert!(
        !db.read_task(&check.task.task_id)
            .unwrap()
            .unwrap()
            .claims
            .is_empty()
    );
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
}
#[test]
fn stale_finish_and_stale_inspection_plans_cannot_commit() {
    let (_dir, db, original, check) = fixture();
    let tasks = claimed(&db, &check);
    let current = db.read_task(&check.task.task_id).unwrap().unwrap();
    let stop = crate::task_verification::decision::prepare(
        current,
        original.clone(),
        "check-run",
        "owner",
        chrono::Utc::now(),
        &crate::task_verification::decision::UnverifiedStopInspector,
    )
    .unwrap();
    drop(tasks);
    let facts = db.inspection_facts(&check.task.task_id).unwrap();
    let probe = db.probe_execution(&facts).unwrap();
    let plan = prepare(
        "stale",
        crate::verification_inspection::command(&check.task.task_id).unwrap(),
        facts,
        probe.observation.clone(),
        "manager",
        "owner",
        chrono::Utc::now(),
    )
    .unwrap();
    db.transition(&check.task.task_id, 2, "check-run", Change::RequestCancel)
        .unwrap();
    assert!(matches!(
        db.commit_inspection(&plan, &probe),
        Err(Error::RevisionConflict)
    ));
    assert!(db.read_request("stale").unwrap().is_none());
    drop(probe);
    inspect(&db, "fresh", &check.task.task_id, "manager", "owner").unwrap();
    use crate::task_verification::decision::StopRepository;
    assert!(matches!(
        db.commit_stop_check(&stop),
        Err(Error::RevisionConflict)
    ));
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
}
#[test]
fn inspection_two_connections_same_request_replay_and_different_target_conflicts() {
    use std::sync::{Arc, Barrier};
    let (_dir, db, original, check) = fixture();
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let db = db.clone();
            let target = check.task.task_id.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                inspect(&db, "same", &target, "manager", "owner")
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results[0].record, results[1].record);
    assert_eq!(count(&db, "verification_inspections"), 1);
    assert!(matches!(
        inspect(&db, "same", &original.task.task_id, "manager", "owner"),
        Err(Error::RequestConflict)
    ));
    assert!(matches!(
        inspect(&db, "check", &check.task.task_id, "manager", "owner"),
        Err(Error::RequestConflict)
    ));
}
#[test]
fn inspection_rejects_missing_and_corrupt_records_without_creating_alternatives() {
    let (_dir, db, _original, check) = fixture();
    assert!(matches!(
        inspect(&db, "missing", "absent", "manager", "owner"),
        Err(Error::TaskNotFound)
    ));
    assert!(db.read_request("missing").unwrap().is_none());
    inspect(&db, "inspect", &check.task.task_id, "manager", "owner").unwrap();
    let c = Connection::open(db.path()).unwrap();
    c.execute("UPDATE verification_inspections SET audit_sha256='bad'", [])
        .unwrap();
    assert!(matches!(
        db.read_request("inspect"),
        Err(Error::NeedsVerification(_))
    ));
    assert!(matches!(
        db.read_task(&check.task.task_id),
        Err(Error::NeedsVerification(_))
    ));
    assert!(inspect(&db, "new", &check.task.task_id, "manager", "owner").is_err());
    assert!(db.read_request("new").unwrap().is_none());
}
#[test]
fn inspection_missing_runtime_file_is_insufficient_not_stopped() {
    let (_dir, db, _original, check) = fixture();
    let tasks = claimed(&db, &check);
    drop(tasks);
    let registration = db
        .runtime_registration(&check.task.task_id)
        .unwrap()
        .unwrap();
    std::fs::remove_file(&registration.descriptor.lock_path).unwrap();
    let result = inspect(&db, "inspect", &check.task.task_id, "manager", "owner").unwrap();
    assert_eq!(
        receipt(&result).conclusion,
        Conclusion::InsufficientEvidence
    );
    assert!(!registration.descriptor.lock_path.exists());
    assert!(
        !db.read_task(&check.task.task_id)
            .unwrap()
            .unwrap()
            .claims
            .is_empty()
    );
}
#[test]
fn inspection_commit_child() {
    let Ok(path) = std::env::var("X_DATA_MCP_TEST_INSPECTION_DB") else {
        return;
    };
    let target = std::env::var("X_DATA_MCP_TEST_INSPECTION_TASK").unwrap();
    inspect(
        &SqliteTasks::new(path),
        "inspect",
        &target,
        "manager",
        "owner",
    )
    .unwrap();
    panic!("expected actual process exit at SQLite commit");
}
#[test]
fn inspection_actual_exit_is_atomic_and_lost_response_replays_without_duplicate_finish() {
    for point in ["before_commit", "after_commit"] {
        let (_dir, db, original, check) = fixture();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sqlite_verification_inspection::tests::inspection_commit_child",
                "--nocapture",
            ])
            .env("X_DATA_MCP_TEST_INSPECTION_DB", db.path())
            .env("X_DATA_MCP_TEST_INSPECTION_TASK", &check.task.task_id)
            .env("X_DATA_MCP_TEST_SQLITE_CRASH", point)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(78));
        if point == "before_commit" {
            assert!(db.read_request("inspect").unwrap().is_none());
            assert_eq!(
                db.read_task(&check.task.task_id).unwrap(),
                Some(check.clone())
            );
            assert_eq!(count(&db, "verification_inspections"), 0);
        } else {
            assert_eq!(count(&db, "verification_inspections"), 1);
        }
        let result = inspect(&db, "inspect", &check.task.task_id, "manager", "owner").unwrap();
        assert_eq!(result.reused, point == "after_commit");
        assert_eq!(receipt(&result).conclusion, Conclusion::Interrupted);
        assert_eq!(count(&db, "tasks"), 2);
        assert_eq!(count(&db, "verification_inspections"), 1);
        assert_eq!(
            db.read_task(&original.task.task_id).unwrap(),
            Some(original)
        );
    }
}

#[test]
fn inspection_storage_failure_busy_and_unknown_schema_do_not_consume_or_release() {
    let (_dir, db, _original, check) = fixture();
    let db = db.with_busy_timeout(std::time::Duration::from_millis(1));
    let c = Connection::open(db.path()).unwrap();
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(matches!(
        inspect(&db, "busy", &check.task.task_id, "manager", "owner"),
        Err(Error::StorageUnavailable(_))
    ));
    c.execute_batch("ROLLBACK").unwrap();
    assert!(db.read_request("busy").unwrap().is_none());
    c.execute_batch("CREATE TRIGGER reject_inspection BEFORE INSERT ON verification_inspections BEGIN SELECT RAISE(ABORT,'fixture rejected audit'); END;").unwrap();
    assert!(inspect(&db, "failed", &check.task.task_id, "manager", "owner").is_err());
    assert!(db.read_request("failed").unwrap().is_none());
    assert_eq!(
        db.read_task(&check.task.task_id).unwrap(),
        Some(check.clone())
    );
    c.execute_batch("DROP TRIGGER reject_inspection; PRAGMA user_version=999;")
        .unwrap();
    assert!(matches!(
        inspect(&db, "unknown", &check.task.task_id, "manager", "owner"),
        Err(Error::StorageUnavailable(_))
    ));
    c.pragma_update(None, "user_version", crate::sqlite_tasks::SCHEMA_VERSION)
        .unwrap();
    assert!(db.read_request("unknown").unwrap().is_none());
    assert_eq!(db.read_task(&check.task.task_id).unwrap(), Some(check));
    let dir = tempfile::tempdir().unwrap();
    let bad = SqliteTasks::new(dir.path().join("bad"));
    assert!(inspect(&bad, "absent", "target", "manager", "owner").is_err());
    assert!(!bad.path().exists());
    std::fs::write(bad.path(), b"not SQLite").unwrap();
    assert!(inspect(&bad, "bad", "target", "manager", "owner").is_err());
    assert_eq!(std::fs::read(bad.path()).unwrap(), b"not SQLite");
}
#[test]
fn inspection_rejects_missing_binding_occupancy_or_forged_serial_scope() {
    for fault in ["binding", "occupancy", "serial_scope"] {
        let (_dir, db, _original, check) = fixture();
        let c = Connection::open(db.path()).unwrap();
        match fault {
            "binding" => {
                c.execute(
                    "DELETE FROM requests WHERE task_id=?1",
                    [&check.task.task_id],
                )
                .unwrap();
            }
            "occupancy" => {
                c.execute(
                    "DELETE FROM scope_occupancy WHERE task_id=?1",
                    [&check.task.task_id],
                )
                .unwrap();
            }
            _ => {
                let mut work = check.task.work.clone();
                let scopes = ["wrong:business:resource".to_owned()].into_iter().collect();
                work.scopes = scopes;
                work.items[0].scope_keys = work.scopes.clone();
                c.execute(
                    "UPDATE tasks SET frozen_work_json=?1 WHERE task_id=?2",
                    params![serde_json::to_string(&work).unwrap(), check.task.task_id],
                )
                .unwrap();
                c.execute(
                    "UPDATE task_items SET scope_keys_json=?1 WHERE task_id=?2",
                    params![
                        serde_json::to_string(&work.scopes).unwrap(),
                        check.task.task_id
                    ],
                )
                .unwrap();
                c.execute(
                    "UPDATE task_scopes SET scope_key='wrong:business:resource' WHERE task_id=?1",
                    [&check.task.task_id],
                )
                .unwrap();
                c.execute("UPDATE scope_occupancy SET scope_key='wrong:business:resource' WHERE task_id=?1",[&check.task.task_id]).unwrap();
            }
        }
        assert!(
            matches!(
                inspect(&db, "rejected", &check.task.task_id, "manager", "owner"),
                Err(Error::NeedsVerification(_))
            ),
            "{fault}"
        );
        assert!(db.read_request("rejected").unwrap().is_none());
        assert_eq!(count(&db, "verification_inspections"), 0);
    }
}
#[test]
fn inspection_historical_request_rejects_missing_audit_or_orphan_binding() {
    for fault in ["audit", "binding", "terminal", "target", "command"] {
        let (_dir, db, _original, check) = fixture();
        inspect(&db, "inspect", &check.task.task_id, "manager", "owner").unwrap();
        let c = Connection::open(db.path()).unwrap();
        // Simulate out-of-band corruption. Production connections keep FK on.
        c.pragma_update(None, "foreign_keys", false).unwrap();
        match fault {
            "audit" => {
                c.execute("DELETE FROM verification_inspections", [])
                    .unwrap();
            }
            "binding" => {
                c.execute("DELETE FROM requests WHERE request_id='inspect'", [])
                    .unwrap();
            }
            "terminal" => {
                c.execute(
                    "UPDATE requests SET terminal_json='{}' WHERE request_id='inspect'",
                    [],
                )
                .unwrap();
            }
            "command" => {
                let command = crate::task_control::Command {
                    key: LegacyCollection.key(),
                    intent: json!({"symbols":["sh.600006"],"periods":["day"]}),
                };
                let raw = serde_json::to_string(&command).unwrap();
                c.execute("UPDATE requests SET command_json=?1,command_digest=?2 WHERE request_id='inspect'",params![raw,crate::hash::sha256(raw.as_bytes())]).unwrap();
            }
            _ => {
                c.execute(
                    "UPDATE verification_inspections SET target_task_id='other'",
                    [],
                )
                .unwrap();
            }
        }
        assert!(
            matches!(db.read_request("inspect"), Err(Error::NeedsVerification(_))),
            "{fault}"
        );
        assert!(inspect(&db, "inspect", &check.task.task_id, "manager", "owner").is_err());
    }
}
#[test]
fn verified_finished_result_is_not_rewritten_and_inspection_preserves_market_bytes() {
    use crate::task_verification::runner::StopRunner;
    let (_dir, db, original, check) = fixture();
    let market = crate::sqlite_market::SqliteMarket::new(db.clone());
    let samples: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/issue79/legacy-records.json"
    ))
    .unwrap();
    market.stage(&samples[0]["record"]).unwrap();
    fn market_rows(db: &SqliteTasks) -> Vec<Vec<String>> {
        let c = Connection::open(db.path()).unwrap();
        let mut all = vec![];
        for table in [
            "data_versions",
            "daily_bars",
            "yearly_bars",
            "version_evidence",
            "yearly_lineage",
            "effective_heads",
            "publication_batches",
            "task_publications",
        ] {
            let mut statement = c
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
                .unwrap();
            let columns = statement.column_count();
            all.extend(
                statement
                    .query_map([], |row| {
                        Ok((0..columns)
                            .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                            .collect::<Vec<_>>())
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
            );
        }
        all
    }
    let before = market_rows(&db);
    let runner = StopRunner {
        repository: crate::sqlite_verification_runtime::guarded::GuardedTasks::open(
            db.clone(),
            &check.task.task_id,
            "check-run",
        )
        .unwrap(),
        run_id: "check-run".into(),
        actor: "owner".into(),
    };
    runner
        .run(
            &check.task.task_id,
            &crate::task_verification::decision::UnverifiedStopInspector,
        )
        .unwrap();
    drop(runner);
    let finished = db.read_task(&check.task.task_id).unwrap().unwrap();
    let result = inspect(&db, "inspect", &check.task.task_id, "manager", "owner").unwrap();
    assert_eq!(receipt(&result).conclusion, Conclusion::AlreadyFinished);
    assert_eq!(db.read_task(&check.task.task_id).unwrap(), Some(finished));
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
    assert_eq!(market_rows(&db), before);
}

#[test]
fn active_inspection_execution_child() {
    use std::io::Write;
    let Ok(path) = std::env::var("X_DATA_MCP_TEST_INSPECTION_ACTIVE_DB") else {
        return;
    };
    let target = std::env::var("X_DATA_MCP_TEST_INSPECTION_ACTIVE_TASK").unwrap();
    let db = SqliteTasks::new(path);
    let check = db.read_task(&target).unwrap().unwrap();
    let _tasks = claimed(&db, &check);
    println!("EXECUTION_READY");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    // Bypass Rust Drop. Only actual process exit releases the kernel guard.
    std::process::exit(106);
}
#[test]
fn actual_live_execution_is_protected_and_only_actual_exit_allows_check_finish() {
    use std::io::{BufRead, BufReader, Write};
    let (_dir, db, original, check) = fixture();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sqlite_verification_inspection::tests::active_inspection_execution_child",
            "--nocapture",
        ])
        .env("X_DATA_MCP_TEST_INSPECTION_ACTIVE_DB", db.path())
        .env(
            "X_DATA_MCP_TEST_INSPECTION_ACTIVE_TASK",
            &check.task.task_id,
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut ready = false;
    for _ in 0..6 {
        let mut line = String::new();
        if output.read_line(&mut line).unwrap() == 0 {
            break;
        }
        if line.trim() == "EXECUTION_READY" {
            ready = true;
            break;
        }
    }
    assert!(ready);
    assert!(child.try_wait().unwrap().is_none());
    let live = inspect(&db, "live", &check.task.task_id, "manager", "owner").unwrap();
    assert_eq!(receipt(&live).conclusion, Conclusion::InsufficientEvidence);
    assert!(
        !db.read_task(&check.task.task_id)
            .unwrap()
            .unwrap()
            .claims
            .is_empty()
    );
    writeln!(child.stdin.as_mut().unwrap(), "exit").unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(106));
    let after = inspect(&db, "after-exit", &check.task.task_id, "manager", "owner").unwrap();
    assert_eq!(receipt(&after).conclusion, Conclusion::Interrupted);
    assert_eq!(
        inspect(&db, "live", &check.task.task_id, "manager", "owner")
            .unwrap()
            .record,
        live.record
    );
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
}

#[test]
fn application_rechecks_a_changed_sqlite_snapshot_once_without_reusing_old_plan() {
    use crate::task_control::{Command, FrozenWork, RequestRecord, RetryGuard};
    struct Racing {
        db: SqliteTasks,
        target: String,
        calls: std::cell::Cell<usize>,
        twice: bool,
    }
    impl TaskRepository for Racing {
        fn read_request(&self, id: &str) -> Result<Option<RequestRecord>, Error> {
            self.db.read_request(id)
        }
        fn read_task(&self, id: &str) -> Result<Option<TaskSnapshot>, Error> {
            self.db.read_task(id)
        }
        fn accept(
            &self,
            _: &str,
            _: &Command,
            _: &FrozenWork,
            _: &str,
            _: Option<&RetryGuard>,
        ) -> Result<Acceptance, Error> {
            panic!("inspection must not accept async work")
        }
        fn transition(&self, _: &str, _: u64, _: &str, _: Change) -> Result<TaskSnapshot, Error> {
            panic!("inspection must use its sealed conditional plan")
        }
        fn candidates(&self, _: &str) -> Result<Vec<TaskSnapshot>, Error> {
            panic!("inspection must not dispatch work")
        }
        fn reconcile(&self, _: &str) -> Result<usize, Error> {
            panic!("inspection must not reconcile unrelated tasks")
        }
    }
    impl Repository for Racing {
        fn inspection_facts(&self, id: &str) -> Result<Facts, Error> {
            self.db.inspection_facts(id)
        }
        fn probe_execution(&self, f: &Facts) -> Result<Probe, Error> {
            self.db.probe_execution(f)
        }
        fn commit_inspection(&self, p: &Plan, probe: &Probe) -> Result<Acceptance, Error> {
            let call = self.calls.get() + 1;
            self.calls.set(call);
            assert!(call <= 2);
            if call == 1 {
                self.db
                    .transition(&self.target, 0, "check-run", Change::RequestCancel)?;
            } else if self.twice {
                self.db
                    .transition(&self.target, 1, "manager", Change::Reconcile)?;
            }
            self.db.commit_inspection(p, probe)
        }
    }
    for twice in [false, true] {
        let (_dir, db, original, check) = fixture();
        let repo = Racing {
            db: db.clone(),
            target: check.task.task_id.clone(),
            calls: std::cell::Cell::new(0),
            twice,
        };
        let result = inspect(&repo, "inspect", &check.task.task_id, "manager", "owner");
        assert_eq!(repo.calls.get(), 2);
        if twice {
            assert!(matches!(result, Err(Error::RevisionConflict)));
            assert!(db.read_request("inspect").unwrap().is_none());
        } else {
            assert_eq!(
                receipt(&result.unwrap()).conclusion,
                Conclusion::InsufficientEvidence
            );
        }
        assert!(
            !db.read_task(&check.task.task_id)
                .unwrap()
                .unwrap()
                .claims
                .is_empty()
        );
        assert_eq!(
            db.read_task(&original.task.task_id).unwrap(),
            Some(original)
        );
    }
}

#[test]
fn inspection_v8_requires_explicit_upgrade_and_preserves_existing_task_binding() {
    let (_dir, db, original, check) = fixture();
    let c = Connection::open(db.path()).unwrap();
    c.execute_batch("DROP TABLE verification_inspections; DROP TABLE verification_runtime_guards; DELETE FROM schema_migrations WHERE version=9; PRAGMA user_version=8;").unwrap();
    assert!(matches!(
        inspect(&db, "inspect", &check.task.task_id, "manager", "owner"),
        Err(Error::StorageUnavailable(_))
    ));
    let version: u32 = c
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 8);
    db.upgrade().unwrap();
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
    assert_eq!(
        db.read_task(&check.task.task_id).unwrap(),
        Some(check.clone())
    );
    assert!(db.read_request("inspect").unwrap().is_none());
    assert_eq!(count(&db, "verification_inspections"), 0);
    inspect(&db, "inspect", &check.task.task_id, "manager", "owner").unwrap();
    db.upgrade().unwrap();
    assert_eq!(count(&db, "verification_inspections"), 1);
}
#[test]
fn inspection_two_connections_different_parameters_cannot_share_one_global_request() {
    use std::sync::{Arc, Barrier};
    let (_dir, db, original, check) = fixture();
    let barrier = Arc::new(Barrier::new(2));
    let handles = [original.task.task_id, check.task.task_id]
        .into_iter()
        .map(|target| {
            let db = db.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                inspect(&db, "same", &target, "manager", "owner")
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::RequestConflict)))
            .count(),
        1
    );
    assert_eq!(count(&db, "verification_inspections"), 1);
    assert_eq!(count(&db, "tasks"), 2);
}
#[test]
fn inspection_facts_use_one_snapshot_when_claim_and_registration_commit_concurrently() {
    let (_dir, db, _original, check) = fixture();
    let guard =
        crate::verification_runtime::create(db.path(), &check.task.task_id, "check-run").unwrap();
    let claim = crate::verification_execution::prepare_claim(
        check.clone(),
        guard.descriptor().clone(),
        chrono::Utc::now(),
    )
    .unwrap();
    let facts = db
        .read(|c| {
            assert_eq!(
                read_bound_task(c, &check.task.task_id)?,
                Some(check.clone())
            );
            db.claim_with_runtime_guard(&claim, &guard)?;
            read_facts(c, &check.task.task_id)
        })
        .unwrap();
    assert_eq!(facts.target, check);
    assert!(facts.registration.is_none());
    let plan = prepare(
        "stale",
        crate::verification_inspection::command(&check.task.task_id).unwrap(),
        facts,
        Observation::NotNeeded,
        "manager",
        "owner",
        chrono::Utc::now(),
    )
    .unwrap();
    assert!(matches!(
        db.commit_inspection(
            &plan,
            &Probe {
                observation: Observation::NotNeeded,
                guard: None
            }
        ),
        Err(Error::RevisionConflict)
    ));
    assert!(db.read_request("stale").unwrap().is_none());
    let result = inspect(&db, "fresh", &check.task.task_id, "manager", "owner").unwrap();
    assert_eq!(
        receipt(&result).conclusion,
        Conclusion::InsufficientEvidence
    );
}

#[test]
fn publication_verification_inspection_checks_its_own_finish_without_deciding_original_publication()
{
    use crate::publication_verification::{
        admission::{PublicationVerification, command_key},
        runner::Runner,
    };
    for completed in [false, true] {
        let (_dir, db, original, stop_check) = fixture();
        db.transition(
            &stop_check.task.task_id,
            0,
            "check-run",
            Change::RequestCancel,
        )
        .unwrap();
        db.transition(
            &stop_check.task.task_id,
            1,
            "check-run",
            Change::FinishCancellation,
        )
        .unwrap();
        let mut original = db
            .transition(&original.task.task_id, 0, "old", Change::Claim)
            .unwrap();
        original = db
            .transition(
                &original.task.task_id,
                1,
                "old",
                Change::StartItem(original.items[0].spec.item_id.clone()),
            )
            .unwrap();
        let control = TaskControl::new(
            db.clone(),
            "check-run".into(),
            vec![Box::new(
                PublicationVerification::new(db.clone(), "check-run".into()).unwrap(),
            )],
        )
        .unwrap();
        let RequestOutcome::Task(check) = control
            .accept(
                "publication-check",
                command_key(),
                json!({"target_task_id":original.task.task_id}),
            )
            .unwrap()
            .record
            .outcome
        else {
            panic!()
        };
        {
            let tasks = crate::sqlite_verification_runtime::guarded::GuardedTasks::open(
                db.clone(),
                &check.task.task_id,
                "check-run",
            )
            .unwrap();
            if completed {
                Runner {
                    tasks,
                    evidence: crate::sqlite_market::SqliteMarket::new(db.clone()),
                    run_id: "check-run".into(),
                    actor: "owner".into(),
                }
                .run(&check.task.task_id)
                .unwrap();
            } else {
                tasks
                    .transition(&check.task.task_id, 0, "check-run", Change::Claim)
                    .unwrap();
                tasks
                    .transition(
                        &check.task.task_id,
                        1,
                        "check-run",
                        Change::StartItem("publication-results".into()),
                    )
                    .unwrap();
            }
        }
        let result = inspect(
            &db,
            "inspect-publication",
            &check.task.task_id,
            "check-run",
            "owner",
        )
        .unwrap();
        assert_eq!(
            receipt(&result).conclusion,
            if completed {
                Conclusion::AlreadyFinished
            } else {
                Conclusion::Interrupted
            }
        );
        assert_eq!(
            db.read_task(&original.task.task_id).unwrap(),
            Some(original)
        );
        assert_eq!(count(&db, "publication_batches"), 0);
        assert_eq!(count(&db, "task_publications"), 0);
        assert_eq!(count(&db, "effective_heads"), 0);
    }
}
