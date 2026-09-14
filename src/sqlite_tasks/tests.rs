//! Test-only process/snapshot seams: absent from library and production binaries.
use super::*;
use crate::task_control::{legacy::LegacyCollection, service::TaskControl};
use crate::task_verification::{
    StopVerification,
    decision::{self, Observation, StopInspector, StopRepository},
};
use std::{cell::RefCell, process::Command as ProcessCommand};
thread_local! {
    static AFTER_ROW: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}
pub(super) fn after_task_row() {
    let hook = AFTER_ROW.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
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
