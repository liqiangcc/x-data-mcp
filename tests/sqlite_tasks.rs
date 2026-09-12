//! #78 acceptance evidence uses real temporary SQLite databases, never a fake repository.
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use x_data_mcp::{
    sqlite_tasks::SqliteTasks,
    task_control::{legacy::LegacyCollection, service::TaskControl, *},
};

fn database() -> (TempDir, SqliteTasks) {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("tasks.sqlite"));
    db.initialize().unwrap();
    (dir, db)
}
fn control(db: &SqliteTasks, run: &str) -> TaskControl<SqliteTasks> {
    TaskControl::new(db.clone(), run.into(), vec![Box::new(LegacyCollection)]).unwrap()
}
fn payload(symbols: &[&str], periods: &[&str]) -> Value {
    json!({"symbols":symbols,"periods":periods})
}
fn accept(db: &SqliteTasks, id: &str, symbols: &[&str]) -> Acceptance {
    control(db, "run-a")
        .accept(id, LegacyCollection.key(), payload(symbols, &["day"]))
        .unwrap()
}
fn snapshot(a: Acceptance) -> TaskSnapshot {
    match a.record.outcome {
        RequestOutcome::Task(s) => *s,
        _ => panic!("expected task"),
    }
}
fn count(db: &SqliteTasks, table: &str) -> i64 {
    Connection::open(db.path())
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
struct Complete(bool);
impl Executor for Complete {
    fn execute(&self, _: &CommandKey, _: &ItemSpec) -> ItemOutcome {
        ItemOutcome {
            success: self.0,
            error: (!self.0).then(|| "test failure".into()),
        }
    }
}

#[test]
fn normalized_compound_request_is_one_task_and_complete_intent_controls_replay() {
    let (_dir, db) = database();
    let ctl = control(&db, "run-a");
    let first = ctl
        .accept(
            "r",
            LegacyCollection.key(),
            payload(&["b", "a", "a"], &["year", "day", "day"]),
        )
        .unwrap();
    let id = snapshot(first).task.task_id;
    let replay = ctl
        .accept(
            "r",
            LegacyCollection.key(),
            json!({"symbols":["a","b"],"periods":["day","year"],"adjustment":"qfq"}),
        )
        .unwrap();
    assert!(replay.reused);
    assert_eq!(snapshot(replay).task.task_id, id);
    assert_eq!(count(&db, "requests"), 1);
    assert_eq!(count(&db, "tasks"), 1);
    assert_eq!(count(&db, "task_items"), 4);
    assert_eq!(count(&db, "scope_occupancy"), 4);
    for key in [
        CommandKey {
            operation: "other".into(),
            ..LegacyCollection.key()
        },
        CommandKey {
            dataset: "other".into(),
            ..LegacyCollection.key()
        },
        CommandKey {
            contract_version: "v2".into(),
            ..LegacyCollection.key()
        },
    ] {
        assert!(matches!(
            ctl.accept("r", key, payload(&["a"], &["day"])),
            Err(Error::RequestConflict)
        ));
    }
    assert!(matches!(
        ctl.accept("r", LegacyCollection.key(), payload(&["a"], &["day"])),
        Err(Error::RequestConflict)
    ));
    assert!(matches!(
        ctl.accept(
            "bad",
            LegacyCollection.key(),
            json!({"symbols":[],"periods":["day"]})
        ),
        Err(Error::InvalidCommand(_))
    ));
    assert!(db.read_request("bad").unwrap().is_none());
}

#[test]
fn two_connections_serialize_same_id_same_semantics() {
    let (_dir, db) = database();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let db = db.clone().with_busy_timeout(Duration::from_secs(3));
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                accept(&db, "r", &["a"])
            })
        })
        .collect();
    let mut results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_ne!(results[0].reused, results[1].reused);
    assert_eq!(
        snapshot(results.pop().unwrap()).task.task_id,
        snapshot(results.pop().unwrap()).task.task_id
    );
    for t in ["requests", "tasks", "task_items", "scope_occupancy"] {
        assert_eq!(count(&db, t), 1);
    }
}
#[test]
fn two_connections_conflicting_id_or_scope_never_partially_accept() {
    for same_id in [true, false] {
        let (_dir, db) = database();
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let db = db.clone().with_busy_timeout(Duration::from_secs(3));
                let b = barrier.clone();
                std::thread::spawn(move || {
                    b.wait();
                    control(&db, "run-a").accept(
                        if same_id || i == 0 { "r1" } else { "r2" },
                        LegacyCollection.key(),
                        payload(if same_id && i == 1 { &["b"] } else { &["a"] }, &["day"]),
                    )
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(results.iter().any(|r| if same_id {
            matches!(r, Err(Error::RequestConflict))
        } else {
            matches!(r, Err(Error::RangeConflict(_)))
        }));
        assert_eq!(count(&db, "requests"), 1);
        assert_eq!(count(&db, "scope_occupancy"), 1);
    }
}
#[test]
fn partial_batch_conflict_rolls_back_all_and_unrelated_scope_is_available() {
    let (_dir, db) = database();
    accept(&db, "r1", &["a"]);
    assert!(matches!(
        control(&db, "run-a").accept("r2", LegacyCollection.key(), payload(&["b", "a"], &["day"])),
        Err(Error::RangeConflict(_))
    ));
    assert!(db.read_request("r2").unwrap().is_none());
    assert_eq!(count(&db, "task_items"), 1);
    accept(&db, "r2", &["b"]);
    assert_eq!(count(&db, "tasks"), 2);
}
#[test]
fn current_running_terminal_and_read_only_state_are_distinct_from_missing_evidence() {
    let (_dir, db) = database();
    let s = snapshot(accept(&db, "r", &["a"]));
    let running = db
        .transition(&s.task.task_id, 0, "run-a", Change::Claim)
        .unwrap();
    assert_eq!(running.visible_status("run-a"), Status::Running);
    assert_eq!(running.task.accepted_run_id, "run-a");
    assert_eq!(running.task.owner_run_id.as_deref(), Some("run-a"));
    assert!(matches!(
        db.transition(&s.task.task_id, 0, "run-a", Change::RequestCancel),
        Err(Error::RevisionConflict)
    ));
    for _ in 0..3 {
        assert_eq!(db.read_task(&s.task.task_id).unwrap().unwrap(), running);
        assert!(db.read_request("r").unwrap().is_some());
    }
    let item = running.items[0].spec.item_id.clone();
    let started = db
        .transition(&s.task.task_id, 1, "run-a", Change::StartItem(item.clone()))
        .unwrap();
    assert!(matches!(
        db.transition(
            &s.task.task_id,
            started.task.state_revision,
            "run-a",
            Change::FinishExecution
        ),
        Err(Error::NeedsVerification(_))
    ));
    let done = db
        .transition(
            &s.task.task_id,
            started.task.state_revision,
            "run-a",
            Change::FinishItem(
                item,
                ItemOutcome {
                    success: true,
                    error: None,
                },
            ),
        )
        .unwrap();
    let terminal = db
        .transition(
            &s.task.task_id,
            done.task.state_revision,
            "run-a",
            Change::FinishExecution,
        )
        .unwrap();
    assert_eq!(terminal.task.status, Status::Succeeded);
    assert!(terminal.claims.is_empty());
    assert_eq!(db.read_task(&s.task.task_id).unwrap().unwrap(), terminal);
    assert_eq!(
        control(&db, "run-a")
            .cancel(&s.task.task_id, terminal.task.state_revision)
            .unwrap(),
        terminal
    );
    assert_eq!(
        snapshot(accept(&db, "r", &["a"])).task.status,
        Status::Succeeded
    );
}
#[test]
fn residual_records_fail_closed_without_read_repair() {
    for sql in [
        "DELETE FROM tasks",
        "DELETE FROM task_items",
        "DELETE FROM scope_occupancy",
        "DELETE FROM task_scopes",
        "UPDATE tasks SET status='succeeded',result_json='{}'",
        "UPDATE requests SET command_digest='wrong'",
        "DELETE FROM requests",
    ] {
        let (_dir, db) = database();
        let s = snapshot(accept(&db, "r", &["a"]));
        let c = Connection::open(db.path()).unwrap();
        c.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        c.execute_batch(sql).unwrap();
        let counts: Vec<_> = ["requests", "tasks", "task_items", "scope_occupancy"]
            .iter()
            .map(|t| count(&db, t))
            .collect();
        assert!(
            matches!(
                db.read_task(&s.task.task_id),
                Err(Error::NeedsVerification(_))
            ),
            "{sql}"
        );
        assert!(
            control(&db, "run-a")
                .accept("new", LegacyCollection.key(), payload(&["b"], &["day"]))
                .is_err(),
            "{sql}"
        );
        let after: Vec<_> = ["requests", "tasks", "task_items", "scope_occupancy"]
            .iter()
            .map(|t| count(&db, t))
            .collect();
        assert_eq!(counts, after);
    }
}
#[test]
fn absent_bad_unknown_schema_and_busy_do_not_initialize_or_fall_back() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("absent.sqlite"));
    assert!(matches!(
        db.read_request("r"),
        Err(Error::StorageUnavailable(_))
    ));
    assert!(!db.path().exists());
    std::fs::write(dir.path().join("requests.json"), "{}").unwrap();
    std::fs::write(db.path(), b"not SQLite").unwrap();
    let bytes = std::fs::read(db.path()).unwrap();
    assert!(matches!(
        db.read_request("r"),
        Err(Error::StorageUnavailable(_))
    ));
    assert_eq!(std::fs::read(db.path()).unwrap(), bytes);
    let (_dir, db) = database();
    let c = Connection::open(db.path()).unwrap();
    c.pragma_update(None, "user_version", 999).unwrap();
    assert!(matches!(
        db.read_request("r"),
        Err(Error::StorageUnavailable(_))
    ));
    assert!(db.upgrade().is_err());
    c.pragma_update(None, "user_version", 2).unwrap();
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(matches!(
        control(&db, "run-a").accept("r", LegacyCollection.key(), payload(&["a"], &["day"])),
        Err(Error::StorageUnavailable(_))
    ));
    assert!(db.read_request("r").unwrap().is_none());
    c.execute_batch("ROLLBACK").unwrap();
    accept(&db, "r", &["a"]);
}
#[test]
fn every_old_nonterminal_run_is_visible_but_not_executable_or_released() {
    for change in [None, Some(Change::Claim), Some(Change::RequestCancel)] {
        let (_dir, db) = database();
        let mut s = snapshot(accept(&db, "r", &["a"]));
        if let Some(change) = change {
            s = db.transition(&s.task.task_id, 0, "run-a", change).unwrap();
        }
        let old = s.clone();
        let other = control(&db, "run-b");
        assert_eq!(
            other
                .read_task(&s.task.task_id)
                .unwrap()
                .visible_status("run-b"),
            Status::NeedsVerification
        );
        assert!(other.run_once(&Complete(true)).unwrap().is_none());
        assert!(matches!(
            other.cancel(&s.task.task_id, s.task.state_revision),
            Err(Error::NeedsVerification(_))
        ));
        assert_eq!(db.read_task(&s.task.task_id).unwrap().unwrap(), old);
        assert_eq!(db.reconcile("run-b").unwrap(), 1);
        s = db.read_task(&s.task.task_id).unwrap().unwrap();
        assert_eq!(s.task.status, Status::NeedsVerification);
        assert_eq!(s.task.last_progress_at, old.task.last_progress_at);
        assert_eq!(s.task.owner_run_id, old.task.owner_run_id);
        assert_eq!(s.claims.len(), old.claims.len());
        assert_eq!(db.reconcile("run-b").unwrap(), 0);
        assert!(matches!(
            other.accept("new", LegacyCollection.key(), payload(&["a"], &["day"])),
            Err(Error::RangeConflict(_))
        ));
    }
}
#[test]
fn cancel_is_request_until_controlled_stop_and_retry_preserves_original() {
    let (_dir, db) = database();
    let ctl = control(&db, "run-a");
    let original = snapshot(accept(&db, "r", &["a", "b"]));
    let cancelled = ctl.cancel(&original.task.task_id, 0).unwrap();
    assert_eq!(cancelled.task.status, Status::CancelRequested);
    assert_eq!(cancelled.claims.len(), 2);
    assert_eq!(ctl.cancel(&original.task.task_id, 1).unwrap(), cancelled);
    struct Never;
    impl Executor for Never {
        fn execute(&self, _: &CommandKey, _: &ItemSpec) -> ItemOutcome {
            panic!("cancelled pending task must not execute")
        }
    }
    let stopped = ctl.run_once(&Never).unwrap().unwrap();
    assert_eq!(stopped.task.status, Status::Cancelled);
    assert!(stopped.claims.is_empty());
    let selected = BTreeSet::from([original.items[0].spec.item_id.clone()]);
    let retried = ctl
        .retry(&original.task.task_id, "retry", selected.clone())
        .unwrap();
    let new = snapshot(retried);
    assert_eq!(
        new.task.retry_of.as_deref(),
        Some(original.task.task_id.as_str())
    );
    assert_eq!(
        ctl.run_once(&Complete(false)).unwrap().unwrap().task.status,
        Status::Failed
    );
    let replay = ctl
        .retry(&original.task.task_id, "retry", selected)
        .unwrap();
    assert!(replay.reused);
    assert_eq!(snapshot(replay).task.status, Status::Failed);
    assert_eq!(ctl.read_task(&original.task.task_id).unwrap(), stopped);
}
#[test]
fn completion_during_cancellation_uses_actual_outcomes_and_keeps_unfinished_items() {
    for n in [1, 2] {
        let (_dir, db) = database();
        let s = snapshot(accept(&db, "r", if n == 1 { &["a"] } else { &["a", "b"] }));
        struct CancelInExecutor<'a> {
            db: &'a SqliteTasks,
            id: String,
        }
        impl Executor for CancelInExecutor<'_> {
            fn execute(&self, _: &CommandKey, _: &ItemSpec) -> ItemOutcome {
                let s = self.db.read_task(&self.id).unwrap().unwrap();
                assert!(s.items.iter().any(|i| i.stage == Stage::Running));
                let requested = self
                    .db
                    .transition(
                        &self.id,
                        s.task.state_revision,
                        "run-a",
                        Change::RequestCancel,
                    )
                    .unwrap();
                assert!(!requested.claims.is_empty());
                ItemOutcome {
                    success: true,
                    error: None,
                }
            }
        }
        let result = control(&db, "run-a")
            .run_once(&CancelInExecutor {
                db: &db,
                id: s.task.task_id.clone(),
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            result.task.status,
            if n == 1 {
                Status::Succeeded
            } else {
                Status::Cancelled
            }
        );
        assert!(result.claims.is_empty());
        let succeeded = result
            .items
            .iter()
            .find(|i| i.outcome.as_ref().is_some_and(|o| o.success))
            .unwrap();
        assert!(matches!(
            control(&db, "run-a").retry(
                &s.task.task_id,
                "bad-retry",
                BTreeSet::from([succeeded.spec.item_id.clone()])
            ),
            Err(Error::InvalidRetrySelection)
        ));
        assert!(db.read_request("bad-retry").unwrap().is_none());
    }
}

