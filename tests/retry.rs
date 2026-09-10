use chrono::DateTime;
use rust_decimal::Decimal;
use tempfile::tempdir;
use x_data_mcp::{
    execution::TaskExecutionService,
    market_data::{
        Adjustment, Bar, MarketDataBatch, MarketDataSource, Period, Provenance, SourceError,
    },
    task::{
        FrozenScope, TaskAcceptanceService, TaskError, TaskRetryService, TaskStatus, TaskStore,
    },
    worker::Worker,
};

#[derive(Clone)]
struct FakeSource {
    failures: Vec<(String, Period)>,
}

impl MarketDataSource for FakeSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        if self
            .failures
            .iter()
            .any(|item| item == &(symbol.to_owned(), period))
        {
            return Err(SourceError::Invocation("planned failure".into()));
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
        })
    }
}

fn accept(store: &TaskStore, request_id: &str) -> String {
    TaskAcceptanceService::new(store.clone())
        .accept(
            request_id,
            &["000001", "600000"],
            &[Period::Day, Period::Year],
            Adjustment::Forward,
        )
        .unwrap()
        .task_id
}

fn scope(symbol: &str, period: Period) -> FrozenScope {
    FrozenScope {
        symbol: symbol.into(),
        period,
    }
}

fn terminal_original(store: &TaskStore, failures: Vec<(String, Period)>) -> String {
    let task_id = accept(store, "original");
    TaskExecutionService::new(FakeSource { failures }, store.clone())
        .execute(&task_id)
        .unwrap();
    task_id
}

#[test]
fn retries_only_unfinished_subset_and_preserves_original_after_restart() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let original_task_id = terminal_original(&store, vec![("600000".into(), Period::Year)]);
    let original_binding = store.get_task(&original_task_id).unwrap().unwrap();
    let original_execution = store.get_execution(&original_task_id).unwrap().unwrap();

    let retry = TaskRetryService::new(TaskStore::new(dir.path()))
        .retry_task(
            &original_task_id,
            "retry-1",
            &[scope("600000", Period::Year), scope("600000", Period::Year)],
            Adjustment::Forward,
        )
        .unwrap();
    assert!(!retry.reused);
    assert_eq!(
        retry.binding.frozen_range,
        vec![scope("600000", Period::Year)]
    );
    assert_eq!(
        retry.binding.retry_of.as_deref(),
        Some(original_task_id.as_str())
    );

    let rebuilt = TaskStore::new(dir.path());
    assert_eq!(
        rebuilt.get_task(&retry.task_id).unwrap().unwrap().retry_of,
        Some(original_task_id.clone())
    );
    assert_eq!(
        rebuilt.get_task(&original_task_id).unwrap().unwrap(),
        original_binding
    );
    assert_eq!(
        rebuilt.get_execution(&original_task_id).unwrap().unwrap(),
        original_execution
    );

    let result = Worker::new(
        FakeSource {
            failures: Vec::new(),
        },
        rebuilt.clone(),
    )
    .run_once()
    .unwrap()
    .unwrap();
    assert_eq!(result.task_id, retry.task_id);
    assert_eq!(result.status, TaskStatus::Succeeded);
    assert_eq!(
        rebuilt.get_execution(&original_task_id).unwrap().unwrap(),
        original_execution
    );
}

#[test]
fn invalid_retry_selection_rejects_as_a_whole_without_writes() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let original_task_id = terminal_original(&store, vec![("600000".into(), Period::Year)]);
    let retry = TaskRetryService::new(store.clone());
    let before = store.list().unwrap();

    assert!(matches!(
        retry.retry_task(&original_task_id, "empty", &[], Adjustment::Forward),
        Err(TaskError::RetrySelectionEmpty)
    ));
    assert!(matches!(
        retry.retry_task(
            &original_task_id,
            "completed",
            &[scope("000001", Period::Day)],
            Adjustment::Forward
        ),
        Err(TaskError::RetrySelectionInvalid { .. })
    ));
    assert!(matches!(
        retry.retry_task(
            &original_task_id,
            "outside",
            &[scope("999999", Period::Day)],
            Adjustment::Forward
        ),
        Err(TaskError::RetrySelectionInvalid { .. })
    ));
    assert!(matches!(
        retry.retry_task(
            &original_task_id,
            "mixed",
            &[scope("600000", Period::Year), scope("999999", Period::Day),],
            Adjustment::Forward
        ),
        Err(TaskError::RetrySelectionInvalid { .. })
    ));

    assert_eq!(store.list().unwrap(), before);
    for request_id in ["empty", "completed", "outside", "mixed"] {
        assert!(store.get(request_id).unwrap().is_none());
    }
    assert!(store.list_occupancy().unwrap().is_empty());
}

#[test]
fn retry_request_is_idempotent_and_conflicting_parameters_are_rejected() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let original_task_id = terminal_original(
        &store,
        vec![
            ("000001".into(), Period::Day),
            ("600000".into(), Period::Year),
        ],
    );
    let retry = TaskRetryService::new(store.clone());
    let first = retry
        .retry_task(
            &original_task_id,
            "retry-same",
            &[scope("600000", Period::Year)],
            Adjustment::Forward,
        )
        .unwrap();
    let second = retry
        .retry_task(
            &original_task_id,
            "retry-same",
            &[scope("600000", Period::Year), scope("600000", Period::Year)],
            Adjustment::Forward,
        )
        .unwrap();
    assert_eq!(first.task_id, second.task_id);
    assert!(second.reused);
    assert!(matches!(
        retry.retry_task(
            &original_task_id,
            "retry-same",
            &[scope("000001", Period::Day)],
            Adjustment::Forward
        ),
        Err(TaskError::RequestConflict { .. })
    ));
    assert!(matches!(
        retry.retry_task(
            &original_task_id,
            "original",
            &[scope("000001", Period::Day)],
            Adjustment::Forward
        ),
        Err(TaskError::RetryRequestIdConflict)
    ));
}

#[test]
fn retry_occupancy_conflict_rejects_without_creating_retry_task() {
    let dir = tempdir().unwrap();
    let store = TaskStore::new(dir.path());
    let original_task_id = terminal_original(&store, vec![("600000".into(), Period::Year)]);
    let blocker = TaskAcceptanceService::new(store.clone())
        .accept("blocker", &["600000"], &[Period::Year], Adjustment::Forward)
        .unwrap();
    let error = TaskRetryService::new(store.clone())
        .retry_task(
            &original_task_id,
            "retry-busy",
            &[scope("600000", Period::Year)],
            Adjustment::Forward,
        )
        .unwrap_err();
    assert!(
        matches!(error, TaskError::RangeConflict { scopes } if scopes == vec![scope("600000", Period::Year)])
    );
    assert!(store.get("retry-busy").unwrap().is_none());
    assert_eq!(store.list_occupancy().unwrap().len(), 1);
    assert_eq!(store.list_occupancy().unwrap()[0].task_id, blocker.task_id);
}
