use chrono::DateTime;
use rust_decimal::Decimal;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use x_data_mcp::{
    market_data::{
        Adjustment, Bar, MarketDataBatch, MarketDataPipeline, Period, Provenance, SourceError,
    },
    mcp::{
        CancelTaskArgs, GetTaskArgs, McpApi, RetryScopeArgs, RetryTaskArgs, StartCollectionArgs,
    },
    task::{ScopeExecutionResult, TaskExecutionRecord, TaskStatus, TaskStore},
};

#[derive(Clone)]
struct FakeSource {
    calls: Arc<Mutex<Vec<(String, Period)>>>,
}

impl FakeSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        self.calls.lock().unwrap().push((symbol.to_owned(), period));
        let source_period = match period {
            Period::Day => "day",
            Period::Year => "day",
        };
        Ok(MarketDataBatch {
            symbol: symbol.to_owned(),
            period,
            adjustment,
            bars: vec![Bar {
                date: chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
                open: Decimal::ONE,
                high: Decimal::ONE,
                low: Decimal::ONE,
                close: Decimal::ONE,
                volume: Some(Decimal::ONE),
                amount: Some(Decimal::ONE),
                r7_raw: None,
            }],
            provenance: Provenance {
                source: "fake".into(),
                source_period: source_period.into(),
                source_adjustment: "qfq".into(),
                symbol: symbol.to_owned(),
                acquired_at: DateTime::from_timestamp(0, 0).unwrap(),
                adapter_version: "test".into(),
                yearly_derivation: None,
                snapshot_inputs: None,
            },
            coverage: x_data_mcp::market_data::CoverageEvidence::complete("test fixture"),
            unit_currency: x_data_mcp::market_data::UnitCurrencyEvidence::confirmed(
                "CNY",
                "share",
                "lot",
                "CNY",
                "yuan",
                "test fixture",
            ),
            adjustment_evidence: x_data_mcp::market_data::AdjustmentEvidence::confirmed(
                "qfq",
                "test fixture",
            ),
        })
    }
}

fn api() -> (McpApi<FakeSource>, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let api = McpApi::new(
        FakeSource {
            calls: Arc::new(Mutex::new(Vec::new())),
        },
        TaskStore::new(dir.path()),
    );
    (api, dir)
}

fn start_args(request_id: &str, symbols: &[&str], periods: &[&str]) -> StartCollectionArgs {
    StartCollectionArgs {
        request_id: request_id.into(),
        symbols: symbols.iter().map(|symbol| (*symbol).into()).collect(),
        periods: periods.iter().map(|period| (*period).into()).collect(),
    }
}

#[test]
fn start_collection_is_idempotent_for_same_request() {
    let (api, _) = api();
    let first = api
        .start_collection(start_args("r1", &["000001"], &["day"]))
        .unwrap();
    let second = api
        .start_collection(start_args("r1", &["000001"], &["day"]))
        .unwrap();

    assert_eq!(first.task_id, second.task_id);
    assert!(!first.reused);
    assert!(second.reused);
}

#[test]
fn start_collection_reports_request_conflict() {
    let (api, _) = api();
    api.start_collection(start_args("r1", &["000001"], &["day"]))
        .unwrap();

    let error = api
        .start_collection(start_args("r1", &["600000"], &["day"]))
        .unwrap_err();
    assert_eq!(error.code, "request_conflict");
    assert!(!error.message.is_empty());
}

#[test]
fn start_collection_reports_range_conflict_with_scopes() {
    let (api, _) = api();
    api.start_collection(start_args("r1", &["000001"], &["day"]))
        .unwrap();

    let error = api
        .start_collection(start_args("r2", &["000001", "600000"], &["day"]))
        .unwrap_err();
    assert_eq!(error.code, "range_conflict");
    assert_eq!(
        error.message,
        "requested range is busy; no task was accepted"
    );
    assert_eq!(
        error.scopes.unwrap(),
        vec![x_data_mcp::mcp::ScopeView {
            symbol: "000001".into(),
            period: "day".into(),
        }]
    );
}

#[test]
fn cancel_task_returns_cancelled_and_is_idempotent() {
    let (api, _) = api();
    let started = api
        .start_collection(start_args("cancel", &["000001"], &["day"]))
        .unwrap();

    let first = api
        .cancel_task(CancelTaskArgs {
            task_id: started.task_id.clone(),
        })
        .unwrap();
    assert_eq!(first.status, "cancelled");
    assert!(first.changed);

    let second = api
        .cancel_task(CancelTaskArgs {
            task_id: started.task_id.clone(),
        })
        .unwrap();
    assert_eq!(second.status, "cancelled");
    assert!(!second.changed);
    assert!(api.run_worker_once().unwrap().idle);
    assert_eq!(
        api.get_task(GetTaskArgs {
            task_id: started.task_id,
        })
        .unwrap()
        .status,
        "cancelled"
    );
}

