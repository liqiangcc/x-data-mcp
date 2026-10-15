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
    task_control::TaskRepository,
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
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(SqliteMarket::new(
            t.clone(),
        )),
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
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(SqliteMarket::new(
            SqliteTasks::new(&p),
        )),
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

use crate::support_market as market;
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
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store.clone()).clone(),
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
    let adjustment = first.version_manifest[0]
        .metadata
        .as_ref()
        .unwrap()
        .adjustment
        .as_ref()
        .unwrap();
    assert_eq!(adjustment.field, "price_adjustment");
    assert_eq!(adjustment.status, "confirmed");
    assert_eq!(adjustment.value.as_deref(), Some("qfq"));
    assert!(!adjustment.reason.is_empty());
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
            "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','0')",
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
            json!({"request_id":"r","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600006"},{"exchange":"sz","code":"000001"}]}}),
        )
        .unwrap();
    let task = start["task_id"].as_str().unwrap();
    let observer = Observer::new(
        SqliteSupply {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(SqliteMarket::new(t)),
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
            json!({"request_id":"overlap","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[e]}})
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
    let o = Observer::new(
        SqliteSupply {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
        },
        "run".into(),
    );
    let first = o.availability(args(), &Access::local()).unwrap();
    assert_eq!(first.items[0].availability, Availability::Unknown);
    rusqlite::Connection::open(t.path())
        .unwrap()
        .execute(
            "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','0')",
            [],
        )
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
    let repo = SqliteSupply {
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
    };
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
                            scheme: None,
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
    assert_eq!(
        p["version_manifest"][0]["metadata"]
            .as_object()
            .unwrap()
            .get("adjustment"),
        Some(&json!(null))
    );
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
    let q = Queries::new(SqliteSupply {
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store),
    });
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

#[test]
fn availability_reports_open_proven_gaps_with_canonical_evidence() {
    use x_data_mcp::{
        data_supply::observe::Observer,
        market_data::Period,
        market_storage::{GapCovering, ProvenGap},
    };
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t);
    market::published_fixture(&store, &market::samples()[0]["record"]);
    let gap = |id: &str, start: &str, end: &str| ProvenGap {
        gap_id: id.into(),
        symbol: "sh.600006".into(),
        period: Period::Day,
        start_date: start.parse().unwrap(),
        end_date: end.parse().unwrap(),
        evidence_kind: "fixture_evidence".into(),
        evidence_id: format!("object-{id}"),
        evidence_digest: "a".repeat(64),
        observed_at: "2026-09-15T00:00:00Z".parse().unwrap(),
    };
    store
        .record_proven_gap(&gap("g1", "2025-03-01", "2025-03-10"))
        .unwrap();
    store
        .record_proven_gap(&gap("g2", "2025-01-05", "2025-01-06"))
        .unwrap();
    let o = Observer::new(
        SqliteSupply {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store.clone())
                .clone(),
        },
        "run".into(),
    );
    let first = o.availability(args(), &Access::local()).unwrap();
    let gaps = &first.items[0].confirmed_gaps;
    assert_eq!(gaps.len(), 2);
    assert_eq!(
        gaps[0].range,
        Filter::Daily {
            start_date: "2025-01-05".parse().unwrap(),
            end_date: "2025-01-06".parse().unwrap()
        }
    );
    assert_eq!(
        gaps[0].evidence,
        format!(
            "{{\"kind\":\"fixture_evidence\",\"id\":\"object-g2\",\"sha256\":\"{}\"}}",
            "a".repeat(64)
        )
    );
    assert_eq!(
        gaps[1].range,
        Filter::Daily {
            start_date: "2025-03-01".parse().unwrap(),
            end_date: "2025-03-10".parse().unwrap()
        }
    );
    store
        .cover_gap(&GapCovering {
            gap_id: "g1".into(),
            version_id: "legacy-daily".into(),
            covered_at: chrono::Utc::now(),
        })
        .unwrap();
    let covered = o.availability(args(), &Access::local()).unwrap();
    let remaining = &covered.items[0].confirmed_gaps;
    assert_eq!(
        remaining.as_slice(),
        &[ConfirmedGap {
            range: Filter::Daily {
                start_date: "2025-01-05".parse().unwrap(),
                end_date: "2025-01-06".parse().unwrap()
            },
            evidence: format!(
                "{{\"kind\":\"fixture_evidence\",\"id\":\"object-g2\",\"sha256\":\"{}\"}}",
                "a".repeat(64)
            ),
        }]
    );
}

#[test]
fn availability_maps_yearly_gaps_and_fails_closed_on_unnatural_year_range() {
    use x_data_mcp::{
        data_supply::observe::Observer, market_data::Period, market_storage::ProvenGap,
    };
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t);
    let mut yearly = ProvenGap {
        gap_id: "y1".into(),
        symbol: "sh.600006".into(),
        period: Period::Year,
        start_date: "2024-01-01".parse().unwrap(),
        end_date: "2025-12-31".parse().unwrap(),
        evidence_kind: "fixture_evidence".into(),
        evidence_id: "year-object".into(),
        evidence_digest: "b".repeat(64),
        observed_at: "2026-09-15T00:00:00Z".parse().unwrap(),
    };
    store.record_proven_gap(&yearly).unwrap();
    let o = Observer::new(
        SqliteSupply {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(store.clone())
                .clone(),
        },
        "run".into(),
    );
    let mut a = args();
    a.dataset = Dataset::Yearly;
    a.filter = Filter::Yearly {
        start_year: 2024,
        end_year: 2025,
    };
    let page = o.availability(a.clone(), &Access::local()).unwrap();
    assert_eq!(
        page.items[0].confirmed_gaps.as_slice(),
        &[ConfirmedGap {
            range: Filter::Yearly {
                start_year: 2024,
                end_year: 2025
            },
            evidence: format!(
                "{{\"kind\":\"fixture_evidence\",\"id\":\"year-object\",\"sha256\":\"{}\"}}",
                "b".repeat(64)
            ),
        }]
    );
    yearly.gap_id = "y2".into();
    yearly.start_date = "2024-03-01".parse().unwrap();
    store.record_proven_gap(&yearly).unwrap();
    assert!(o.availability(a, &Access::local()).is_err());
}

