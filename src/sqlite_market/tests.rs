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
    // Simulate a migrated sharer: its manifest still references the parent's
    // segment, but its records carry numerically-equal bars whose text
    // differs, so the envelope keeps the exact array in payloads.
    let p = envelope::array_payload(&b["records"]).unwrap();
    envelope::write_payloads(&c, std::slice::from_ref(&p)).unwrap();
    let (env, more) = envelope::pack_daily(&b, envelope::ref_payload(&p.id)).unwrap();
    envelope::write_payloads(&c, &more).unwrap();
    c.execute(
        "INSERT INTO data_versions(version_id,symbol,period,adjustment,state,source,published_at,record_count,unique_count,duplicate_count,coverage_status,coverage_reason,first_date,last_date,envelope_json,content_digest,digest_format,evidence_digest,manifest_root) SELECT 'legacy-daily-b',symbol,period,adjustment,'staging',source,published_at,record_count,unique_count,duplicate_count,coverage_status,coverage_reason,first_date,last_date,?1,?2,digest_format,evidence_digest,manifest_root FROM data_versions WHERE version_id='legacy-daily'",
        params![
            serde_json::to_string(&env).unwrap(),
            b["integrity_sha256"].as_str().unwrap()
        ],
    )
    .unwrap();
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
fn daily_raw(id: &str, first_day: i64, days: usize) -> Value {
    let samples: Vec<Value> = serde_json::from_str(include_str!(
        "../../tests/fixtures/issue79/legacy-records.json"
    ))
    .unwrap();
    let mut r = samples[0]["record"].clone();
    let start = chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();
    let bars: Vec<Value> = (0..days)
        .map(|i| {
            let d = start + chrono::Duration::days(first_day + i as i64);
            serde_json::json!({"date":d.to_string(),"open":"10.00","high":"11.00","low":"9.00","close":"10.50","volume":null,"amount":null})
        })
        .collect();
    r["version_id"] = serde_json::json!(id);
    r["records"] = serde_json::json!(bars);
    r["batch"]["bars"] = serde_json::json!(bars);
    r["validation"]["record_count"] = serde_json::json!(days);
    r["validation"]["unique_record_count"] = serde_json::json!(days);
    r["integrity_sha256"] =
        serde_json::json!(crate::market_storage::compat::legacy_digest(&r).unwrap());
    r
}
fn publish_daily(db: &SqliteMarket, raw: &Value, head: Option<&str>) {
    db.stage(raw).unwrap();
    let ctl = TaskControl::new(
        db.tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let a = ctl
        .accept(
            raw["version_id"].as_str().unwrap(),
            LegacyCollection.key(),
            serde_json::json!({"symbols":[raw["symbol"]],"periods":["day"]}),
        )
        .unwrap();
    let RequestOutcome::Task(s) = a.record.outcome else {
        panic!()
    };
    let s = db
        .tasks
        .transition(&s.task.task_id, s.task.state_revision, "run", Change::Claim)
        .unwrap();
    let s = db
        .tasks
        .transition(
            &s.task.task_id,
            s.task.state_revision,
            "run",
            Change::StartItem(s.items[0].spec.item_id.clone()),
        )
        .unwrap();
    let plan = Publisher {
        repository: db.clone(),
    }
    .prepare(Publication {
        publication_id: format!("pub-{}", raw["version_id"].as_str().unwrap()),
        version_id: raw["version_id"].as_str().unwrap().into(),
        task_id: s.task.task_id,
        item_id: s.items[0].spec.item_id.clone(),
        run_id: "run".into(),
        expected_revision: s.task.state_revision,
        intent: PublishIntent::Daily {
            expected_head: head.map(str::to_owned),
        },
    })
    .unwrap();
    db.commit(&plan).unwrap();
}
#[test]
fn manifest_append_versions_share_parent_subtrees_with_bounded_growth() {
    let (_dir, db, plan) = setup();
    db.commit(&plan).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    let nodes = |c: &Connection| -> i64 {
        c.query_row("SELECT count(*) FROM manifest_nodes", [], |r| r.get(0))
            .unwrap()
    };
    // 250 daily updates, each appending one bar to the previous head.
    let mut prev = "legacy-daily".to_owned();
    let mut days = 3usize;
    for i in 0..250 {
        days += 1;
        let id = format!("d{i}");
        publish_daily(&db, &daily_raw(&id, 0, days), Some(&prev));
        prev = id;
    }
    let total = nodes(&c);
    // Persistent splicing path-copies O(log n) interior nodes plus one leaf per
    // update; the retired flat manifest restated every clip (O(days^2)).
    assert!(
        total < 250 * 64,
        "manifest growth must stay near-linear, got {total} nodes"
    );
    let head_height: i64 = c
        .query_row(
            "SELECT n.height FROM data_versions v JOIN manifest_nodes n ON n.node_id=v.manifest_root WHERE v.version_id=?1",
            [&prev],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        head_height <= 16,
        "tree height must stay logarithmic: {head_height}"
    );
    // Every version still reconstructs its exact records through the tree.
    for i in [0usize, 100, 249] {
        let fixed = db.fixed(&format!("d{i}")).unwrap();
        assert_eq!(fixed.version.records.len(), 4 + i);
    }
}
#[test]
fn manifest_splice_matches_a_flat_reference_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(dir.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let c = Connection::open(tasks.path()).unwrap();
    let put_seg = |id: i64, first: i64, days: usize| -> Vec<String> {
        let start = chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();
        let dates: Vec<String> = (0..days)
            .map(|i| (start + chrono::Duration::days(first + i as i64)).to_string())
            .collect();
        c.execute(
            "INSERT INTO bar_segments VALUES(?1,'sh.600006',?2,?3,?4)",
            params![id, dates[0], dates[days - 1], days as i64],
        )
        .unwrap();
        let mut s = c
            .prepare("INSERT INTO segment_bars VALUES(?1,?2,?3)")
            .unwrap();
        for d in &dates {
            s.execute(params![id, d, format!("{{\"date\":\"{d}\",\"open\":\"1\",\"high\":\"1\",\"low\":\"1\",\"close\":\"1\",\"volume\":null,\"amount\":null}}")])
                .unwrap();
        }
        dates
    };
    let seg1 = put_seg(1, 0, 100);
    let new_a = put_seg(2, 500, 10);
    let new_b = put_seg(3, 600, 5);
    let base = manifest::build(
        &c,
        None,
        &[manifest::Piece::New {
            segment: 1,
            first_date: seg1[0].clone(),
            last_date: seg1[99].clone(),
            count: 100,
        }],
    )
    .unwrap();
    let dates = |root: i64| -> Vec<String> {
        manifest::bars(&c, root)
            .unwrap()
            .iter()
            .map(|b| b["date"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(dates(base), seg1);
    // Mixed splice: new + base[0..50) + new + base[50..100).
    let v2 = manifest::build(
        &c,
        Some(base),
        &[
            manifest::Piece::New {
                segment: 2,
                first_date: new_a[0].clone(),
                last_date: new_a[9].clone(),
                count: 10,
            },
            manifest::Piece::Base { first: 0, last: 49 },
            manifest::Piece::New {
                segment: 3,
                first_date: new_b[0].clone(),
                last_date: new_b[4].clone(),
                count: 5,
            },
            manifest::Piece::Base {
                first: 50,
                last: 99,
            },
        ],
    )
    .unwrap();
    let expected: Vec<String> = new_a
        .iter()
        .chain(&seg1[..50])
        .chain(&new_b)
        .chain(&seg1[50..])
        .cloned()
        .collect();
    assert_eq!(dates(v2), expected);
    // The parent tree is untouched and still resolves identically.
    assert_eq!(dates(base), seg1);
    // Base pieces are independent slices: an out-of-order pair produces
    // exactly the requested sequence rather than corrupting it silently.
    let reordered = manifest::build(
        &c,
        Some(base),
        &[
            manifest::Piece::Base {
                first: 50,
                last: 99,
            },
            manifest::Piece::Base { first: 0, last: 49 },
        ],
    )
    .unwrap();
    let expected_reorder: Vec<String> = seg1[50..].iter().chain(&seg1[..50]).cloned().collect();
    assert_eq!(dates(reordered), expected_reorder);
    // An out-of-bounds base range fails closed.
    assert!(matches!(
        manifest::build(
            &c,
            Some(base),
            &[manifest::Piece::Base {
                first: 0,
                last: 100
            }]
        ),
        Err(Error::Integrity(_))
    ));
    // Range clips follow the spliced order (production records are
    // date-sorted; this synthetic sequence exercises pure sequence order).
    let clips = manifest::clips_in_range(&c, v2, "2019-01-01", "2023-01-01").unwrap();
    let seg_order: Vec<i64> = clips.iter().map(|(s, _, _)| *s).collect();
    assert_eq!(seg_order, vec![2, 1, 3, 1]);
}
#[test]
fn manifest_and_evidence_corruption_fail_closed() {
    for sql in [
        // A dropped interior/leaf node.
        "DELETE FROM manifest_nodes WHERE node_id=(SELECT manifest_root FROM data_versions WHERE version_id='legacy-daily')",
        // A dangling child pointer.
        "UPDATE manifest_nodes SET left_id=99999 WHERE node_id=(SELECT manifest_root FROM data_versions WHERE version_id='legacy-daily')",
        // A corrupt leaf clip bound.
        "UPDATE manifest_nodes SET clip_end='1900-01-01' WHERE node_id=(SELECT manifest_root FROM data_versions WHERE version_id='legacy-daily')",
        // A corrupt declared leaf length.
        "UPDATE manifest_nodes SET row_count=99 WHERE node_id=(SELECT manifest_root FROM data_versions WHERE version_id='legacy-daily')",
        // A missing segment bar under a referenced clip.
        "DELETE FROM segment_bars WHERE trade_date=(SELECT min(trade_date) FROM segment_bars)",
        // A segment-backed record with no manifest root.
        "UPDATE data_versions SET manifest_root=NULL WHERE version_id='legacy-daily'",
        // A sealed evidence projection that no longer matches the record.
        "UPDATE data_versions SET evidence_digest='tampered' WHERE version_id='legacy-daily'",
    ] {
        let (_dir, db, plan) = setup();
        db.commit(&plan).unwrap();
        // Disable FK checks so the simulated damage lands; reads must still
        // fail closed rather than reconstruct around it.
        Connection::open(db.tasks.path())
            .unwrap()
            .execute_batch(&format!("PRAGMA foreign_keys=OFF; {sql}"))
            .unwrap();
        assert!(
            matches!(db.fixed("legacy-daily"), Err(Error::Integrity(_))),
            "{sql}"
        );
    }
}
#[test]
fn manifest_rebuilds_from_v16_flat_clips_and_migrates_idempotently() {
    let (_dir, db, plan) = setup();
    db.commit(&plan).unwrap();
    publish_daily(&db, &daily_raw("d1", 0, 4), Some("legacy-daily"));
    let original = db.fixed("d1").unwrap().raw().clone();
    let c = Connection::open(db.tasks.path()).unwrap();
    // Rewind to schema 16: flat per-version clips, no manifest, no digests.
    c.execute_batch(
        "CREATE TABLE version_segments(version_id TEXT NOT NULL,seq INTEGER NOT NULL,segment_id INTEGER NOT NULL,clip_start TEXT NOT NULL,clip_end TEXT NOT NULL,PRIMARY KEY(version_id,seq)) WITHOUT ROWID;
         INSERT INTO version_segments
           WITH RECURSIVE t(version_id,i) AS (SELECT version_id,manifest_root FROM data_versions WHERE manifest_root IS NOT NULL
             UNION ALL SELECT t.version_id,n.left_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.left_id IS NOT NULL
             UNION ALL SELECT t.version_id,n.right_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.right_id IS NOT NULL)
           SELECT t.version_id,ROW_NUMBER() OVER (PARTITION BY t.version_id ORDER BY m.clip_start)-1,m.segment_id,m.clip_start,m.clip_end
           FROM t JOIN manifest_nodes m ON m.node_id=t.i AND m.segment_id IS NOT NULL;
         DROP TABLE manifest_nodes;
         ALTER TABLE data_versions DROP COLUMN manifest_root;
         ALTER TABLE data_versions DROP COLUMN evidence_digest;
         ALTER TABLE tasks DROP COLUMN evidence_digest;
         DELETE FROM schema_migrations WHERE version>=17;
         PRAGMA user_version=16;",
    )
    .unwrap();
    drop(c);
    db.tasks.upgrade().unwrap();
    // Rebuilt trees serve the exact same records.
    assert_eq!(db.fixed("d1").unwrap().raw(), &original);
    let c = Connection::open(db.tasks.path()).unwrap();
    let nodes: i64 = c
        .query_row("SELECT count(*) FROM manifest_nodes", [], |r| r.get(0))
        .unwrap();
    assert!(nodes > 0);
    let has_clips: bool = c
        .query_row(
            "SELECT count(*)>0 FROM sqlite_master WHERE name='version_segments'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!has_clips);
    // The data migration is idempotent on an already-migrated database.
    manifest::migrate(&c).unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM manifest_nodes", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        nodes
    );
}
