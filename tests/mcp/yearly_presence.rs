//! get_yearly_base_years (XP-014, generic-data-v4): frozen contract tests.
//! Mandatory cases + wire/authorization/discovery coverage on the legacy
//! backend; the compact verified path is covered in crate tests.
use serde_json::{Value, json};
use std::collections::BTreeSet;
use x_data_mcp::{
    data_supply::{Access, mcp::DataApi, wire::*},
    market_storage::PublishIntent,
    mcp::ToolHandler,
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
};

use crate::support_market as market;

fn daily_with_dates(id: &str, symbol: &str, dates: &[&str]) -> Value {
    let mut r = market::samples()[0]["record"].clone();
    r["version_id"] = json!(id);
    r["symbol"] = json!(symbol);
    r["batch"]["symbol"] = json!(symbol);
    r["batch"]["provenance"]["symbol"] = json!(symbol);
    let bars: Vec<Value> = dates
        .iter()
        .map(|d| {
            let mut b = r["batch"]["bars"][0].clone();
            b["date"] = json!(d);
            b
        })
        .collect();
    r["batch"]["bars"] = json!(bars);
    r["records"] = r["batch"]["bars"].clone();
    r["validation"] = json!({
        "record_count": dates.len(),
        "unique_record_count": dates.len(),
        "duplicate_count": 0,
    });
    r["integrity_sha256"] = json!(x_data_mcp::market_storage::compat::legacy_digest(&r).unwrap());
    r
}

fn store() -> (tempfile::TempDir, SqliteMarket) {
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    (d, SqliteMarket::new(t))
}

fn call(api: &DataApi, items: Value) -> Result<Value, x_data_mcp::mcp::ApiError> {
    api.call(
        "get_yearly_base_years",
        json!({"contract_version":"v1","items":items}),
    )
}

fn presence(
    api: &DataApi,
    entity: Value,
    version_id: &str,
    extra: Value,
) -> Result<Value, x_data_mcp::mcp::ApiError> {
    let mut item = json!({"entity":entity,"base_daily_version_id":version_id});
    item.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    call(api, json!([item]))
}