#[test]
fn catalog_availability_never_reports_proven_gaps() {
    use x_data_mcp::{catalog::*, data_supply::observe::Observer, sqlite_catalog::SqliteCatalog};
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    SqliteCatalog::new(t.clone())
        .publish_fixture(
            "catalog-1",
            CatalogSnapshot {
                source: "fixture:catalog".into(),
                acquired_at: chrono::Utc::now(),
                completeness: CompletenessEvidence::Complete { expected_count: 1 },
                records: vec![SecurityRecord {
                    symbol: "600006".into(),
                    name: "股票600006".into(),
                    exchange: x_data_mcp::catalog::Exchange::Shanghai,
                    security_type: SecurityType::AShare,
                    source_security_id: "600006".into(),
                    industry: IndustryInfo::Missing {
                        reason: "not mapped".into(),
                    },
                    source: "fixture:catalog".into(),
                    acquired_at: chrono::Utc::now(),
                }],
            },
            None,
        )
        .unwrap();
    let o = Observer::new(
        SqliteSupply {
            market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(SqliteMarket::new(t)),
        },
        "run".into(),
    );
    let a: Query = serde_json::from_value(json!({"dataset":"securities.catalog","contract_version":"v1","selection":{"kind":"catalog","scope":"current_sh_sz_a"},"filter":{"kind":"catalog"},"version":{"kind":"catalog_version","version_id":"catalog-1"}})).unwrap();
    let page = o.availability(a, &Access::local()).unwrap();
    assert_eq!(page.items[0].availability, Availability::Available);
    assert!(page.items[0].confirmed_gaps.is_empty());
}

