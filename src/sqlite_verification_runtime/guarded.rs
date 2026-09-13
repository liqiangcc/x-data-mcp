//! A runner-scoped repository owns the guard until every runner path returns.
use super::{SqliteTasks, runtime_error};
use crate::{
    task_control::{
        Acceptance, Change, Command, Error, FrozenWork, RequestRecord, RetryGuard, TaskRepository,
        TaskSnapshot,
    },
    task_verification::decision::{Receipt, StopPlan, StopRepository},
    verification_runtime::ExecutionGuard,
};

pub(crate) struct GuardedTasks {
    repository: SqliteTasks,
    guard: ExecutionGuard,
}
impl GuardedTasks {
    pub(crate) fn open(repository: SqliteTasks, task: &str, run: &str) -> Result<Self, Error> {
        let guard = crate::verification_runtime::create(repository.path(), task, run)
            .map_err(runtime_error)?;
        Ok(Self { repository, guard })
    }
    fn bound(&self, task: &str) -> Result<(), Error> {
        if self.guard.descriptor().task_id != task {
            return Err(Error::RevisionConflict);
        }
        self.guard
            .verify_binding(self.repository.path())
            .map_err(runtime_error)
    }
}
impl TaskRepository for GuardedTasks {
    fn read_request(&self, id: &str) -> Result<Option<RequestRecord>, Error> {
        self.repository.read_request(id)
    }
    fn read_task(&self, id: &str) -> Result<Option<TaskSnapshot>, Error> {
        self.repository.read_task(id)
    }
    fn accept(
        &self,
        _: &str,
        _: &Command,
        _: &FrozenWork,
        _: &str,
        _: Option<&RetryGuard>,
    ) -> Result<Acceptance, Error> {
        Err(Error::UnsupportedContract)
    }
    fn transition(
        &self,
        id: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error> {
        self.bound(id)?;
        if run != self.guard.descriptor().run_id {
            return Err(Error::RevisionConflict);
        }
        if matches!(change, Change::Claim) {
            let task = self.repository.read_task(id)?.ok_or(Error::TaskNotFound)?;
            if task.task.state_revision != revision {
                return Err(Error::RevisionConflict);
            }
            let plan = crate::verification_execution::prepare_claim(
                task,
                self.guard.descriptor().clone(),
                chrono::Utc::now(),
            )?;
            self.repository.claim_with_runtime_guard(&plan, &self.guard)
        } else {
            self.repository.transition(id, revision, run, change)
        }
    }
    fn candidates(&self, _: &str) -> Result<Vec<TaskSnapshot>, Error> {
        Err(Error::UnsupportedContract)
    }
    fn reconcile(&self, _: &str) -> Result<usize, Error> {
        Err(Error::UnsupportedContract)
    }
}
impl StopRepository for GuardedTasks {
    fn commit_stop_check(&self, plan: &StopPlan) -> Result<Receipt, Error> {
        self.bound(&plan.receipt().verification_task_id)?;
        self.repository.commit_stop_check(plan)
    }
    fn stop_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.bound(id)?;
        self.repository.stop_receipt(id)
    }
}
