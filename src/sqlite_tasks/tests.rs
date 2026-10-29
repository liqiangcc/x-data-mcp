//! Test-only process/snapshot seams: absent from library and production binaries.
use super::*;
use crate::task_control::{legacy::LegacyCollection, service::TaskControl};
use crate::task_verification::{
    StopVerification,
    decision::{self, Observation, StopInspector, StopRepository},
};
use std::{cell::RefCell, process::Command as ProcessCommand};
thread_local! {
    static AFTER_ROW: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static WRITES: RefCell<Option<std::collections::BTreeMap<&'static str, usize>>> =
        const { RefCell::new(None) };
}
pub(super) fn after_task_row() {
    let hook = AFTER_ROW.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}
/// Counts each UPDATE/DELETE statement by target, for O(items) transition proofs.
pub(super) fn count_write(kind: &'static str) {
    WRITES.with(|slot| {
        if let Some(map) = slot.borrow_mut().as_mut() {
            *map.entry(kind).or_default() += 1;
        }
    });
}
fn take_writes() -> std::collections::BTreeMap<&'static str, usize> {
    WRITES.with(|slot| slot.borrow_mut().take().unwrap_or_default())
}
fn reset_writes() {
    WRITES.with(|slot| *slot.borrow_mut() = Some(Default::default()));
}
pub(super) fn crash_point(point: &str) {
    if std::env::var("X_DATA_MCP_TEST_SQLITE_CRASH").as_deref() == Ok(point) {
        std::process::exit(78);
    }
}
fn accept(db: SqliteTasks) -> Acceptance {
    TaskControl::new(db, "test-run".into(), vec![Box::new(LegacyCollection)])
        .unwrap()
        .accept(
            "crash-request",
            LegacyCollection.key(),
            serde_json::json!({"symbols":["a","b"],"periods":["day","year"]}),
        )
        .unwrap()
}
#[test]
fn crash_child() {
    let Ok(path) = std::env::var("X_DATA_MCP_TEST_SQLITE_PATH") else {
        return;
    };
    accept(SqliteTasks::new(path));
    panic!("crash point was not reached");
}
#[test]
fn process_exit_before_commit_rolls_back_and_after_commit_replays_same_task() {
    for point in ["before_commit", "after_commit"] {
        let dir = tempfile::tempdir().unwrap();
        let db = SqliteTasks::new(dir.path().join("task.sqlite"));
        db.initialize().unwrap();
        let status = ProcessCommand::new(std::env::current_exe().unwrap())
            .args(["--exact", "sqlite_tasks::tests::crash_child", "--nocapture"])
            .env("X_DATA_MCP_TEST_SQLITE_PATH", db.path())
            .env("X_DATA_MCP_TEST_SQLITE_CRASH", point)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(78));
        let original = db.read_request("crash-request").unwrap();
        let c = Connection::open(db.path()).unwrap();
        for (table, committed) in [
            ("requests", 1),
            ("tasks", 1),
            ("task_items", 4),
            ("task_scopes", 4),
            ("scope_occupancy", 4),
        ] {
            let count: i64 = c
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                count,
                if point == "before_commit" {
                    0
                } else {
                    committed
                },
                "{point}: {table}"
            );
        }
        let replay = accept(db.clone());
        assert_eq!(replay.reused, point == "after_commit");
        if let Some(original) = original {
            assert_eq!(original, replay.record);
        }
        let count: i64 = c
            .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
