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
        Ok(record)
    }

    pub fn get(&self, task_id: &str) -> Result<Option<TaskExecutionRecord>, ExecutionError> {
        Ok(self.store.get_execution(task_id)?)
    }
}
