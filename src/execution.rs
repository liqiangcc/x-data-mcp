use crate::{
    collector::Collector,
    market_data::MarketDataPipeline,
    storage::JsonFileStore,
    task::{FrozenScope, ScopeExecutionResult, TaskExecutionRecord, TaskStatus, TaskStore},
};

const CANCELLED_SCOPE_ERROR: &str = "cancelled before execution";

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("task {0} was not found")]
    NotFound(String),
    #[error("task execution storage failed: {0}")]
    Storage(#[from] std::io::Error),
    #[error("task {task_id} is not recoverable (expected running status)")]
    NotRunning { task_id: String },
}

pub struct TaskExecutionService<S> {
    source: S,
    store: TaskStore,
}

impl<S: MarketDataPipeline> TaskExecutionService<S> {
    pub fn new(source: S, store: TaskStore) -> Self {
        Self { source, store }
    }

    pub fn execute(&self, task_id: &str) -> Result<TaskExecutionRecord, ExecutionError> {
        let binding = self
            .store
            .get_task(task_id)?
            .ok_or_else(|| ExecutionError::NotFound(task_id.to_owned()))?;
        self.store.save_execution(TaskExecutionRecord {
            task_id: task_id.to_owned(),
            status: TaskStatus::Running,
            scope_results: Vec::new(),
        })?;
        let collector = Collector::new(&self.source, JsonFileStore::new(self.store.root()));
        let mut scope_results = Vec::new();
        let mut cancelled = false;
        for (index, scope) in binding.frozen_range.iter().enumerate() {
            if self.is_cancel_requested(task_id)? {
                append_cancelled_scopes(&binding.frozen_range, index, &mut scope_results);
                cancelled = true;
                break;
            }
            let item = collector
                .collect_frozen(std::slice::from_ref(scope), binding.adjustment)
                .into_iter()
                .next()
                .expect("one frozen scope produces one result");
            scope_results.push(ScopeExecutionResult {
                symbol: item.symbol,
                period: item.period,
                success: item.result.is_ok(),
                error: item.result.err(),
            });
            if self.is_cancel_requested(task_id)? {
                append_cancelled_scopes(&binding.frozen_range, index + 1, &mut scope_results);
                cancelled = true;
                break;
            }
        }
        let record = TaskExecutionRecord {
            task_id: task_id.to_owned(),
            status: if cancelled {
                TaskStatus::Cancelled
            } else {
                terminal_status(&scope_results)
            },
            scope_results,
        };
        self.store.save_execution(record.clone())?;
        self.store.release_occupancy(task_id)?;
        Ok(record)
    }

    pub fn get(&self, task_id: &str) -> Result<Option<TaskExecutionRecord>, ExecutionError> {
        Ok(self.store.get_execution(task_id)?)
    }

    pub fn resume(&self, task_id: &str) -> Result<TaskExecutionRecord, ExecutionError> {
        let binding = self
            .store
            .get_task(task_id)?
            .ok_or_else(|| ExecutionError::NotFound(task_id.to_owned()))?;
        let mut record =
            self.store
                .get_execution(task_id)?
                .ok_or_else(|| ExecutionError::NotRunning {
                    task_id: task_id.to_owned(),
                })?;
        if record.status != TaskStatus::Running {
            return Err(ExecutionError::NotRunning {
                task_id: task_id.to_owned(),
            });
        }
        let collector = Collector::new(&self.source, JsonFileStore::new(self.store.root()));
        let mut cancelled = false;
        for (index, scope) in binding.frozen_range.iter().enumerate() {
            if self.is_cancel_requested(task_id)? {
                append_cancelled_scopes(&binding.frozen_range, index, &mut record.scope_results);
                cancelled = true;
                break;
            }
            if record.scope_results.iter().any(|item| {
                item.symbol == scope.symbol && item.period == scope.period && item.success
            }) {
                continue;
            }
            let item = collector.collect_frozen(std::slice::from_ref(scope), binding.adjustment);
            let result = item
                .into_iter()
                .next()
                .expect("one frozen scope produces one result");
            let scope_result = ScopeExecutionResult {
                symbol: result.symbol,
                period: result.period,
                success: result.result.is_ok(),
                error: result.result.err(),
            };
            record.scope_results.retain(|existing| {
                existing.symbol != scope_result.symbol || existing.period != scope_result.period
            });
            record.scope_results.push(scope_result);
            let cancellation_requested = self.is_cancel_requested(task_id)?;
            if cancellation_requested {
                append_cancelled_scopes(
                    &binding.frozen_range,
                    index + 1,
                    &mut record.scope_results,
                );
                record.status = TaskStatus::CancelRequested;
                cancelled = true;
            }
            self.store.save_execution(record.clone())?;
            if cancelled {
                break;
            }
        }
        record.scope_results.sort_by_key(|item| {
            binding
                .frozen_range
                .iter()
                .position(|scope| scope.symbol == item.symbol && scope.period == item.period)
                .unwrap_or(usize::MAX)
        });
        record.status = if cancelled {
            TaskStatus::Cancelled
        } else {
            terminal_status(&record.scope_results)
        };
        self.store.save_execution(record.clone())?;
        self.store.release_occupancy(task_id)?;
        Ok(record)
    }

    pub fn finalize_cancel_requested(
        &self,
        task_id: &str,
    ) -> Result<TaskExecutionRecord, ExecutionError> {
        let binding = self
            .store
            .get_task(task_id)?
            .ok_or_else(|| ExecutionError::NotFound(task_id.to_owned()))?;
        let mut record =
            self.store
                .get_execution(task_id)?
                .ok_or_else(|| ExecutionError::NotRunning {
                    task_id: task_id.to_owned(),
                })?;
        if record.status != TaskStatus::CancelRequested {
            return Err(ExecutionError::NotRunning {
                task_id: task_id.to_owned(),
            });
        }
        append_cancelled_scopes(&binding.frozen_range, 0, &mut record.scope_results);
        record.scope_results.sort_by_key(|item| {
            binding
                .frozen_range
                .iter()
                .position(|scope| scope.symbol == item.symbol && item.period == scope.period)
                .unwrap_or(usize::MAX)
        });
        record.status = TaskStatus::Cancelled;
        self.store.save_execution(record.clone())?;
        self.store.release_occupancy(task_id)?;
        Ok(record)
    }

    fn is_cancel_requested(&self, task_id: &str) -> Result<bool, ExecutionError> {
        Ok(self
            .store
            .get_execution(task_id)?
            .map(|record| record.status == TaskStatus::CancelRequested)
            .unwrap_or(false))
    }
}

fn terminal_status(scope_results: &[ScopeExecutionResult]) -> TaskStatus {
    let successes = scope_results.iter().filter(|item| item.success).count();
    if successes == scope_results.len() {
        TaskStatus::Succeeded
    } else if successes == 0 {
        TaskStatus::Failed
    } else {
        TaskStatus::PartiallyFailed
    }
}

fn append_cancelled_scopes(
    frozen_range: &[FrozenScope],
    start: usize,
    scope_results: &mut Vec<ScopeExecutionResult>,
) {
    for scope in frozen_range.iter().skip(start) {
        if scope_results
            .iter()
            .any(|item| item.symbol == scope.symbol && item.period == scope.period)
        {
            continue;
        }
        scope_results.push(ScopeExecutionResult {
            symbol: scope.symbol.clone(),
            period: scope.period,
            success: false,
            error: Some(CANCELLED_SCOPE_ERROR.into()),
        });
    }
}