fn started_item(db: &SqliteTasks, run: &str) -> (String, String, TaskSnapshot) {
    let ctl = TaskControl::new(db.clone(), run.into(), vec![Box::new(LegacyCollection)]).unwrap();
    let RequestOutcome::Task(t) = ctl
        .accept(
            &format!("req-{run}"),
            LegacyCollection.key(),
            serde_json::json!({"symbols":["a"],"periods":["day"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let tid = t.task.task_id.clone();
    let item = t.items[0].spec.item_id.clone();
    let s = db.transition(&tid, 0, run, Change::Claim).unwrap();
    let s = db
        .transition(
            &tid,
            s.task.state_revision,
            run,
            Change::StartItem(item.clone()),
        )
        .unwrap();
    (tid, item, s)
}
struct StoppedInspector;
impl StopInspector for StoppedInspector {
    fn inspect(&self, s: &TaskSnapshot) -> Result<Observation, Error> {
        Ok(Observation::Stopped {
            snapshot_sha256: decision::snapshot_digest(s)?,
            evidence_ref: "fixture-exit".into(),
        })
    }
}
#[test]
fn accepted_and_pending_items_have_no_execution_start() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("t.sqlite"));
    db.initialize().unwrap();
    let ctl =
        TaskControl::new(db.clone(), "run-a".into(), vec![Box::new(LegacyCollection)]).unwrap();
    let RequestOutcome::Task(t) = ctl
        .accept(
            "req",
            LegacyCollection.key(),
            serde_json::json!({"symbols":["a"],"periods":["day"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    for i in &t.items {
        assert!(
            !db.item_execution_started(&t.task.task_id, &i.spec.item_id)
                .unwrap()
        );
    }
    let s = db
        .transition(&t.task.task_id, 0, "run-a", Change::Claim)
        .unwrap();
    assert!(s.items.iter().all(|i| i.stage == Stage::Pending));
    for i in &t.items {
        assert!(
            !db.item_execution_started(&t.task.task_id, &i.spec.item_id)
                .unwrap()
        );
    }
}
#[test]
fn start_item_commits_marker_atomically_with_running() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("t.sqlite"));
    db.initialize().unwrap();
    let (tid, item, s) = started_item(&db, "run-a");
    assert_eq!(s.items[0].stage, Stage::Running);
    assert!(db.item_execution_started(&tid, &item).unwrap());
    let c = Connection::open(db.path()).unwrap();
    let (run_id, rev, at): (String, i64, String) = c
        .query_row(
            "SELECT run_id,start_revision,started_at FROM task_item_execution_starts WHERE task_id=?1 AND item_id=?2",
            params![&tid, &item],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(run_id, "run-a");
    assert_eq!(rev as u64, s.task.state_revision);
    assert!(!at.is_empty());
}
#[test]
fn execution_start_marker_failure_rolls_back_start_item() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("t.sqlite"));
    db.initialize().unwrap();
    let ctl =
        TaskControl::new(db.clone(), "run-a".into(), vec![Box::new(LegacyCollection)]).unwrap();
    let RequestOutcome::Task(t) = ctl
        .accept(
            "req",
            LegacyCollection.key(),
            serde_json::json!({"symbols":["a"],"periods":["day"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let tid = t.task.task_id.clone();
    let item = t.items[0].spec.item_id.clone();
    let s = db.transition(&tid, 0, "run-a", Change::Claim).unwrap();
    let before = db.read_task(&tid).unwrap().unwrap();
    Connection::open(db.path())
        .unwrap()
        .execute(
            "CREATE TRIGGER fixture_marker_failure BEFORE INSERT ON task_item_execution_starts \
             BEGIN SELECT RAISE(ABORT,'fixture marker failure'); END",
            [],
        )
        .unwrap();
    assert!(
        db.transition(
            &tid,
            s.task.state_revision,
            "run-a",
            Change::StartItem(item)
        )
        .is_err()
    );
    let after = db.read_task(&tid).unwrap().unwrap();
    assert_eq!(after.task.status, before.task.status);
    assert_eq!(after.task.state_revision, before.task.state_revision);
    assert_eq!(after.task.owner_run_id, before.task.owner_run_id);
    assert_eq!(after.task.last_progress_at, before.task.last_progress_at);
    assert_eq!(after.items, before.items);
    assert_eq!(marker_count(&db), 0);
    Connection::open(db.path())
        .unwrap()
        .execute("DROP TRIGGER fixture_marker_failure", [])
        .unwrap();
    drop(db);
    let re = SqliteTasks::new(dir.path().join("t.sqlite"));
    let after = re.read_task(&tid).unwrap().unwrap();
    assert_eq!(after.task.status, before.task.status);
    assert_eq!(after.task.state_revision, before.task.state_revision);
    assert_eq!(after.task.owner_run_id, before.task.owner_run_id);
    assert_eq!(after.task.last_progress_at, before.task.last_progress_at);
    assert_eq!(after.items, before.items);
    assert_eq!(marker_count(&re), 0);
}
#[test]
fn finish_cancel_and_failed_states_retain_execution_start() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("t.sqlite"));
    db.initialize().unwrap();
    // failed item keeps its start marker
    let (tid, item, s) = started_item(&db, "run-a");
    let s = db
        .transition(
            &tid,
            s.task.state_revision,
            "run-a",
            Change::FinishItem(
                item.clone(),
                ItemOutcome {
                    success: false,
                    error: Some("fixture failure".into()),
                    evidence: None,
                },
            ),
        )
        .unwrap();
    assert!(db.item_execution_started(&tid, &item).unwrap());
    let s = db
        .transition(
            &tid,
            s.task.state_revision,
            "run-a",
            Change::FinishExecution,
        )
        .unwrap();
    assert_eq!(s.task.status, Status::Failed);
    assert!(db.item_execution_started(&tid, &item).unwrap());
    // started then cancelled retains the marker too
    let (tid2, item2, s2) = started_item(&db, "run-b");
    let s2 = db
        .transition(
            &tid2,
            s2.task.state_revision,
            "run-b",
            Change::RequestCancel,
        )
        .unwrap();
    let s2 = db
        .transition(
            &tid2,
            s2.task.state_revision,
            "run-b",
            Change::FinishItem(
                item2.clone(),
                ItemOutcome {
                    success: false,
                    error: Some("stopped".into()),
                    evidence: None,
                },
            ),
        )
        .unwrap();
    let s2 = db
        .transition(
            &tid2,
            s2.task.state_revision,
            "run-b",
            Change::FinishCancellation,
        )
        .unwrap();
    assert_eq!(s2.task.status, Status::Cancelled);
    assert!(db.item_execution_started(&tid2, &item2).unwrap());
}
#[test]
fn verified_interruption_reset_retains_execution_start() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("t.sqlite"));
    db.initialize().unwrap();
    let ctl = TaskControl::new(db.clone(), "old".into(), vec![Box::new(LegacyCollection)]).unwrap();
    let RequestOutcome::Task(t) = ctl
        .accept(
            "req",
            LegacyCollection.key(),
            serde_json::json!({"symbols":["a","b"],"periods":["day"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let tid = t.task.task_id.clone();
    let started = t.items[0].spec.item_id.clone();
    let sibling = t.items[1].spec.item_id.clone();
    let s = db.transition(&tid, 0, "old", Change::Claim).unwrap();
    let target = db
        .transition(
            &tid,
            s.task.state_revision,
            "old",
            Change::StartItem(started.clone()),
        )
        .unwrap();
    // only the started item proves ever-started; its sibling never ran
    assert!(db.item_execution_started(&tid, &started).unwrap());
    assert!(!db.item_execution_started(&tid, &sibling).unwrap());
    let contract = StopVerification::new(db.clone(), "new".into()).unwrap();
    let key = contract.key();
    let ctl = TaskControl::new(db.clone(), "new".into(), vec![Box::new(contract)]).unwrap();
    let RequestOutcome::Task(check) = ctl
        .accept("check", key, serde_json::json!({"target_task_id": tid}))
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let s = db
        .transition(&check.task.task_id, 0, "new", Change::Claim)
        .unwrap();
    let s = db
        .transition(
            &check.task.task_id,
            s.task.state_revision,
            "new",
            Change::StartItem("stopped-writes".into()),
        )
        .unwrap();
    let plan = decision::prepare(
        s,
        target,
        "new",
        "actor",
        "2026-09-13T00:00:00Z".parse().unwrap(),
        &StoppedInspector,
    )
    .unwrap();
    db.commit_stop_check(&plan).unwrap();
    let after = db.read_task(&tid).unwrap().unwrap();
    assert_eq!(after.task.status, Status::Interrupted);
    assert!(after.items.iter().all(|i| i.stage == Stage::Pending));
    assert!(db.item_execution_started(&tid, &started).unwrap());
    assert!(!db.item_execution_started(&tid, &sibling).unwrap());
    drop(db);
    let re = SqliteTasks::new(dir.path().join("t.sqlite"));
    let after = re.read_task(&tid).unwrap().unwrap();
    assert_eq!(after.task.status, Status::Interrupted);
    assert!(after.items.iter().all(|i| i.stage == Stage::Pending));
    assert!(re.item_execution_started(&tid, &started).unwrap());
    assert!(!re.item_execution_started(&tid, &sibling).unwrap());
}
#[test]
fn execution_start_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("t.sqlite"));
    db.initialize().unwrap();
    let (tid, item, _) = started_item(&db, "run-a");
    drop(db);
    let re = SqliteTasks::new(dir.path().join("t.sqlite"));
    assert!(re.item_execution_started(&tid, &item).unwrap());
}
#[test]
fn execution_start_marker_row_is_immutable() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("t.sqlite"));
    db.initialize().unwrap();
    let (tid, item, _) = started_item(&db, "run-a");
    let row = |db: &SqliteTasks| {
        Connection::open(db.path())
            .unwrap()
            .query_row(
                "SELECT task_id,item_id,run_id,start_revision,started_at \
                 FROM task_item_execution_starts WHERE task_id=?1 AND item_id=?2",
                params![&tid, &item],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .unwrap()
    };
    let before = row(&db);
    let c = Connection::open(db.path()).unwrap();
    assert!(
        c.execute(
            "UPDATE task_item_execution_starts SET run_id='x' WHERE task_id=?1",
            [&tid]
        )
        .is_err()
    );
    assert!(
        c.execute(
            "DELETE FROM task_item_execution_starts WHERE task_id=?1",
            [&tid]
        )
        .is_err()
    );
    drop(c);
    assert_eq!(row(&db), before);
    assert_eq!(marker_count(&db), 1);
    drop(db);
    let re = SqliteTasks::new(dir.path().join("t.sqlite"));
    assert_eq!(row(&re), before);
    assert_eq!(marker_count(&re), 1);
    assert!(re.item_execution_started(&tid, &item).unwrap());
}
#[test]
fn only_the_committed_start_writes_the_marker() {
    // deterministic: a stale-revision start after a committed one is rejected
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("t.sqlite"));
    db.initialize().unwrap();
    let (tid, item, s) = started_item(&db, "run-a");
    assert!(
        db.transition(&tid, 1, "run-a", Change::StartItem(item.clone()))
            .is_err()
    );
    assert!(
        db.transition(
            &tid,
            s.task.state_revision,
            "run-a",
            Change::StartItem(item.clone())
        )
        .is_err()
    );
    assert_eq!(marker_count(&db), 1);
    // real two-connection race on the same item
    for _ in 0..8 {
        let dir = tempfile::tempdir().unwrap();
        let db = SqliteTasks::new(dir.path().join("t.sqlite"));
        db.initialize().unwrap();
        let ctl =
            TaskControl::new(db.clone(), "run-a".into(), vec![Box::new(LegacyCollection)]).unwrap();
        let RequestOutcome::Task(t) = ctl
            .accept(
                "req",
                LegacyCollection.key(),
                serde_json::json!({"symbols":["a"],"periods":["day"]}),
            )
            .unwrap()
            .record
            .outcome
        else {
            panic!()
        };
        let tid = t.task.task_id.clone();
        let item = t.items[0].spec.item_id.clone();
        db.transition(&tid, 0, "run-a", Change::Claim).unwrap();
        let a = db.clone();
        let (t2, i2) = (tid.clone(), item.clone());
        let t1 = std::thread::spawn(move || a.transition(&t2, 1, "run-a", Change::StartItem(i2)));
        let b = SqliteTasks::new(db.path());
        let r2 = b.transition(&tid, 1, "run-a", Change::StartItem(item.clone()));
        let r1 = t1.join().unwrap();
        assert_eq!(r1.is_ok() as u8 + r2.is_ok() as u8, 1);
        let n: i64 = marker_count(&db);
        assert_eq!(n, 1);
    }
}
fn marker_count(db: &SqliteTasks) -> i64 {
    Connection::open(db.path())
        .unwrap()
        .query_row("SELECT count(*) FROM task_item_execution_starts", [], |r| {
            r.get(0)
        })
        .unwrap()
}
#[test]
fn upgraded_v9_database_has_no_fabricated_execution_starts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");
    {
        let c = Connection::open(&path).unwrap();
        c.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        for (i, sql) in MIGRATIONS[..9].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.execute(
                "INSERT INTO schema_migrations VALUES(?1,?2,?3)",
                params![
                    i as u32 + 1,
                    crate::hash::sha256(sql.as_bytes()),
                    "2026-09-13T00:00:00Z"
                ],
            )
            .unwrap();
            c.pragma_update(None, "user_version", i as u32 + 1).unwrap();
        }
        // A pre-upgrade task that finished an item: proves finished only, never started.
        c.execute("INSERT INTO tasks VALUES('old-task','{}','succeeded',3,'old-run',NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','{}','{}',NULL)", []).unwrap();
        c.execute("INSERT INTO task_items VALUES('old-task','[\"market\",\"a\",\"day\"]','{}','u','{}','[]','finished','{\"success\":true}')", []).unwrap();
    }
    let db = SqliteTasks::new(&path);
    db.upgrade().unwrap();
    assert!(
        !db.item_execution_started("old-task", "[\"market\",\"a\",\"day\"]")
            .unwrap()
    );
    assert_eq!(marker_count(&db), 0);
}
#[test]
fn start_item_crash_child() {
    let Ok(path) = std::env::var("X_DATA_MCP_TEST_SQLITE_PATH") else {
        return;
    };
    let db = SqliteTasks::new(path);
    let task_id = std::env::var("X_DATA_MCP_TEST_TASK").unwrap();
    let item_id = std::env::var("X_DATA_MCP_TEST_ITEM").unwrap();
    db.transition(&task_id, 1, "test-run", Change::StartItem(item_id))
        .unwrap();
    panic!("crash point was not reached");
}
#[test]
fn start_item_real_exit_keeps_marker_commit_atomic() {
    for point in ["before_commit", "after_commit"] {
        let dir = tempfile::tempdir().unwrap();
        let db = SqliteTasks::new(dir.path().join("task.sqlite"));
        db.initialize().unwrap();
        let RequestOutcome::Task(t) = accept(db.clone()).record.outcome else {
            panic!()
        };
        let tid = t.task.task_id.clone();
        let item = t.items[0].spec.item_id.clone();
        db.transition(&tid, 0, "test-run", Change::Claim).unwrap();
        let status = ProcessCommand::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sqlite_tasks::tests::start_item_crash_child",
                "--nocapture",
            ])
            .env("X_DATA_MCP_TEST_SQLITE_PATH", db.path())
            .env("X_DATA_MCP_TEST_TASK", &tid)
            .env("X_DATA_MCP_TEST_ITEM", &item)
            .env("X_DATA_MCP_TEST_SQLITE_CRASH", point)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(78), "{point}");
        let re = SqliteTasks::new(db.path());
        let s = re.read_task(&tid).unwrap().unwrap();
        if point == "before_commit" {
            assert_eq!(s.task.state_revision, 1);
            assert_eq!(s.items[0].stage, Stage::Pending);
            assert!(!re.item_execution_started(&tid, &item).unwrap());
        } else {
            assert_eq!(s.items[0].stage, Stage::Running);
            assert!(re.item_execution_started(&tid, &item).unwrap());
            assert!(
                re.transition(&tid, 1, "test-run", Change::StartItem(item.clone()))
                    .is_err()
            );
            assert_eq!(marker_count(&re), 1);
        }
    }
}
#[test]
fn read_uses_one_snapshot_when_another_connection_commits_between_queries() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("task.sqlite"));
    db.initialize().unwrap();
    let RequestOutcome::Task(initial) = accept(db.clone()).record.outcome else {
        panic!()
    };
    let other = db.clone();
    let id = initial.task.task_id.clone();
    AFTER_ROW.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let next = other.transition(&id, 0, "test-run", Change::Claim).unwrap();
            assert_eq!(next.task.state_revision, 1);
        }))
    });
    let read = db.read_request("crash-request").unwrap().unwrap();
    let RequestOutcome::Task(snapshot) = read.outcome else {
        panic!()
    };
    assert_eq!(snapshot, initial); // task + items + claims from before the other commit
    let newer = db.read_task(&initial.task.task_id).unwrap().unwrap();
    assert_eq!(newer.task.state_revision, 1);
    assert!(newer.claims.iter().all(|c| c.state_revision == 1));
}
#[test]
fn item_transitions_write_o1_rows_regardless_of_task_size() {
    struct Ok;
    impl Executor for Ok {
        fn execute(&self, _: &CommandKey, _: &ItemSpec) -> ItemOutcome {
            ItemOutcome {
                success: true,
                error: None,
                evidence: None,
            }
        }
    }
    for symbols in [1usize, 40] {
        let dir = tempfile::tempdir().unwrap();
        let db = SqliteTasks::new(dir.path().join("task.sqlite"));
        db.initialize().unwrap();
        let ctl =
            TaskControl::new(db.clone(), "run".into(), vec![Box::new(LegacyCollection)]).unwrap();
        let items: Vec<String> = (0..symbols).map(|i| format!("s{i}")).collect();
        let accepted = ctl
            .accept(
                "o1",
                LegacyCollection.key(),
                serde_json::json!({"symbols":items,"periods":["day"]}),
            )
            .unwrap();
        let RequestOutcome::Task(task) = accepted.record.outcome else {
            panic!()
        };
        reset_writes();
        let done = ctl.run_selected(*task, &Ok).unwrap().unwrap();
        assert_eq!(done.task.status, Status::Succeeded);
        let writes = take_writes();
        // Each item is started and finished once: exactly 2 UPDATEs per
        // item, one guarded task-row UPDATE per transition, and a single
        // terminal occupancy release. No per-item whole-task rewrites.
        assert_eq!(writes.get("task_items").copied().unwrap_or(0), 2 * symbols);
        assert_eq!(writes.get("tasks").copied().unwrap_or(0), 2 * symbols + 2);
        assert_eq!(writes.get("scope_occupancy").copied().unwrap_or(0), 1);
    }
}
/// The head+item execution path must persist byte-identical task state to
/// the held-snapshot path for the same change sequence — including the
/// stale-head refresh a concurrent writer can force.
#[test]
fn head_item_transitions_persist_identically_to_snapshot_transitions() {
    let dir = tempfile::tempdir().unwrap();
    let full_db = SqliteTasks::new(dir.path().join("full.sqlite"));
    let head_db = SqliteTasks::new(dir.path().join("head.sqlite"));
    full_db.initialize().unwrap();
    head_db.initialize().unwrap();
    let accepted = |db: &SqliteTasks| {
        let ctl =
            TaskControl::new(db.clone(), "run".into(), vec![Box::new(LegacyCollection)]).unwrap();
        let accepted = ctl
            .accept(
                "req",
                LegacyCollection.key(),
                serde_json::json!({"symbols":["a","b","c"],"periods":["day"]}),
            )
            .unwrap();
        let RequestOutcome::Task(task) = accepted.record.outcome else {
            panic!()
        };
        task
    };
    let mut s = *accepted(&full_db);
    let head_task = accepted(&head_db);
    let sid = s.task.task_id.clone();
    let tid = head_task.task.task_id.clone();
    let item_ids: Vec<String> = head_task
        .items
        .iter()
        .map(|i| i.spec.item_id.clone())
        .collect();
    let mut head = head_db.read(|c| read_task_head(c, &tid)).unwrap().unwrap();
    let outcome = ItemOutcome {
        success: true,
        error: None,
        evidence: None,
    };
    // Wall-clock fields necessarily differ between the two executions;
    // every persisted decision/state field must be identical.
    let normalize = |mut s: TaskSnapshot| {
        let epoch = chrono::DateTime::parse_from_rfc3339("1970-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        s.task.created_at = epoch;
        s.task.last_progress_at = epoch;
        s.task.task_id = "task".into();
        for claim in &mut s.claims {
            claim.task_id = "task".into();
        }
        s
    };
    let snapshot_of = |db: &SqliteTasks, id: &str| normalize(db.read_task(id).unwrap().unwrap());
    // Claim through both paths; persisted snapshots must stay identical.
    full_db
        .transition_light(&mut s, "run", Change::Claim)
        .unwrap();
    head_db
        .write(|c| transition_head_tx(c, &mut head, None, "run", Change::Claim, Utc::now()))
        .unwrap();
    assert_eq!(snapshot_of(&full_db, &sid), snapshot_of(&head_db, &tid));
    for item_id in &item_ids {
        let mut item = head_db
            .read(|c| read_task_item(c, &tid, item_id))
            .unwrap()
            .unwrap();
        for change in [
            Change::StartItem(item_id.clone()),
            Change::FinishItem(item_id.clone(), outcome.clone()),
        ] {
            full_db
                .transition_light(&mut s, "run", change.clone())
                .unwrap();
            head_db
                .write(|c| {
                    transition_head_tx(c, &mut head, Some(&mut item), "run", change, Utc::now())
                })
                .unwrap();
            assert_eq!(
                snapshot_of(&full_db, &sid),
                snapshot_of(&head_db, &tid),
                "{item_id}"
            );
        }
        // The execution-start marker must land on the head path too.
        assert!(head_db.item_execution_started(&tid, item_id).unwrap());
    }
    full_db
        .transition_light(&mut s, "run", Change::FinishExecution)
        .unwrap();
    head_db
        .write(|c| {
            transition_head_tx(
                c,
                &mut head,
                None,
                "run",
                Change::FinishExecution,
                Utc::now(),
            )
        })
        .unwrap();
    let done = head_db.read_task(&tid).unwrap().unwrap();
    assert_eq!(snapshot_of(&full_db, &sid), normalize(done.clone()));
    assert_eq!(done.task.status, Status::Succeeded);
    assert!(done.claims.is_empty());
}
/// A caller-held head that another writer advanced is refreshed inside the
/// transition instead of failing or deciding on stale facts.
#[test]
fn head_transition_refreshes_a_stale_held_head() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("task.sqlite"));
    db.initialize().unwrap();
    let ctl = TaskControl::new(db.clone(), "run".into(), vec![Box::new(LegacyCollection)]).unwrap();
    let accepted = ctl
        .accept(
            "req",
            LegacyCollection.key(),
            serde_json::json!({"symbols":["a"],"periods":["day"]}),
        )
        .unwrap();
    let RequestOutcome::Task(task) = accepted.record.outcome else {
        panic!()
    };
    let tid = task.task.task_id.clone();
    let item_id = task.items[0].spec.item_id.clone();
    // The runner holds a pending head while another writer claims.
    let mut head = db.read(|c| read_task_head(c, &tid)).unwrap().unwrap();
    let mut item = db
        .read(|c| read_task_item(c, &tid, &item_id))
        .unwrap()
        .unwrap();
    db.transition(&tid, 0, "run", Change::Claim).unwrap();
    // StartItem on the stale pair refreshes to the claimed state, then
    // applies the legal change.
    db.write(|c| {
        transition_head_tx(
            c,
            &mut head,
            Some(&mut item),
            "run",
            Change::StartItem(item_id.clone()),
            Utc::now(),
        )
    })
    .unwrap();
    let s = db.read_task(&tid).unwrap().unwrap();
    assert_eq!(s.task.status, Status::Running);
    assert_eq!(s.items[0].stage, Stage::Running);
    assert_eq!(head.state_revision, s.task.state_revision);
    assert!(db.item_execution_started(&tid, &item_id).unwrap());
}