#[test]
fn sparse_and_bounded_years_resolve_exactly() {
    let (_d, store) = store();
    market::published_fixture(
        &store,
        &daily_with_dates("v-sparse", "sh.600006", &["2024-01-05", "2026-03-04"]),
    );
    let api = DataApi::new(store.tasks.clone(), "run".into(), Access::local()).unwrap();
    let e = json!({"exchange":"sh","code":"600006"});
    let reply = presence(&api, e.clone(), "v-sparse", json!({})).unwrap();
    assert_eq!(reply["items"][0]["status"], "resolved");
    assert_eq!(
        reply["items"][0]["present_years"],
        json!([2024, 2026]),
        "mandatory sparse case: daily 2024+2026 must resolve exactly [2024,2026]"
    );
    assert_eq!(reply["items"][0]["version_ref"]["version_id"], "v-sparse");
    assert_eq!(
        reply["items"][0]["version_ref"]["integrity_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(reply["page_complete"], true);
    assert!(reply["snapshot_id"].is_string());
    assert!(reply["snapshot_time"].is_string());
    // Bounds intersect the verified set: in-window subset and legal empty.
    let reply = presence(&api, e.clone(), "v-sparse", json!({"start_year":2025})).unwrap();
    assert_eq!(reply["items"][0]["present_years"], json!([2026]));
    let reply = presence(&api, e, "v-sparse", json!({"end_year":2025})).unwrap();
    assert_eq!(reply["items"][0]["present_years"], json!([2024]));
}

#[test]
fn empty_history_resolves_empty_while_missing_and_unpublished_are_unavailable() {
    let (_d, store) = store();
    market::published_fixture(&store, &daily_with_dates("v-empty", "sh.600006", &[]));
    market::published_fixture(
        &store,
        &daily_with_dates("v-real", "sh.600007", &["2025-06-30"]),
    );
    let staged = daily_with_dates("v-staged", "sh.600008", &["2025-01-02"]);
    store.stage(&staged).unwrap();
    let api = DataApi::new(store.tasks.clone(), "run".into(), Access::local()).unwrap();
    let reply = call(
        &api,
        json!([
            {"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":"v-empty"},
            {"entity":{"exchange":"sh","code":"600007"},"base_daily_version_id":"v-missing"},
            {"entity":{"exchange":"sh","code":"600008"},"base_daily_version_id":"v-staged"},
            {"entity":{"exchange":"sh","code":"600009"},"base_daily_version_id":"v-real"},
        ]),
    )
    .unwrap();
    let items = reply["items"].as_array().unwrap();
    // Output is sorted by Security.
    assert_eq!(items[0]["entity"]["code"], "600006");
    assert_eq!(items[0]["status"], "resolved");
    assert_eq!(
        items[0]["present_years"],
        json!([]),
        "valid empty fixed daily resolves [], never fabricated"
    );
    assert_eq!(items[0]["version_ref"]["version_id"], "v-empty");
    assert_eq!(items[0]["reason_code"], Value::Null);
    assert_eq!(items[1]["status"], "unavailable");
    assert_eq!(items[1]["reason_code"], "base_unavailable");
    assert_eq!(items[1]["version_ref"], Value::Null);
    assert_eq!(items[1]["present_years"], Value::Null);
    assert_eq!(items[2]["status"], "unavailable");
    assert_eq!(items[2]["reason_code"], "base_unavailable");
    // Entity/version mismatch is integrity_failure, never base_unavailable.
    assert_eq!(items[3]["status"], "unavailable");
    assert_eq!(items[3]["reason_code"], "integrity_failure");
}

#[test]
fn period_mismatch_and_corrupt_base_fail_as_integrity_failure() {
    use x_data_mcp::market_storage::MarketRepository;
    let (d, store) = store();
    market::published_fixture(
        &store,
        &daily_with_dates("v-daily", "sh.600006", &["2025-01-02"]),
    );
    let yearly = market::samples()[2]["record"].clone();
    let plan = market::prepare(&store, &yearly, PublishIntent::HistoricalFixedBase);
    store.commit(&plan).unwrap();
    let api = DataApi::new(store.tasks.clone(), "run".into(), Access::local()).unwrap();
    let e = json!({"exchange":"sh","code":"600006"});
    let reply = presence(&api, e.clone(), "legacy-yearly", json!({})).unwrap();
    assert_eq!(reply["items"][0]["status"], "unavailable");
    assert_eq!(reply["items"][0]["reason_code"], "integrity_failure");
    // Corrupt the published daily's stored bars: the full verified read
    // must fail closed as integrity_failure, never resolved/absent.
    rusqlite::Connection::open(d.path().join("db"))
        .unwrap()
        .execute(
            "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','0')",
            [],
        )
        .unwrap();
    let reply = presence(&api, e, "v-daily", json!({})).unwrap();
    assert_eq!(reply["items"][0]["status"], "unavailable");
    assert_eq!(reply["items"][0]["reason_code"], "integrity_failure");
}

#[test]
fn pinned_base_reads_stay_fixed_after_head_moves() {
    use x_data_mcp::market_storage::MarketRepository;
    let (_d, store) = store();
    let base = daily_with_dates("v1", "sh.600006", &["2024-06-30", "2025-06-30"]);
    market::published_fixture(&store, &base);
    // A newer head carries 2026 too; the pinned read must not see it.
    let next = daily_with_dates(
        "v2",
        "sh.600006",
        &["2024-06-30", "2025-06-30", "2026-06-30"],
    );
    let plan = market::prepare(
        &store,
        &next,
        PublishIntent::Daily {
            expected_head: Some("v1".into()),
        },
    );
    store.commit(&plan).unwrap();
    let api = DataApi::new(store.tasks.clone(), "run".into(), Access::local()).unwrap();
    let e = json!({"exchange":"sh","code":"600006"});
    let reply = presence(&api, e, "v1", json!({})).unwrap();
    assert_eq!(reply["items"][0]["status"], "resolved");
    assert_eq!(reply["items"][0]["present_years"], json!([2024, 2025]));
    assert_eq!(reply["items"][0]["version_ref"]["version_id"], "v1");
    let reply = presence(
        &api,
        json!({"exchange":"sh","code":"600006"}),
        "v2",
        json!({}),
    )
    .unwrap();
    assert_eq!(
        reply["items"][0]["present_years"],
        json!([2024, 2025, 2026])
    );
}

#[test]
fn future_dated_year_is_a_read_fact_not_dropped() {
    let (_d, store) = store();
    market::published_fixture(
        &store,
        &daily_with_dates("v-future", "sh.600006", &["2025-06-30", "2099-01-05"]),
    );
    let api = DataApi::new(store.tasks.clone(), "run".into(), Access::local()).unwrap();
    let reply = presence(
        &api,
        json!({"exchange":"sh","code":"600006"}),
        "v-future",
        json!({"end_year":2026}),
    )
    .unwrap();
    assert_eq!(reply["items"][0]["present_years"], json!([2025]));
    let reply = presence(
        &api,
        json!({"exchange":"sh","code":"600006"}),
        "v-future",
        json!({}),
    )
    .unwrap();
    assert_eq!(
        reply["items"][0]["present_years"],
        json!([2025, 2099]),
        "future-dated content is reported, never silently omitted"
    );
}

#[test]
fn request_level_validation_rejects_shape_duplicates_bounds_and_fields() {
    let (_d, store) = store();
    market::published_fixture(
        &store,
        &daily_with_dates("v1", "sh.600006", &["2025-01-02"]),
    );
    let api = DataApi::new(store.tasks.clone(), "run".into(), Access::local()).unwrap();
    let e = json!({"exchange":"sh","code":"600006"});
    let err = |items: Value| call(&api, items).unwrap_err().code;
    assert_eq!(err(json!([])), "invalid_arguments");
    assert_eq!(
        err(
            json!([{"entity":e,"base_daily_version_id":"v1"},{"entity":e,"base_daily_version_id":"v1"}])
        ),
        "invalid_arguments"
    );
    assert_eq!(
        err(json!([{"entity":{"exchange":"sh","code":"abc"},"base_daily_version_id":"v1"}])),
        "invalid_arguments"
    );
    assert_eq!(
        err(json!([{"entity":e,"base_daily_version_id":""}])),
        "invalid_arguments"
    );
    assert_eq!(
        err(json!([{"entity":e,"base_daily_version_id":"v1","start_year":2026,"end_year":2025}])),
        "invalid_arguments"
    );
    assert_eq!(
        err(json!([{"entity":e,"base_daily_version_id":"v1","start_year":0}])),
        "invalid_arguments"
    );
    assert_eq!(
        err(json!([{"entity":e,"base_daily_version_id":"v1","end_year":10000}])),
        "invalid_arguments"
    );
    assert_eq!(
        err(json!([{"entity":e,"base_daily_version_id":"v1","bogus":1}])),
        "invalid_arguments"
    );
    let oversized: Vec<Value> = (0..101)
        .map(|n| json!({"entity":{"exchange":"sh","code":format!("{n:06}")},"base_daily_version_id":"v1"}))
        .collect();
    assert_eq!(err(json!(oversized)), "invalid_arguments");
    // contract_version != v1 is rejected by the shared gate.
    assert_eq!(
        api.call(
            "get_yearly_base_years",
            json!({"contract_version":"v2","items":[{"entity":e,"base_daily_version_id":"v1"}]}),
        )
        .unwrap_err()
        .code,
        "unsupported_contract"
    );
}

#[test]
fn authorization_preflight_completes_before_any_version_read() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("no-db");
    let mut restricted = Access::local();
    restricted.securities = Some(BTreeSet::new());
    let api = DataApi::new(SqliteTasks::new(&p), "run".into(), restricted).unwrap();
    let error = api
        .call(
            "get_yearly_base_years",
            json!({"contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":"v1"}]}),
        )
        .unwrap_err();
    assert_eq!(error.code, "forbidden");
    assert!(!p.exists(), "forbidden must precede storage access");
    // A mixed batch is an all-or-nothing request-level forbidden, never a
    // per-item silent drop.
    let mut mixed = Access::local();
    mixed.securities = Some(BTreeSet::from([Security {
        exchange: Exchange::Sh,
        code: "600006".into(),
    }]));
    let api = DataApi::new(SqliteTasks::new(d.path().join("db2")), "run".into(), mixed).unwrap();
    let error = api
        .call(
            "get_yearly_base_years",
            json!({"contract_version":"v1","items":[
                {"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":"v1"},
                {"entity":{"exchange":"sz","code":"000001"},"base_daily_version_id":"v2"}]}),
        )
        .unwrap_err();
    assert_eq!(error.code, "forbidden");
}

