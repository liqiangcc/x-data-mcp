//! Response fault wrapper around real SQLite, not a simulated database.
use x_data_mcp::{
    sqlite_tasks::SqliteTasks,
    task_control::*,
    task_verification::decision::{Receipt, StopPlan, StopRepository},
};
pub struct Fault {
    pub db: SqliteTasks,
    pub commit: bool,
}
impl StopRepository for Fault {
    fn commit_stop_check(&self, p: &StopPlan) -> Result<Receipt, Error> {
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