/// Schema-21 latest-only compact set (SPEC-LATEST-ONLY §1): the schema-19
/// version-manifest/result tables below are replaced by `market_series` +
/// `series_bars` + `yearly_series_lineage` + `market_events`.
const COMPACT_TABLES: [&str; 20] = [
    "evidence_bundles",
    "evidence_parts",
    "source_bindings",
    "market_series",
    "series_bars",
    "yearly_series_lineage",
    "market_events",
    "staged_records",
    // Schema 22 (M1 PR-4, issue-383 §3.6/§3.7): the parsed factor facts and
    // the non-bar dataset version registration.
    "adjustment_events",
    "derived_dataset_versions",
    // Schema 22 (M1 PR-5, issue-383 §3.9): per-index unit/aggregation scope.
    "index_series_meta",
    // Schema 22 (M1 PR-6, issue-383 §3.8): the derived trading calendar.
    "trading_days",
    // Schema 23 (M2 PR-1, issue-391 §9.2/§8.1): the seven table groups of the
    // parsed-fact layer plus the two trading-rule tables.
    "security_master",
    "code_migrations",
    "security_name_history",
    "capital_events",
    "risk_warning_intervals",
    "suspension_intervals",
    "price_limit_rules",
    "min_tick_rules",
    // Schema 25 (M3 PR-1, issue-446 §9.2): the five index tables over the
    // source store, created empty; writers land in PR-2/PR-6/PR-8.
    "industry_snapshots",
    "industry_assignments",
    "exchange_source_snapshots",
    "generation_facts",
    "retire_exemptions",
];
/// Dropped by migration 021 — their absence is part of the v21 shape.
const DROPPED_V19_TABLES: [&str; 12] = [
    "bar_segments",
    "segment_bars",
    "data_versions",
    "version_checkpoint_clips",
    "version_delta_clips",
    "yearly_lineage",
    "result_registry",
    "market_update_results",
    "market_update_delta_clips",
    "publication_results",
    "task_publications",
    "effective_heads",
];
const LEGACY_MARKET_TABLES: [&str; 7] = [
    "payloads",
    "version_evidence",
    "yearly_bars",
    "publication_batches",
    "yearly_execution_results",
    "manifest_nodes",
    "version_segments",
];
const PRESERVED_TABLES: [&str; 16] = [
    "tasks",
    "task_items",
    "task_scopes",
    "scope_occupancy",
    "requests",
    "catalog_versions",
    "catalog_members",
    "catalog_head",
    "stopped_write_checks",
    "publication_checks",
    "verification_runtime_guards",
    "verification_inspections",
    "task_item_execution_starts",
    "proven_gaps",
    "gap_coverings",
    "gap_verification_decisions",
];

