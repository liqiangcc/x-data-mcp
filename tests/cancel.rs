use std::sync::{Arc, Mutex};

use chrono::DateTime;
use rust_decimal::Decimal;
use tempfile::tempdir;
use x_data_mcp::{
    execution::TaskExecutionService,
    market_data::{
        Adjustment, Bar, MarketDataBatch, MarketDataSource, Period, Provenance, SourceError,
    },
    task::{
        TaskAcceptanceService, TaskCancellationService, TaskExecutionRecord, TaskStatus, TaskStore,
    },
    worker::Worker,
};

#[derive(Clone)]
struct FakeSource {
    calls: Arc<Mutex<Vec<(String, Period)>>>,
    cancel_on_first: Option<(TaskCancellationService, String)>,
}

impl MarketDataSource for FakeSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        let mut calls = self.calls.lock().unwrap();
        calls.push((symbol.to_owned(), period));
        let first = calls.len() == 1;
        drop(calls);
        if first && let Some((cancellation, task_id)) = &self.cancel_on_first {
            cancellation.cancel_task(task_id).unwrap();
        }
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
            }],
            provenance: Provenance {
                source: "fake".into(),
                source_period: match period {
                    Period::Day => "day",
                    Period::Year => "year",
                }
                .into(),
                source_adjustment: "qfq".into(),
                symbol: symbol.to_owned(),
                acquired_at: DateTime::from_timestamp(0, 0).unwrap(),
                adapter_version: "test".into(),
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

fn accept(store: &TaskStore, request_id: &str, symbols: &[&str]) -> String {
    TaskAcceptanceService::new(store.clone())
        .accept(request_id, symbols, &[Period::Day], Adjustment::Forward)
        .unwrap()
        .task_id
}

fn source(calls: Arc<Mutex<Vec<(String, Period)>>>) -> FakeSource {
    FakeSource {
        calls,
        cancel_on_first: None,
    }
}

#[test]
fn pending_cancel_never_calls_source_and_worker_skips_it() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(&store, "pending", &["000001"]);
    let cancellation = TaskCancellationService::new(store.clone());

    let result = cancellation.cancel_task(&task_id).unwrap();
    assert_eq!(result.status, TaskStatus::Cancelled);
    assert!(result.changed);
    assert!(store.list_occupancy().unwrap().is_empty());

    let calls = Arc::new(Mutex::new(Vec::new()));
    assert!(
        Worker::new(source(calls.clone()), store.clone())
            .run_once()
            .unwrap()
            .is_none()
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn running_cancel_finishes_current_scope_then_stops_and_releases_once() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(&store, "running", &["000001", "600000"]);
    let cancellation = TaskCancellationService::new(store.clone());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let source = FakeSource {
        calls: calls.clone(),
        cancel_on_first: Some((cancellation.clone(), task_id.clone())),
    };

    let record = Worker::new(source, store.clone())
        .run_once()
        .unwrap()
        .unwrap();
    assert_eq!(record.task_id, task_id);
    assert_eq!(record.status, TaskStatus::Cancelled);
    assert_eq!(calls.lock().unwrap().len(), 1);
    let execution = store.get_execution(&task_id).unwrap().unwrap();
    assert_eq!(execution.scope_results.len(), 2);
    assert!(execution.scope_results[0].success);
    assert_eq!(
        execution.scope_results[1].error.as_deref(),
        Some("cancelled before execution")
    );
    assert!(store.list_occupancy().unwrap().is_empty());

    let before = store.get_execution(&task_id).unwrap().unwrap();
    let repeated = cancellation.cancel_task(&task_id).unwrap();
    assert_eq!(repeated.status, TaskStatus::Cancelled);
    assert!(!repeated.changed);
    assert_eq!(store.get_execution(&task_id).unwrap().unwrap(), before);
}

#[test]
fn controlled_resume_preserves_cancel_requested_after_current_scope() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(&store, "resume", &["000001", "600000"]);
    store
        .save_execution(TaskExecutionRecord {
            task_id: task_id.clone(),
            status: TaskStatus::Running,
            scope_results: Vec::new(),
        })
        .unwrap();
    let cancellation = TaskCancellationService::new(store.clone());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let record = TaskExecutionService::new(
        FakeSource {
            calls: calls.clone(),
            cancel_on_first: Some((cancellation, task_id.clone())),
        },
        store.clone(),
    )
    .resume(&task_id)
    .unwrap();
    assert_eq!(record.status, TaskStatus::Cancelled);
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(record.scope_results.len(), 2);
    assert!(store.list_occupancy().unwrap().is_empty());
}

#[test]
fn persisted_cancel_requested_is_finalized_without_source() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(&store, "requested", &["000001", "600000"]);
    store
        .save_execution(TaskExecutionRecord {
            task_id: task_id.clone(),
            status: TaskStatus::CancelRequested,
            scope_results: Vec::new(),
        })
        .unwrap();

    let calls = Arc::new(Mutex::new(Vec::new()));
    let step = Worker::new(source(calls.clone()), store.clone())
        .run_once()
        .unwrap()
        .unwrap();
    assert_eq!(step.status, TaskStatus::Cancelled);
    assert!(calls.lock().unwrap().is_empty());
    assert!(store.list_occupancy().unwrap().is_empty());
}

#[test]
fn needs_verification_cancel_does_not_release_or_change_status() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(&store, "verify", &["000001"]);
    store
        .save_execution(TaskExecutionRecord {
            task_id: task_id.clone(),
            status: TaskStatus::NeedsVerification,
            scope_results: Vec::new(),
        })
        .unwrap();

    let result = TaskCancellationService::new(store.clone())
        .cancel_task(&task_id)
        .unwrap();
    assert_eq!(result.status, TaskStatus::NeedsVerification);
    assert!(!result.changed);
    assert_eq!(store.list_occupancy().unwrap().len(), 1);
}

#[test]
fn terminal_cancel_returns_actual_terminal_record_without_rewriting() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(&store, "terminal", &["000001"]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let completed = TaskExecutionService::new(source(calls), store.clone())
        .execute(&task_id)
        .unwrap();
    assert_eq!(completed.status, TaskStatus::Succeeded);

    let result = TaskCancellationService::new(store.clone())
        .cancel_task(&task_id)
        .unwrap();
    assert_eq!(result.status, TaskStatus::Succeeded);
    assert!(!result.changed);
    assert_eq!(store.get_execution(&task_id).unwrap().unwrap(), completed);
}
