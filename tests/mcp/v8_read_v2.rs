//! v8 PR-3 (#387 §5, AC-V8-4/AC-V8-12): columnar read responses v2.
//!
//! The golden fixtures are synthetic snapshots with a committed expected
//! response; each one is projected through the same pure functions the read
//! handlers use, so a shape change fails byte-for-byte. `scripts/test_v8_token_budget.py`
//! asserts the same fixture files stay inside their token budgets.
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use x_data_mcp::{
    data_supply::{
        Access,
        columnar::{self, PageMeta},
        mcp::DataApi,
        query::QueryPlan,
        wire::*,
    },
    mcp::{ApiError, TextMode, ToolHandler, handle_json, resolve_text_mode},
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
};

#[derive(Deserialize)]
struct Fixture {
    input: Input,
    expected: Value,
}

#[derive(Deserialize)]
#[serde(tag = "layout", rename_all = "snake_case")]
enum Input {
    Panel {
        dataset: Dataset,
        manifest: Vec<ManifestEntry>,
        entities: Vec<Security>,
        rows: Vec<Row>,
        fields: Option<Vec<String>>,
        meta: MetaInput,
    },
    Catalog {
        manifest: Vec<ManifestEntry>,
        rows: Vec<Row>,
        fields: Option<Vec<String>>,
        meta: MetaInput,
    },
    CrossSection {
        dataset: Dataset,
        manifest: Vec<ManifestEntry>,
        trade_days: Vec<NaiveDate>,
        entities: Vec<Security>,
        rows: Vec<Row>,
        fields: Option<Vec<String>>,
        meta: MetaInput,
    },
    Versions {
        dataset: Dataset,
        items: Vec<VersionInfo>,
        meta: MetaInput,
    },
    Availability {
        dataset: Dataset,
        items: Vec<AvailabilityItem>,
        fields: Option<Vec<String>>,
        meta: MetaInput,
    },
}

#[derive(Deserialize)]
struct MetaInput {
    snapshot_id: String,
    snapshot_time: DateTime<Utc>,
    next_cursor: Option<String>,
    page_complete: bool,
    #[serde(default)]
    limitations: Vec<String>,
}

impl MetaInput {
    fn page(&self) -> PageMeta<'_> {
        PageMeta {
            snapshot_id: &self.snapshot_id,
            snapshot_time: self.snapshot_time,
            next_cursor: self.next_cursor.as_deref(),
            page_complete: self.page_complete,
            limitations: &self.limitations,
        }
    }
}

fn project(input: Input) -> Value {
    match input {
        Input::Panel {
            dataset,
            manifest,
            entities,
            rows,
            fields,
            meta,
        } => {
            let (allowed, default) = columnar::query_fields(dataset);
            let fields =
                columnar::select(fields.as_deref(), allowed, default, dataset.name()).unwrap();
            columnar::panel(dataset, &manifest, &entities, &rows, &fields, &meta.page())
        }
        Input::Catalog {
            manifest,
            rows,
            fields,
            meta,
        } => {
            let fields = columnar::select(
                fields.as_deref(),
                columnar::CATALOG_FIELDS,
                columnar::CATALOG_DEFAULT,
                "securities.catalog",
            )
            .unwrap();
            columnar::catalog_table(&manifest, &rows, &fields, &meta.page())
        }
        Input::CrossSection {
            dataset,
            manifest,
            trade_days,
            entities,
            rows,
            fields,
            meta,
        } => {
            let (allowed, default) = columnar::query_fields(dataset);
            let fields =
                columnar::select(fields.as_deref(), allowed, default, dataset.name()).unwrap();
            columnar::cross_section(
                dataset,
                &manifest,
                &trade_days,
                &entities,
                &rows,
                &fields,
                &meta.page(),
            )
        }
        Input::Versions {
            dataset,
            items,
            meta,
        } => columnar::versions_table(dataset, &items, &meta.page()),
        Input::Availability {
            dataset,
            items,
            fields,
            meta,
        } => {
            let fields = columnar::select(
                fields.as_deref(),
                columnar::AVAILABILITY_FIELDS,
                columnar::AVAILABILITY_FIELDS,
                "get_data_availability",
            )
            .unwrap();
            columnar::availability_table(dataset, &items, &fields, &meta.page())
        }
    }
}

