//! v8 PR-5 (#387 §14, AC-V8-9): task surface v2.
//!
//! Two layers: the golden fixture proves the one-call diagnosis shape and its
//! token budget (`scripts/test_v8_token_budget.py`), the SQLite tests prove
//! the same shape survives a real task store and that a returned `item_id`
//! still round-trips into `retry_task`'s string spelling.
use serde::Deserialize;
use serde_json::{Value, json};
use x_data_mcp::{
    data_supply::{Access, mcp::DataApi, task_v2, wire::*},
    mcp::ToolHandler,
    sqlite_tasks::SqliteTasks,
};

#[derive(Deserialize)]
struct Fixture {
    input: Input,
    expected: Value,
}

#[derive(Deserialize)]
struct Input {
    task: TaskSummary,
    counts: TaskCounts,
    failed_by_reason: std::collections::BTreeMap<String, usize>,
    sample: Vec<ProgressItem>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("../../fixtures/v8/task-diagnosis.json")).unwrap()
}

/// AC-V8-9 (T6): one `get_task` call answers "what failed, how much, and
/// why" — the class counts, the first errors and the structured item ids.
#[test]
fn one_call_diagnosis_matches_the_golden_fixture() {
    let fixture = fixture();
    let view = task_v2::task_view(
        fixture.input.task,
        fixture.input.counts,
        fixture.input.failed_by_reason,
        &fixture.input.sample,
    );
    assert_eq!(view, fixture.expected);
    assert_eq!(view["wire_contract_version"], "v2");
    assert!(
        view.get("contract_version").is_none(),
        "the task's own version must not shadow the wire version: {view}"
    );
    assert_eq!(view["failed_by_reason"]["REVISION_UNVERIFIED"], 20);
    assert_eq!(view["errors"].as_array().unwrap().len(), 20);
    // The sample is bounded: the classification is complete, the error list
    // is not, and the difference is visible.
    let counted: usize = view["failed_by_reason"]
        .as_object()
        .unwrap()
        .values()
        .map(|count| count.as_u64().unwrap() as usize)
        .sum();
    assert_eq!(counted, 24);
    assert!(counted > view["errors"].as_array().unwrap().len());
    // A server-internal derived item carries no entity and no period.
    assert_eq!(
        view["errors"][0]["item_id"],
        json!({"dataset": "derived:calendar"})
    );
}

#[test]
fn item_ids_round_trip_into_retry_task_and_v1_is_unchanged() {
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
    let task = start["task_id"].as_str().unwrap().to_owned();
    api.call("run_worker_once", json!({})).unwrap();

    let v1 = api
        .call(
            "get_task",
            json!({"contract_version": "v1", "task_id": task}),
        )
        .unwrap();
    assert_eq!(v1["task_id"], task);
    assert!(v1.get("wire_contract_version").is_none(), "{v1}");
    assert!(v1.get("failed_by_reason").is_none(), "{v1}");
    assert!(v1.get("contract_version").is_some(), "{v1}");

    let v2 = api
        .call(
            "get_task",
            json!({"contract_version": "v2", "task_id": task}),
        )
        .unwrap();
    assert_eq!(v2["wire_contract_version"], "v2");
    assert_eq!(v2["task_contract_version"], v1["contract_version"]);
    assert_eq!(v2["failed"], 2);
    let errors = v2["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2, "{v2}");
    assert!(!v2["failed_by_reason"].as_object().unwrap().is_empty());
    for error in errors {
        assert!(
            task_v2::REASON_CODES.contains(&error["reason_code"].as_str().unwrap()),
            "{error}"
        );
        assert!(error["item_id"]["entity"].is_string(), "{error}");
        assert!(error["item_id"]["period"].is_string(), "{error}");
    }

    // The structured id still round-trips into the string spelling
    // `retry_task` accepts (#387 §7.2 row 9).
    let returned: ItemId = serde_json::from_value(errors[0]["item_id"].clone()).unwrap();
    assert_eq!(
        returned.text(),
        format!(
            "[\"market\",\"{}\",\"{}\"]",
            returned.entity.as_deref().unwrap(),
            returned.period.as_deref().unwrap()
        )
    );
    let retried = api
        .call(
            "retry_task",
            json!({
                "contract_version": "v1",
                "original_task_id": task,
                "new_request_id": "retry-1",
                "item_ids": [returned.text()],
            }),
        )
        .unwrap();
    assert_eq!(retried["item_ids"], json!([returned.text()]));
}

#[test]
fn progress_v2_structures_items_and_lists_by_reason_code() {
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
    let task = start["task_id"].as_str().unwrap().to_owned();
    api.call("run_worker_once", json!({})).unwrap();
    // A second task that never failed: the `reason_code` filter must exclude
    // it (started after the worker, so it stays pending).
    api.call(
        "start_collection",
        json!({"request_id":"r2","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600007"}]}}),
    )
    .unwrap();

    let v2 = api
        .call(
            "get_task",
            json!({"contract_version": "v2", "task_id": task}),
        )
        .unwrap();
    let code = v2["failed_by_reason"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();

    let page = api
        .call(
            "get_task_progress",
            json!({"contract_version": "v2", "task_id": task}),
        )
        .unwrap();
    assert_eq!(page["wire_contract_version"], "v2");
    assert_eq!(page["failed_by_reason"][code.as_str()], 2);
    assert_eq!(page["unfinished"].as_array().unwrap().len(), 2);
    assert!(
        page["unfinished"][0]["item_id"]["entity"].is_string(),
        "{page}"
    );
    assert_eq!(page["unfinished"][0]["reason_code"], code.as_str());
    assert!(
        page["unfinished"][0]["error"]
            .as_str()
            .is_none_or(|error| error.chars().count() <= 300),
        "{page}"
    );

    let all = api
        .call("list_tasks", json!({"contract_version": "v2"}))
        .unwrap();
    assert_eq!(
        all["items"].as_array().unwrap().len(),
        2,
        "both tasks must be listed: {all}"
    );
    let listed = api
        .call(
            "list_tasks",
            json!({"contract_version": "v2", "reason_code": code.as_str()}),
        )
        .unwrap();
    assert_eq!(listed["wire_contract_version"], "v2");
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "filtered: {listed} all: {all}");
    assert_eq!(items[0]["task_id"], task);
    assert_eq!(
        items[0]["task_contract_version"],
        v2["task_contract_version"]
    );

    // `status` keeps working next to the new filter (#387 §14). `pending` is
    // the one status the stored column and the visible status spell alike.
    let by_status = api
        .call(
            "list_tasks",
            json!({"contract_version": "v2", "status": ["pending"]}),
        )
        .unwrap();
    let status_items = by_status["items"].as_array().unwrap();
    assert_eq!(status_items.len(), 1, "{by_status}");
    assert_eq!(status_items[0]["status"], "pending");
    assert_ne!(status_items[0]["task_id"], task);

    // v1 never reads item failures: the filter is rejected, not ignored.
    let error = api
        .call(
            "list_tasks",
            json!({"contract_version": "v1", "reason_code": code.as_str()}),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    let error = api
        .call(
            "list_tasks",
            json!({"contract_version": "v2", "reason_code": "NO_SUCH_CLASS"}),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
}
