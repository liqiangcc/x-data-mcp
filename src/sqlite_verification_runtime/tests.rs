use super::*;
use crate::{
    task_control::{
        Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection, service::TaskControl,
    },
    task_verification::StopVerification,
    verification_execution::prepare_claim,
    verification_runtime::{create, try_acquire},
};
fn fixture() -> (tempfile::TempDir, SqliteTasks, TaskSnapshot, TaskSnapshot) {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("claim.sqlite"));
    db.initialize().unwrap();
    let old = TaskControl::new(db.clone(), "old".into(), vec![Box::new(LegacyCollection)]).unwrap();
    let RequestOutcome::Task(original) = old
        .accept(
            "original",
            LegacyCollection.key(),
            serde_json::json!({"symbols":["sh.600006"],"periods":["day"]}),
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
            serde_json::json!({"target_task_id":original.task.task_id}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    (dir, db, *original, *check)
}
#[test]
fn runtime_claim_is_atomic_and_registration_is_bound_to_actual_owner() {
    let (_dir, db, original, check) = fixture();
    let guard = create(db.path(), &check.task.task_id, "check-run").unwrap();
    let plan = prepare_claim(
        check.clone(),
        guard.descriptor().clone(),
        chrono::Utc::now(),
    )
    .unwrap();
    let c = Connection::open(db.path()).unwrap();
    c.execute_batch("CREATE TRIGGER reject_guard BEFORE INSERT ON verification_runtime_guards BEGIN SELECT RAISE(ABORT,'fixture registration failure'); END;").unwrap();
    assert!(db.claim_with_runtime_guard(&plan, &guard).is_err());
    assert_eq!(
        db.read_task(&check.task.task_id).unwrap(),
        Some(check.clone())
    );
    assert!(
        db.runtime_registration(&check.task.task_id)
            .unwrap()
            .is_none()
    );
    c.execute_batch("DROP TRIGGER reject_guard").unwrap();
    let claimed = db.claim_with_runtime_guard(&plan, &guard).unwrap();
    assert_eq!(&claimed, plan.claimed());
    let registration = db
        .runtime_registration(&check.task.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(registration.descriptor, *guard.descriptor());
    assert_eq!(registration.claim_revision, claimed.task.state_revision);
    assert!(db.claim_with_runtime_guard(&plan, &guard).is_err());
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
    c.execute(
        "UPDATE verification_runtime_guards SET descriptor_sha256='damaged'",
        [],
    )
    .unwrap();
    assert!(db.runtime_registration(&check.task.task_id).is_err());
}
#[test]
fn runtime_claim_rejects_nonverification_task_wrong_guard_and_stale_state() {
    let (_dir, db, original, check) = fixture();
    let business_guard = create(db.path(), &original.task.task_id, "old").unwrap();
    assert!(matches!(
        prepare_claim(
            original,
            business_guard.descriptor().clone(),
            chrono::Utc::now()
        ),
        Err(Error::UnsupportedContract)
    ));
    let guard = create(db.path(), &check.task.task_id, "check-run").unwrap();
    let plan = prepare_claim(
        check.clone(),
        guard.descriptor().clone(),
        chrono::Utc::now(),
    )
    .unwrap();
    assert!(db.claim_with_runtime_guard(&plan, &business_guard).is_err());
    let cancelled = db
        .transition(
            &check.task.task_id,
            0,
            "check-run",
            crate::task_control::Change::RequestCancel,
        )
        .unwrap();
    assert!(db.claim_with_runtime_guard(&plan, &guard).is_err());
    assert_eq!(db.read_task(&check.task.task_id).unwrap(), Some(cancelled));
    assert!(
        db.runtime_registration(&check.task.task_id)
            .unwrap()
            .is_none()
    );
}
#[test]
fn runtime_claim_child() {
    let Ok(path) = std::env::var("X_DATA_MCP_TEST_RUNTIME_CLAIM_DB") else {
        return;
    };
    let db = SqliteTasks::new(path);
    let id = std::env::var("X_DATA_MCP_TEST_RUNTIME_CLAIM_TASK").unwrap();
    let check = db.read_task(&id).unwrap().unwrap();
    let guard = create(db.path(), &id, "check-run").unwrap();
    let plan = prepare_claim(check, guard.descriptor().clone(), chrono::Utc::now()).unwrap();
    db.claim_with_runtime_guard(&plan, &guard).unwrap();
    panic!("expected real commit exit");
}
#[test]
fn actual_exit_during_runtime_claim_never_leaves_half_registered_execution() {
    for point in ["before_commit", "after_commit"] {
        let (_dir, db, original, check) = fixture();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sqlite_verification_runtime::tests::runtime_claim_child",
                "--nocapture",
            ])
            .env("X_DATA_MCP_TEST_RUNTIME_CLAIM_DB", db.path())
            .env("X_DATA_MCP_TEST_RUNTIME_CLAIM_TASK", &check.task.task_id)
            .env("X_DATA_MCP_TEST_SQLITE_CRASH", point)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(78));
        let registration = db.runtime_registration(&check.task.task_id).unwrap();
        if point == "before_commit" {
            assert!(registration.is_none());
            assert_eq!(
                db.read_task(&check.task.task_id).unwrap(),
                Some(check.clone())
            );
        } else {
            let registration = registration.unwrap();
            let claimed = db.read_task(&check.task.task_id).unwrap().unwrap();
            assert_eq!(claimed.task.status, crate::task_control::Status::Running);
            assert_eq!(claimed.task.state_revision, registration.claim_revision);
            let ended = try_acquire(db.path(), &registration.descriptor).unwrap();
            ended.verify_binding(db.path()).unwrap();
        }
        assert_eq!(
            db.read_task(&original.task.task_id).unwrap(),
            Some(original)
        );
    }
}