#[test]
fn batch_is_sorted_and_discovery_exposes_the_tool_at_v4() {
    let (_d, store) = store();
    market::published_fixture(
        &store,
        &daily_with_dates("v-sh", "sh.600006", &["2025-01-02"]),
    );
    market::published_fixture(
        &store,
        &daily_with_dates("v-sz", "sz.000001", &["2024-01-02"]),
    );
    let api = DataApi::new(store.tasks.clone(), "run".into(), Access::local()).unwrap();
    let reply = call(
        &api,
        json!([
            {"entity":{"exchange":"sz","code":"000001"},"base_daily_version_id":"v-sz"},
            {"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":"v-sh"},
        ]),
    )
    .unwrap();
    let items = reply["items"].as_array().unwrap();
    assert_eq!(items[0]["entity"]["exchange"], "sh");
    assert_eq!(items[1]["entity"]["exchange"], "sz");
    // Discovery: tools/list carries the tool and its schema validates the
    // registered example; capability revision is generic-data-v4.
    let tools = api.tools();
    let tool = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "get_yearly_base_years")
        .expect("tool must be discovered");
    assert_eq!(tools.as_array().unwrap().len(), 21);
    jsonschema::validator_for(&tool["inputSchema"])
        .unwrap()
        .validate(&json!({"contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":"published-daily-id","start_year":2020,"end_year":2026}]}))
        .unwrap();
    let caps = api.call("get_data_capabilities", json!({})).unwrap();
    assert_eq!(caps["capability_revision"], "generic-data-v4");
    assert!(
        caps["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["tool_name"] == "get_yearly_base_years" && i["implemented"] == true)
    );
}