// Deliberately test-only, non-security contract. No symbol/period in public controls.
struct Documents {
    resolves: Arc<AtomicUsize>,
    current: Vec<String>,
}
impl Contract for Documents {
    fn key(&self) -> CommandKey {
        CommandKey {
            operation: "index".into(),
            dataset: "test_documents".into(),
            contract_version: "v1".into(),
        }
    }
    fn normalize(&self, p: Value) -> Result<Value, Error> {
        let selection = p
            .get("selection")
            .cloned()
            .ok_or_else(|| Error::InvalidCommand("selection".into()))?;
        let selection = if let Some(a) = selection.as_array() {
            let ids = a
                .iter()
                .map(|x| x.as_str().ok_or_else(|| Error::InvalidCommand("id".into())))
                .collect::<Result<BTreeSet<_>, _>>()?;
            json!(ids)
        } else if selection == "all" {
            selection
        } else {
            return Err(Error::InvalidCommand("selection".into()));
        };
        Ok(json!({"selection":selection,"mode":p.get("mode").cloned().unwrap_or(json!("full"))}))
    }
    fn resolve(&self, intent: &Value) -> Result<FrozenWork, Error> {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        let ids = if intent["selection"] == "all" {
            self.current.clone()
        } else {
            serde_json::from_value(intent["selection"].clone()).unwrap()
        };
        let items = ids
            .into_iter()
            .map(|id: String| ItemSpec {
                item_id: id.clone(),
                target: json!({"document":id}),
                progress_unit: "document".into(),
                frozen_inputs: json!({"manifest":"snapshot-1"}),
                scope_keys: BTreeSet::from([format!("document:{id}")]),
            })
            .collect::<Vec<_>>();
        Ok(FrozenWork {
            scopes: items
                .iter()
                .flat_map(|i| i.scope_keys.iter().cloned())
                .collect(),
            items,
        })
    }
}
#[test]
fn non_security_contract_shares_control_and_replay_never_resolves_dynamic_selection_again() {
    let (_dir, db) = database();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = Documents {
        resolves: calls.clone(),
        current: vec!["doc-a".into()],
    };
    let key = handler.key();
    let ctl = TaskControl::new(db.clone(), "run-a".into(), vec![Box::new(handler)]).unwrap();
    let first = snapshot(
        ctl.accept("r", key.clone(), json!({"selection":"all"}))
            .unwrap(),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let changed = TaskControl::new(
        db.clone(),
        "run-a".into(),
        vec![Box::new(Documents {
            resolves: calls.clone(),
            current: vec!["doc-b".into()],
        })],
    )
    .unwrap();
    let replay = changed
        .accept("r", key.clone(), json!({"selection":"all","mode":"full"}))
        .unwrap();
    assert!(replay.reused);
    assert_eq!(snapshot(replay), first);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        changed.accept("r", key, json!({"selection":["doc-a"]})),
        Err(Error::RequestConflict)
    ));
    let failed = ctl.run_once(&Complete(false)).unwrap().unwrap();
    assert_eq!(failed.items[0].spec.progress_unit, "document");
    let retried = snapshot(
        ctl.retry(
            &first.task.task_id,
            "retry",
            BTreeSet::from(["doc-a".into()]),
        )
        .unwrap(),
    );
    assert_eq!(retried.claims[0].scope_key, "document:doc-a");
    let cancel = ctl.cancel(&retried.task.task_id, 0).unwrap();
    assert_eq!(cancel.task.status, Status::CancelRequested);
    assert_eq!(
        ctl.run_once(&Complete(true)).unwrap().unwrap().task.status,
        Status::Cancelled
    );
    assert!(matches!(
        control(&db, "run-a").read_request("r"),
        Err(Error::UnsupportedContract)
    ));
}
#[test]
fn explicit_schema_upgrade_is_transactional_and_checks_migration_identity() {
    for fail in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = SqliteTasks::new(dir.path().join("v1.sqlite"));
        let c = Connection::open(db.path()).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        let sql = include_str!("../migrations/tasks/001.sql");
        c.execute_batch(sql).unwrap();
        c.execute(
            "INSERT INTO schema_migrations VALUES(1,?1,'test')",
            [x_data_mcp::hash::sha256(sql.as_bytes())],
        )
        .unwrap();
        c.pragma_update(None, "user_version", 1).unwrap();
        assert!(matches!(
            db.read_request("r"),
            Err(Error::StorageUnavailable(_))
        ));
        if fail {
            c.execute_batch("CREATE TABLE tasks_status_created(dummy INTEGER)")
                .unwrap();
        }
        let result = db.upgrade();
        assert_eq!(result.is_err(), fail);
        let version: u32 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, if fail { 1 } else { 2 });
        assert_eq!(count(&db, "schema_migrations"), i64::from(version));
        if !fail {
            assert!(db.read_request("r").unwrap().is_none());
            assert!(db.initialize().is_err());
            c.execute(
                "UPDATE schema_migrations SET checksum=?1 WHERE version=1",
                params!["wrong"],
            )
            .unwrap();
            assert!(db.upgrade().is_err());
        }
    }
}