fn raw_tables(path: &std::path::Path) -> Vec<String> {
    let c = Connection::open(path).unwrap();
    let mut stmt = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
fn raw_user_version(path: &std::path::Path) -> u32 {
    Connection::open(path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}
fn raw_columns(path: &std::path::Path, table: &str) -> Vec<String> {
    let c = Connection::open(path).unwrap();
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})")).unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
fn assert_compact_shape(path: &std::path::Path) {
    let tables = raw_tables(path);
    assert_eq!(raw_user_version(path), COMPACT_SCHEMA_VERSION);
    for table in COMPACT_TABLES {
        assert!(tables.contains(&table.to_string()), "missing {table}");
    }
    for table in LEGACY_MARKET_TABLES {
        assert!(!tables.contains(&table.to_string()), "legacy {table} kept");
    }
    for table in DROPPED_V19_TABLES {
        assert!(
            !tables.contains(&table.to_string()),
            "v19 table {table} kept"
        );
    }
    for table in PRESERVED_TABLES {
        assert!(
            tables.contains(&table.to_string()),
            "preserved {table} lost"
        );
    }
    let series = raw_columns(path, "market_series");
    assert!(series.contains(&"series_key".to_string()));
    assert!(series.contains(&"content_digest".to_string()));
    assert!(series.contains(&"version_id".to_string()));
    // Schema 22 (issue-383 §3.2): 口径 and 精度 are series properties.
    for column in ["adjustment", "instrument_class", "price_scale"] {
        assert!(series.contains(&column.to_string()), "missing {column}");
    }
    let bars = raw_columns(path, "series_bars");
    assert!(bars.contains(&"series_key".to_string()));
    // §3.3: prices are scale-interpreted integers, so the `_cents` names are
    // gone; `volume_quantum` reserves the #381 option-1 precision column.
    for column in ["open_units", "high_units", "low_units", "close_units"] {
        assert!(bars.contains(&column.to_string()), "missing {column}");
        assert!(
            !bars.contains(&column.replace("units", "cents")),
            "{column} still claims 分"
        );
    }
    assert!(bars.contains(&"volume_quantum".to_string()));
    // §3.4/§3.5: events carry the 口径, lineage the base 口径.
    let events = raw_columns(path, "market_events");
    assert!(events.contains(&"adjustment".to_string()));
    assert!(events.contains(&"result_id".to_string()));
    let lineage = raw_columns(path, "yearly_series_lineage");
    assert!(lineage.contains(&"base_adjustment".to_string()));
    assert!(lineage.contains(&"base_instrument_class".to_string()));
    // §3.6: the immutable factor bits and the derived decimals share one row,
    // and the 知晓时间 is a separate nullable column (never inferred).
    let factors = raw_columns(path, "adjustment_events");
    for column in [
        "entity",
        "ordinal",
        "effective_day",
        "category",
        "c_bits_1",
        "c_bits_4",
        "c1",
        "c4",
        "source_binding_digest",
        "evidence_digest",
        "known_at",
    ] {
        assert!(factors.contains(&column.to_string()), "missing {column}");
    }
    // §3.7: identity + audit only — no business columns in the registry.
    let derived = raw_columns(path, "derived_dataset_versions");
    for column in [
        "dataset_kind",
        "dataset_key",
        "version_id",
        "content_digest",
        "digest_format",
        "record_count",
        "first_day",
        "last_day",
        "algorithm_version",
        "task_id",
        "item_id",
    ] {
        assert!(derived.contains(&column.to_string()), "missing {column}");
    }
    // §3.8: one row per trading day and the input mask — no `is_open` column
    // to misread, and no future-day prediction column either.
    let calendar = raw_columns(path, "trading_days");
    for column in ["trade_day", "sources"] {
        assert!(calendar.contains(&column.to_string()), "missing {column}");
    }
    assert_eq!(
        calendar.len(),
        2,
        "trading_days is exactly (trade_day, sources)"
    );
    // §3.9: unit and aggregation scope are data, one row per index series.
    let index = raw_columns(path, "index_series_meta");
    for column in [
        "series_key",
        "aliases",
        "aggregate_scope",
        "member_count",
        "price_unit",
        "volume_unit",
        "amount_unit",
        "evidence_digest",
        "evidence_note",
    ] {
        assert!(index.contains(&column.to_string()), "missing {column}");
    }
    // §9.2 (schema 23): the 时点 columns are per table, so a missing
    // `effective_from`/`effective_to`/`known_at` or a lost second-source column
    // is a shape error, not a writer bug. Column lists are checked, not the
    // CHECK bodies: the constraints themselves are exercised by
    // `schema23_constraints_reject_duplicates_and_bad_values`.
    let master = raw_columns(path, "security_master");
    for column in [
        "exchange",
        "code",
        "effective_from",
        "effective_to",
        "instrument_class",
        "issuer_key",
        "issuer_resolution",
        "listing_date",
        "listing_date_tdx",
        "listing_source",
        "b_listing_date",
        "delisting_date",
        "delisting_date_tdx",
        "delisting_source",
        "delisting_reason_class",
        "arrangement_start",
        "arrangement_end",
        "listing_conflict",
        "delisting_conflict",
        "known_at",
        "source_binding_digest",
        "evidence_digest",
    ] {
        assert!(master.contains(&column.to_string()), "missing {column}");
    }
    let migrations = raw_columns(path, "code_migrations");
    for column in ["from_code", "to_code", "effective_from", "certainty"] {
        assert!(migrations.contains(&column.to_string()), "missing {column}");
    }
    let names = raw_columns(path, "security_name_history");
    for column in ["name", "certainty", "known_at", "source_kind"] {
        assert!(names.contains(&column.to_string()), "missing {column}");
    }
    // §9.2 note 2: the immutable f32 bits and the derived 股 counts share one
    // row, and 知晓时间 is a separate nullable column.
    let capital = raw_columns(path, "capital_events");
    for column in [
        "ordinal",
        "effective_day",
        "category",
        "float_before_bits",
        "total_after_bits",
        "float_after",
        "total_after",
        "prev_is_lagged",
        "suspicious",
        "known_at",
        "source_binding_digest",
        "evidence_digest",
    ] {
        assert!(capital.contains(&column.to_string()), "missing {column}");
    }
    // §6.3: 公告时间 and 知晓时间 are separate columns; the raw status code is
    // kept next to the normalised vocabulary.
    let warnings = raw_columns(path, "risk_warning_intervals");
    for column in [
        "raw_status_code",
        "normalized_status",
        "announced_at",
        "known_at",
        "source_kind",
    ] {
        assert!(warnings.contains(&column.to_string()), "missing {column}");
    }
    // §7.3/Q6: one row per interval with its missing-day count.
    let suspensions = raw_columns(path, "suspension_intervals");
    for column in [
        "start_day",
        "end_day",
        "certainty",
        "missing_days",
        "reason",
    ] {
        assert!(
            suspensions.contains(&column.to_string()),
            "missing {column}"
        );
    }
    // §8.1: 幅度/取整/上市阶段 are rule data, and `basis_ref` names the 出处.
    let limits = raw_columns(path, "price_limit_rules");
    for column in [
        "market",
        "board",
        "status_class",
        "listing_phase",
        "band_bp",
        "band_side",
        "rounding",
        "effective_from",
        "effective_to",
        "basis_ref",
        "basis_status",
        // Schema 25: the nullable minimum-band floor (owner 2026-10-29).
        "min_band_ticks",
    ] {
        assert!(limits.contains(&column.to_string()), "missing {column}");
    }
    let ticks = raw_columns(path, "min_tick_rules");
    for column in [
        "market",
        "board",
        "tick_units",
        "price_scale",
        "effective_from",
        "basis_ref",
        "basis_status",
    ] {
        assert!(ticks.contains(&column.to_string()), "missing {column}");
    }
}
fn assert_legacy_shape(path: &std::path::Path) {
    assert_eq!(raw_user_version(path), SCHEMA_VERSION);
    assert!(raw_columns(path, "data_versions").contains(&"envelope_json".to_string()));
    assert!(!raw_columns(path, "data_versions").contains(&"version_key".to_string()));
    assert!(raw_tables(path).contains(&"payloads".to_string()));
}

/// Boundary (RESULT-S3 intentional assertion change): (1) a schema-21 file
/// is read/write-accepted by the standalone task-store handle — the binary
/// manages that schema end-to-end; (2) a compact 19/20 checkpoint stays
/// fail-closed — the current handle refuses it; (3) `upgrade_compact` still
/// refuses an already-compact file.
#[test]
fn initialize_compact_builds_compact_schema_and_refuses_legacy_ops() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compact.db");
    let db = SqliteTasks::new(&path);
    db.initialize_compact().unwrap();
    assert_compact_shape(&path);
    db.read(|_| Ok(())).unwrap();
    db.write(|_| Ok(())).unwrap();

    // A 19/20-era checkpoint file: user_version 20 with the matching
    // migration rows — the current handle still refuses read/write.
    db.checkpoint_truncate().unwrap();
    let path20 = dir.path().join("checkpoint20.db");
    std::fs::copy(&path, &path20).unwrap();
    let c = Connection::open(&path20).unwrap();
    c.pragma_update(None, "user_version", 20).unwrap();
    c.execute("DELETE FROM schema_migrations WHERE version>20", [])
        .unwrap();
    drop(c);
    let db20 = SqliteTasks::new(&path20);
    let e = db20.read(|_| Ok(())).unwrap_err().to_string();
    assert!(e.contains("unsupported/uninitialized schema"), "{e}");
    let e = db20.write(|_| Ok(())).unwrap_err().to_string();
    assert!(e.contains("unsupported/uninitialized schema"), "{e}");
}

