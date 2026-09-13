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
    assert_eq!(latest.unfinished.len(), 1);
    assert_eq!(latest.unfinished[0].success, Some(false));
    assert_eq!(latest.execution_percent, "100");
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

#[test]
fn capabilities_do_not_advertise_unauthorized_controls_and_new_errors_are_explicit() {
    use x_data_mcp::{data_supply::mcp::DataApi, mcp::ToolHandler};
    let d = tempfile::tempdir().unwrap();
    let mut a = Access::local();
    a.principal = "reader".into();
    let api = DataApi::new(SqliteTasks::new(d.path().join("missing")), "run".into(), a).unwrap();
    let caps = api.call("get_data_capabilities", json!({})).unwrap();
    assert!(
        caps["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["tool_name"] != "start_collection" && i["tool_name"] != "run_worker_once")
    );
    assert!(
        !api.tools()
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "run_worker_once")
    );
    assert_eq!(
        api.call("run_worker_once", json!({})).unwrap_err().code,
        "forbidden"
    );
    let e = api
        .call("query_data", json!({"dataset":"unknown"}))
        .unwrap_err();
    assert_eq!(e.code, "unknown_dataset");
    assert_eq!(e.outcome_certainty.as_deref(), Some("not_applicable"));
}

#[test]
fn capability_dataset_examples_validate_against_the_actual_registered_schema() {
    use x_data_mcp::{data_supply::mcp::DataApi, mcp::ToolHandler};
    let d = tempfile::tempdir().unwrap();
    let api = DataApi::new(
        SqliteTasks::new(d.path().join("db")),
        "run".into(),
        Access::local(),
    )
    .unwrap();
    for dataset in ["securities.catalog", "market.daily", "market.yearly"] {
        for operation in [
            "query_data",
            "list_data_versions",
            "get_data_availability",
            "start_collection",
        ] {
            let detail = api
                .call(
                    "get_data_capabilities",
                    json!({"dataset":dataset,"operation":operation}),
                )
                .unwrap();
            for i in detail["items"].as_array().unwrap() {
                jsonschema::validator_for(&i["input_schema"])
                    .unwrap()
                    .validate(&i["example"])
                    .unwrap();
                assert_eq!(i["example"]["dataset"], dataset);
            }
        }
    }
}

#[test]
fn availability_classifies_corrupt_version_but_not_bad_database_as_absent() {
    use x_data_mcp::data_supply::observe::Observer;
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t.clone());
    market::published_fixture(&store, &market::samples()[0]["record"]);
    let o = Observer::new(SqliteSupply { market: store }, "run".into());
    let first = o.availability(args(), &Access::local()).unwrap();
    assert_eq!(first.items[0].availability, Availability::Unknown);
    rusqlite::Connection::open(t.path())
        .unwrap()
        .execute("UPDATE daily_bars SET close='0'", [])
        .unwrap();
    let bad = o.availability(args(), &Access::local()).unwrap();
    assert_eq!(bad.items[0].availability, Availability::Unavailable);
    rusqlite::Connection::open(t.path())
        .unwrap()
        .execute_batch("PRAGMA user_version=999")
        .unwrap();
    assert!(o.availability(args(), &Access::local()).is_err());
}

