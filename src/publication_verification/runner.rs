//! Bounded execution of an accepted read-only publication inspection.
use super::{
    EvidenceRepository,
    admission::command_key,
    decision::{AuditStore, Receipt, prepare},
};
use crate::{
    market_storage::Error,
    task_control::{Change, ItemOutcome, Stage, Status, TaskRepository, TaskSnapshot},
};
#[derive(Debug)]
pub enum Outcome {
    Completed(Receipt),
    Cancelled(Box<TaskSnapshot>),
}
pub struct Runner<T, E> {
    pub tasks: T,
    pub evidence: E,
    pub run_id: String,
    pub actor: String,
}
impl<T: TaskRepository, E: EvidenceRepository + AuditStore> Runner<T, E> {
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error> {
        self.tasks
            .read_task(id)?
            .ok_or(Error::Task(crate::task_control::Error::TaskNotFound))
    }
    fn cancel(&self, mut check: TaskSnapshot) -> Result<Outcome, Error> {
        if check.visible_status(&self.run_id) != Status::CancelRequested {
            return Err(Error::Conflict("check cancellation state changed".into()));
        }
        // Called only before inspection or after synchronous inspection returned.
        if check.items.iter().any(|i| i.stage == Stage::Running) {
            check = self.tasks.transition(
                &check.task.task_id,
                check.task.state_revision,
                &self.run_id,
                Change::FinishItem(
                    "publication-results".into(),
                    ItemOutcome {
                        success: false,
                        error: Some("publication inspection cancelled; target unchanged".into()),
                    },
                ),
            )?;
        }
        Ok(Outcome::Cancelled(Box::new(self.tasks.transition(
            &check.task.task_id,
            check.task.state_revision,
            &self.run_id,
            Change::FinishCancellation,
        )?)))
    }
    fn preserve_unresolved(&self, id: &str) -> Result<(), Error> {
        let check = self.task(id)?;
        if matches!(
            check.visible_status(&self.run_id),
            Status::Running | Status::CancelRequested
        ) {
            self.tasks.transition(id, check.task.state_revision, &self.run_id,
                Change::RequireVerification {
                    item_id: "publication-results".into(),
                    reason: "publication inspection ended without a reliable committed check receipt; original task unchanged".into(),
                })?;
        }
        Ok(())
    }
    fn facts(&self, check: &str, target: &str) -> Result<super::Facts, Error> {
        match self.evidence.publication_facts(target) {
            Ok(facts) => Ok(facts),
            Err(error) => {
                self.preserve_unresolved(check)?;
                Err(error)
            }
        }
    }
    pub fn run(&self, id: &str) -> Result<Outcome, Error> {
        if self.run_id.trim().is_empty() || self.actor.trim().is_empty() {
            return Err(Error::Invalid(
                "missing publication inspection runtime identity".into(),
            ));
        }
        if let Some(receipt) = self.evidence.check_receipt(id)? {
            return Ok(Outcome::Completed(receipt));
        }
        let mut check = self.task(id)?;
        if check.task.key != command_key() || check.items.len() != 1 {
            return Err(Error::Task(crate::task_control::Error::UnsupportedContract));
        }
        if check.task.status == Status::Cancelled {
            return Ok(Outcome::Cancelled(Box::new(check)));
        }
        if check.visible_status(&self.run_id) == Status::NeedsVerification
            || check.items[0].stage == Stage::Running
        {
            return Err(Error::OutcomeUnknown(
                "old or running check has no reliable receipt; no automatic re-execution".into(),
            ));
        }
        if check.task.status == Status::CancelRequested {
            return self.cancel(check);
        }
        if check.task.status != Status::Pending {
            return Err(Error::OutcomeUnknown(
                "check has no reliable final receipt".into(),
            ));
        }
        let target = check.items[0].spec.frozen_inputs["target_task_id"]
            .as_str()
            .ok_or_else(|| Error::Integrity("publication check target missing".into()))?
            .to_owned();
        check =
            self.tasks
                .transition(id, check.task.state_revision, &self.run_id, Change::Claim)?;
        check = self.tasks.transition(
            id,
            check.task.state_revision,
            &self.run_id,
            Change::StartItem("publication-results".into()),
        )?;
        let facts = self.facts(id, &target)?;
        let plan = prepare(check, facts, &self.run_id, &self.actor, chrono::Utc::now())?;
        let current = self.task(id)?;
        if current.task.status == Status::CancelRequested {
            return self.cancel(current);
        }
        match self.evidence.commit_check(&plan) {
            Ok(receipt) => Ok(Outcome::Completed(receipt)),
            Err(error) => {
                if let Some(receipt) = self.evidence.check_receipt(id)? {
                    return Ok(Outcome::Completed(receipt));
                }
                if !matches!(error, Error::Conflict(_)) {
                    self.preserve_unresolved(id)?;
                    return Err(error);
                }
                let current = self.task(id)?;
                if current.task.status == Status::CancelRequested {
                    return self.cancel(current);
                }
                if current != plan.audit.verification_before {
                    return Err(error);
                }
                // One fresh verified read, never the stale proof or a loop.
                let facts = self.facts(id, &target)?;
                let fresh = prepare(
                    current,
                    facts,
                    &self.run_id,
                    &self.actor,
                    chrono::Utc::now(),
                )?;
                match self.evidence.commit_check(&fresh) {
                    Ok(receipt) => Ok(Outcome::Completed(receipt)),
                    Err(error) => match self.evidence.check_receipt(id)? {
                        Some(receipt) => Ok(Outcome::Completed(receipt)),
                        None => {
                            self.preserve_unresolved(id)?;
                            Err(error)
                        }
                    },
                }
            }
        }
    }
}
