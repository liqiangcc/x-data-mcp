use crate::{
    collector::Collector,
    market_data::MarketDataSource,
    storage::JsonFileStore,
    task::{ScopeExecutionResult, TaskExecutionRecord, TaskStatus, TaskStore},
};

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

impl<S: MarketDataSource> TaskExecutionService<S> {
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
        let raw = collector.collect_frozen(&binding.frozen_range, binding.adjustment);
        let scope_results: Vec<ScopeExecutionResult> = raw
            .into_iter()
            .map(|item| ScopeExecutionResult {
                symbol: item.symbol,
                period: item.period,
                success: item.result.is_ok(),
                error: item.result.err(),
            })
            .collect();
        let successes = scope_results.iter().filter(|item| item.success).count();
        let status = if successes == scope_results.len() {
            TaskStatus::Succeeded
        } else if successes == 0 {
            TaskStatus::Failed
        } else {
            TaskStatus::PartiallyFailed
        };
        let record = TaskExecutionRecord {
            task_id: task_id.to_owned(),
            status,
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
        for scope in &binding.frozen_range {
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
            self.store.save_execution(record.clone())?;
        }
        record.scope_results.sort_by_key(|item| {
            binding
                .frozen_range
                .iter()
                .position(|scope| scope.symbol == item.symbol && scope.period == item.period)
                .unwrap_or(usize::MAX)
        });
        let successes = record
            .scope_results
            .iter()
            .filter(|item| item.success)
            .count();
        record.status = if successes == binding.frozen_range.len() {
            TaskStatus::Succeeded
        } else if successes == 0 {
            TaskStatus::Failed
        } else {
            TaskStatus::PartiallyFailed
        };
        self.store.save_execution(record.clone())?;
        self.store.release_occupancy(task_id)?;
        Ok(record)
    }
}
