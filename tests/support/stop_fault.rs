//! Response fault wrapper around real SQLite, not a simulated database.
use x_data_mcp::{
    sqlite_tasks::SqliteTasks,
    task_control::*,
    task_verification::decision::{Receipt, StopPlan, StopRepository},
};
pub struct Fault {
    pub db: SqliteTasks,
    pub commit: bool,
    pub mutation: Option<bool>, // true: cancellation, false: target starts before commit
}
impl StopRepository for Fault {
    fn commit_stop_check(&self, p: &StopPlan) -> Result<Receipt, Error> {
        if let Some(cancel) = self.mutation {
            let id = if cancel {
                &p.receipt().verification_task_id
            } else {
                &p.receipt().target_task_id
            };
            let state = self.db.read_task(id)?.unwrap();
            if cancel {
                self.db.transition(
                    id,
                    state.task.state_revision,
                    "new-run",
                    Change::RequestCancel,
                )?;
            } else if state.task.status == Status::Pending {
                self.db.transition(
                    id,
                    state.task.state_revision,
                    &state.task.accepted_run_id,
                    Change::Claim,
                )?;
            } else {
                return self.db.commit_stop_check(p);
            }
            return Err(Error::RevisionConflict);
        }
        if self.commit {
            self.db.commit_stop_check(p)?;
        }
        Err(Error::OutcomeUnknown("test: response unavailable".into()))
    }
    fn stop_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.db.stop_receipt(id)
    }
}
impl TaskRepository for Fault {
    fn read_request(&self, id: &str) -> Result<Option<RequestRecord>, Error> {
        self.db.read_request(id)
    }
    fn read_task(&self, id: &str) -> Result<Option<TaskSnapshot>, Error> {
        self.db.read_task(id)
    }
    fn accept(
        &self,
        id: &str,
        c: &Command,
        w: &FrozenWork,
        r: &str,
        g: Option<&RetryGuard>,
    ) -> Result<Acceptance, Error> {
        self.db.accept(id, c, w, r, g)
    }
    fn transition(&self, id: &str, v: u64, r: &str, c: Change) -> Result<TaskSnapshot, Error> {
        self.db.transition(id, v, r, c)
    }
    fn candidates(&self, r: &str) -> Result<Vec<TaskSnapshot>, Error> {
        self.db.candidates(r)
    }
    fn reconcile(&self, r: &str) -> Result<usize, Error> {
        self.db.reconcile(r)
    }
}
