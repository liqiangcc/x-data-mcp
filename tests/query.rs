use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tempfile::tempdir;
use x_data_mcp::{
    market_data::*,
    publish::{EffectiveVersion, ValidationSummary},
    query::{MarketQuery, QueryService, VersionQuery},
    storage::JsonFileStore,
};
fn date(s: &str) -> NaiveDate {
    s.parse().unwrap()
}
fn now() -> DateTime<Utc> {
    "2026-09-12T00:00:00Z".parse().unwrap()
}
fn daily(id: &str) -> EffectiveVersion {
    EffectiveVersion {
        version_id: id.into(),
        symbol: "sh.600006".into(),
        period: Period::Day,
        adjustment: Adjustment::Forward,
        source: "official-files:TEST_ONLY".into(),
        records: ["2024-12-31", "2025-01-02", "2025-12-30", "2026-01-05"]
            .iter()
            .map(|s| Bar {
                date: date(s),
                open: Decimal::ONE,
                high: Decimal::ONE,
                low: Decimal::ONE,
                close: Decimal::ONE,
                volume: None,
                amount: None,
            })
            .collect(),
        published_at: now(),
        validation: ValidationSummary {
            record_count: 4,
            unique_record_count: 4,
            duplicate_count: 0,
        },
        unit_currency: UnitCurrencyEvidence::confirmed(
            "CNY", "yuan", "share", "CNY", "yuan", "fixture",
        ),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "fixture"),
        yearly_derivation: None,
    }
}
fn query() -> MarketQuery {
    serde_json::from_value(json!({"exchange":"sh","code":"600006","period":"day","start_date":"2024-01-01","end_date":"2026-12-31","page_size":1})).unwrap()
}
#[test]
fn pages_fix_version_conditions_time_and_detect_later_damage() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let v = daily("v1");
    store.publish_effective(&v).unwrap();
    let service = QueryService::new(store.clone());
    let first = service.market(query(), now()).unwrap();
    assert_eq!(first["records"][0]["date"], "2024-12-31");
    assert_eq!(first["unit_currency"], json!(v.unit_currency));
    assert_eq!(first["coverage"], "UNKNOWN");
    assert_eq!(first["future_interval_not_occurred"], true);
    store.publish_effective(&daily("v2")).unwrap();
    let mut next = query();
    next.cursor = Some(first["next_cursor"].as_str().unwrap().into());
    let second = service
        .market(next.clone(), now() + chrono::Duration::days(2))
        .unwrap();
    assert_eq!(second["version_id"], "v1");
    assert_eq!(second["query_time"], first["query_time"]);
    assert_eq!(second["records"][0]["date"], "2025-01-02");
    let mut changed = next.clone();
    changed.end_date = date("2025-12-31");
    assert!(service.market(changed, now()).is_err());
    assert!(
        QueryService::new(store.clone())
            .market(next.clone(), now())
            .is_err()
    );
    assert_eq!(service.market(query(), now()).unwrap()["version_id"], "v2");
    let path = dir.path().join("effective/sh.600006.day.qfq.json");
    let mut records: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    records[0]["records"][0]["close"] = json!("2");
    std::fs::write(path, serde_json::to_vec(&records).unwrap()).unwrap();
    assert!(service.market(next, now()).is_err());
}
#[test]
fn explicit_version_empty_future_invalid_scope_and_no_writes() {
    let dir = tempdir().unwrap();
    let absent = dir.path().join("absent");
    let service = QueryService::new(JsonFileStore::new(&absent));
    let mut q = query();
    q.start_date = date("2027-01-01");
    q.end_date = date("2027-12-31");
    let result = service.market(q.clone(), now()).unwrap();
    assert_eq!(result["records"], json!([]));
    assert_eq!(result["version_id"], Value::Null);
    assert_eq!(result["future_interval_not_occurred"], true);
    assert!(!absent.exists());
    q.version_id = Some("missing".into());
    assert!(service.market(q, now()).is_err());
    for (exchange, code, size) in [
        ("us", "600006", 1),
        ("sh", "../bad", 1),
        ("sh", "600006", 0),
        ("sh", "600006", 1001),
    ] {
        let mut q = query();
        q.exchange = exchange.into();
        q.code = code.into();
        q.page_size = Some(size);
        assert!(service.market(q, now()).is_err());
    }
    let mut q = query();
    q.start_date = date("2027-01-01");
    assert!(service.market(q, now()).is_err());
    assert!(!absent.exists());
}
#[test]
fn version_list_is_fixed_sorted_and_historical_reads_do_not_fallback() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    store.publish_effective(&daily("b")).unwrap();
    store.publish_effective(&daily("a")).unwrap();
    let service = QueryService::new(store.clone());
    let mut q: VersionQuery = serde_json::from_value(
        json!({"exchange":"sh","code":"600006","period":"day","page_size":1}),
    )
    .unwrap();
    let first = service.versions(q.clone(), now()).unwrap();
    assert_eq!(first["records"][0]["version_id"], "a");
    store.publish_effective(&daily("c")).unwrap();
    q.cursor = Some(first["next_cursor"].as_str().unwrap().into());
    let last = service.versions(q, now()).unwrap();
    assert_eq!(last["records"][0]["version_id"], "b");
    assert_eq!(last["next_cursor"], Value::Null);
    let mut q = query();
    q.version_id = Some("b".into());
    assert_eq!(service.market(q.clone(), now()).unwrap()["version_id"], "b");
    q.version_id = Some("missing".into());
    assert!(service.market(q, now()).is_err());
}
#[test]
fn historical_year_uses_full_natural_year_and_requires_exact_published_derivation() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let d = daily("daily");
    store.publish_effective(&d).unwrap();
    let annual = x_data_mcp::yearly::aggregate(
        &d,
        Some(&std::collections::BTreeSet::from([2025])),
        date("2026-09-12"),
    )
    .unwrap();
    let r = &annual.records[0];
    let mut y = d.clone();
    y.version_id = "year".into();
    y.period = Period::Year;
    y.records = vec![r.bar.clone()];
    y.yearly_derivation = Some(r.lineage.clone());
    store.publish_effective(&y).unwrap();
    let service = QueryService::new(store.clone());
    let mut q = query();
    q.period = "year".into();
    q.start_date = date("2025-06-01");
    q.end_date = date("2025-07-01");
    let result = service.market(q.clone(), now()).unwrap();
    assert_eq!(result["records"][0]["bar"]["date"], "2025-12-30");
    assert_eq!(
        result["records"][0]["lineage"]["first_trading_date"],
        "2025-01-02"
    );
    assert_eq!(result["records"][0]["year_unfinished"], false);
    assert_eq!(result["derivation"]["kind"], "local_aggregation");
    y.version_id = "bad".into();
    y.records[0].close = Decimal::TWO;
    store.publish_effective(&y).unwrap();
    assert!(service.market(q.clone(), now()).is_err());
    y.version_id = "no-lineage".into();
    y.yearly_derivation = None;
    store.publish_effective(&y).unwrap();
    assert!(service.market(q, now()).is_err());
}

#[test]
fn bounded_cursor_cache_expires_oldest_page_without_switching_version() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    store.publish_effective(&daily("v")).unwrap();
    let service = QueryService::new(store);
    let first = service.market(query(), now()).unwrap();
    for _ in 0..64 {
        service.market(query(), now()).unwrap();
    }
    let mut q = query();
    q.cursor = Some(first["next_cursor"].as_str().unwrap().into());
    assert!(service.market(q, now()).is_err());
}