#[test]
fn query_budgets_cursor_instance_eviction_and_permissions_are_enforced() {
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t);
    market::published_fixture(&store, &market::samples()[0]["record"]);
    let repo = SqliteSupply { market: store };
    let q = Queries::new(repo.clone());
    for size in [0, 1001] {
        let mut a = args();
        a.page_size = size;
        assert!(q.query(a, &Access::local()).is_err());
    }
    let mut a = args();
    a.selection = Selection::Batch {
        entities: (0..101)
            .map(|n| Security {
                exchange: Exchange::Sh,
                code: format!("{n:06}"),
            })
            .collect(),
    };
    assert!(q.query(a, &Access::local()).is_err());
    let first = q.query(args(), &Access::local()).unwrap();
    let mut next = args();
    next.cursor = first.next_cursor;
    assert!(
        Queries::new(repo)
            .query(next.clone(), &Access::local())
            .is_err()
    );
    let mut revoked = Access::local();
    revoked.principal = "another-context".into();
    assert!(q.query(next.clone(), &revoked).is_err());
    for _ in 0..33 {
        q.query(args(), &Access::local()).unwrap();
    }
    assert!(q.query(next, &Access::local()).is_err());
}
#[test]
fn catalog_filter_and_selector_freeze_version_without_inventing_industry() {
    use x_data_mcp::{
        catalog::*, data_supply::mcp::DataApi, mcp::ToolHandler, sqlite_catalog::SqliteCatalog,
    };
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let now = chrono::Utc::now();
    let source = "fixture:selector".to_string();
    let record = |symbol: &str, industry: IndustryInfo| SecurityRecord {
        symbol: symbol.into(),
        name: format!("股票{symbol}"),
        exchange: x_data_mcp::catalog::Exchange::Shanghai,
        security_type: SecurityType::AShare,
        source_security_id: symbol.into(),
        industry,
        source: source.clone(),
        acquired_at: now,
    };
    SqliteCatalog::new(t.clone())
        .publish_fixture(
            "catalog-1",
            CatalogSnapshot {
                source: source.clone(),
                acquired_at: now,
                completeness: CompletenessEvidence::Complete { expected_count: 2 },
                records: vec![
                    record(
                        "600006",
                        IndustryInfo::Assigned {
                            code: "I".into(),
                            name: "行业".into(),
                        },
                    ),
                    record(
                        "600007",
                        IndustryInfo::Missing {
                            reason: "not mapped".into(),
                        },
                    ),
                ],
            },
            None,
        )
        .unwrap();
    let api = DataApi::new(t, "run".into(), Access::local()).unwrap();
    let a = json!({"dataset":"securities.catalog","contract_version":"v1","selection":{"kind":"catalog","scope":"current_sh_sz_a"},"filter":{"kind":"catalog","industry_code":"I","name_query":"股票"},"version":{"kind":"catalog_version","version_id":"catalog-1"}});
    let p = api.call("query_data", a).unwrap();
    assert_eq!(p["records"].as_array().unwrap().len(), 1);
    let accepted=api.call("start_collection",json!({"dataset":"market.daily","contract_version":"v1","request_id":"selector","selection":{"kind":"catalog","version_id":p["version_manifest"][0]["version_ref"]["version_id"],"industry_code":"I"}})).unwrap();
    assert_eq!(accepted["item_ids"].as_array().unwrap().len(), 1);
}
#[test]
fn yearly_collection_requires_exact_fixed_base_map_and_stores_frozen_years() {
    use x_data_mcp::{data_supply::mcp::DataApi, mcp::ToolHandler, task_control::TaskRepository};
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t.clone());
    market::published_fixture(&store, &market::samples()[0]["record"]);
    let api = DataApi::new(t.clone(), "run".into(), Access::local()).unwrap();
    let e = json!({"exchange":"sh","code":"600006"});
    let mut a = json!({"dataset":"market.yearly","contract_version":"v1","request_id":"year","selection":{"kind":"explicit","securities":[e.clone() ]},"base_daily_versions":[{"entity":e,"version_id":"legacy-daily"}],"years":[2025,2025]});
    let accepted = api.call("start_collection", a.clone()).unwrap();
    a["years"] = json!([2025]);
    assert_eq!(
        api.call("start_collection", a.clone()).unwrap()["reused"],
        true
    );
    let task = t
        .read_task(accepted["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(task.items[0].spec.frozen_inputs["years"], json!([2025]));
    assert_eq!(
        task.items[0].spec.frozen_inputs["base_daily_version_id"],
        "legacy-daily"
    );
    a["request_id"] = json!("bad-map");
    a["base_daily_versions"] = json!([]);
    assert!(api.call("start_collection", a).is_err());
    assert!(t.read_request("bad-map").unwrap().is_none());
}

#[test]
fn yearly_query_returns_actual_participation_and_unfinished_semantics() {
    use x_data_mcp::market_storage::{MarketRepository, PublishIntent};
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t);
    market::published_fixture(&store, &market::samples()[0]["record"]);
    let yearly = market::samples()[2]["record"].clone();
    let plan = market::prepare(&store, &yearly, PublishIntent::HistoricalFixedBase);
    store.commit(&plan).unwrap();
    let mut a = args();
    a.dataset = Dataset::Yearly;
    a.filter = Filter::Yearly {
        start_year: 2025,
        end_year: 2025,
    };
    a.version = VersionChoice::ExplicitVersionMap {
        versions: vec![VersionBinding {
            entity: Security {
                exchange: Exchange::Sh,
                code: "600006".into(),
            },
            version_id: yearly["version_id"].as_str().unwrap().into(),
        }],
    };
    let q = Queries::new(SqliteSupply { market: store });
    let p = q.query(a, &Access::local()).unwrap();
    match &p.records[0] {
        Row::Market {
            first_trading_date,
            last_trading_date,
            year_unfinished,
            ..
        } => {
            assert_eq!(*first_trading_date, Some("2025-01-02".parse().unwrap()));
            assert_eq!(*last_trading_date, Some("2025-12-31".parse().unwrap()));
            assert_eq!(*year_unfinished, Some(false));
        }
        _ => panic!(),
    }
}

#[test]
fn range_controls_are_not_discovered_or_callable_without_owner_access() {
    use x_data_mcp::{data_supply::mcp::DataApi, mcp::ToolHandler};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing");
    let mut access = Access::local();
    access.principal = "reader".into();
    let api = DataApi::new(SqliteTasks::new(&path), "run".into(), access).unwrap();
    for name in [
        "update_data",
        "backfill_data",
        "verify_stopped_writes",
        "verify_publication_result",
        "inspect_verification_task",
    ] {
        assert!(
            !api.tools()
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == name)
        );
        assert!(
            api.call("get_data_capabilities", json!({"operation":name}))
                .unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(api.call(name, json!({})).unwrap_err().code, "forbidden");
    }
    assert!(!path.exists());
}

#[test]
fn range_storage_failure_is_not_reported_as_invalid_input_or_success() {
    use x_data_mcp::{data_supply::mcp::DataApi, mcp::ToolHandler};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing");
    let api = DataApi::new(SqliteTasks::new(&path), "run".into(), Access::local()).unwrap();
    let error=api.call("update_data",json!({"request_id":"r","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"missing","start_date":"2025-01-01","end_date":"2025-12-31"}]})).unwrap_err();
    assert_eq!(error.code, "storage_error");
    assert_eq!(error.outcome_certainty.as_deref(), Some("unknown"));
    assert!(!path.exists());
}
