use serde_json::json;
use x_data_mcp::{
    data_supply::{
        Access,
        query::{Queries, QueryPlan},
        wire::*,
    },
    sqlite_market::SqliteMarket,
    sqlite_supply::SqliteSupply,
    sqlite_tasks::SqliteTasks,
};
fn args() -> Query {
    serde_json::from_value(json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},"filter":{"kind":"daily","start_date":"2025-01-01","end_date":"2025-12-31"},"version":{"kind":"latest_per_entity"},"page_size":1})).unwrap()
}
#[test]
fn closed_query_contract_rejects_extra_nested_fields_and_semantic_mismatch() {
    let mut v = serde_json::to_value(args()).unwrap();
    v["selection"]["entity"]["extra"] = json!(1);
    assert!(serde_json::from_value::<Query>(v).is_err());
    let mut a = args();
    a.dataset = Dataset::Yearly;
    assert!(QueryPlan::new(a, &Access::local()).is_err());
    let mut a = args();
    a.version = VersionChoice::ExplicitVersionMap { versions: vec![] };
    assert!(QueryPlan::new(a, &Access::local()).is_err());
}
#[test]
fn absent_is_explicit_and_read_never_creates_a_task() {
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let q = Queries::new(SqliteSupply {
        market: SqliteMarket::new(t.clone()),
    });
    let page = q.query(args(), &Access::local()).unwrap();
    assert_eq!(page.version_manifest[0].status, ManifestStatus::Absent);
    assert!(page.records.is_empty());
    let count: i64 = rusqlite::Connection::open(t.path())
        .unwrap()
        .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn forbidden_selection_is_rejected_before_storage_access() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("no-db");
    let q = Queries::new(SqliteSupply {
        market: SqliteMarket::new(SqliteTasks::new(&p)),
    });
    let mut access = Access::local();
    access.securities = Some(Default::default());
    assert!(
        q.query(args(), &access)
            .unwrap_err()
            .to_string()
            .contains("forbidden")
    );
    assert!(!p.exists());
}
