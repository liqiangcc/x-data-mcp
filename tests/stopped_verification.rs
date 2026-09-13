//! #99: real SQLite admission and independent verification occupancy.
use serde_json::{Value, json};
use std::sync::{Arc, Barrier};
use x_data_mcp::{
    sqlite_tasks::SqliteTasks,
    task_control::{legacy::LegacyCollection, service::TaskControl, *},
    task_verification::{StopVerification, verification_scope},
};
fn task(a: Acceptance) -> TaskSnapshot {
    let RequestOutcome::Task(s) = a.record.outcome else {
        panic!("task required")
    };
    *s
}
fn db() -> (tempfile::TempDir, SqliteTasks) {
    let d = tempfile::tempdir().unwrap();
    let r = SqliteTasks::new(d.path().join("tasks.sqlite"));
    r.initialize().unwrap();
    (d, r)
}
fn original_task(r: &SqliteTasks, id: &str) -> TaskSnapshot {
    let control = TaskControl::new(
        r.clone(),
        "old-run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    task(
        control
            .accept(
                id,
                LegacyCollection.key(),
                json!({"symbols":[id],"periods":["day","year"]}),
            )
            .unwrap(),
    )
}
fn control(r: &SqliteTasks, run: &str) -> TaskControl<SqliteTasks> {
    TaskControl::new(
        r.clone(),
        run.into(),
        vec![Box::new(
            StopVerification::new(r.clone(), run.into()).unwrap(),
        )],
    )
    .unwrap()
}
fn key(r: &SqliteTasks) -> CommandKey {
    StopVerification::new(r.clone(), "new-run".into())
        .unwrap()
        .key()
}
#[test]
fn verification_admission_never_takes_original_claims_or_reinterprets_replay() {
    let (_d, r) = db();
    let original = original_task(&r, "first");
    let ctl = control(&r, "new-run");
    let input = json!({"target_task_id":original.task.task_id});
    let accepted = task(ctl.accept("inspection", key(&r), input.clone()).unwrap());
    assert_eq!(
        r.read_task(&original.task.task_id).unwrap().unwrap(),
        original
    );
    assert_eq!(accepted.items[0].spec.progress_unit, "evidence_check");
    assert_eq!(accepted.claims.len(), 1);
    assert_eq!(
        accepted.claims[0].scope_key,
        verification_scope(&original.task.task_id)
    );
    assert!(
        accepted
            .claims
            .iter()
            .all(|v| original.claims.iter().all(|o| o.scope_key != v.scope_key))
    );
    // Replay is a durable accepted fact, even if the caller's current run differs.
    let replay = control(&r, "old-run")
        .accept("inspection", key(&r), input)
        .unwrap();
    assert!(replay.reused);
    assert_eq!(task(replay).task.task_id, accepted.task.task_id);
    assert!(matches!(
        ctl.accept(
            "first",
            key(&r),
            json!({"target_task_id":original.task.task_id})
        ),
        Err(Error::RequestConflict)
    ));
    let other = original_task(&r, "other");
    assert!(matches!(
        ctl.accept(
            "inspection",
            key(&r),
            json!({"target_task_id":other.task.task_id})
        ),
        Err(Error::RequestConflict)
    ));
    ctl.accept(
        "other-inspection",
        key(&r),
        json!({"target_task_id":other.task.task_id}),
    )
    .unwrap();
}
#[test]
fn verification_missing_normal_recursive_and_untrusted_flags_never_bind() {
    let (_d, r) = db();
    let target = original_task(&r, "first");
    assert!(matches!(
        control(&r, "old-run").accept(
            "normal",
            key(&r),
            json!({"target_task_id":target.task.task_id})
        ),
        Err(Error::InvalidCommand(_))
    ));
    let ctl = control(&r, "new-run");
    assert!(matches!(
        ctl.accept("missing", key(&r), json!({"target_task_id":"missing"})),
        Err(Error::TaskNotFound)
    ));
    for field in ["stopped", "force", "owner_dead", "evidence_approved"] {
        let mut p = json!({"target_task_id":target.task.task_id});
        p[field] = json!(true);
        assert!(matches!(
            ctl.accept(field, key(&r), p),
            Err(Error::InvalidCommand(_))
        ));
        assert!(r.read_request(field).unwrap().is_none());
    }
    let inspection = task(
        ctl.accept(
            "valid",
            key(&r),
            json!({"target_task_id":target.task.task_id}),
        )
        .unwrap(),
    );
    assert!(matches!(
        ctl.accept(
            "recursive",
            key(&r),
            json!({"target_task_id":inspection.task.task_id})
        ),
        Err(Error::InvalidCommand(_))
    ));
    for id in ["normal", "missing", "recursive"] {
        assert!(r.read_request(id).unwrap().is_none());
    }
    assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
}
#[test]
fn verification_two_connections_replay_same_request_and_reject_competing_ids() {
    for same_id in [true, false] {
        let (_d, r) = db();
        let target = original_task(&r, "first");
        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|i| {
                let r = r
                    .clone()
                    .with_busy_timeout(std::time::Duration::from_secs(5));
                let barrier = barrier.clone();
                let target = target.task.task_id.clone();
                std::thread::spawn(move || {
                    let ctl = control(&r, "new-run");
                    barrier.wait();
                    ctl.accept(
                        if same_id || i == 0 { "a" } else { "b" },
                        key(&r),
                        json!({"target_task_id":target}),
                    )
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            if same_id { 2 } else { 1 }
        );
        if same_id {
            assert_eq!(
                results
                    .iter()
                    .filter(|r| r.as_ref().unwrap().reused)
                    .count(),
                1
            );
        } else {
            assert!(
                results
                    .iter()
                    .any(|r| matches!(r, Err(Error::RangeConflict(_))))
            );
        }
        assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
    }
}
struct OtherKind(StopVerification<SqliteTasks>);
impl Contract for OtherKind {
    fn key(&self) -> CommandKey {
        CommandKey {
            operation: "test_other_verification".into(),
            ..self.0.key()
        }
    }
    fn normalize(&self, p: Value) -> Result<Value, Error> {
        self.0.normalize(p)
    }
    fn resolve(&self, p: &Value) -> Result<FrozenWork, Error> {
        self.0.resolve(p)
    }
}
#[test]
fn verification_serial_scope_is_shared_across_types_and_survives_cancel_and_restart() {
    let (_d, r) = db();
    let target = original_task(&r, "first");
    let ctl = control(&r, "new-run");
    let first = task(
        ctl.accept(
            "one",
            key(&r),
            json!({"target_task_id":target.task.task_id}),
        )
        .unwrap(),
    );
    let other = OtherKind(StopVerification::new(r.clone(), "new-run".into()).unwrap());
    let other_key = other.key();
    let other_ctl = TaskControl::new(r.clone(), "new-run".into(), vec![Box::new(other)]).unwrap();
    let cancelled = ctl
        .cancel(&first.task.task_id, first.task.state_revision)
        .unwrap();
    assert_eq!(cancelled.task.status, Status::CancelRequested);
    assert!(matches!(
        other_ctl.accept(
            "two",
            other_key.clone(),
            json!({"target_task_id":target.task.task_id})
        ),
        Err(Error::RangeConflict(_))
    ));
    r.reconcile("third-run").unwrap();
    assert!(matches!(
        other_ctl.accept(
            "two",
            other_key,
            json!({"target_task_id":target.task.task_id})
        ),
        Err(Error::RangeConflict(_))
    ));
    assert!(r.read_request("two").unwrap().is_none());
    assert_eq!(
        r.read_task(&target.task.task_id)
            .unwrap()
            .unwrap()
            .claims
            .len(),
        2
    );
    assert!(r.candidates("third-run").unwrap().is_empty());
}
#[test]
fn verification_corrupt_original_is_not_missing_or_permission_to_unlock() {
    let (_d, r) = db();
    let target = original_task(&r, "first");
    rusqlite::Connection::open(r.path())
        .unwrap()
        .execute(
            "DELETE FROM scope_occupancy WHERE task_id=?1",
            [&target.task.task_id],
        )
        .unwrap();
    assert!(matches!(
        control(&r, "new-run").accept(
            "inspect",
            key(&r),
            json!({"target_task_id":target.task.task_id})
        ),
        Err(Error::NeedsVerification(_))
    ));
    assert!(r.read_request("inspect").unwrap().is_none());
}
