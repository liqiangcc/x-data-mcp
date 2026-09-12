//! Test-only process/snapshot seams: absent from library and production binaries.
use super::*;
use crate::task_control::{legacy::LegacyCollection, service::TaskControl};
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