#[test]
fn disjoint_dates_do_not_narrow_security_period_occupancy() {
    struct DatedFixture;
    impl Contract for DatedFixture {
        fn key(&self) -> CommandKey {
            CommandKey {
                operation: "test_dated_collection".into(),
                dataset: "test_only_market".into(),
                contract_version: "v1".into(),
            }
        }
        fn normalize(&self, payload: Value) -> Result<Value, Error> {
            Ok(payload)
        }
        fn resolve(&self, intent: &Value) -> Result<FrozenWork, Error> {
            let scopes = BTreeSet::from([x_data_mcp::task_control::legacy::scope_id(
                "sh.600006",
                "day",
            )]);
            Ok(FrozenWork {
                items: vec![ItemSpec {
                    item_id: "fixture".into(),
                    target: json!({"symbol":"sh.600006","period":"day"}),
                    progress_unit: "security_period".into(),
                    frozen_inputs: intent.clone(),
                    scope_keys: scopes.clone(),
                }],
                scopes,
            })
        }
    }
    let (_dir, db) = database();
    let ctl = TaskControl::new(db.clone(), "run-a".into(), vec![Box::new(DatedFixture)]).unwrap();
    let first = snapshot(
        ctl.accept(
            "2024",
            DatedFixture.key(),
            json!({"from":"2024-01-01","to":"2024-12-31"}),
        )
        .unwrap(),
    );
    assert_eq!(first.items[0].spec.frozen_inputs["from"], "2024-01-01");
    assert!(matches!(
        ctl.accept(
            "2025",
            DatedFixture.key(),
            json!({"from":"2025-01-01","to":"2025-12-31"})
        ),
        Err(Error::RangeConflict(_))
    ));
    assert!(db.read_request("2025").unwrap().is_none());
    assert_eq!(count(&db, "tasks"), 1);
    assert_eq!(count(&db, "scope_occupancy"), 1);
}
