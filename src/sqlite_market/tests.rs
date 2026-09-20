use super::*;
use crate::task_control::{
    Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
    service::TaskControl,
};
use std::{cell::RefCell, process::Command};
thread_local! {static AFTER_VERIFICATION:RefCell<Option<Box<dyn FnOnce()>>>=RefCell::new(None);}
pub(super) fn after_verification() {
    let hook = AFTER_VERIFICATION.with(|s| s.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}
pub(super) fn crash_point(point: &str) {
    if std::env::var("X_DATA_MCP_TEST_MARKET_CRASH").as_deref() == Ok(point) {
        std::process::exit(79);
    }
}
pub(crate) fn setup() -> (tempfile::TempDir, SqliteMarket, PublishPlan) {
    let dir = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(dir.path().join("market.sqlite"));
    tasks.initialize().unwrap();
    let db = SqliteMarket::new(tasks);
    let samples: Vec<Value> = serde_json::from_str(include_str!(
        "../../tests/fixtures/issue79/legacy-records.json"
    ))
    .unwrap();
    let raw = &samples[0]["record"];
    db.stage(raw).unwrap();
    let ctl = TaskControl::new(
        db.tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let a = ctl
        .accept(
            "r",
            LegacyCollection.key(),
            serde_json::json!({"symbols":["sh.600006"],"periods":["day"]}),
        )
        .unwrap();
    let RequestOutcome::Task(s) = a.record.outcome else {
        panic!()
    };
    let s = db
        .tasks
        .transition(&s.task.task_id, 0, "run", Change::Claim)
        .unwrap();
    let s = db
        .tasks
        .transition(
            &s.task.task_id,
            1,
            "run",
            Change::StartItem(s.items[0].spec.item_id.clone()),
        )
        .unwrap();
    let plan = Publisher {
        repository: db.clone(),
    }
    .prepare(Publication {
        publication_id: "p".into(),
        version_id: "legacy-daily".into(),
        task_id: s.task.task_id,
        item_id: s.items[0].spec.item_id.clone(),
        run_id: "run".into(),
        expected_revision: 2,
        intent: PublishIntent::Daily {
            expected_head: None,
        },
    })
    .unwrap();
    (dir, db, plan)
}
#[test]
fn crash_child() {
    let Ok(path) = std::env::var("X_DATA_MCP_TEST_MARKET_PATH") else {
        return;
    };
    let command: Publication =
        serde_json::from_str(&std::env::var("X_DATA_MCP_TEST_MARKET_COMMAND").unwrap()).unwrap();
    let db = SqliteMarket::new(SqliteTasks::new(path));
    let plan = Publisher {
        repository: db.clone(),
    }
    .prepare(command)
    .unwrap();
    db.commit(&plan).unwrap();
    panic!("crash not reached");
}
#[test]
fn real_publication_exit_before_commit_and_response_loss_after_commit() {
    for point in ["before_commit", "after_commit"] {
        let (_dir, db, plan) = setup();
        let before = db.task(&plan.command.task_id).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sqlite_market::tests::crash_child",
                "--nocapture",
            ])
            .env("X_DATA_MCP_TEST_MARKET_PATH", db.tasks.path())
            .env(
                "X_DATA_MCP_TEST_MARKET_COMMAND",
                text(&plan.command).unwrap(),
            )
            .env("X_DATA_MCP_TEST_MARKET_CRASH", point)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(79));
        let committed = point == "after_commit";
        assert_eq!(db.receipt("p").unwrap().is_some(), committed);
        assert_eq!(db.fixed("legacy-daily").is_ok(), committed);
        let c = Connection::open(db.tasks.path()).unwrap();
        for table in [
            "publication_batches",
            "task_publications",
            "effective_heads",
        ] {
            let n: i64 = c
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, i64::from(committed));
        }
        if !committed {
            assert_eq!(db.task(&plan.command.task_id).unwrap(), before);
        }
        let receipt = db.commit(&plan).unwrap();
        assert_eq!(db.receipt("p").unwrap(), Some(receipt.clone()));
        assert_eq!(db.commit(&plan).unwrap(), receipt);
        assert!(db.task(&plan.command.task_id).unwrap().claims.is_empty());
        let n: i64 = c
            .query_row("SELECT count(*) FROM data_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
#[test]
fn verified_projection_and_sql_return_use_one_snapshot_then_detect_later_damage() {
    let (_dir, db, plan) = setup();
    db.commit(&plan).unwrap();
    let path = db.tasks.path().to_owned();
    AFTER_VERIFICATION.with(|s| {
        *s.borrow_mut() = Some(Box::new(move || {
            Connection::open(path)
                .unwrap()
                .execute(
                    "UPDATE segment_bars SET close='99' WHERE trade_date='2025-01-02'",
                    [],
                )
                .unwrap();
        }))
    });
    let range = Range {
        start: "2025-01-01".parse().unwrap(),
        end: "2025-12-31".parse().unwrap(),
        after: None,
        limit: 1,
    };
    let scope = Scope {
        symbol: "sh.600006".into(),
        period: Period::Day,
    };
    let page = db.range(&scope, Some("legacy-daily"), &range).unwrap();
    assert_eq!(page.rows[0].close.to_string(), "10.05");
    assert!(matches!(
        db.range(&scope, Some("legacy-daily"), &range),
        Err(Error::Integrity(_))
    ));
}