macro_rules! fixture {
    ($name:literal) => {
        serde_json::from_str::<Fixture>(include_str!(concat!("../../fixtures/v8/", $name))).unwrap()
    };
}

fn assert_fixture(name: &str, fixture: Fixture) {
    let actual = project(fixture.input);
    assert_eq!(
        serde_json::to_string(&actual).unwrap(),
        serde_json::to_string(&fixture.expected).unwrap(),
        "{name}"
    );
}

#[test]
fn columnar_golden_fixtures_are_byte_exact() {
    assert_fixture("query-panel-five.json", fixture!("query-panel-five.json"));
    assert_fixture("query-panel-242.json", fixture!("query-panel-242.json"));
    assert_fixture(
        "query-panel-batch-ten.json",
        fixture!("query-panel-batch-ten.json"),
    );
    assert_fixture(
        "query-catalog-industry.json",
        fixture!("query-catalog-industry.json"),
    );
    assert_fixture(
        "query-cross-section-two-days.json",
        fixture!("query-cross-section-two-days.json"),
    );
    assert_fixture("query-versions.json", fixture!("query-versions.json"));
    assert_fixture(
        "availability-single.json",
        fixture!("availability-single.json"),
    );
    assert_fixture(
        "availability-batch-ten.json",
        fixture!("availability-batch-ten.json"),
    );
}

#[test]
fn catalog_default_fields_are_the_design_core_columns() {
    assert_eq!(
        columnar::select(
            None,
            columnar::CATALOG_FIELDS,
            columnar::CATALOG_DEFAULT,
            "securities.catalog",
        )
        .unwrap(),
        vec!["name", "industry_code", "industry_name"]
    );
}

#[test]
fn unknown_or_duplicate_fields_name_the_options() {
    let unknown = vec!["nope".to_owned()];
    let error = columnar::select(
        Some(&unknown),
        columnar::DAILY_FIELDS,
        columnar::DAILY_FIELDS,
        "market.daily",
    )
    .unwrap_err();
    assert!(error.to_string().contains("allowed fields: open"));
    let duplicate = vec!["close".to_owned(), "close".to_owned()];
    let error = columnar::select(
        Some(&duplicate),
        columnar::DAILY_FIELDS,
        columnar::DAILY_FIELDS,
        "market.daily",
    )
    .unwrap_err();
    assert!(error.to_string().contains("duplicate field"));
    let empty: Vec<String> = Vec::new();
    let error = columnar::select(
        Some(&empty),
        columnar::DAILY_FIELDS,
        columnar::DAILY_FIELDS,
        "market.daily",
    )
    .unwrap_err();
    assert!(error.to_string().contains("at least one field"));
}

fn query_args(version: &str) -> Query {
    serde_json::from_value(json!({
        "dataset":"market.daily",
        "contract_version":version,
        "selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},
        "filter":{"kind":"daily","start_date":"2025-01-01","end_date":"2025-12-31"},
        "version":{"kind":"latest_per_entity"},
        "page_size":1
    }))
    .unwrap()
}

#[test]
fn fields_is_a_v2_only_input() {
    let mut args = query_args("v1");
    args.fields = Some(vec!["close".into()]);
    let error = QueryPlan::new(args, &Access::local()).err().unwrap();
    assert!(error.to_string().contains("contract_version v2"));
}

/// A minimal read-surface stub for the text grading tests.
struct Stub {
    mode: TextMode,
    value: Value,
    error: Option<ApiError>,
}

impl ToolHandler for Stub {
    fn tools(&self) -> Value {
        json!([])
    }
    fn call(&self, _name: &str, _args: Value) -> Result<Value, ApiError> {
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(self.value.clone()),
        }
    }
    fn text_mode(&self) -> TextMode {
        self.mode
    }
}

fn text_of(stub: &Stub) -> Value {
    let request = json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"get_task_progress","arguments":{}}
    })
    .to_string();
    let response: Value = serde_json::from_str(&handle_json(stub, &request).unwrap()).unwrap();
    response["result"]["content"].clone()
}