#[test]
fn upgrade_compact_replaces_empty_schema18_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let db = SqliteTasks::new(&path);
    db.initialize().unwrap();
    assert_legacy_shape(&path);
    db.upgrade_compact().unwrap();
    assert_compact_shape(&path);
    let c = Connection::open(&path).unwrap();
    let broken: bool = c
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .exists([])
        .unwrap();
    assert!(!broken);
    // The latest compact revision is the managed current schema: read/write
    // accept it, while a second `upgrade_compact` still refuses the compact
    // file.
    db.read(|_| Ok(())).unwrap();
    db.write(|_| Ok(())).unwrap();
    let e = db.upgrade_compact().unwrap_err().to_string();
    assert!(e.contains("cannot upgrade to compact"), "{e}");
}

fn hex32(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

/// Schema 25 length/order assertion (issue-446 §9.1/§9.3): the fixed array
/// length, the schema constant and the `schema_migrations` rows must agree, and
/// the tail of the chain must be the M3 index-table step. 022/023/024 stay
/// immutable.
#[test]
fn compact_migration_chain_matches_schema_version() {
    assert_eq!(
        MIGRATIONS.len() + COMPACT_MIGRATIONS.len(),
        COMPACT_SCHEMA_VERSION as usize,
        "migration chain and COMPACT_SCHEMA_VERSION disagree"
    );
    let tail = COMPACT_MIGRATIONS[COMPACT_MIGRATIONS.len() - 1];
    assert!(
        tail.contains("CREATE TABLE industry_snapshots(")
            && tail.contains("CREATE TABLE industry_assignments(")
            && tail.contains("CREATE TABLE exchange_source_snapshots(")
            && tail.contains("CREATE TABLE generation_facts(")
            && tail.contains("CREATE TABLE retire_exemptions(")
            && tail.contains("ADD COLUMN min_band_ticks INTEGER"),
        "the tail of the compact chain is not migration 025"
    );
    let previous = COMPACT_MIGRATIONS[COMPACT_MIGRATIONS.len() - 2];
    assert!(
        previous.contains("CREATE TABLE capital_events_v24(")
            && previous.contains("category IN (2,3,5,7,8,9,10)"),
        "migration 024 was not kept immutable before 025"
    );
    let earlier = COMPACT_MIGRATIONS[COMPACT_MIGRATIONS.len() - 3];
    assert!(
        earlier.contains("CREATE TABLE security_master(")
            && earlier.contains("CREATE TABLE price_limit_rules("),
        "migration 023 was not kept immutable before 024"
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain.db");
    SqliteTasks::new(&path).initialize_compact().unwrap();
    let c = Connection::open(&path).unwrap();
    let rows: Vec<(u32, String)> = c
        .prepare("SELECT version,checksum FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), COMPACT_SCHEMA_VERSION as usize);
    let chain: Vec<&str> = MIGRATIONS
        .iter()
        .chain(COMPACT_MIGRATIONS.iter())
        .copied()
        .collect();
    for (index, (version, checksum)) in rows.iter().enumerate() {
        assert_eq!(*version as usize, index + 1, "chain gap at {index}");
        assert_eq!(
            *checksum,
            crate::hash::sha256(chain[index].as_bytes()),
            "checksum mismatch at version {version}"
        );
    }
}

/// AC-M2-14 first half: the migration builds on both paths (fresh compact init
/// and the empty 18 -> compact upgrade) and creates the new tables empty.
#[test]
fn schema24_builds_on_fresh_init_and_empty_upgrade() {
    for fresh in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m2.db");
        let db = SqliteTasks::new(&path);
        if fresh {
            db.initialize_compact().unwrap();
        } else {
            db.initialize().unwrap();
            db.upgrade_compact().unwrap();
        }
        assert_compact_shape(&path);
        let c = Connection::open(&path).unwrap();
        for table in [
            "security_master",
            "code_migrations",
            "security_name_history",
            "capital_events",
            "risk_warning_intervals",
            "suspension_intervals",
            "price_limit_rules",
            "min_tick_rules",
        ] {
            let n: i64 = c
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} must be created empty (fresh={fresh})");
        }
        let broken: bool = c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!broken, "foreign key violations (fresh={fresh})");
        for version in [23, 24] {
            let rows: i64 = c
                .query_row(
                    "SELECT count(*) FROM schema_migrations WHERE version=?1",
                    [version],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(rows, 1, "migration {version:03} missing (fresh={fresh})");
        }
    }
}

/// M3 PR-1 (issue-446 §9.3): schema 25 builds on both init paths — the fresh
/// compact init and the empty 18 -> compact upgrade — with `user_version=25`,
/// the five index tables created empty, `price_limit_rules.min_band_ticks`
/// present and NULL everywhere, and clean foreign keys. The "24 -> 25" step
/// itself is fresh-init only (D-08); the separate refused-not-upgraded test
/// covers what a schema-24 file gets.
#[test]
fn schema25_builds_on_fresh_init_and_empty_upgrade() {
    for fresh in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m3.db");
        let db = SqliteTasks::new(&path);
        if fresh {
            db.initialize_compact().unwrap();
        } else {
            db.initialize().unwrap();
            db.upgrade_compact().unwrap();
        }
        assert_compact_shape(&path);
        assert_eq!(
            raw_user_version(&path),
            COMPACT_SCHEMA_VERSION,
            "user_version must be 25 (fresh={fresh})"
        );
        let c = Connection::open(&path).unwrap();
        for table in [
            "industry_snapshots",
            "industry_assignments",
            "exchange_source_snapshots",
            "generation_facts",
            "retire_exemptions",
        ] {
            let n: i64 = c
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} must be created empty (fresh={fresh})");
        }
        let floors: i64 = c
            .query_row(
                "SELECT count(*) FROM price_limit_rules WHERE min_band_ticks IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(floors, 0, "min_band_ticks must start NULL (fresh={fresh})");
        assert_eq!(
            raw_columns(&path, "price_limit_rules")
                .last()
                .map(String::as_str),
            Some("min_band_ticks"),
            "025 must append min_band_ticks instead of rebuilding the table (fresh={fresh})"
        );
        let broken: bool = c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!broken, "foreign key violations (fresh={fresh})");
        for version in [24, 25] {
            let rows: i64 = c
                .query_row(
                    "SELECT count(*) FROM schema_migrations WHERE version=?1",
                    [version],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(rows, 1, "migration {version:03} missing (fresh={fresh})");
        }
    }
}

/// A schema-24 file is neither upgraded in place nor served: the current
/// binary refuses it (D-08 fresh-init only), which is also the branch an older
/// binary takes on a schema-25 file (`version > COMPACT_SCHEMA_VERSION`).
#[test]
fn schema24_files_are_refused_not_upgraded_to_25() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v24.db");
    let db = SqliteTasks::new(&path);
    db.initialize_compact().unwrap();
    {
        let c = Connection::open(&path).unwrap();
        // A schema-24 file: migration rows 1..24 and user_version 24. The
        // migration identity check must accept the prefix (024 is untouched),
        // so the refusal comes from the version guard, not from a stale
        // checksum.
        c.execute("DELETE FROM schema_migrations WHERE version>24", [])
            .unwrap();
        c.pragma_update(None, "user_version", 24).unwrap();
    }
    let error = db.read(|_| Ok(())).unwrap_err().to_string();
    assert!(
        error.contains("unsupported/uninitialized schema 24"),
        "{error}"
    );
    let error = db.write(|_| Ok(())).unwrap_err().to_string();
    assert!(
        error.contains("unsupported/uninitialized schema 24"),
        "{error}"
    );
    let error = SqliteTasks::new(&path)
        .upgrade_compact()
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("requires exact schema 18"),
        "a schema-24 file took an upgrade path: {error}"
    );
    assert_eq!(
        raw_user_version(&path),
        24,
        "the schema-24 file was rewritten"
    );
}

