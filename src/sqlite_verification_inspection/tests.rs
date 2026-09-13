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
