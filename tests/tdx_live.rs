use x_data_mcp::collector::Collector;
use x_data_mcp::execution::TaskExecutionService;
use x_data_mcp::market_data::{Adjustment, CoverageStatus, MarketDataSource, Period};
use x_data_mcp::mcp::{GetTaskArgs, McpApi, StartCollectionArgs};
use x_data_mcp::storage::JsonFileStore;
use x_data_mcp::task::{
    ScopeExecutionResult, TaskAcceptanceService, TaskExecutionRecord, TaskStatus, TaskStore,
};
use x_data_mcp::tdx::TdxSource;
use x_data_mcp::worker::Worker;

#[test]
fn fetches_and_roundtrips_two_live_day_and_year_qfq_samples() {
    if std::env::var_os("X_DATA_MCP_TDX_LIVE").is_none() {
        return;
    }
    let python = std::env::var("X_DATA_MCP_TDX_PYTHON").expect("set X_DATA_MCP_TDX_PYTHON");
    let source = TdxSource::new(python, "scripts/tdx_bridge.py");
    let dir = tempfile::tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());

    for symbol in ["000001", "600000"] {
        for period in [Period::Day, Period::Year] {
            let batch = source.fetch(symbol, period, Adjustment::Forward).unwrap();
            assert!(!batch.bars.is_empty());
            assert_eq!(batch.coverage.status, CoverageStatus::Unknown);
            assert!(batch.coverage.reason.contains("no reliable"));
        }
    }
    let collector = Collector::new(source, store.clone());
    let results = collector.collect(
        &["000001", "600000"],
        &[Period::Day, Period::Year],
        Adjustment::Forward,
    );
    assert_eq!(results.len(), 4);
    for item in results {
        assert!(item.result.is_err(), "unexpected publish: {item:?}");
        assert!(item.result.unwrap_err().contains("coverage Unknown"));
        assert!(
            store
                .get_latest_effective(&item.symbol, item.period)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn recovers_live_tdx_task_without_repeating_preset_success() {
    if std::env::var_os("X_DATA_MCP_TDX_LIVE").is_none() {
        return;
    }
    let python = std::env::var("X_DATA_MCP_TDX_PYTHON").expect("set X_DATA_MCP_TDX_PYTHON");
    let dir = tempfile::tempdir().unwrap();
    let task_store = TaskStore::new(dir.path());
    let accepted = TaskAcceptanceService::new(task_store.clone())
        .accept(
            "live-recovery",
            &["000001", "600000"],
            &[Period::Day, Period::Year],
            Adjustment::Forward,
        )
        .unwrap();
    task_store
        .save_execution(TaskExecutionRecord {
            task_id: accepted.task_id.clone(),
            status: TaskStatus::Running,
            scope_results: vec![ScopeExecutionResult {
                symbol: "000001".into(),
                period: Period::Day,
                success: true,
                error: None,
            }],
        })
        .unwrap();
    let service = TaskExecutionService::new(
        TdxSource::new(python, "scripts/tdx_bridge.py"),
        task_store.clone(),
    );
    let record = service.resume(&accepted.task_id).unwrap();
    assert_eq!(record.status, TaskStatus::PartiallyFailed);
    assert_eq!(record.scope_results.len(), 4);
    assert!(record.scope_results[0].success);
    assert!(record.scope_results[1..].iter().all(|item| {
        !item.success && item.error.as_deref().unwrap().contains("coverage Unknown")
    }));
}

#[test]
fn mcp_live_collection_requires_coverage_evidence_before_publish() {
    if std::env::var_os("X_DATA_MCP_TDX_LIVE").is_none() {
        return;
    }
    let python = std::env::var("X_DATA_MCP_TDX_PYTHON").expect("set X_DATA_MCP_TDX_PYTHON");
    let dir = tempfile::tempdir().unwrap();
    let api = McpApi::new(
        TdxSource::new(python, "scripts/tdx_bridge.py"),
        TaskStore::new(dir.path()),
    );
    let started = api
        .start_collection(StartCollectionArgs {
            request_id: "live-mcp-collection".into(),
            symbols: vec!["000001".into(), "600000".into()],
            periods: vec!["day".into(), "year".into()],
        })
        .unwrap();
    assert_eq!(started.status, "pending");
    assert_eq!(started.frozen_range.len(), 4);

    let run = api.run_worker_once().unwrap();
    assert!(!run.idle);
    assert_eq!(run.task_id.as_deref(), Some(started.task_id.as_str()));
    assert_eq!(run.status.as_deref(), Some("failed"));

    let view = api
        .get_task(GetTaskArgs {
            task_id: started.task_id,
        })
        .unwrap();
    assert_eq!(view.status, "failed");
    assert_eq!(view.scope_results.len(), 4);
    assert!(view.scope_results.iter().all(|result| {
        !result.success
            && result
                .error
                .as_deref()
                .unwrap()
                .contains("coverage Unknown")
    }));
    assert_eq!(
        view.scope_results
            .iter()
            .map(|result| (result.symbol.as_str(), result.period.as_str()))
            .collect::<std::collections::BTreeSet<_>>(),
        [
            ("000001", "day"),
            ("000001", "year"),
            ("600000", "day"),
            ("600000", "year"),
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn worker_processes_two_live_tdx_tasks_to_terminal() {
    if std::env::var_os("X_DATA_MCP_TDX_LIVE").is_none() {
        return;
    }
    let python = std::env::var("X_DATA_MCP_TDX_PYTHON").expect("set X_DATA_MCP_TDX_PYTHON");
    let dir = tempfile::tempdir().unwrap();
    let task_store = TaskStore::new(dir.path());
    let acceptance = TaskAcceptanceService::new(task_store.clone());
    let accepted_000001 = acceptance
        .accept(
            "live-worker-000001",
            &["000001"],
            &[Period::Day, Period::Year],
            Adjustment::Forward,
        )
        .unwrap();
    let accepted_600000 = acceptance
        .accept(
            "live-worker-600000",
            &["600000"],
            &[Period::Day, Period::Year],
            Adjustment::Forward,
        )
        .unwrap();
    let worker = Worker::new(
        TdxSource::new(python, "scripts/tdx_bridge.py"),
        task_store.clone(),
    );

    let first = worker.run_once().unwrap().unwrap();
    let second = worker.run_once().unwrap().unwrap();
    assert_eq!(first.task_id, accepted_000001.task_id);
    assert_eq!(second.task_id, accepted_600000.task_id);
    assert_eq!(first.status, TaskStatus::Failed);
    assert_eq!(second.status, TaskStatus::Failed);
    assert!(worker.run_once().unwrap().is_none());

    let market_store = JsonFileStore::new(dir.path());
    for symbol in ["000001", "600000"] {
        let record = task_store
            .get_execution(if symbol == "000001" {
                &accepted_000001.task_id
            } else {
                &accepted_600000.task_id
            })
            .unwrap()
            .unwrap();
        assert_eq!(record.status, TaskStatus::Failed);
        assert_eq!(record.scope_results.len(), 2);
        for period in [Period::Day, Period::Year] {
            assert!(
                market_store
                    .get_latest_effective(symbol, period)
                    .unwrap()
                    .is_none()
            );
        }
    }
}
