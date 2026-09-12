use super::*;
use crate::{
    market_data::{CoverageEvidence, CoverageStatus},
    task_control::{
        Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
    yearly_publication::{Derivation, DerivationEvidence},
};
fn command(db: &SqliteMarket, id: &str, prior: Option<&str>) -> Publication {
    let ctl = TaskControl::new(
        db.tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let a = ctl
        .accept(
            id,
            LegacyCollection.key(),
            serde_json::json!({"symbols":["sh.600006"],"periods":["year"]}),
        )
        .unwrap();
    let RequestOutcome::Task(task) = a.record.outcome else {
        panic!()
    };
    let task = db
        .tasks
        .transition(&task.task.task_id, 0, "run", Change::Claim)
        .unwrap();
    let task = db
        .tasks
        .transition(
            &task.task.task_id,
            task.task.state_revision,
            "run",
            Change::StartItem(task.items[0].spec.item_id.clone()),
        )
        .unwrap();
    Publication {
        publication_id: format!("result-{id}"),
        version_id: id.into(),
        task_id: task.task.task_id,
        item_id: task.items[0].spec.item_id.clone(),
        run_id: "run".into(),
        expected_revision: task.task.state_revision,
        intent: PublishIntent::CurrentHead {
            expected_daily_head: "legacy-daily".into(),
            expected_yearly_head: prior.map(str::to_owned),
        },
    }
}
fn approve(db: &SqliteMarket, command: Publication, changed: bool) -> YearlyResultPlan {
    let base = db.fixed("legacy-daily").unwrap();
    let request = Derivation {
        symbol: "sh.600006".into(),
        base_daily_version_id: "legacy-daily".into(),
        version_id: command.version_id.clone(),
        years: [2025].into(),
        derived_at: "2026-01-02T00:00:00Z".parse().unwrap(),
    };
    let evidence = DerivationEvidence {
        base_integrity_sha256: base.raw["integrity_sha256"].as_str().unwrap().into(),
        years: request.years.clone(),
        as_of: "2026-01-02".parse().unwrap(),
        coverage: CoverageEvidence {
            status: CoverageStatus::Complete,
            reason: if changed {
                "fixture: changed coverage"
            } else {
                "fixture: range only"
            }
            .into(),
        },
    };
    let publisher = Publisher {
        repository: db.clone(),
    };
    let candidate = publisher.derive_yearly(&request, &evidence).unwrap();
    let prior = match &command.intent {
        PublishIntent::CurrentHead {
            expected_yearly_head,
            ..
        } => expected_yearly_head.clone(),
        _ => None,
    };
    publisher
        .prepare_yearly_result(command, &candidate, prior.as_deref())
        .unwrap()
}
#[test]
fn yearly_crash_child() {
    let Ok(path) = std::env::var("X_DATA_MCP_TEST_YEARLY_PATH") else {
        return;
    };
    let command =
        serde_json::from_str(&std::env::var("X_DATA_MCP_TEST_YEARLY_COMMAND").unwrap()).unwrap();
    let changed = std::env::var("X_DATA_MCP_TEST_YEARLY_CHANGED").unwrap() == "true";
    let db = SqliteMarket::new(SqliteTasks::new(path));
    let plan = approve(&db, command, changed);
    db.commit_yearly(&plan).unwrap();
    panic!("crash not reached");
}
#[test]
fn yearly_real_exit_rolls_back_or_replays_committed_reuse_and_new_publication() {
    for changed in [false, true] {
        for point in ["before_commit", "after_commit"] {
            let (_dir, db, daily) = super::super::super::tests::setup();
            db.commit(&daily).unwrap();
            let first = approve(&db, command(&db, "first", None), false);
            db.commit_yearly(&first).unwrap();
            let command = command(&db, "second", Some("first"));
            let before = db.task(&command.task_id).unwrap();
            let plan = approve(&db, command.clone(), changed);
            assert_eq!(plan.is_no_change(), !changed);
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sqlite_market::yearly_results::tests::yearly_crash_child",
                    "--nocapture",
                ])
                .env("X_DATA_MCP_TEST_YEARLY_PATH", db.tasks.path())
                .env("X_DATA_MCP_TEST_YEARLY_COMMAND", text(&command).unwrap())
                .env("X_DATA_MCP_TEST_YEARLY_CHANGED", changed.to_string())
                .env("X_DATA_MCP_TEST_MARKET_CRASH", point)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(79));
            let committed = point == "after_commit";
            assert_eq!(
                db.yearly_receipt("result-second").unwrap().is_some(),
                committed
            );
            assert_eq!(db.fixed("second").is_ok(), changed && committed);
            let c = Connection::open(db.tasks.path()).unwrap();
            let results: i64 = c
                .query_row("SELECT count(*) FROM yearly_execution_results", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let publications: i64 = c
                .query_row("SELECT count(*) FROM publication_batches", [], |r| r.get(0))
                .unwrap();
            assert_eq!(results, 1 + i64::from(committed));
            assert_eq!(publications, 2 + i64::from(changed && committed));
            if !committed {
                assert_eq!(db.task(&command.task_id).unwrap(), before);
            }
            let receipt = db.commit_yearly(&plan).unwrap();
            assert_eq!(db.commit_yearly(&plan).unwrap(), receipt);
            assert!(db.task(&command.task_id).unwrap().claims.is_empty());
            let versions: i64 = c
                .query_row("SELECT count(*) FROM data_versions", [], |r| r.get(0))
                .unwrap();
            assert_eq!(versions, 2 + i64::from(changed));
        }
    }
}