#[test]
fn retry_task_links_new_task_and_get_task_exposes_association() {
    let (api, dir) = api();
    let started = api
        .start_collection(start_args("original", &["000001", "600000"], &["day"]))
        .unwrap();
    let store = x_data_mcp::task::TaskStore::new(dir.path());
    store
        .save_execution(TaskExecutionRecord {
            task_id: started.task_id.clone(),
            status: TaskStatus::Failed,
            scope_results: vec![
                ScopeExecutionResult {
                    symbol: "000001".into(),
                    period: Period::Day,
                    success: true,
                    error: None,
                },
                ScopeExecutionResult {
                    symbol: "600000".into(),
                    period: Period::Day,
                    success: false,
                    error: Some("planned".into()),
                },
            ],
        })
        .unwrap();

    let retry = api
        .retry_task(RetryTaskArgs {
            original_task_id: started.task_id.clone(),
            new_request_id: "retry-request".into(),
            selected_scopes: vec![RetryScopeArgs {
                symbol: "600000".into(),
                period: "day".into(),
            }],
        })
        .unwrap();
    assert_eq!(retry.status, "pending");
    assert_eq!(retry.retry_of.as_deref(), Some(started.task_id.as_str()));

    let view = api
        .get_task(GetTaskArgs {
            task_id: retry.task_id.clone(),
        })
        .unwrap();
    assert_eq!(view.retry_of.as_deref(), Some(started.task_id.as_str()));
    assert_eq!(view.frozen_range.len(), 1);
    assert_eq!(
        api.run_worker_once().unwrap().status.as_deref(),
        Some("succeeded")
    );
    assert_eq!(
        api.get_task(GetTaskArgs {
            task_id: retry.task_id,
        })
        .unwrap()
        .status,
        "succeeded"
    );
}

#[test]
fn get_task_exposes_pending_and_terminal_results() {
    let (api, _) = api();
    let started = api
        .start_collection(start_args("r1", &["000001"], &["day"]))
        .unwrap();

    let pending = api
        .get_task(GetTaskArgs {
            task_id: started.task_id.clone(),
        })
        .unwrap();
    assert_eq!(pending.status, "pending");
    assert_eq!(pending.frozen_range.len(), 1);
    assert!(pending.scope_results.is_empty());

    let run = api.run_worker_once().unwrap();
    assert_eq!(run.status.as_deref(), Some("succeeded"));
    let terminal = api
        .get_task(GetTaskArgs {
            task_id: started.task_id,
        })
        .unwrap();
    assert_eq!(terminal.status, "succeeded");
    assert_eq!(terminal.scope_results.len(), 1);
    assert!(terminal.scope_results[0].success);
}

#[test]
fn run_worker_once_processes_at_most_one_task() {
    let (api, dir) = api();
    let first = api
        .start_collection(start_args("r1", &["000001"], &["day"]))
        .unwrap();
    let second = api
        .start_collection(start_args("r2", &["600000"], &["day"]))
        .unwrap();

    let run = api.run_worker_once().unwrap();
    assert_eq!(run.task_id.as_deref(), Some(first.task_id.as_str()));
    assert_eq!(run.status.as_deref(), Some("succeeded"));
    let second_view = api
        .get_task(GetTaskArgs {
            task_id: second.task_id,
        })
        .unwrap();
    assert_eq!(second_view.status, "pending");
    assert!(dir.path().join("task-state.json").is_file());
}

#[test]
fn run_worker_once_returns_explicit_idle_when_queue_is_empty() {
    let (api, _) = api();
    let started = api
        .start_collection(start_args("r1", &["000001"], &["day"]))
        .unwrap();
    api.run_worker_once().unwrap();

    let idle = api.run_worker_once().unwrap();
    assert!(idle.idle);
    assert!(idle.task_id.is_none());
    assert!(idle.status.is_none());
    assert_eq!(
        api.get_task(GetTaskArgs {
            task_id: started.task_id,
        })
        .unwrap()
        .status,
        "succeeded"
    );
}

