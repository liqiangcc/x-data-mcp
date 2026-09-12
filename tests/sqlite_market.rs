mod day_file {
    pub use x_data_mcp::hash::sha256;
}
use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::{Value, json};
use x_data_mcp::{
    market_data::Period,
    market_storage::{Error, Range, Scope, compat},
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
};

mod frozen_legacy {
    use serde_json::Value;
    use std::io;
    // Only the crate path is supplied locally; body below is verbatim fixed old code.

    include!("fixtures/issue79/legacy_record_digest.rs.txt");
    pub fn digest(v: &Value) -> String {
        record_digest(v).unwrap()
    }
}
fn samples() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/issue79/legacy-records.json")).unwrap()
}
fn db() -> (tempfile::TempDir, SqliteMarket) {
    let dir = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(dir.path().join("market.sqlite"));
    tasks.initialize().unwrap();
    (dir, SqliteMarket::new(tasks))
}
fn range() -> Range {
    Range {
        start: "2025-01-01".parse().unwrap(),
        end: "2025-12-31".parse().unwrap(),
        after: None,
        limit: 1,
    }
}
fn scope() -> Scope {
    Scope {
        symbol: "sh.600006".into(),
        period: Period::Day,
    }
}
// First read slice only: explicit test fixture makes the staged version published.
// This is not publication atomicity evidence; that uses the next slice's real use case.
fn published_fixture(db: &SqliteMarket, raw: &Value) {
    db.stage(raw).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    c.execute(
        "UPDATE data_versions SET state='published' WHERE version_id=?1",
        [raw["version_id"].as_str().unwrap()],
    )
    .unwrap();
    c.execute(
        "INSERT INTO effective_heads VALUES('sh.600006','day','qfq',?1)",
        [raw["version_id"].as_str().unwrap()],
    )
    .unwrap();
}
#[test]
fn frozen_old_digest_matches_independent_literals_and_preserves_missing_null() {
    let fixtures = samples();
    assert_ne!(fixtures[0]["expected"], fixtures[1]["expected"]);
    for sample in fixtures {
        let raw = &sample["record"];
        assert_eq!(frozen_legacy::digest(raw), sample["expected"]);
        assert_eq!(compat::legacy_digest(raw).unwrap(), sample["expected"]);
        assert_eq!(compat::check(raw).unwrap().raw(), raw);
        let (_dir, store) = db();
        store.stage(raw).unwrap();
        assert_eq!(store.candidate("legacy-daily").unwrap().raw(), raw);
    }
}
#[test]
fn verified_sql_range_is_ordered_precise_and_fixed() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let page = store.range(&scope(), None, &range()).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].open.to_string(), "10.000000000000000001");
    assert_eq!(page.rows[0].volume, None);
    assert_eq!(
        page.rows[0].date,
        "2025-01-02".parse::<NaiveDate>().unwrap()
    );
    let mut next = range();
    next.after = page.next_after;
    let page2 = store
        .range(&scope(), Some(&page.version.version_id), &next)
        .unwrap();
    assert_eq!(
        page2.rows[0].date,
        "2025-12-31".parse::<NaiveDate>().unwrap()
    );
    assert_eq!(page2.next_after, None);
}
#[test]
fn compat_bar_metadata_and_evidence_tampering_reject_without_fallback() {
    for sql in [
        "UPDATE data_versions SET compat_json=json_set(compat_json,'$.source','altered')",
        "UPDATE daily_bars SET close='10.06' WHERE trade_date='2025-01-02'",
        "UPDATE data_versions SET source='altered'",
        "UPDATE version_evidence SET value='USD' WHERE field='price_currency'",
        "UPDATE data_versions SET content_digest='wrong'",
    ] {
        let (_dir, store) = db();
        published_fixture(&store, &samples()[0]["record"]);
        Connection::open(store.tasks.path())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        assert!(
            matches!(
                store.range(&scope(), Some("legacy-daily"), &range()),
                Err(Error::Integrity(_))
            ),
            "{sql}"
        );
        assert!(store.range(&scope(), None, &range()).is_err());
    }
}
#[test]
fn missing_digest_or_unverified_input_is_never_promoted_by_staging() {
    let (_dir, store) = db();
    let mut raw = samples()[0]["record"].clone();
    raw.as_object_mut().unwrap().remove("integrity_sha256");
    assert!(store.stage(&raw).is_err());
    raw["batch"]["coverage"]["status"] = json!("Unknown");
    raw["integrity_sha256"] = json!(frozen_legacy::digest(&raw));
    assert!(store.stage(&raw).is_err());
    assert!(matches!(store.fixed("legacy-daily"), Err(Error::NotFound)));
}
