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
        ScopeExecutionResult, TaskAcceptanceService, TaskError, TaskExecutionRecord, TaskStatus,
        TaskStore,
    },
};

#[derive(Clone)]
struct FakeSource {
    calls: Arc<Mutex<Vec<(String, Period)>>>,
}

impl MarketDataSource for FakeSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        self.calls.lock().unwrap().push((symbol.to_owned(), period));
        Ok(MarketDataBatch {
            symbol: symbol.to_owned(),
            period,
            adjustment,
            bars: vec![Bar {
                date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
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
        })
    }
}

fn accept(
    store: &TaskStore,
    request_id: &str,
    symbols: &[&str],
    periods: &[Period],
) -> Result<String, TaskError> {
    Ok(TaskAcceptanceService::new(store.clone())
        .accept(request_id, symbols, periods, Adjustment::Forward)?
        .task_id)
}

#[test]
fn one_conflicting_scope_rejects_the_whole_batch_without_writes() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let first = accept(&store, "first", &["000001"], &[Period::Day]).unwrap();
    let before = store.list().unwrap();
    let error = accept(&store, "second", &["000001", "600000"], &[Period::Day]).unwrap_err();
    assert!(
        matches!(error, TaskError::RangeConflict { scopes } if scopes.len() == 1 && scopes[0].symbol == "000001")
    );
    assert_eq!(store.list().unwrap(), before);
    assert!(store.get("second").unwrap().is_none());
    assert_eq!(store.list_occupancy().unwrap().len(), 1);
    assert_eq!(store.list_occupancy().unwrap()[0].task_id, first);
    assert!(accept(&store, "third", &["600000"], &[Period::Day]).is_ok());
}

#[test]
fn same_request_is_idempotent_before_its_own_occupancy_is_checked() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let first = accept(&store, "same", &["000001"], &[Period::Day]).unwrap();
    let second = TaskAcceptanceService::new(store.clone())
        .accept("same", &["000001"], &[Period::Day], Adjustment::Forward)
        .unwrap();
    assert_eq!(second.task_id, first);
    assert!(second.reused);
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn occupancy_survives_store_rebuild_and_needs_verification_keeps_it() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(&store, "running", &["000001"], &[Period::Year]).unwrap();
    store
        .save_execution(TaskExecutionRecord {
            task_id,
            status: TaskStatus::NeedsVerification,
            scope_results: vec![ScopeExecutionResult {
                symbol: "000001".into(),
                period: Period::Year,
                success: true,
                error: None,
            }],
        })
        .unwrap();
    let rebuilt = TaskStore::new(dir.path());
    let error = accept(&rebuilt, "other", &["000001"], &[Period::Year]).unwrap_err();
    assert!(matches!(error, TaskError::RangeConflict { .. }));
}

#[test]
fn terminal_execution_releases_all_scopes_together() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(
        &store,
        "complete",
        &["000001", "600000"],
        &[Period::Day, Period::Year],
    )
    .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let record = TaskExecutionService::new(FakeSource { calls }, store.clone())
        .execute(&task_id)
        .unwrap();
    assert!(record.status.is_terminal());
    assert!(store.list_occupancy().unwrap().is_empty());
    assert!(
        accept(
            &store,
            "next",
            &["000001", "600000"],
            &[Period::Day, Period::Year]
        )
        .is_ok()
    );
}

#[test]
fn running_status_remains_occupied_until_terminal_and_does_not_execute_on_read() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(&store, "running", &["000001"], &[Period::Day]).unwrap();
    store
        .save_execution(TaskExecutionRecord {
            task_id: task_id.clone(),
            status: TaskStatus::Running,
            scope_results: Vec::new(),
        })
        .unwrap();
    let error = accept(&store, "other", &["000001"], &[Period::Day]).unwrap_err();
    assert!(matches!(error, TaskError::RangeConflict { .. }));
}