/// AC-M3-3/5/9 storage surface: the unique keys, CHECKs and foreign key of the
/// five schema-25 tables reject duplicates, unregistered vocabulary and
/// impossible intervals. `industry_assignments` is the one that also encodes
/// the §9.3 rules: `effective_to > effective_from` and a NOT NULL `known_at`.
#[test]
fn schema25_constraints_reject_duplicates_and_bad_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m3-constraints.db");
    SqliteTasks::new(&path).initialize_compact().unwrap();
    let c = Connection::open(&path).unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    fn rejected(label: &str, r: rusqlite::Result<usize>) {
        assert!(r.is_err(), "{label} was accepted");
    }
    let member = hex32(0x61);
    let store = hex32(0x62);
    let generation = hex32(0x63);

    // industry_snapshots: content identity is the primary key.
    let snapshot = |source_kind: &str, grade: &str| {
        format!(
            "INSERT INTO industry_snapshots(member_sha256,store_sha256,\
             generation_manifest_sha256,source_kind,ingested_at,evidence_grade,\
             parser_version,row_count,scope_member_count) \
             VALUES(X'{member}',X'{store}',X'{generation}','{source_kind}',\
             '2026-10-29T00:00:00Z','{grade}','tdxhy-v1',10,10)"
        )
    };
    c.execute(&snapshot("generation", "B"), []).unwrap();
    rejected(
        "duplicate industry_snapshots member_sha256",
        c.execute(&snapshot("generation", "B"), []),
    );
    rejected(
        "unregistered industry_snapshots source_kind",
        c.execute(&snapshot("streamed", "B"), []),
    );
    rejected(
        "unregistered industry_snapshots evidence_grade",
        c.execute(&snapshot("generation", "D"), []),
    );

    // industry_assignments: PK, FK to the snapshot, half-open interval, NOT
    // NULL known_at and the closed vocabularies.
    fn assignment(
        exchange: &str,
        from: i64,
        to: &str,
        known_at: &str,
        from_basis: &str,
        name_status: &str,
        member: &str,
    ) -> String {
        format!(
            "INSERT INTO industry_assignments(exchange,code,scheme,level,industry_code,\
             name,name_status,effective_from,effective_to,known_at,effective_from_basis,\
             evidence_grade,snapshot_member_sha256) \
             VALUES('{exchange}','600000','tdx_research_industry',1,'T01','银行',\
             '{name_status}',{from},{to},'{known_at}','{from_basis}','B',X'{member}')"
        )
    }
    c.execute(
        &assignment(
            "sh",
            1000,
            "NULL",
            "2026-10-29T00:00:00Z",
            "publisher_last_modified",
            "resolved",
            &member,
        ),
        [],
    )
    .unwrap();
    rejected(
        "duplicate industry_assignments key",
        c.execute(
            &assignment(
                "sh",
                1000,
                "NULL",
                "2026-10-29T00:00:00Z",
                "publisher_last_modified",
                "resolved",
                &member,
            ),
            [],
        ),
    );
    rejected(
        "industry_assignments effective_to == effective_from",
        c.execute(
            &assignment(
                "sh",
                1100,
                "1100",
                "2026-10-29T00:00:00Z",
                "publisher_last_modified",
                "resolved",
                &member,
            ),
            [],
        ),
    );
    rejected(
        "industry_assignments effective_to < effective_from",
        c.execute(
            &assignment(
                "sh",
                1100,
                "1099",
                "2026-10-29T00:00:00Z",
                "publisher_last_modified",
                "resolved",
                &member,
            ),
            [],
        ),
    );
    rejected(
        "industry_assignments NULL known_at",
        c.execute(
            &assignment(
                "sh",
                1100,
                "NULL",
                "NULL",
                "publisher_last_modified",
                "resolved",
                &member,
            ),
            [],
        ),
    );
    rejected(
        "industry_assignments missing snapshot (foreign key)",
        c.execute(
            &assignment(
                "sh",
                1100,
                "NULL",
                "2026-10-29T00:00:00Z",
                "publisher_last_modified",
                "resolved",
                &hex32(0x64),
            ),
            [],
        ),
    );
    rejected(
        "unregistered industry_assignments exchange",
        c.execute(
            &assignment(
                "bj",
                1100,
                "NULL",
                "2026-10-29T00:00:00Z",
                "publisher_last_modified",
                "resolved",
                &member,
            ),
            [],
        ),
    );
    rejected(
        "unregistered industry_assignments effective_from_basis",
        c.execute(
            &assignment(
                "sh",
                1100,
                "NULL",
                "2026-10-29T00:00:00Z",
                "guess",
                "resolved",
                &member,
            ),
            [],
        ),
    );
    rejected(
        "unregistered industry_assignments name_status",
        c.execute(
            &assignment(
                "sh",
                1100,
                "NULL",
                "2026-10-29T00:00:00Z",
                "publisher_last_modified",
                "pending",
                &member,
            ),
            [],
        ),
    );

    // exchange_source_snapshots: snapshot_id (byte sha) is the primary key;
    // the UNIQUE includes it, so re-freezing the same request with different
    // bytes is allowed while a duplicate index row is not.
    let exchange = |snapshot_id: &str, store_kind: &str, basis_status: &str| {
        format!(
            "INSERT INTO exchange_source_snapshots(snapshot_id,store_sha256,store_kind,\
             source_id,url,request_fingerprint,fetched_at,as_of,size_bytes,row_count,\
             parser_version,basis_status) \
             VALUES('{snapshot_id}',X'{store}','{store_kind}','sse_listing',\
             'https://example.invalid/list','fp-1','2026-10-29T00:00:00Z',\
             '2026-10-29',100,10,'sse-listing-v1','{basis_status}')"
        )
    };
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    c.execute(&exchange(&first, "http_fetch", "approved_source"), [])
        .unwrap();
    rejected(
        "duplicate exchange_source_snapshots snapshot_id",
        c.execute(&exchange(&first, "http_fetch", "approved_source"), []),
    );
    c.execute(
        &exchange(&second, "archive_import", "owner_risk_accepted"),
        [],
    )
    .unwrap();
    rejected(
        "unregistered exchange_source_snapshots store_kind",
        c.execute(
            &exchange(&"c".repeat(64), "streamed", "approved_source"),
            [],
        ),
    );
    rejected(
        "unregistered exchange_source_snapshots basis_status",
        c.execute(&exchange(&"d".repeat(64), "http_fetch", "pending"), []),
    );

    // generation_facts: one fact per (generation, role, kind).
    let fact = |role: &str, kind: &str| {
        format!(
            "INSERT INTO generation_facts(generation_manifest_sha256,role,fact_kind,\
             fact_digest,registered_at) \
             VALUES(X'{generation}','{role}','{kind}',X'{store}','2026-10-29T00:00:00Z')"
        )
    };
    c.execute(&fact("base.zip", "security_master"), []).unwrap();
    rejected(
        "duplicate generation_facts key",
        c.execute(&fact("base.zip", "security_master"), []),
    );
    rejected(
        "unregistered generation_facts role",
        c.execute(&fact("day.zip", "security_master"), []),
    );

    // retire_exemptions: an exemption must carry a non-empty reason.
    let exemption = |kind: &str, reason: &str| {
        format!(
            "INSERT INTO retire_exemptions(generation_manifest_sha256,missing_kind,\
             reason,recorded_by,recorded_at) \
             VALUES(X'{generation}','{kind}','{reason}','owner','2026-10-29T00:00:00Z')"
        )
    };
    c.execute(&exemption("industry_snapshots", "owner accepted"), [])
        .unwrap();
    rejected(
        "duplicate retire_exemptions key",
        c.execute(&exemption("industry_snapshots", "owner accepted"), []),
    );
    rejected(
        "empty retire_exemptions reason",
        c.execute(&exemption("generation_facts", ""), []),
    );
}

/// D-50 migration proof: 024 rebuilds `capital_events` and copies a schema-23
/// row verbatim. The fresh-init path applies 023 and 024 back to back on an
/// empty table, so this test stops at 023, inserts a row, then applies 024.
#[test]
fn schema24_rebuild_copies_capital_rows_and_keeps_the_columns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rebuild.db");
    SqliteTasks::new(&path).initialize().unwrap();
    let c = Connection::open(&path).unwrap();
    c.pragma_update(None, "foreign_keys", false).unwrap();
    for sql in COMPACT_MIGRATIONS.iter().take(5) {
        c.execute_batch(sql).unwrap();
    }
    c.pragma_update(None, "foreign_keys", true).unwrap();
    let evidence = hex32(0x11);
    let binding = hex32(0x22);
    c.execute(
        &format!("INSERT INTO evidence_bundles VALUES(X'{evidence}','derived','{{}}')"),
        [],
    )
    .unwrap();
    c.execute(
        &format!(
            "INSERT INTO source_bindings VALUES(X'{binding}','gen-1',X'{}',X'{}',X'{}',X'{evidence}')",
            hex32(0x33),
            hex32(0x44),
            hex32(0x55)
        ),
        [],
    )
    .unwrap();
    let capital = |ordinal: i64, category: i64| {
        format!(
            "INSERT INTO capital_events(exchange,code,ordinal,effective_day,category,\
             float_before_bits,total_before_bits,float_after_bits,total_after_bits,\
             float_after,total_after,source_binding_digest,evidence_digest) \
             VALUES('sz','000001',{ordinal},1000,{category},0,0,0,0,{ordinal},{ordinal},\
             X'{binding}',X'{evidence}')"
        )
    };
    c.execute(&capital(0, 2), []).unwrap();
    c.execute_batch(COMPACT_MIGRATIONS[5]).unwrap();
    let copied: (i64, i64, i64) = c
        .query_row(
            "SELECT ordinal,category,float_after FROM capital_events \
             WHERE exchange='sz' AND code='000001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(copied, (0, 2, 0), "the schema-23 row was not copied");
    c.execute(&capital(1, 5), []).unwrap();
    assert!(c.execute(&capital(2, 1), []).is_err());
    assert_eq!(
        raw_columns(&path, "capital_events"),
        vec![
            "exchange",
            "code",
            "ordinal",
            "effective_day",
            "category",
            "float_before_bits",
            "total_before_bits",
            "float_after_bits",
            "total_after_bits",
            "float_after",
            "total_after",
            "prev_is_lagged",
            "suspicious",
            "known_at",
            "source_binding_digest",
            "evidence_digest",
        ],
        "024 changed a capital_events column"
    );
}