#[test]
fn text_grading_full_summary_and_off() {
    let small = json!({"status":"succeeded","total":2,"finished":2});
    let content = text_of(&Stub {
        mode: TextMode::Full,
        value: small.clone(),
        error: None,
    });
    assert_eq!(
        content[0]["text"],
        serde_json::to_string(&small).unwrap(),
        "full mode sends the same JSON as structuredContent"
    );

    // Small payloads stay whole even in summary mode.
    let content = text_of(&Stub {
        mode: TextMode::Summary,
        value: small.clone(),
        error: None,
    });
    assert_eq!(content[0]["text"], serde_json::to_string(&small).unwrap());

    // Large payloads become a bounded summary with the fields a poller needs.
    let large = json!({
        "status":"running",
        "total":5224,
        "finished":100,
        "succeeded":100,
        "failed":0,
        "pending":5124,
        "running":0,
        "next_cursor":"cursor-1",
        "task_id":"task-1",
        "unfinished": (0..40).map(|i| json!({"item_id":format!("item-{i}-{}","x".repeat(80))})).collect::<Vec<_>>()
    });
    assert!(serde_json::to_string(&large).unwrap().len() > 2048);
    let content = text_of(&Stub {
        mode: TextMode::Summary,
        value: large,
        error: None,
    });
    let text = content[0]["text"].as_str().unwrap();
    assert!(text.len() <= 300, "{text}");
    let summary: Value = serde_json::from_str(text).unwrap();
    assert_eq!(summary["status"], "running");
    assert_eq!(summary["counts"]["total"], 5224);
    assert_eq!(summary["next_cursor"], "cursor-1");
    assert_eq!(summary["refs"]["task_id"], "task-1");
    assert_eq!(summary["detail"], "see structuredContent");

    // Errors keep the code in the summary.
    let content = text_of(&Stub {
        mode: TextMode::Summary,
        value: Value::Null,
        error: Some(ApiError {
            code: "plan_stale".into(),
            message: "resolved plan drifted".into(),
            scopes: None,
            outcome_certainty: None,
            details: None,
        }),
    });
    let summary: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["error"], "plan_stale");

    // `off` sends no text at all.
    let content = text_of(&Stub {
        mode: TextMode::Off,
        value: small,
        error: None,
    });
    assert_eq!(content, json!([]));
}

#[test]
fn text_mode_defaults_by_entry_and_env_override() {
    assert_eq!(resolve_text_mode(None, None), TextMode::Full);
    assert_eq!(resolve_text_mode(Some("research"), None), TextMode::Full);
    assert_eq!(resolve_text_mode(Some("ops"), None), TextMode::Summary);
    assert_eq!(resolve_text_mode(Some("maint"), None), TextMode::Summary);
    assert_eq!(resolve_text_mode(Some("ops"), Some("full")), TextMode::Full);
    assert_eq!(
        resolve_text_mode(Some("research"), Some("off")),
        TextMode::Off
    );
    assert_eq!(
        resolve_text_mode(Some("research"), Some("nonsense")),
        TextMode::Full,
        "an unknown value falls back to the entry default; data_service refuses it at startup"
    );
    assert_eq!(TextMode::parse("summary"), Some(TextMode::Summary));
}

use crate::support_market as market;

fn read_api() -> (DataApi, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(dir.path().join("db"));
    tasks.initialize().unwrap();
    let store = SqliteMarket::new(tasks.clone());
    market::published_fixture(&store, &market::samples()[0]["record"]);
    let api = DataApi::new(tasks, "run".into(), Access::local()).unwrap();
    (api, dir)
}

fn request(version: &str) -> Value {
    json!({
        "dataset":"market.daily",
        "contract_version":version,
        "selection":{"kind":"single","entity":"sh.600006"},
        "filter":{"kind":"daily","start_date":"2025-01-01","end_date":"2025-12-31"},
        "version":{"kind":"latest_per_entity"},
        "page_size":1
    })
}