/// #147 Slice 3 wire: `selection {"kind":"default"}` for market.daily
/// update/backfill — closed shape, snapshot resolution, replay identity.
mod default_selection_wire {
    use super::market;
    use serde_json::json;
    use x_data_mcp::{
        catalog::{
            CatalogSnapshot, CompletenessEvidence, Exchange, IndustryInfo, SecurityRecord,
            SecurityType,
        },
        data_supply::{Access, mcp::DataApi, wire::*},
        market_data::Period,
        market_storage::ProvenGap,
        mcp::ToolHandler,
        sqlite_catalog::SqliteCatalog,
        sqlite_market::SqliteMarket,
        sqlite_tasks::SqliteTasks,
        task_control::TaskRepository,
    };
    fn catalog(catalog: &SqliteCatalog, id: &str, codes: &[&str]) {
        let records = codes
            .iter()
            .map(|code| SecurityRecord {
                symbol: (*code).into(),
                name: (*code).into(),
                exchange: Exchange::Shanghai,
                security_type: SecurityType::AShare,
                source_security_id: (*code).into(),
                industry: IndustryInfo::Missing {
                    reason: "fixture".into(),
                },
                source: "fixture:catalog".into(),
                acquired_at: chrono::Utc::now(),
            })
            .collect::<Vec<_>>();
        catalog
            .publish_fixture(
                id,
                CatalogSnapshot {
                    source: "fixture:catalog".into(),
                    acquired_at: chrono::Utc::now(),
                    completeness: CompletenessEvidence::Complete {
                        expected_count: records.len(),
                    },
                    records,
                },
                None,
            )
            .unwrap();
    }
    fn gap(id: &str, symbol: &str, start: &str, end: &str) -> ProvenGap {
        ProvenGap {
            gap_id: id.into(),
            symbol: symbol.into(),
            period: Period::Day,
            start_date: start.parse().unwrap(),
            end_date: end.parse().unwrap(),
            evidence_kind: "gap_verification_decision".into(),
            evidence_id: format!("evidence-{id}"),
            evidence_digest: "e".repeat(64),
            observed_at: "2026-09-15T00:00:00Z".parse().unwrap(),
        }
    }
    fn daily_write(request_id: &str) -> serde_json::Value {
        json!({"request_id":request_id,"dataset":"market.daily","contract_version":"v1","selection":{"kind":"default"}})
    }
    #[test]
    fn default_selection_is_closed_and_in_the_registered_schema() {
        // Serde level: the variant exists and override fields fail decode.
        assert_eq!(
            serde_json::from_value::<WriteSelection>(json!({"kind":"default"})).unwrap(),
            WriteSelection::Default
        );
        for field in ["base_version_id", "start_date", "end_date", "gap_ids"] {
            let mut v = json!({"kind":"default"});
            v[field] = json!("x");
            assert!(serde_json::from_value::<RangeWriteSelection>(v).is_err());
        }
        assert!(serde_json::from_value::<RangeWriteSelection>(json!({"kind":"explicit"})).is_err());
        // Registered tool schema advertises the closed variant.
        let d = tempfile::tempdir().unwrap();
        let api = DataApi::new(
            SqliteTasks::new(d.path().join("db")),
            "run".into(),
            Access::local(),
        )
        .unwrap();
        let detail = api
            .call(
                "get_data_capabilities",
                json!({"dataset":"market.daily","operation":"update_data"}),
            )
            .unwrap();
        let schema = &detail["items"][0]["input_schema"];
        assert!(schema.to_string().contains("\"default\""));
        jsonschema::validator_for(schema)
            .unwrap()
            .validate(&detail["items"][0]["example"])
            .unwrap();
    }
    #[test]
    fn daily_default_wire_rejects_overrides_conflicts_and_other_kinds() {
        let d = tempfile::tempdir().unwrap();
        let t = SqliteTasks::new(d.path().join("db"));
        t.initialize().unwrap();
        let api = DataApi::new(t, "run".into(), Access::local()).unwrap();
        for (i, mut a) in [
            json!({"dataset":"market.daily","contract_version":"v1","request_id":"w0","selection":{"kind":"default","base_version_id":"x"}}),
            json!({"dataset":"market.daily","contract_version":"v1","request_id":"w1","selection":{"kind":"default","start_date":"2025-01-01"}}),
            json!({"dataset":"market.daily","contract_version":"v1","request_id":"w2","selection":{"kind":"default","gap_ids":["g"]}}),
            json!({"dataset":"market.daily","contract_version":"v1","request_id":"w3","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600006"}]}}),
            json!({"dataset":"market.daily","contract_version":"v1","request_id":"w4","selection":{"kind":"catalog","version_id":"c1"}}),
            json!({"dataset":"market.daily","contract_version":"v1","request_id":"w5","selection":{"kind":"default"},"items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"v","start_date":"2025-01-01","end_date":"2025-02-01"}]}),
            json!({"dataset":"market.daily","contract_version":"v1","request_id":"w6"}),
            json!({"dataset":"market.yearly","contract_version":"v1","request_id":"w7","selection":{"kind":"default"},"items":[{"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":"v","years":[2025],"publication":{"mode":"current_head"}}]}),
        ]
        .into_iter()
        .enumerate()
        {
            a["request_id"] = json!(format!("w{i}"));
            assert_eq!(
                api.call("update_data", a).unwrap_err().code,
                "invalid_arguments"
            );
        }
        // start_collection does not accept the default selection either.
        assert_eq!(
            api.call(
                "start_collection",
                json!({"request_id":"c0","dataset":"market.daily","contract_version":"v1","selection":{"kind":"default"}})
            )
            .unwrap_err()
            .code,
            "invalid_arguments"
        );
        assert_eq!(
            api.call(
                "start_collection",
                json!({"request_id":"c1","dataset":"market.yearly","contract_version":"v1","selection":{"kind":"default"},"base_daily_versions":[],"years":[2025]})
            )
            .unwrap_err()
            .code,
            "invalid_arguments"
        );
    }
    #[test]
    fn update_default_freezes_snapshot_evidence_and_replays_without_recompute() {
        let d = tempfile::tempdir().unwrap();
        let t = SqliteTasks::new(d.path().join("db"));
        t.initialize().unwrap();
        let store = SqliteMarket::new(t.clone());
        catalog(&SqliteCatalog::new(t.clone()), "catalog-1", &["600006"]);
        market::published_fixture(&store, &market::samples()[0]["record"]);
        store
            .record_proven_gap(&gap("g1", "sh.600006", "2025-02-01", "2025-02-10"))
            .unwrap();
        let api = DataApi::new(t.clone(), "run".into(), Access::local()).unwrap();
        let first = api.call("update_data", daily_write("d1")).unwrap();
        assert_eq!(first["reused"], json!(false));
        assert_eq!(first["contract_version"], json!("default-range-task-v1"));
        let task_id = first["task_id"].as_str().unwrap().to_owned();
        // Frozen per-item inputs carry both provenance classes.
        let snap = t.read_task(&task_id).unwrap().unwrap();
        let f = &snap.items[0].spec.frozen_inputs;
        // The item carries only the evidence digest marker; the resolved
        // selection object is stored once in the task's shared evidence.
        let shared = snap.task.work.shared_evidence.as_ref().unwrap();
        assert_eq!(
            f["selection_evidence"]["evidence_digest"],
            shared["evidence_digest"]
        );
        assert_eq!(shared["catalog_version_id"], json!("catalog-1"));
        assert_eq!(shared["securities"], json!(["sh.600006"]));
        assert_eq!(f["base_version_id"], json!("legacy-daily"));
        assert_eq!(f["base_integrity_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(f["refresh_range"]["start_date"], json!("2024-12-31"));
        assert_eq!(f["proven_gap_ranges"][0]["gap_id"], json!("g1"));
        assert_eq!(
            f["proven_gap_ranges"][0]["evidence_digest"],
            json!("e".repeat(64))
        );
        // Progress projects refresh + gap ranges from the default frozen
        // shape instead of failing closed on the new contract version.
        let progress = api
            .call(
                "get_task_progress",
                json!({"task_id":task_id,"page_size":100}),
            )
            .unwrap();
        let ranges = progress["frozen_daily_ranges"].as_array().unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0]["start_date"], json!("2024-12-31"));
        assert_eq!(ranges[1]["start_date"], json!("2025-02-01"));
        // Facts change after acceptance; the same request replays the
        // original task instead of recomputing.
        store
            .record_proven_gap(&gap("g9", "sh.600006", "2025-03-01", "2025-03-02"))
            .unwrap();
        let replay = api.call("update_data", daily_write("d1")).unwrap();
        assert_eq!(replay["reused"], json!(true));
        assert_eq!(replay["task_id"], json!(task_id));
        let after = t
            .read_task(replay["task_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(after.items[0].spec.frozen_inputs, *f);
        // Same request_id with different arguments conflicts.
        let mut different = daily_write("d1");
        different.as_object_mut().unwrap().remove("selection");
        different["items"] = json!([{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"legacy-daily","start_date":"2025-01-01","end_date":"2025-12-31"}]);
        assert_eq!(
            api.call("update_data", different).unwrap_err().code,
            "request_conflict"
        );
    }
    #[test]
    fn backfill_default_creates_only_gap_items_and_empty_fails_closed() {
        let d = tempfile::tempdir().unwrap();
        let t = SqliteTasks::new(d.path().join("db"));
        t.initialize().unwrap();
        let store = SqliteMarket::new(t.clone());
        catalog(&SqliteCatalog::new(t.clone()), "catalog-1", &["600006"]);
        market::published_fixture(&store, &market::samples()[0]["record"]);
        let api = DataApi::new(t.clone(), "run".into(), Access::local()).unwrap();
        // No open proven gaps -> fail closed, nothing persisted.
        assert_eq!(
            api.call("backfill_data", daily_write("b0"))
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
        assert!(t.read_request("b0").unwrap().is_none());
        store
            .record_proven_gap(&gap("g1", "sh.600006", "2025-02-01", "2025-02-10"))
            .unwrap();
        let first = api.call("backfill_data", daily_write("b1")).unwrap();
        assert_eq!(first["contract_version"], json!("default-range-task-v1"));
        let snap = t
            .read_task(first["task_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(snap.items.len(), 1);
        let f = &snap.items[0].spec.frozen_inputs;
        assert!(f.get("refresh_range").is_none());
        assert_eq!(f["proven_gap_ranges"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn explicit_daily_update_is_unchanged() {
        let d = tempfile::tempdir().unwrap();
        let t = SqliteTasks::new(d.path().join("db"));
        t.initialize().unwrap();
        let store = SqliteMarket::new(t.clone());
        catalog(&SqliteCatalog::new(t.clone()), "catalog-1", &["600006"]);
        market::published_fixture(&store, &market::samples()[0]["record"]);
        let api = DataApi::new(t, "run".into(), Access::local()).unwrap();
        let first = api
            .call(
                "update_data",
                json!({"request_id":"e1","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"legacy-daily","start_date":"2025-12-01","end_date":"2025-12-31"}]}),
            )
            .unwrap();
        assert_eq!(first["contract_version"], json!("explicit-range-task-v1"));
        let replay = api
            .call(
                "update_data",
                json!({"request_id":"e1","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"legacy-daily","start_date":"2025-12-01","end_date":"2025-12-31"}]}),
            )
            .unwrap();
        assert_eq!(replay["reused"], json!(true));
    }
}
#[test]
fn verify_data_gap_wire_is_closed_and_freezes_subject() {
    use x_data_mcp::{data_supply::mcp::DataApi, mcp::ToolHandler};
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t.clone());
    market::published_fixture(&store, &market::samples()[0]["record"]);
    let api = DataApi::new(t.clone(), "run".into(), Access::local()).unwrap();
    // The tool is on the public surface with a closed schema.
    let tools = api.tools();
    assert!(
        tools
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "verify_data_gap")
    );
    let payload = |id: &str| json!({"contract_version":"v1","request_id":id,"target":{"symbol":"sh.600006","period":"day","start_date":"2025-01-01","end_date":"2025-01-05"}});
    // Any caller pass-through field fails decode, top-level or nested.
    for field in [
        "conclusion",
        "basis_sha256",
        "decision_sha256",
        "decided_by",
        "verifier_id",
        "subject_version_id",
        "subject_version_digest",
    ] {
        let mut top = payload("pt");
        top[field] = json!("x");
        assert_eq!(
            api.call("verify_data_gap", top).unwrap_err().code,
            "invalid_arguments",
            "top-level {field}"
        );
        let mut nested = payload("pn");
        nested["target"][field] = json!("x");
        assert_eq!(
            api.call("verify_data_gap", nested).unwrap_err().code,
            "invalid_arguments",
            "nested {field}"
        );
    }
    // period=year and inverted/bad targets reject before persistence.
    let mut year = payload("year");
    year["target"]["period"] = json!("year");
    assert_eq!(
        api.call("verify_data_gap", year).unwrap_err().code,
        "invalid_arguments"
    );
    let mut inverted = payload("inv");
    inverted["target"]["start_date"] = json!("2025-01-05");
    inverted["target"]["end_date"] = json!("2025-01-01");
    assert_eq!(
        api.call("verify_data_gap", inverted).unwrap_err().code,
        "invalid_arguments"
    );
    assert!(t.read_request("year").unwrap().is_none());
    assert!(t.read_request("inv").unwrap().is_none());
    // Acceptance freezes the published subject head into item inputs.
    let accepted = api.call("verify_data_gap", payload("g1")).unwrap();
    assert_eq!(
        accepted["contract_version"],
        json!("gap-verification-task-v1")
    );
    assert_eq!(accepted["dataset"], json!("market.gap_verification"));
    assert_eq!(accepted["reused"], json!(false));
    assert_eq!(accepted["runtime_readiness"], json!("blocked"));
    let snap = t
        .read_task(accepted["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let frozen = &snap.items[0].spec.frozen_inputs;
    assert_eq!(frozen["subject_version_id"], json!("legacy-daily"));
    assert_eq!(frozen["subject_version_digest"].as_str().unwrap().len(), 64);
    assert_eq!(frozen["target"]["symbol"], json!("sh.600006"));
    // Same request_id replays the original acceptance.
    let replay = api.call("verify_data_gap", payload("g1")).unwrap();
    assert_eq!(replay["reused"], json!(true));
    assert_eq!(replay["task_id"], accepted["task_id"]);
}
#[test]
fn verify_data_gap_worker_official_stub_fails_closed() {
    use x_data_mcp::{data_supply::mcp::DataApi, mcp::ToolHandler};
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t.clone());
    market::published_fixture(&store, &market::samples()[0]["record"]);
    let api = DataApi::new(t.clone(), "run".into(), Access::local()).unwrap();
    let accepted = api
        .call(
            "verify_data_gap",
            json!({"contract_version":"v1","request_id":"g-official","target":{"symbol":"sh.600006","period":"day","start_date":"2025-01-01","end_date":"2025-01-05"}}),
        )
        .unwrap();
    let worked = api.call("run_worker_once", json!({})).unwrap();
    assert_eq!(worked["task_id"], accepted["task_id"]);
    let snap = t
        .read_task(accepted["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let outcome = snap.items[0].outcome.as_ref().unwrap();
    assert!(!outcome.success);
    assert!(
        outcome
            .error
            .as_ref()
            .unwrap()
            .starts_with("GAP_BASIS_NOT_VERIFIED")
    );
    let c = rusqlite::Connection::open(t.path()).unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM gap_verification_decisions", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM proven_gaps", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn verify_data_gap_fixture_verifier_end_to_end_reports_open_gap() {
    use x_data_mcp::{
        data_supply::mcp::DataApi,
        gap_verification::{
            GapBasis, GapTarget, SourceGapVerifier, SubjectBinding,
            runner::{GapVerifyRunner, gap_decision_id},
        },
        market_storage::Error,
        mcp::ToolHandler,
    };
    struct Fixture;
    impl SourceGapVerifier for Fixture {
        fn verifier_id(&self) -> &str {
            "fixture-verifier"
        }
        fn verify(&self, target: &GapTarget, subject: &SubjectBinding) -> Result<GapBasis, Error> {
            let mut basis = GapBasis {
                source_identity: "fixture-source".into(),
                source_contract: "fixture-source-v1".into(),
                symbol: target.symbol.clone(),
                start_date: target.start_date,
                end_date: target.end_date,
                subject_version_id: subject.version_id.clone(),
                subject_version_digest: subject.integrity_sha256.clone(),
                artifact_identity: "fixture:source-facts".into(),
                artifact_sha256: "a".repeat(64),
                conclusion: "confirmed_missing_range".into(),
                observed_at: "2026-09-15T00:00:00Z".parse().unwrap(),
                verifier_id: "fixture-verifier".into(),
                basis_sha256: String::new(),
            };
            basis.basis_sha256 = basis.content_digest();
            Ok(basis)
        }
    }
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    let store = SqliteMarket::new(t.clone());
    market::published_fixture(&store, &market::samples()[0]["record"]);
    let api = DataApi::new(t.clone(), "run".into(), Access::local()).unwrap();
    let accepted = api
        .call(
            "verify_data_gap",
            json!({"contract_version":"v1","request_id":"g-e2e","target":{"symbol":"sh.600006","period":"day","start_date":"2025-01-01","end_date":"2025-01-05"}}),
        )
        .unwrap();
    let task_id = accepted["task_id"].as_str().unwrap().to_owned();
    let snap = t.read_task(&task_id).unwrap().unwrap();
    let decision_id = gap_decision_id(&task_id, &snap.items[0].spec.item_id);
    let runner = GapVerifyRunner {
        repository: store.clone(),
        run_id: "run".into(),
    };
    runner.run(&task_id, &Fixture, chrono::Utc::now()).unwrap();
    // Decision + proven gap committed atomically under the derived id.
    let decision = store
        .gap_verification_decision(&decision_id)
        .unwrap()
        .unwrap();
    assert_eq!(decision.conclusion, "confirmed_missing_range");
    assert_eq!(decision.verifier_id, "fixture-verifier");
    // Availability reports the open gap for the verified range.
    let availability = api
        .call(
            "get_data_availability",
            json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},"filter":{"kind":"daily","start_date":"2025-01-01","end_date":"2025-01-05"},"version":{"kind":"latest_per_entity"}}),
        )
        .unwrap();
    let gaps = availability["items"][0]["confirmed_gaps"]
        .as_array()
        .unwrap();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0]["range"]["start_date"], json!("2025-01-01"));
    assert_eq!(gaps[0]["range"]["end_date"], json!("2025-01-05"));
    assert_eq!(
        gaps[0]["evidence"],
        json!(format!(
            "{{\"kind\":\"gap_verification_decision\",\"id\":\"{decision_id}\",\"sha256\":\"{}\"}}",
            decision.decision_sha256
        ))
    );
    // A decision never schedules update/backfill work by itself.
    let replay = api
        .call(
            "verify_data_gap",
            json!({"contract_version":"v1","request_id":"g-e2e","target":{"symbol":"sh.600006","period":"day","start_date":"2025-01-01","end_date":"2025-01-05"}}),
        )
        .unwrap();
    assert_eq!(replay["reused"], json!(true));
}

#[test]
fn every_advertised_data_tool_schema_has_an_object_root() {
    use x_data_mcp::{data_supply::mcp::DataApi, mcp::ToolHandler};
    let d = tempfile::tempdir().unwrap();
    let api = DataApi::new(
        SqliteTasks::new(d.path().join("db")),
        "run".into(),
        Access::local(),
    )
    .unwrap();
    let tools = api.tools();
    let tools = tools.as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    // The tools whose root schemars rendered as a bare oneOf/anyOf must be
    // covered by this assertion rather than silently absent.
    for expected in [
        "update_data",
        "backfill_data",
        "republish_data",
        "start_collection",
        "plan_market_update",
        "run_market_update",
        "verify_publication_result",
        "verify_stopped_writes",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        assert_eq!(tool["inputSchema"]["type"], json!("object"), "input {name}");
        if let Some(output) = tool.get("outputSchema") {
            assert_eq!(output["type"], json!("object"), "output {name}");
        }
    }
}
