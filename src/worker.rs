use crate::{
    execution::{ExecutionError, TaskExecutionService},
    market_data::MarketDataSource,
    task::{TaskExecutionRecord, TaskStatus, TaskStore},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerStep {
    pub task_id: String,
    pub status: TaskStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker task store failed: {0}")]
    Storage(#[from] std::io::Error),
    #[error("worker execution failed: {0}")]
    Execution(#[from] ExecutionError),
}

pub struct Worker<S> {
    execution: TaskExecutionService<S>,
    store: TaskStore,
}

impl<S: MarketDataSource> Worker<S> {
    pub fn new(source: S, store: TaskStore) -> Self {
        Self {
            execution: TaskExecutionService::new(source, store.clone()),
            store,
        }
    }

    pub fn run_once(&self) -> Result<Option<WorkerStep>, WorkerError> {
        let Some((task_id, status)) = self.next_runnable()? else {
            return Ok(None);
        };

        let record = match status {
            TaskStatus::Pending => {
                self.store.save_execution(TaskExecutionRecord {
                    task_id: task_id.clone(),
                    status: TaskStatus::Running,
                    scope_results: Vec::new(),
                })?;
                self.execution.execute(&task_id)?
            }
            TaskStatus::Running => self.execution.resume(&task_id)?,
            TaskStatus::Succeeded | TaskStatus::PartiallyFailed | TaskStatus::Failed => {
                unreachable!("terminal tasks are filtered before execution")
            }
        };

        Ok(Some(WorkerStep {
            task_id,
            status: record.status,
        }))
    }

    fn next_runnable(&self) -> Result<Option<(String, TaskStatus)>, std::io::Error> {
        for binding in self.store.list()? {
            let status = self
                .store
                .get_execution(&binding.task_id)?
                .map(|record| record.status)
                .unwrap_or(TaskStatus::Pending);
            if matches!(status, TaskStatus::Pending | TaskStatus::Running) {
                return Ok(Some((binding.task_id, status)));
            }
        }
        Ok(None)
    }
}