#[test]
fn v1_and_v2_agree_on_values_versions_and_pagination() {
    let (api, _dir) = read_api();
    let v1 = api.call("query_data", request("v1")).unwrap();
    let v2 = api.call("query_data", request("v2")).unwrap();
    assert_eq!(v2["layout"], "panel");
    assert_eq!(v2["entities"][0], "sh.600006");
    assert_eq!(v2["dates"][0][0], v1["records"][0]["date"]);
    assert_eq!(v2["columns"]["close"][0][0], v1["records"][0]["close"]);
    assert_eq!(v2["columns"]["open"][0][0], v1["records"][0]["open"]);
    assert_eq!(v2["row_count"], 1);
    assert_eq!(
        v2["versions"][0]["version_id"],
        v1["version_manifest"][0]["version_ref"]["version_id"]
    );
    assert!(v2["versions"][0].get("integrity_sha256").is_none());
    assert!(v1["next_cursor"].is_string() && v2["next_cursor"].is_string());

    // Page two through each contract's own cursor: same rows, same boundary.
    let mut next_v1 = request("v1");
    next_v1["cursor"] = v1["next_cursor"].clone();
    let mut next_v2 = request("v2");
    next_v2["cursor"] = v2["next_cursor"].clone();
    let page_v1 = api.call("query_data", next_v1).unwrap();
    let page_v2 = api.call("query_data", next_v2).unwrap();
    assert_eq!(page_v2["dates"][0][0], page_v1["records"][0]["date"]);
    assert_eq!(
        page_v2["columns"]["close"][0][0],
        page_v1["records"][0]["close"]
    );
    assert!(page_v1["next_cursor"].is_null() && page_v2["next_cursor"].is_null());

    // A v1 cursor on a v2 request names the contract version.
    let mut mixed = request("v2");
    mixed["cursor"] = v1["next_cursor"].clone();
    let error = api.call("query_data", mixed).unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(
        error.message.contains("contract_version"),
        "{}",
        error.message
    );
}

#[test]
fn fields_projection_keeps_the_same_values() {
    let (api, _dir) = read_api();
    let mut all = request("v2");
    all["page_size"] = json!(10);
    let full = api.call("query_data", all).unwrap();
    let mut projected = request("v2");
    projected["page_size"] = json!(10);
    projected["fields"] = json!(["close"]);
    let only = api.call("query_data", projected).unwrap();
    let keys: Vec<&str> = only["columns"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, vec!["close"]);
    assert_eq!(only["columns"]["close"], full["columns"]["close"]);
}

#[test]
fn v1_rejects_fields_and_versions_and_availability_serve_v2() {
    let (api, _dir) = read_api();
    let mut bad = request("v1");
    bad["fields"] = json!(["close"]);
    let error = api.call("query_data", bad).unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(error.message.contains("contract_version v2"));

    let versions = api
        .call(
            "list_data_versions",
            json!({
                "dataset":"market.daily",
                "contract_version":"v2",
                "selection":{"kind":"single","entity":"sh.600006"}
            }),
        )
        .unwrap();
    assert_eq!(versions["layout"], "table");
    assert_eq!(versions["row_count"], 1);
    assert!(versions["versions"][0].get("integrity_sha256").is_none());

    let availability = api
        .call(
            "get_data_availability",
            json!({
                "dataset":"market.daily",
                "contract_version":"v2",
                "selection":{"kind":"single","entity":"sh.600006"},
                "filter":{"kind":"daily","start_date":"2025-01-01","end_date":"2025-12-31"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(availability["layout"], "table");
    assert_eq!(availability["row_count"], 1);
    assert_eq!(availability["entities"][0], "sh.600006");
    assert!(availability["columns"]["availability"].is_array());

    // Tools outside the v2 read surfaces keep the v1-only gate. `get_task`
    // and the rest of the task surface moved to v2 in v8 PR-5 (#387 §14).
    let error = api
        .call(
            "get_request",
            json!({"contract_version":"v2","request_id":"nope"}),
        )
        .unwrap_err();
    assert_eq!(error.code, "unsupported_contract");
}

#[test]
fn the_queries_type_keeps_v1_unchanged() {
    // A compile-time/behavioral guard: v1 still returns `DataPage`, so no
    // caller of the old surface had to change.
    let (api, _dir) = read_api();
    let page: Value = api.call("query_data", request("v1")).unwrap();
    assert!(page.get("records").is_some());
    assert!(page.get("version_manifest").is_some());
    assert!(page.get("columns").is_none());
}