#[test]
fn get_task_reports_needs_verification_and_worker_does_not_resume_it() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let api = McpApi::new(
        FakeSource {
            calls: calls.clone(),
        },
        store.clone(),
    );
    let started = api
        .start_collection(start_args("r1", &["000001", "600000"], &["day"]))
        .unwrap();
    store
        .save_execution(TaskExecutionRecord {
            task_id: started.task_id.clone(),
            status: TaskStatus::Running,
            scope_results: vec![ScopeExecutionResult {
                symbol: "000001".into(),
                period: Period::Day,
                success: true,
                error: None,
            }],
        })
        .unwrap();

    let view = api
        .get_task(GetTaskArgs {
            task_id: started.task_id.clone(),
        })
        .unwrap();
    assert_eq!(view.status, "needs_verification");
    assert_eq!(view.scope_results.len(), 1);
    assert_eq!(view.scope_results[0].symbol, "000001");

    let idle = api.run_worker_once().unwrap();
    assert!(idle.idle);
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(
        store
            .get_execution(&started.task_id)
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::NeedsVerification
    );
}

impl MarketDataPipeline for FakeSource {
    fn materialize_daily(
        &self,
        symbol: &str,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        self.fetch(symbol, Period::Day, adjustment)
    }

    fn derive_yearly(
        &self,
        daily: &x_data_mcp::publish::EffectiveVersion,
    ) -> Result<MarketDataBatch, SourceError> {
        let mut batch = self.fetch(&daily.symbol, Period::Year, daily.adjustment)?;
        batch.provenance.source_period = "day".into();
        batch.provenance.yearly_derivation = Some(x_data_mcp::market_data::YearlyDerivation {
            base_daily_version_id: daily.version_id.clone(),
            algorithm_version: "fake-yearly-fixture".into(),
            first_trading_date: daily.records.first().unwrap().date,
            last_trading_date: daily.records.last().unwrap().date,
        });
        Ok(batch)
    }
}

#[test]
fn request_lookup_is_bound_only_after_complete_acceptance_and_reports_storage_errors() {
    use x_data_mcp::mcp::GetRequestArgs;
    let (api, dir) = api();
    assert_eq!(
        api.get_request(GetRequestArgs {
            request_id: "r".into()
        })
        .unwrap()["bound"],
        false
    );
    let accepted = api
        .start_collection(start_args("r", &["sh.600006"], &["day"]))
        .unwrap();
    let bytes = std::fs::read(dir.path().join("task-state.json")).unwrap();
    let result = api
        .get_request(GetRequestArgs {
            request_id: "r".into(),
        })
        .unwrap();
    assert_eq!(result["task"]["task_id"], accepted.task_id);
    assert_eq!(result["task"]["status"], "pending");
    assert_eq!(
        std::fs::read(dir.path().join("task-state.json")).unwrap(),
        bytes
    );
    let mut state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    state["executions"] = serde_json::json!([]);
    std::fs::write(
        dir.path().join("task-state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    assert_eq!(
        api.get_request(GetRequestArgs {
            request_id: "r".into()
        })
        .unwrap_err()
        .code,
        "request_needs_verification"
    );
    std::fs::write(dir.path().join("task-state.json"), b"invalid-json").unwrap();
    assert_eq!(
        api.get_request(GetRequestArgs {
            request_id: "r".into()
        })
        .unwrap_err()
        .code,
        "storage_error"
    );
}

#[test]
fn every_tool_rejects_unknown_fields_before_any_action() {
    use serde_json::json;
    use x_data_mcp::mcp::handle_json;
    let (api, dir) = api();
    let cases = [
        (
            "start_collection",
            json!({"request_id":"r","symbols":["sh.600006"],"periods":["day"]}),
        ),
        ("get_task", json!({"task_id":"t"})),
        ("cancel_task", json!({"task_id":"t"})),
        (
            "retry_task",
            json!({"original_task_id":"t","new_request_id":"r","selected_scopes":[{"symbol":"sh.600006","period":"day"}]}),
        ),
        ("run_worker_once", json!({})),
        ("get_request", json!({"request_id":"r"})),
        (
            "get_market_data",
            json!({"exchange":"sh","code":"600006","period":"day","start_date":"2025-01-01","end_date":"2025-12-31"}),
        ),
        (
            "list_market_data_versions",
            json!({"exchange":"sh","code":"600006","period":"day"}),
        ),
    ];
    for (name, mut args) in cases {
        args["unknown"] = json!(true);
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}});
        let response: serde_json::Value =
            serde_json::from_str(&handle_json(&api, &request.to_string()).unwrap()).unwrap();
        assert_eq!(
            response["result"]["structuredContent"]["code"], "invalid_arguments",
            "{name}"
        );
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
