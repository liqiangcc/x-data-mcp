use std::sync::{Arc, Mutex};

use chrono::DateTime;
use rust_decimal::Decimal;
use tempfile::tempdir;
use x_data_mcp::{
    market_data::{
        Adjustment, Bar, MarketDataBatch, MarketDataPipeline, Period, Provenance, SourceError,
    },
    task::{
        ScopeExecutionResult, TaskAcceptanceService, TaskExecutionRecord, TaskStatus, TaskStore,
    },
    worker::Worker,
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
            }],
            provenance: Provenance {
                source: "fake".into(),
                source_period: source_period.into(),
                source_adjustment: "qfq".into(),
                symbol: symbol.to_owned(),
                acquired_at: DateTime::from_timestamp(0, 0).unwrap(),
                adapter_version: "test".into(),
                yearly_derivation: None,
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

fn accept(store: &TaskStore, request_id: &str, symbols: &[&str], periods: &[Period]) -> String {
    TaskAcceptanceService::new(store.clone())
        .accept(request_id, symbols, periods, Adjustment::Forward)
        .unwrap()
        .task_id
}

fn worker(store: &TaskStore, calls: Arc<Mutex<Vec<(String, Period)>>>) -> Worker<FakeSource> {
    Worker::new(FakeSource { calls }, store.clone())
}

#[test]
fn one_run_once_processes_only_one_of_multiple_pending_tasks() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let first = accept(&store, "r1", &["000001"], &[Period::Day]);
    let second = accept(&store, "r2", &["600000"], &[Period::Day]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let worker = worker(&store, calls.clone());

    let step = worker.run_once().unwrap().unwrap();

    assert_eq!(step.task_id, first);
    assert_eq!(step.status, TaskStatus::Succeeded);
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        store.get_execution(&second).unwrap().unwrap().status,
        TaskStatus::Pending
    );
}

#[test]
fn terminal_task_is_not_executed_again() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(&store, "r1", &["000001"], &[Period::Day]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let worker = worker(&store, calls.clone());

    assert_eq!(
        worker.run_once().unwrap().unwrap().status,
        TaskStatus::Succeeded
    );
    assert!(worker.run_once().unwrap().is_none());
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        [("000001".into(), Period::Day)]
    );
    assert_eq!(
        store.get_execution(&task_id).unwrap().unwrap().status,
        TaskStatus::Succeeded
    );
}

#[test]
fn rebuilt_worker_continues_with_the_next_pending_task() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let first = accept(&store, "r1", &["000001"], &[Period::Day]);
    let second = accept(&store, "r2", &["600000"], &[Period::Day]);
    let first_calls = Arc::new(Mutex::new(Vec::new()));

    assert_eq!(
        worker(&store, first_calls)
            .run_once()
            .unwrap()
            .unwrap()
            .task_id,
        first
    );

    let rebuilt_calls = Arc::new(Mutex::new(Vec::new()));
    let rebuilt = worker(&store, rebuilt_calls.clone());
    assert_eq!(rebuilt.run_once().unwrap().unwrap().task_id, second);
    assert!(rebuilt.run_once().unwrap().is_none());
    assert_eq!(
        rebuilt_calls.lock().unwrap().as_slice(),
        [("600000".into(), Period::Day)]
    );
}

#[test]
fn persisted_running_task_needs_verification_without_source() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let task_id = accept(&store, "r1", &["000001", "600000"], &[Period::Day]);
    store
        .save_execution(TaskExecutionRecord {
            task_id: task_id.clone(),
            status: TaskStatus::Running,
            scope_results: vec![ScopeExecutionResult {
                symbol: "000001".into(),
                period: Period::Day,
                success: true,
                error: None,
            }],
        })
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let worker = worker(&store, calls.clone());

    assert!(worker.run_once().unwrap().is_none());
    assert!(calls.lock().unwrap().is_empty());
    let record = store.get_execution(&task_id).unwrap().unwrap();
    assert_eq!(record.status, TaskStatus::NeedsVerification);
    assert_eq!(record.scope_results.len(), 1);
    assert_eq!(record.scope_results[0].symbol, "000001");
}

#[test]
fn needs_verification_does_not_block_a_new_pending_task() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let interrupted = accept(&store, "r1", &["000001"], &[Period::Day]);
    let pending = accept(&store, "r2", &["600000"], &[Period::Day]);
    store
        .save_execution(TaskExecutionRecord {
            task_id: interrupted,
            status: TaskStatus::Running,
            scope_results: vec![],
        })
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));

    let step = worker(&store, calls.clone()).run_once().unwrap().unwrap();

    assert_eq!(step.task_id, pending);
    assert_eq!(step.status, TaskStatus::Succeeded);
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        [("600000".into(), Period::Day)]
    );
}

#[test]
fn queue_order_is_creation_order() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let first = accept(&store, "r1", &["000001"], &[Period::Day]);
    let second = accept(&store, "r2", &["000001"], &[Period::Year]);
    let third = accept(&store, "r3", &["600000"], &[Period::Day]);
    let worker = worker(&store, Arc::new(Mutex::new(Vec::new())));

    let processed = (0..3)
        .map(|_| worker.run_once().unwrap().unwrap().task_id)
        .collect::<Vec<_>>();

    assert_eq!(processed, vec![first, second, third]);
    assert!(worker.run_once().unwrap().is_none());
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