#[test]
fn guarded_runner_keeps_exclusion_through_evidence_and_commit() {
    use crate::task_verification::decision::{Observation, StopInspector};
    struct Probe {
        db: SqliteTasks,
        check: String,
    }
    impl StopInspector for Probe {
        fn inspect(&self, _: &TaskSnapshot) -> Result<Observation, Error> {
            let registration = self.db.runtime_registration(&self.check)?.unwrap();
            assert!(matches!(
                try_acquire(self.db.path(), &registration.descriptor),
                Err(crate::verification_runtime::Error::Busy)
            ));
            Ok(Observation::Insufficient {
                reason: "fixture has no business writer proof".into(),
            })
        }
    }
    let (_dir, db, original, check) = fixture();
    {
        let tasks =
            super::guarded::GuardedTasks::open(db.clone(), &check.task.task_id, "check-run")
                .unwrap();
        let runner = crate::task_verification::runner::StopRunner {
            repository: tasks,
            run_id: "check-run".into(),
            actor: "test-owner".into(),
        };
        runner
            .run(
                &check.task.task_id,
                &Probe {
                    db: db.clone(),
                    check: check.task.task_id.clone(),
                },
            )
            .unwrap();
        let registration = db
            .runtime_registration(&check.task.task_id)
            .unwrap()
            .unwrap();
        assert!(matches!(
            try_acquire(db.path(), &registration.descriptor),
            Err(crate::verification_runtime::Error::Busy)
        ));
        assert!(
            db.read_task(&check.task.task_id)
                .unwrap()
                .unwrap()
                .task
                .status
                .terminal()
        );
        assert_eq!(
            db.read_task(&original.task.task_id).unwrap(),
            Some(original)
        );
    }
    let registration = db
        .runtime_registration(&check.task.task_id)
        .unwrap()
        .unwrap();
    try_acquire(db.path(), &registration.descriptor).unwrap();
}

#[test]
fn guarded_repository_cannot_transition_another_task_or_claim_twice() {
    let (_dir, db, original, check) = fixture();
    let tasks =
        super::guarded::GuardedTasks::open(db.clone(), &check.task.task_id, "check-run").unwrap();
    assert!(
        tasks
            .transition(
                &original.task.task_id,
                0,
                "old",
                crate::task_control::Change::Claim
            )
            .is_err()
    );
    assert!(
        super::guarded::GuardedTasks::open(db.clone(), &check.task.task_id, "check-run").is_err()
    );
    tasks
        .transition(
            &check.task.task_id,
            0,
            "check-run",
            crate::task_control::Change::Claim,
        )
        .unwrap();
    assert!(
        db.transition(
            &check.task.task_id,
            0,
            "check-run",
            crate::task_control::Change::Claim
        )
        .is_err()
    );
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
}

#[test]
fn guarded_runner_error_retains_claims_but_releases_only_runtime_lock_on_return() {
    use crate::task_verification::decision::{Observation, StopInspector};
    struct Failing;
    impl StopInspector for Failing {
        fn inspect(&self, _: &TaskSnapshot) -> Result<Observation, Error> {
            Err(Error::StorageUnavailable(
                "fixture evidence read failure".into(),
            ))
        }
    }
    let (_dir, db, original, check) = fixture();
    {
        let runner = crate::task_verification::runner::StopRunner {
            repository: super::guarded::GuardedTasks::open(
                db.clone(),
                &check.task.task_id,
                "check-run",
            )
            .unwrap(),
            run_id: "check-run".into(),
            actor: "test-owner".into(),
        };
        assert!(runner.run(&check.task.task_id, &Failing).is_err());
        let registration = db
            .runtime_registration(&check.task.task_id)
            .unwrap()
            .unwrap();
        assert!(matches!(
            try_acquire(db.path(), &registration.descriptor),
            Err(crate::verification_runtime::Error::Busy)
        ));
    }
    let registration = db
        .runtime_registration(&check.task.task_id)
        .unwrap()
        .unwrap();
    let _ended = try_acquire(db.path(), &registration.descriptor).unwrap();
    let retained = db.read_task(&check.task.task_id).unwrap().unwrap();
    assert!(!retained.task.status.terminal());
    assert_eq!(retained.claims.len(), check.claims.len());
    for (actual, before) in retained.claims.iter().zip(&check.claims) {
        assert_eq!(actual.scope_key, before.scope_key);
        assert_eq!(actual.task_id, before.task_id);
        assert_eq!(actual.state_revision, retained.task.state_revision);
    }
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
}

#[test]
fn pending_verification_cancellation_does_not_invent_runtime_claim() {
    let (_dir, db, original, check) = fixture();
    db.transition(
        &check.task.task_id,
        0,
        "check-run",
        crate::task_control::Change::RequestCancel,
    )
    .unwrap();
    let runner = crate::task_verification::runner::StopRunner {
        repository: super::guarded::GuardedTasks::open(
            db.clone(),
            &check.task.task_id,
            "check-run",
        )
        .unwrap(),
        run_id: "check-run".into(),
        actor: "test-owner".into(),
    };
    runner
        .run(
            &check.task.task_id,
            &crate::task_verification::decision::UnverifiedStopInspector,
        )
        .unwrap();
    assert!(
        db.runtime_registration(&check.task.task_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.read_task(&check.task.task_id)
            .unwrap()
            .unwrap()
            .task
            .status,
        crate::task_control::Status::Cancelled
    );
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap(),
        Some(original)
    );
}