/// AC-M2-14 second half: the previous compact revision (schema 23) is refused
/// and left untouched (no in-place rewrite), and a file one revision ahead of
/// this binary hits the same `version > COMPACT_SCHEMA_VERSION` guard — which
/// is exactly the branch an older binary takes on a schema-24 file.
#[test]
fn schema24_refuses_foreign_schema_files_without_rewriting_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v23.db");
    let db = SqliteTasks::new(&path);
    db.initialize_compact().unwrap();
    {
        let c = Connection::open(&path).unwrap();
        c.execute("DELETE FROM schema_migrations WHERE version>23", [])
            .unwrap();
        c.pragma_update(None, "user_version", 23).unwrap();
    }
    let e = db.read(|_| Ok(())).unwrap_err().to_string();
    assert!(e.contains("unsupported/uninitialized schema 23"), "{e}");
    let e = db.write(|_| Ok(())).unwrap_err().to_string();
    assert!(e.contains("unsupported/uninitialized schema 23"), "{e}");
    assert_eq!(
        raw_user_version(&path),
        23,
        "the schema-23 file was rewritten"
    );
    {
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "user_version", COMPACT_SCHEMA_VERSION + 1)
            .unwrap();
    }
    let e = db.read(|_| Ok(())).unwrap_err().to_string();
    assert!(
        e.contains(&format!(
            "unsupported/uninitialized schema {}",
            COMPACT_SCHEMA_VERSION + 1
        )),
        "{e}"
    );
    assert_eq!(
        raw_user_version(&path),
        COMPACT_SCHEMA_VERSION + 1,
        "a newer schema file was rewritten"
    );
}

/// AC-M2-14 third part: the unique keys and CHECKs of the schema-24 tables
/// reject duplicates and impossible rows, including the D-50 capital-category
/// vocabulary and the extended `derived_dataset_versions.dataset_kind` list.
#[test]
fn schema24_constraints_reject_duplicates_and_bad_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("constraints.db");
    let db = SqliteTasks::new(&path);
    db.initialize_compact().unwrap();
    let c = Connection::open(&path).unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    let evidence = hex32(0x11);
    let binding = hex32(0x22);
    c.execute(
        &format!("INSERT INTO evidence_bundles VALUES(X'{evidence}','derived','{{}}')"),
        [],
    )
    .unwrap();
    c.execute(
        &format!(
            "INSERT INTO source_bindings VALUES(X'{binding}','gen-1',X'{}',X'{}',X'{}',X'{evidence}')",
            hex32(0x33),
            hex32(0x44),
            hex32(0x55)
        ),
        [],
    )
    .unwrap();
    fn rejected(label: &str, r: rusqlite::Result<usize>) {
        assert!(r.is_err(), "{label} was accepted");
    }
    let master = |exchange: &str,
                  code: &str,
                  from: i64,
                  to: &str,
                  listing: &str,
                  listing_source: &str,
                  evidence: &str| {
        format!(
            "INSERT INTO security_master(exchange,code,effective_from,effective_to,\
             instrument_class,issuer_resolution,listing_date,listing_source,\
             delisting_source,delisting_reason_class,listing_conflict,\
             delisting_conflict,source_binding_digest,evidence_digest) \
             VALUES('{exchange}','{code}',{from},{to},0,'code_date',{listing},\
             '{listing_source}','unknown','other',0,0,X'{binding}',X'{evidence}')"
        )
    };
    c.execute(
        &master(
            "sh",
            "600000",
            1000,
            "NULL",
            "9000",
            "exchange_list",
            &evidence,
        ),
        [],
    )
    .unwrap();
    rejected(
        "duplicate security_master interval",
        c.execute(
            &master(
                "sh",
                "600000",
                1000,
                "NULL",
                "9000",
                "exchange_list",
                &evidence,
            ),
            [],
        ),
    );
    rejected(
        "exchange outside (sh,sz,bj)",
        c.execute(
            &master(
                "xx",
                "600001",
                1000,
                "NULL",
                "9000",
                "exchange_list",
                &evidence,
            ),
            [],
        ),
    );
    rejected(
        "effective_to not after effective_from",
        c.execute(
            &master(
                "sh",
                "600002",
                1000,
                "1000",
                "9000",
                "exchange_list",
                &evidence,
            ),
            [],
        ),
    );
    rejected(
        "listing_date with listing_source='unknown'",
        c.execute(
            &master("sh", "600003", 1000, "NULL", "9000", "unknown", &evidence),
            [],
        ),
    );
    rejected(
        "evidence digest with no evidence bundle",
        c.execute(
            &master(
                "sh",
                "600004",
                1000,
                "NULL",
                "9000",
                "exchange_list",
                &hex32(0x99),
            ),
            [],
        ),
    );
    let capital = |code: &str, ordinal: i64, category: i64, after: i64| {
        format!(
            "INSERT INTO capital_events(exchange,code,ordinal,effective_day,category,\
             float_before_bits,total_before_bits,float_after_bits,total_after_bits,\
             float_after,total_after,source_binding_digest,evidence_digest) \
             VALUES('sz','{code}',{ordinal},1000,{category},0,0,0,0,{after},{after},\
             X'{binding}',X'{evidence}')"
        )
    };
    c.execute(&capital("000001", 0, 2, 100), []).unwrap();
    rejected(
        "duplicate capital_events (exchange,code,ordinal)",
        c.execute(&capital("000001", 0, 2, 100), []),
    );
    for (ordinal, category) in [(1, 5), (2, 7), (3, 8)] {
        c.execute(&capital("000001", ordinal, category, 100), [])
            .unwrap_or_else(|error| panic!("D-50 category {category} rejected: {error}"));
    }
    rejected(
        "capital_events category outside the D-50 admission set",
        c.execute(&capital("000001", 4, 1, 100), []),
    );
    rejected(
        "negative capital_events shares",
        c.execute(&capital("000001", 5, 2, -1), []),
    );
    let suspension = |code: &str, start: i64, end: i64, missing: i64| {
        format!(
            "INSERT INTO suspension_intervals(exchange,code,start_day,end_day,\
             certainty,missing_days,reason,source_kind,evidence_digest) \
             VALUES('sz','{code}',{start},{end},'unresolved_gap',{missing},\
             'missing_bar_no_event','missing_bar',X'{evidence}')"
        )
    };
    c.execute(&suspension("000631", 100, 200, 5), []).unwrap();
    rejected(
        "suspension with missing_days=0",
        c.execute(&suspension("000632", 100, 200, 0), []),
    );
    rejected(
        "suspension ending before it starts",
        c.execute(&suspension("000633", 200, 100, 1), []),
    );
    let rule = |id: &str, phase: &str, band: &str, side: &str, from: i64, basis: &str| {
        format!(
            "INSERT INTO price_limit_rules(rule_id,market,board,status_class,\
             listing_phase,band_bp,band_side,rounding,effective_from,basis_ref,\
             basis_status,evidence_digest) VALUES('{id}','sz','gem','normal',\
             '{phase}',{band},'{side}','half_up',{from},'{basis}','verified',\
             X'{evidence}')"
        )
    };
    c.execute(
        &rule("r1", "seasoned", "2000", "both", 1000, "url#clause"),
        [],
    )
    .unwrap();
    rejected(
        "duplicate rule scope and effective date",
        c.execute(
            &rule("r1b", "seasoned", "2000", "both", 1000, "url#clause"),
            [],
        ),
    );
    rejected(
        "band_side='none' carrying a band",
        c.execute(
            &rule("r2", "first_day", "2000", "none", 1000, "url#clause"),
            [],
        ),
    );
    rejected(
        "bounded side without a band",
        c.execute(
            &rule("r3", "first_5_days", "NULL", "both", 1000, "url#clause"),
            [],
        ),
    );
    rejected(
        "rule without a basis_ref",
        c.execute(&rule("r4", "all", "2000", "both", 1000, ""), []),
    );
    c.execute(
        &rule("r5", "first_day", "NULL", "none", 1000, "url#clause"),
        [],
    )
    .unwrap();
    let tick = |id: &str, from: i64, units: i64| {
        format!(
            "INSERT INTO min_tick_rules(rule_id,market,board,effective_from,tick_units,\
             price_scale,basis_ref,basis_status,evidence_digest) \
             VALUES('{id}','all','all',{from},{units},2,'url#tick','verified',X'{evidence}')"
        )
    };
    c.execute(&tick("t1", 0, 1), []).unwrap();
    rejected(
        "duplicate min_tick_rules (market,board,effective_from)",
        c.execute(&tick("t2", 0, 1), []),
    );
    rejected(
        "min_tick_rules with zero tick",
        c.execute(&tick("t3", 1, 0), []),
    );
    let dataset = |kind: &str, content: &str, version: &str| {
        format!(
            "INSERT INTO derived_dataset_versions(dataset_kind,dataset_key,\
             version_id,content_digest,digest_format,record_count,first_day,\
             last_day,algorithm_version,source_binding_digest,evidence_digest,\
             task_id,item_id,created_at,updated_at) \
             VALUES('{kind}','key','{version}',X'{content}','security-master-v1',10,\
             1000,2000,'security-master-v1',X'{binding}',X'{evidence}',\
             'task','item','2026-10-26T00:00:00Z','2026-10-26T00:00:00Z')"
        )
    };
    // The schema-22 vocabulary stays valid (the rebuild is a strict superset),
    // the six M2 kinds are accepted, and an unregistered kind fails closed.
    c.execute(&dataset("trading_calendar", &hex32(0x61), &hex32(0x61)), [])
        .unwrap();
    c.execute(&dataset("security_master", &hex32(0x62), &hex32(0x62)), [])
        .unwrap();
    rejected(
        "unregistered dataset_kind",
        c.execute(
            &dataset("security_master_v9", &hex32(0x63), &hex32(0x63)),
            [],
        ),
    );
    rejected(
        "version_id not equal to the content digest",
        c.execute(&dataset("suspensions", &hex32(0x64), &hex32(0x65)), []),
    );
}

