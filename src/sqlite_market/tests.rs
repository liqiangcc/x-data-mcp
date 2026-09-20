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
                    "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','99') WHERE trade_date='2025-01-02'",
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
#[test]
fn compact_envelope_preserves_exact_decimal_text_and_digest() {
    let (_dir, db, plan) = setup();
    db.commit(&plan).unwrap();
    // The fixture's first bar carries a 20-digit decimal that any
    // normalization pass would corrupt; reconstruction must be byte-exact.
    let fixed = db.fixed("legacy-daily").unwrap();
    assert_eq!(
        fixed.raw()["records"][0]["open"],
        serde_json::json!("10.000000000000000001")
    );
    let c = Connection::open(db.tasks.path()).unwrap();
    let env: String = c
        .query_row(
            "SELECT envelope_json FROM data_versions WHERE version_id='legacy-daily'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let env: Value = serde_json::from_str(&env).unwrap();
    assert_eq!(env["records"], serde_json::json!({"$ref":"segments"}));
    assert_eq!(env["batch"]["bars"], serde_json::json!({"$ref":"records"}));
    let payloads: i64 = c
        .query_row("SELECT count(*) FROM payloads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(payloads, 0);
}
#[test]
fn numerically_equal_bar_with_different_text_cannot_share_a_segment() {
    let (_dir, db, plan) = setup();
    db.commit(&plan).unwrap();
    let mut r2 = db.fixed("legacy-daily").unwrap().raw().clone();
    r2["version_id"] = serde_json::json!("daily-2");
    r2["records"][0]["open"] = serde_json::json!("10.0000000000000000010");
    r2["batch"]["bars"][0]["open"] = serde_json::json!("10.0000000000000000010");
    r2["integrity_sha256"] =
        serde_json::json!(crate::market_storage::compat::legacy_digest(&r2).unwrap());
    db.stage(&r2).unwrap();
    let ctl = TaskControl::new(
        db.tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let a = ctl
        .accept(
            "r2",
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
    let plan2 = Publisher {
        repository: db.clone(),
    }
    .prepare(Publication {
        publication_id: "p2".into(),
        version_id: "daily-2".into(),
        task_id: s.task.task_id,
        item_id: s.items[0].spec.item_id.clone(),
        run_id: "run".into(),
        expected_revision: 2,
        intent: PublishIntent::Daily {
            expected_head: Some("legacy-daily".into()),
        },
    })
    .unwrap();
    db.commit(&plan2).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    let segments: i64 = c
        .query_row("SELECT count(*) FROM bar_segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(segments, 2);
    let fixed = db.fixed("daily-2").unwrap();
    assert_eq!(
        fixed.raw()["records"][0]["open"],
        serde_json::json!("10.0000000000000000010")
    );
    assert_eq!(
        db.fixed("legacy-daily").unwrap().raw()["records"][0]["open"],
        serde_json::json!("10.000000000000000001")
    );
}
#[test]
fn migrated_sharer_with_different_bar_text_uses_payload_and_fails_closed() {
    let (_dir, db, plan) = setup();
    db.commit(&plan).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    // Simulate a v15 sharer: same segment clip, but its records carry a
    // numerically-equal bar whose text differs from the segment creator's.
    let mut b = db.fixed("legacy-daily").unwrap().raw().clone();
    b["version_id"] = serde_json::json!("legacy-daily-b");
    b["records"][0]["open"] = serde_json::json!("10.0000000000000000010");
    b["batch"]["bars"][0]["open"] = serde_json::json!("10.0000000000000000010");
    b["integrity_sha256"] =
        serde_json::json!(crate::market_storage::compat::legacy_digest(&b).unwrap());
    c.execute(
        "INSERT INTO data_versions(version_id,symbol,period,adjustment,state,source,published_at,record_count,unique_count,duplicate_count,coverage_status,coverage_reason,first_date,last_date,envelope_json,content_digest,digest_format) SELECT 'legacy-daily-b',symbol,period,adjustment,'staging',source,published_at,record_count,unique_count,duplicate_count,coverage_status,coverage_reason,first_date,last_date,?1,?2,digest_format FROM data_versions WHERE version_id='legacy-daily'",
        params![
            serde_json::to_string(&b).unwrap(),
            b["integrity_sha256"].as_str().unwrap()
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO version_segments(version_id,seq,segment_id,clip_start,clip_end) SELECT 'legacy-daily-b',seq,segment_id,clip_start,clip_end FROM version_segments WHERE version_id='legacy-daily'",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO version_evidence(version_id,field,status,value,reason) SELECT 'legacy-daily-b',field,status,value,reason FROM version_evidence WHERE version_id='legacy-daily'",
        [],
    )
    .unwrap();
    envelope::migrate_legacy(&c).unwrap();
    let env: String = c
        .query_row(
            "SELECT envelope_json FROM data_versions WHERE version_id='legacy-daily-b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let env: Value = serde_json::from_str(&env).unwrap();
    assert!(
        env["records"]["$ref"]
            .as_str()
            .unwrap()
            .starts_with("payload:"),
        "migrated sharer must not keep segment refs it cannot reproduce: {env}"
    );
    assert_eq!(
        db.candidate("legacy-daily-b").unwrap().raw()["records"][0]["open"],
        serde_json::json!("10.0000000000000000010")
    );
    // Re-running the data migration is a no-op on already-packed rows.
    envelope::migrate_legacy(&c).unwrap();
    assert_eq!(
        db.candidate("legacy-daily-b").unwrap().raw()["records"][0]["open"],
        serde_json::json!("10.0000000000000000010")
    );
    c.execute("DELETE FROM payloads", []).unwrap();
    assert!(matches!(
        db.candidate("legacy-daily-b"),
        Err(Error::Integrity(_))
    ));
    assert!(db.fixed("legacy-daily").is_ok());
}
#[test]
fn unknown_or_dangling_envelope_reference_fails_closed() {
    let (_dir, db, plan) = setup();
    db.commit(&plan).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    c.execute(
        "UPDATE data_versions SET envelope_json=json_set(envelope_json,'$.records',json('{\"$ref\":\"bogus:1\"}'))",
        [],
    )
    .unwrap();
    assert!(matches!(db.fixed("legacy-daily"), Err(Error::Integrity(_))));
    c.execute(
        "UPDATE data_versions SET envelope_json=json_set(envelope_json,'$.records',json('{\"$ref\":\"payload:0000\"}'))",
        [],
    )
    .unwrap();
    assert!(matches!(db.fixed("legacy-daily"), Err(Error::Integrity(_))));
}
