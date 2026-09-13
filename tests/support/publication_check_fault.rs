//! Faults around actual SQLite reads/commits, never substitute stored facts.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use x_data_mcp::{
    market_storage::Error,
    publication_verification::{
        EvidenceRepository, Facts,
        decision::{AuditStore, Plan, Receipt},
    },
    sqlite_market::SqliteMarket,
    task_control::{Change, TaskRepository},
};
#[derive(Clone, Copy)]
pub enum Mode {
    CancelRead,
    CancelCommit,
    LostBefore,
    LostAfter,
    ChangeTarget,
    ConflictTwice,
}
pub struct Fault {
    pub db: SqliteMarket,
    pub check_id: String,
    pub mode: Mode,
    pub reads: Arc<AtomicUsize>,
    pub commits: Arc<AtomicUsize>,
}
impl Fault {
    fn cancel(&self) -> Result<(), Error> {
        let s = self.db.tasks.read_task(&self.check_id)?.unwrap();
        self.db.tasks.transition(
            &self.check_id,
            s.task.state_revision,
            "inspection-run",
            Change::RequestCancel,
        )?;
        Ok(())
    }
}
impl EvidenceRepository for Fault {
    fn publication_facts(&self, id: &str) -> Result<Facts, Error> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let facts = self.db.publication_facts(id)?;
        if matches!(self.mode, Mode::CancelRead) {
            self.cancel()?;
        }
        Ok(facts)
    }
}
impl AuditStore for Fault {
    fn check_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.db.check_receipt(id)
    }
    fn commit_check(&self, plan: &Plan) -> Result<Receipt, Error> {
        let attempt = self.commits.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            match self.mode {
                Mode::CancelCommit => self.cancel()?,
                Mode::LostBefore => {
                    return Err(Error::OutcomeUnknown(
                        "injected lost commit response before durable commit".into(),
                    ));
                }
                Mode::LostAfter => {
                    self.db.commit_check(plan)?;
                    return Err(Error::OutcomeUnknown(
                        "injected lost response after real commit".into(),
                    ));
                }
                Mode::ChangeTarget | Mode::ConflictTwice => {
                    let target = self
                        .db
                        .tasks
                        .read_task(&plan.receipt().target_task_id)?
                        .unwrap();
                    self.db.tasks.transition(
                        &target.task.task_id,
                        target.task.state_revision,
                        "range-run",
                        Change::RequestCancel,
                    )?;
                }
                _ => {}
            }
        } else if matches!(self.mode, Mode::ConflictTwice) && attempt == 1 {
            let target = self
                .db
                .tasks
                .read_task(&plan.receipt().target_task_id)?
                .unwrap();
            self.db.tasks.transition(
                &target.task.task_id,
                target.task.state_revision,
                "range-run",
                Change::RequireVerification {
                    item_id: target.items[0].spec.item_id.clone(),
                    reason: "target changed again during check".into(),
                },
            )?;
        }
        self.db.commit_check(plan)
    }
}