#[test]
fn upgrade_compact_rejects_any_populated_business_table() {
    for populate in ["market", "task"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("populated.db");
        let db = SqliteTasks::new(&path);
        db.initialize().unwrap();
        match populate {
            "market" => {
                db.write(|c| {
                    c.execute("INSERT INTO payloads VALUES('p','{}')", [])
                        .map_err(storage)
                })
                .unwrap();
            }
            _ => {
                accept(SqliteTasks::new(&path));
            }
        }
        let e = db.upgrade_compact().unwrap_err().to_string();
        assert!(e.contains("FRESH_REBUILD_REQUIRED"), "{populate}: {e}");
        assert_legacy_shape(&path);
        if populate == "market" {
            let n: i64 = db
                .read(|c| {
                    c.query_row("SELECT count(*) FROM payloads", [], |r| r.get(0))
                        .map_err(storage)
                })
                .unwrap();
            assert_eq!(n, 1);
        }
    }
}

#[test]
fn upgrade_stays_schema18_and_never_applies_compact_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    {
        std::fs::File::create(&path).unwrap();
        let mut c = Connection::open(&path).unwrap();
        c.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        for (index, sql) in MIGRATIONS.iter().enumerate().take(17) {
            apply_migration(&tx, index + 1, sql).unwrap();
        }
        tx.commit().unwrap();
    }
    assert_eq!(raw_user_version(&path), 17);
    let db = SqliteTasks::new(&path);
    db.upgrade().unwrap();
    assert_eq!(raw_user_version(&path), SCHEMA_VERSION);
    assert!(raw_columns(&path, "data_versions").contains(&"envelope_json".to_string()));
    db.upgrade().unwrap();
    assert_legacy_shape(&path);
}

// ---- XP-025 A: bounded accept gate + full registry sweep ----

fn fresh() -> (tempfile::TempDir, SqliteTasks) {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("task.sqlite"));
    db.initialize().unwrap();
    (dir, db)
}
/// Inserts `n` self-consistent terminal tasks (request, item, scope), the
/// shape accept leaves behind for completed history.
fn seed_terminal(db: &SqliteTasks, n: usize) {
    let mut c = Connection::open(db.path()).unwrap();
    let tx = c.transaction().unwrap();
    for i in 0..n {
        let id = format!("hist-{i}");
        let key = r#"{"operation":"collect_data","dataset":"legacy_market_compound","contract_version":"v1"}"#;
        let spec = serde_json::json!({"item_id":"i","target":{},"progress_unit":"u","frozen_inputs":{},"scope_keys":["s"]});
        let work = serde_json::json!({"items":[spec],"scopes":["s"]}).to_string();
        tx.execute(
            "INSERT INTO tasks(task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of,item_count,items_pending,items_running,items_finished,items_succeeded) VALUES(?1,?2,'succeeded',3,'seed-run','seed-run','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z',?3,'{}',NULL,1,0,0,1,1)",
            params![id, key, work],
        )
        .unwrap();
        let command = r#"{"key":{},"intent":{}}"#;
        tx.execute(
            "INSERT INTO requests VALUES(?1,?2,?3,'task',?4,NULL)",
            params![
                format!("req-{id}"),
                command,
                crate::hash::sha256(command.as_bytes()),
                id
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO task_items VALUES(?1,'i','{}','u','{}','[\"s\"]','finished','{\"success\":true}')",
            params![id],
        )
        .unwrap();
        tx.execute("INSERT INTO task_scopes VALUES(?1,'s')", params![id])
            .unwrap();
    }
    tx.commit().unwrap();
}
fn accept_new(db: &SqliteTasks, request: &str) -> Result<Acceptance, Error> {
    TaskControl::new(
        db.clone(),
        "test-run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap()
    .accept(
        request,
        LegacyCollection.key(),
        serde_json::json!({"symbols":[request.replace('-', "")],"periods":["day"]}),
    )
}
#[test]
fn active_and_terminal_status_sets_partition_the_schema_enum() {
    let (_dir, db) = fresh();
    let c = Connection::open(db.path()).unwrap();
    let ddl: String = c
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='tasks'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let start = ddl.find("status IN (").unwrap() + "status IN (".len();
    let list = &ddl[start..start + ddl[start..].find(')').unwrap()];
    let schema: std::collections::BTreeSet<String> = list
        .split(',')
        .map(|s| s.trim().trim_matches('\'').to_string())
        .collect();
    let active: std::collections::BTreeSet<String> =
        ACTIVE_STATUSES.iter().map(|s| s.to_string()).collect();
    let terminal: std::collections::BTreeSet<String> =
        TERMINAL_STATUSES.iter().map(|s| s.to_string()).collect();
    assert!(active.is_disjoint(&terminal));
    assert_eq!(
        active
            .union(&terminal)
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        schema
    );
}
#[test]
fn accept_gate_is_six_bounded_statements_without_item_or_request_scans() {
    let (_dir, db) = fresh();
    seed_terminal(&db, 50);
    let c = Connection::open(db.path()).unwrap();
    let gates = registry_gates(GateScope::Active);
    assert_eq!(
        gates.len(),
        5,
        "5 SQL gates + 1 digest select = 6 statements"
    );
    let mut sqls: Vec<String> = gates.into_iter().map(|g| g.sql).collect();
    sqls.push(digest_sql(GateScope::Active));
    for sql in sqls {
        let mut q = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan: Vec<String> = q
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for detail in &plan {
            if let Some(rest) = detail.strip_prefix("SCAN ") {
                let table = rest.split_whitespace().next().unwrap_or("");
                assert!(
                    table == "t" || table == "o" || table == "CONSTANT",
                    "unbounded scan `{detail}` in gate: {sql}"
                );
            }
        }
    }
}
#[test]
fn accept_work_does_not_grow_with_terminal_history() {
    let mut digests = vec![];
    for history in [10usize, 500] {
        let (_dir, db) = fresh();
        seed_terminal(&db, history);
        accept_new(&db, "live-1").unwrap();
        DIGESTS_CHECKED.with(|n| n.set(0));
        accept_new(&db, "live-2").unwrap();
        digests.push(DIGESTS_CHECKED.with(|n| n.get()));
    }
    assert_eq!(
        digests[0], digests[1],
        "digest work must not depend on history"
    );
    assert_eq!(
        digests[0], 1,
        "only the one in-flight task's request is hashed"
    );
}
#[test]
fn historical_corruption_does_not_block_accept_but_sweep_reports_it() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "unbound terminal task",
            "DELETE FROM requests WHERE task_id='hist-0'",
            "accept.gate.orphan",
        ),
        (
            "request bound to missing task",
            "INSERT INTO requests VALUES('ghost','{}', 'x','task','missing-task',NULL)",
            "sweep.request_task",
        ),
        (
            "terminal counters diverge",
            "UPDATE tasks SET items_pending=5 WHERE task_id='hist-0'",
            "accept.gate.item_counters",
        ),
        (
            "terminal without result",
            "UPDATE tasks SET result_json=NULL WHERE task_id='hist-0'",
            "sweep.terminal",
        ),
        (
            "terminal owner/stage",
            "UPDATE tasks SET owner_run_id=NULL WHERE task_id='hist-0'",
            "accept.gate.owner_stage",
        ),
        (
            "history digest",
            "UPDATE requests SET command_digest='wrong' WHERE task_id='hist-0'",
            "sweep.digest",
        ),
    ];
    for (name, tamper, expected) in cases {
        let (_dir, db) = fresh();
        seed_terminal(&db, 3);
        assert!(
            db.registry_sweep().unwrap().is_empty(),
            "{name}: clean before"
        );
        let c = Connection::open(db.path()).unwrap();
        c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        c.execute_batch(tamper).unwrap();
        accept_new(&db, "live-1").unwrap_or_else(|e| panic!("{name}: accept blocked: {e:?}"));
        let findings = db.registry_sweep().unwrap();
        assert!(
            findings.iter().any(|f| f.check == *expected),
            "{name}: expected {expected}, got {findings:?}"
        );
        // The read-only CLI sweep reports the same findings.
        assert_eq!(db.registry_sweep_readonly().unwrap(), findings, "{name}");
    }
}
#[test]
fn in_flight_corruption_still_rejects_accept() {
    for tamper in [
        "DELETE FROM scope_occupancy",
        "DELETE FROM task_scopes",
        "UPDATE tasks SET items_pending=items_pending+1 WHERE status='pending'",
        "UPDATE tasks SET owner_run_id='x' WHERE status='pending'",
        "UPDATE requests SET command_digest='wrong' WHERE task_id IN (SELECT task_id FROM tasks WHERE status='pending')",
        "DELETE FROM requests",
        "UPDATE tasks SET status='succeeded',result_json='{}'",
    ] {
        let (_dir, db) = fresh();
        seed_terminal(&db, 2);
        accept_new(&db, "live-1").unwrap();
        let c = Connection::open(db.path()).unwrap();
        c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        c.execute_batch(tamper).unwrap();
        assert!(accept_new(&db, "live-2").is_err(), "{tamper}");
    }
}
