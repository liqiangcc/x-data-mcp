use crate::{
    execution::{ExecutionError, TaskExecutionService},
    market_data::MarketDataPipeline,
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

impl<S: MarketDataPipeline> Worker<S> {
    pub fn new(source: S, store: TaskStore) -> Self {
        Self {
            execution: TaskExecutionService::new(source, store.clone()),
            store,
        }
    }

    pub fn run_once(&self) -> Result<Option<WorkerStep>, WorkerError> {
        self.mark_running_tasks_needing_verification()?;
        if let Some(task_id) = self.next_cancel_requested()? {
            let record = self.execution.finalize_cancel_requested(&task_id)?;
            return Ok(Some(WorkerStep {
                task_id,
                status: record.status,
            }));
        }
        let Some(task_id) = self.next_pending()? else {
            return Ok(None);
        };

        self.store.save_execution(TaskExecutionRecord {
            task_id: task_id.clone(),
            status: TaskStatus::Running,
            scope_results: Vec::new(),
        })?;
        let record = self.execution.execute(&task_id)?;

        Ok(Some(WorkerStep {
            task_id,
            status: record.status,
        }))
    }

    fn mark_running_tasks_needing_verification(&self) -> Result<(), std::io::Error> {
        for binding in self.store.list()? {
            if let Some(record) = self.store.get_execution(&binding.task_id)?
                && record.status == TaskStatus::Running
            {
                self.store.save_execution(TaskExecutionRecord {
                    task_id: record.task_id,
                    status: TaskStatus::NeedsVerification,
                    scope_results: record.scope_results,
                })?;
            }
        }
        Ok(())
    }

    fn next_pending(&self) -> Result<Option<String>, std::io::Error> {
        for binding in self.store.list()? {
            let status = self
                .store
                .get_execution(&binding.task_id)?
                .map(|record| record.status)
                .unwrap_or(TaskStatus::Pending);
            if status == TaskStatus::Pending {
                return Ok(Some(binding.task_id));
            }
        }
        Ok(None)
    }

    fn next_cancel_requested(&self) -> Result<Option<String>, std::io::Error> {
        for binding in self.store.list()? {
            let status = self
                .store
                .get_execution(&binding.task_id)?
                .map(|record| record.status);
            if status == Some(TaskStatus::CancelRequested) {
                return Ok(Some(binding.task_id));
            }
        }
        Ok(None)
    }
}
