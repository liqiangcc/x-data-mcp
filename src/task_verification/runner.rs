//! Bounded execution of an already accepted check, never a task resumer.
use super::{
    CONTRACT_VERSION, DATASET,
    decision::{Receipt, StopInspector, StopRepository, prepare},
};
use crate::task_control::{
    Change, Error, ItemOutcome, Stage, Status, TaskRepository, TaskSnapshot,
};
use chrono::Utc;

#[derive(Debug)]
pub enum RunOutcome {
    Completed(Receipt),
    Cancelled(Box<TaskSnapshot>),
}
pub struct StopRunner<R> {
    pub repository: R,
    pub run_id: String,
    pub actor: String,
}
impl<R: TaskRepository + StopRepository> StopRunner<R> {
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error> {
        self.repository.read_task(id)?.ok_or(Error::TaskNotFound)
    }
    fn cancel_stopped_check(&self, mut s: TaskSnapshot) -> Result<RunOutcome, Error> {
        // Only called before any inspection or after our synchronous read-only
        // inspector returned. Never used to acknowledge an inherited writer.
        if s.visible_status(&self.run_id) != Status::CancelRequested {
            return Err(Error::RevisionConflict);
        }
        if s.items.iter().any(|i| i.stage == Stage::Running) {
            s=self.repository.transition(&s.task.task_id,s.task.state_revision,&self.run_id,Change::FinishItem("stopped-writes".into(),ItemOutcome{success:false,error:Some("verification cancelled after inspector returned; original task unchanged".into())}))?;
        }
        let done = self.repository.transition(
            &s.task.task_id,
            s.task.state_revision,
            &self.run_id,
            Change::FinishCancellation,
        )?;
        Ok(RunOutcome::Cancelled(Box::new(done)))
    }
    pub fn run(&self, id: &str, inspector: &dyn StopInspector) -> Result<RunOutcome, Error> {
        if self.run_id.trim().is_empty() || self.actor.trim().is_empty() {
            return Err(Error::InvalidCommand(
                "missing verification runtime identity".into(),
            ));
        }
        if let Some(receipt) = self.repository.stop_receipt(id)? {
            return Ok(RunOutcome::Completed(receipt));
        }
        let mut s = self.task(id)?;
        if s.task.key.dataset != DATASET
            || s.task.key.contract_version != CONTRACT_VERSION
            || s.task.key.operation != "verify_stopped_writes"
            || s.items.len() != 1
        {
            return Err(Error::UnsupportedContract);
        }
        if s.task.status == Status::Cancelled {
            return Ok(RunOutcome::Cancelled(Box::new(s)));
        }
        if s.visible_status(&self.run_id) == Status::NeedsVerification
            || s.items[0].stage == Stage::Running
        {
            return Err(Error::NeedsVerification("old or already running verification has no reliable receipt; no automatic execution".into()));
        }
        if s.task.status == Status::CancelRequested {
            return self.cancel_stopped_check(s);
        }
        if s.task.status != Status::Pending {
            return Err(Error::NeedsVerification(
                "verification has no reliable final check receipt".into(),
            ));
        }
        let target_id = s.items[0].spec.frozen_inputs["target_task_id"]
            .as_str()
            .ok_or_else(|| Error::NeedsVerification("verification target binding missing".into()))?
            .to_owned();
        s = self
            .repository
            .transition(id, s.task.state_revision, &self.run_id, Change::Claim)?;
        s = self.repository.transition(
            id,
            s.task.state_revision,
            &self.run_id,
            Change::StartItem("stopped-writes".into()),
        )?;
        let target = self.task(&target_id)?;
        let plan = prepare(s, target, &self.run_id, &self.actor, Utc::now(), inspector)?;
        let current = self.task(id)?;
        if current.task.status == Status::CancelRequested {
            return self.cancel_stopped_check(current);
        }
        match self.repository.commit_stop_check(&plan) {
            Ok(receipt) => Ok(RunOutcome::Completed(receipt)),
            Err(error) => {
                // A lost response is not permission to repeat inspection or
                // release claims. Consult the committed fact first, once.
                if let Some(receipt) = self.repository.stop_receipt(id)? {
                    Ok(RunOutcome::Completed(receipt))
                } else {
                    Err(error)
                }
            }
        }
    }
}
