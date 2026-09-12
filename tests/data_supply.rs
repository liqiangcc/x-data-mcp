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

#[path = "support/market.rs"]
mod market;
#[test]
fn real_batch_manifest_pages_pin_versions_and_reject_any_later_damage() {
    use x_data_mcp::market_storage::{MarketRepository, PublishIntent};
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t.clone());
    let raw = market::samples()[0]["record"].clone();
    market::published_fixture(&store, &raw);
    let q = Queries::new(SqliteSupply {
        market: store.clone(),
    });
    let mut a = args();
    a.selection = Selection::Batch {
        entities: vec![
            Security {
                exchange: Exchange::Sh,
                code: "600006".into(),
            },
            Security {
                exchange: Exchange::Sz,
                code: "000001".into(),
            },
        ],
    };
    let first = q.query(a.clone(), &Access::local()).unwrap();
    assert_eq!(first.records.len(), 1);
    assert_eq!(first.version_manifest[1].status, ManifestStatus::Absent);
    let plan = market::prepare(
        &store,
        &market::revised(&raw, "new"),
        PublishIntent::Daily {
            expected_head: Some("legacy-daily".into()),
        },
    );
    store.commit(&plan).unwrap();
    a.cursor = first.next_cursor.clone();
    let second = q.query(a.clone(), &Access::local()).unwrap();
    assert_eq!(first.version_manifest, second.version_manifest);
    assert_eq!(first.query_time, second.query_time);
    assert_ne!(first.records, second.records);
    let mut changed = a.clone();
    changed.page_size = 2;
    assert!(q.query(changed, &Access::local()).is_err());
    rusqlite::Connection::open(t.path())
        .unwrap()
        .execute(
            "UPDATE daily_bars SET close='0' WHERE version_id='legacy-daily'",
            [],
        )
        .unwrap();
    assert!(q.query(a, &Access::local()).is_err());
}
#[test]
fn real_task_list_and_progress_freeze_state_and_enforce_authorization() {
    use x_data_mcp::{
        data_supply::{mcp::DataApi, observe::Observer},
        mcp::ToolHandler,
    };
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let api = DataApi::new(t.clone(), "run".into(), Access::local()).unwrap();
    let start = api
        .call(
            "start_collection",
            json!({"request_id":"r","symbols":["sh.600006"],"periods":["day","year"]}),
        )
        .unwrap();
    let task = start["task_id"].as_str().unwrap();
    let observer = Observer::new(
        SqliteSupply {
            market: SqliteMarket::new(t),
        },
        "run".into(),
    );
    let p = ProgressQuery {
        task_id: task.into(),
        page_size: 1,
        cursor: None,
    };
    let first = observer.progress(p.clone(), &Access::local()).unwrap();
    assert_eq!(first.total, 2);
    api.call("run_worker_once", json!({})).unwrap();
    let mut next = p.clone();
    next.cursor = first.next_cursor.clone();
    let second = observer.progress(next, &Access::local()).unwrap();
    assert_eq!(first.task, second.task);
    assert_eq!(second.finished, 0);
    assert_ne!(first.unfinished[0].item_id, second.unfinished[0].item_id);
    let latest = observer.progress(p, &Access::local()).unwrap();
    assert_eq!(latest.failed, 2);
    assert!(latest.unfinished.is_empty());
    let mut restricted = Access::local();
    restricted.securities = Some(Default::default());
    let list: TasksQuery = serde_json::from_value(json!({})).unwrap();
    assert!(observer.tasks(list, &restricted).unwrap().items.is_empty());
}
#[test]
fn new_write_sets_replay_globally_and_old_scope_conflicts_are_shared() {
    use x_data_mcp::{data_supply::mcp::DataApi, mcp::ToolHandler};
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let api = DataApi::new(t, "run".into(), Access::local()).unwrap();
    let e = json!({"exchange":"sh","code":"600006"});
    let mut a = json!({"request_id":"new","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[e.clone(),e.clone()]}});
    let first = api.call("start_collection", a.clone()).unwrap();
    a["selection"]["securities"] = json!([e]);
    let reused = api.call("start_collection", a).unwrap();
    assert_eq!(first["task_id"], reused["task_id"]);
    assert_eq!(reused["reused"], true);
    assert_eq!(
        api.call(
            "start_collection",
            json!({"request_id":"legacy","symbols":["sh.600006"],"periods":["day"]})
        )
        .unwrap_err()
        .code,
        "range_conflict"
    );
    assert_eq!(api.call("start_collection",json!({"request_id":"new","dataset":"securities.catalog","contract_version":"v1","selection":{"scope":"current_sh_sz_a"}})).unwrap_err().code,"request_conflict");
    api.call("run_worker_once", json!({})).unwrap();
    let retry=api.call("retry_task",json!({"contract_version":"v1","original_task_id":first["task_id"],"new_request_id":"retry","item_ids":first["item_ids"]})).unwrap();
    assert_eq!(retry["reused"], false);
}
