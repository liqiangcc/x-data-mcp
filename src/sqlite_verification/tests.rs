use super::*;
use crate::{
    task_control::{
        Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
    task_verification::{
        StopVerification,
        decision::{self, Observation, StopInspector},
    },
};
struct Fixture(bool);
impl StopInspector for Fixture {
    fn inspect(&self, s: &TaskSnapshot) -> Result<Observation, Error> {
        Ok(if self.0 {
            Observation::Stopped {
                snapshot_sha256: decision::snapshot_digest(s)?,
                evidence_ref: "fixture-process-exit".into(),
            }
        } else {
            Observation::Insufficient {
                reason: "fixture: no writer evidence".into(),
            }
        })
    }
}
fn setup(db: &SqliteTasks, stopped: bool) -> StopPlan {
    let old = TaskControl::new(db.clone(), "old".into(), vec![Box::new(LegacyCollection)]).unwrap();
    let RequestOutcome::Task(target) = old
        .accept(
            "original",
            LegacyCollection.key(),
            serde_json::json!({"symbols":["a"],"periods":["day"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let contract = StopVerification::new(db.clone(), "new".into()).unwrap();
    let key = contract.key();
    let ctl = TaskControl::new(db.clone(), "new".into(), vec![Box::new(contract)]).unwrap();
    let RequestOutcome::Task(check) = ctl
        .accept(
            "check",
            key,
            serde_json::json!({"target_task_id":target.task.task_id}),
        )
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
            &s.task.task_id,
            s.task.state_revision,
            "new",
            Change::StartItem("stopped-writes".into()),
        )
        .unwrap();
    decision::prepare(
        s,
        *target,
        "new",
        "fixture-owner",
        "2026-09-13T00:00:00Z".parse().unwrap(),
        &Fixture(stopped),
    )
    .unwrap()
}
#[test]
fn stopped_check_crash_child() {
    let Ok(path) = std::env::var("X_DATA_MCP_TEST_STOPPED_DB") else {
        return;
    };
    let audit: Audit = serde_json::from_slice(
        &std::fs::read(std::env::var("X_DATA_MCP_TEST_STOPPED_PLAN").unwrap()).unwrap(),
    )
    .unwrap();
    SqliteTasks::new(path)
        .commit_stop_check(&StopPlan { audit })
        .unwrap();
    panic!("expected real exit seam");
}
#[test]
fn stopped_check_real_exit_before_commit_and_after_commit_preserves_exact_facts() {
    for stopped in [false, true] {
        for point in ["before_commit", "after_commit"] {
            let d = tempfile::tempdir().unwrap();
            let db = SqliteTasks::new(d.path().join("db"));
            db.initialize().unwrap();
            let plan = setup(&db, stopped);
            let path = d.path().join("plan.json");
            std::fs::write(&path, serde_json::to_vec(&plan.audit).unwrap()).unwrap();
            let exit = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sqlite_verification::tests::stopped_check_crash_child",
                    "--nocapture",
                ])
                .env("X_DATA_MCP_TEST_STOPPED_DB", db.path())
                .env("X_DATA_MCP_TEST_STOPPED_PLAN", path)
                .env("X_DATA_MCP_TEST_SQLITE_CRASH", point)
                .status()
                .unwrap();
            assert_eq!(exit.code(), Some(78), "{stopped}/{point}");
            let re = SqliteTasks::new(db.path());
            let a = &plan.audit;
            if point == "before_commit" {
                assert!(
                    re.stop_receipt(&a.receipt.verification_task_id)
                        .unwrap()
                        .is_none()
                );
                assert_eq!(
                    re.read_task(&a.target_before.task.task_id)
                        .unwrap()
                        .unwrap(),
                    a.target_before
                );
                assert_eq!(
                    re.read_task(&a.verification_before.task.task_id)
                        .unwrap()
                        .unwrap(),
                    a.verification_before
                );
            } else {
                assert_eq!(
                    re.stop_receipt(&a.receipt.verification_task_id).unwrap(),
                    Some(a.receipt.clone())
                );
                assert_eq!(
                    re.read_task(&a.target_before.task.task_id)
                        .unwrap()
                        .as_ref(),
                    Some(a.target_after.as_ref().unwrap_or(&a.target_before))
                );
                assert_eq!(
                    re.read_task(&a.verification_after.task.task_id)
                        .unwrap()
                        .unwrap(),
                    a.verification_after
                );
            }
            assert_eq!(re.commit_stop_check(&plan).unwrap(), a.receipt);
            assert_eq!(re.commit_stop_check(&plan).unwrap(), a.receipt);
            let c = Connection::open(re.path()).unwrap();
            assert_eq!(
                c.query_row("SELECT count(*) FROM stopped_write_checks", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM scope_occupancy", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                if stopped { 0 } else { 1 }
            );
        }
    }
}
