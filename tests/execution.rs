use chrono::DateTime;
use rust_decimal::Decimal;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use x_data_mcp::{
    execution::TaskExecutionService,
    market_data::{
        Adjustment, Bar, MarketDataBatch, MarketDataPipeline, Period, Provenance, SourceError,
    },
    task::{
        ScopeExecutionResult, TaskAcceptanceService, TaskExecutionRecord, TaskStatus, TaskStore,
    },
};

#[derive(Clone)]
struct FakeSource {
    calls: Arc<Mutex<Vec<(String, Period)>>>,
    failures: Vec<(String, Period)>,
}
type Calls = Arc<Mutex<Vec<(String, Period)>>>;

impl FakeSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        self.calls.lock().unwrap().push((symbol.to_owned(), period));
        if self
            .failures
            .iter()
            .any(|item| item == &(symbol.to_owned(), period))
        {
            return Err(SourceError::Invocation("planned failure".into()));
        }
        let source_period = match period {
            Period::Day => "day",
            Period::Year => "day",
        };
        Ok(MarketDataBatch {
            symbol: symbol.into(),
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
                source_period: source_period.into(),
                source_adjustment: "qfq".into(),
                symbol: symbol.into(),
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

fn accept(dir: &std::path::Path) -> String {
    TaskAcceptanceService::new(TaskStore::new(dir))
        .accept(
            "request",
            &["000001", "600000"],
            &[Period::Day, Period::Year],
            Adjustment::Forward,
        )
        .unwrap()
        .task_id
}

fn execute(
    failures: Vec<(String, Period)>,
) -> (
    x_data_mcp::task::TaskExecutionRecord,
    Calls,
    tempfile::TempDir,
) {
    let dir = tempdir().unwrap();
    let task_id = accept(dir.path());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let record = TaskExecutionService::new(
        FakeSource {
            calls: calls.clone(),
            failures,
        },
        TaskStore::new(dir.path()),
    )
    .execute(&task_id)
    .unwrap();
    (record, calls, dir)
}

#[test]
fn all_scopes_succeed() {
    let (record, _, _) = execute(Vec::new());
    assert_eq!(record.status, TaskStatus::Succeeded);
    assert_eq!(record.scope_results.len(), 4);
    assert!(record.scope_results.iter().all(|item| item.success));
}

#[test]
fn partial_failure_preserves_successes() {
    let (record, _, dir) = execute(vec![("600000".into(), Period::Year)]);
    assert_eq!(record.status, TaskStatus::PartiallyFailed);
    assert_eq!(
        record
            .scope_results
            .iter()
            .filter(|item| item.success)
            .count(),
        3
    );
    assert!(std::fs::read(dir.path().join("000001.day.qfq.json")).is_ok());
    assert!(std::fs::read(dir.path().join("600000.year.qfq.json")).is_err());
}

#[test]
fn all_scopes_fail() {
    let (record, _, _) = execute(vec![
        ("000001".into(), Period::Day),
        ("000001".into(), Period::Year),
        ("600000".into(), Period::Day),
        ("600000".into(), Period::Year),
    ]);
    assert_eq!(record.status, TaskStatus::Failed);
    assert!(record.scope_results.iter().all(|item| !item.success));
}

#[test]
fn execution_consumes_frozen_range_exactly() {
    let (record, calls, _) = execute(Vec::new());
    let calls = calls.lock().unwrap().clone();
    let executed: Vec<_> = record
        .scope_results
        .iter()
        .map(|item| (item.symbol.clone(), item.period))
        .collect();
    assert_eq!(calls, executed);
}

#[test]
fn rebuilt_service_reads_terminal_record() {
    let dir = tempdir().unwrap();
    let task_id = accept(dir.path());
    let service = TaskExecutionService::new(
        FakeSource {
            calls: Arc::new(Mutex::new(Vec::new())),
            failures: Vec::new(),
        },
        TaskStore::new(dir.path()),
    );
    service.execute(&task_id).unwrap();
    let rebuilt = TaskExecutionService::new(
        FakeSource {
            calls: Arc::new(Mutex::new(Vec::new())),
            failures: Vec::new(),
        },
        TaskStore::new(dir.path()),
    );
    let read = rebuilt.get(&task_id).unwrap().unwrap();
    assert_eq!(read.status, TaskStatus::Succeeded);
    assert_eq!(read.scope_results.len(), 4);
}

fn running_record(dir: &std::path::Path, task_id: &str, results: Vec<ScopeExecutionResult>) {
    // A successful daily scope now supplies the persisted base for local yearly.
    let pipeline = FakeSource {
        calls: Arc::new(Mutex::new(Vec::new())),
        failures: Vec::new(),
    };
    let publisher =
        x_data_mcp::publish::MarketDataPublisher::new(x_data_mcp::storage::JsonFileStore::new(dir));
    for result in &results {
        if result.success && result.period == Period::Day {
            publisher
                .publish(
                    &pipeline
                        .materialize_daily(&result.symbol, Adjustment::Forward)
                        .unwrap(),
                )
                .unwrap();
        }
    }
    TaskStore::new(dir)
        .save_execution(TaskExecutionRecord {
            task_id: task_id.to_owned(),
            status: TaskStatus::Running,
            scope_results: results,
        })
        .unwrap();
}

#[test]
fn resume_runs_only_unexecuted_scopes() {
    let dir = tempdir().unwrap();
    let task_id = accept(dir.path());
    running_record(
        dir.path(),
        &task_id,
        vec![ScopeExecutionResult {
            symbol: "000001".into(),
            period: Period::Day,
            success: true,
            error: None,
        }],
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let record = TaskExecutionService::new(
        FakeSource {
            calls: calls.clone(),
            failures: Vec::new(),
        },
        TaskStore::new(dir.path()),
    )
    .resume(&task_id)
    .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 3);
    assert_eq!(record.status, TaskStatus::Succeeded);
}

#[test]
fn resume_skips_success_and_retries_failure() {
    let dir = tempdir().unwrap();
    let task_id = accept(dir.path());
    running_record(
        dir.path(),
        &task_id,
        vec![
            ScopeExecutionResult {
                symbol: "000001".into(),
                period: Period::Day,
                success: true,
                error: None,
            },
            ScopeExecutionResult {
                symbol: "600000".into(),
                period: Period::Year,
                success: false,
                error: Some("old".into()),
            },
        ],
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let record = TaskExecutionService::new(
        FakeSource {
            calls: calls.clone(),
            failures: Vec::new(),
        },
        TaskStore::new(dir.path()),
    )
    .resume(&task_id)
    .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 3);
    assert_eq!(record.status, TaskStatus::Succeeded);
}

#[test]
fn resume_has_no_duplicate_scope_results() {
    let dir = tempdir().unwrap();
    let task_id = accept(dir.path());
    running_record(
        dir.path(),
        &task_id,
        vec![ScopeExecutionResult {
            symbol: "000001".into(),
            period: Period::Day,
            success: true,
            error: None,
        }],
    );
    let record = TaskExecutionService::new(
        FakeSource {
            calls: Arc::new(Mutex::new(Vec::new())),
            failures: Vec::new(),
        },
        TaskStore::new(dir.path()),
    )
    .resume(&task_id)
    .unwrap();
    assert_eq!(record.scope_results.len(), 4);
    for (i, left) in record.scope_results.iter().enumerate() {
        assert!(
            !record.scope_results[i + 1..]
                .iter()
                .any(|right| left.symbol == right.symbol && left.period == right.period)
        );
    }
}

#[test]
fn all_success_resume_does_not_call_source() {
    let dir = tempdir().unwrap();
    let task_id = accept(dir.path());
    let all = ["000001", "600000"]
        .into_iter()
        .flat_map(|symbol| {
            [Period::Day, Period::Year]
                .into_iter()
                .map(move |period| ScopeExecutionResult {
                    symbol: symbol.into(),
                    period,
                    success: true,
                    error: None,
                })
        })
        .collect();
    running_record(dir.path(), &task_id, all);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let record = TaskExecutionService::new(
        FakeSource {
            calls: calls.clone(),
            failures: Vec::new(),
        },
        TaskStore::new(dir.path()),
    )
    .resume(&task_id)
    .unwrap();
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(record.status, TaskStatus::Succeeded);
}

#[test]
fn rebuilt_service_can_resume_running_task() {
    let dir = tempdir().unwrap();
    let task_id = accept(dir.path());
    running_record(
        dir.path(),
        &task_id,
        vec![ScopeExecutionResult {
            symbol: "000001".into(),
            period: Period::Day,
            success: true,
            error: None,
        }],
    );
    let rebuilt = TaskExecutionService::new(
        FakeSource {
            calls: Arc::new(Mutex::new(Vec::new())),
            failures: Vec::new(),
        },
        TaskStore::new(dir.path()),
    );
    assert_eq!(
        rebuilt.resume(&task_id).unwrap().status,
        TaskStatus::Succeeded
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
