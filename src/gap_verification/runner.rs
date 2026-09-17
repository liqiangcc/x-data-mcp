//! Bounded per-item execution for accepted `verify_data_gap` tasks: the
//! frozen target/subject go to the installed verifier through `decide_gap`;
//! the decision id derives deterministically from task/item identity.
//! A committed decision never triggers update/backfill.
use super::{
    GapDecisionStore, SourceGapVerifier, command_key, contract::parse_gap_frozen, decide_gap,
};
use crate::{
    market_storage::Error,
    task_control::{Change, ItemOutcome, Stage, Status, TaskSnapshot},
};
use chrono::{DateTime, Utc};
/// Task-state port for the gap runner. `SqliteMarket` implements it by
/// delegating to the shared task/market traits.
pub trait GapTaskStore {
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error>;
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error>;
}
impl GapTaskStore for crate::sqlite_market::SqliteMarket {
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error> {
        crate::market_storage::MarketRepository::task(self, id)
    }
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error> {
        crate::market_update::publication::RangeRepository::transition_task(
            self, task, revision, run, change,
        )
    }
}
#[derive(Debug)]
pub enum GapRunOutcome {
    /// Decision row committed (or replayed) and the item closed successfully.
    Decided(Box<TaskSnapshot>),
    /// Item closed without a decision (verifier refusal or cancellation).
    NotDecided(Box<TaskSnapshot>),
}
/// Decision identity derives deterministically from task/item identity:
/// stable across replay and runner restart, never caller- or random-derived.
pub fn gap_decision_id(task_id: &str, item_id: &str) -> String {
    format!("{task_id}:{item_id}")
}
pub struct GapVerifyRunner<R> {
    pub repository: R,
    pub run_id: String,
}
impl<R: GapDecisionStore + GapTaskStore> GapVerifyRunner<R> {
    pub fn run(
        &self,
        task_id: &str,
        verifier: &dyn SourceGapVerifier,
        now: DateTime<Utc>,
    ) -> Result<GapRunOutcome, Error> {
        let mut task = self.repository.task(task_id)?;
        if task.task.key != command_key() {
            return Err(Error::Invalid(
                "unsupported gap verification contract".into(),
            ));
        }
        if task.items.len() != 1 {
            return Err(Error::Invalid(
                "gap verification task must carry exactly one item".into(),
            ));
        }
        let item = task.items[0].clone();
        let item_id = item.spec.item_id.clone();
        let decision_id = gap_decision_id(task_id, &item_id);
        if self.repository.gap_decision(&decision_id)?.is_some() {
            // The decision fact is durable; a lost close-out never re-runs
            // verification. An unfinished item means the commit response was
            // lost — inspect, never overwrite.
            if item.stage == Stage::Finished {
                return Ok(GapRunOutcome::Decided(Box::new(task)));
            }
            return Err(Error::OutcomeUnknown(
                "committed gap decision has unconfirmed item close-out".into(),
            ));
        }
        if task.visible_status(&self.run_id) == Status::NeedsVerification {
            return Err(Error::Task(crate::task_control::Error::NeedsVerification(
                "old run or unresolved execution".into(),
            )));
        }
        if item.stage == Stage::Running {
            return Err(Error::OutcomeUnknown(
                "running item has no verified result; inspect before any retry".into(),
            ));
        }
        if task.task.status.terminal() || item.stage == Stage::Finished {
            if item.outcome.as_ref().is_some_and(|o| o.success) {
                return Err(Error::Integrity(
                    "successful gap item has no committed decision".into(),
                ));
            }
            return Ok(GapRunOutcome::NotDecided(Box::new(task)));
        }
        if task.task.status == Status::CancelRequested {
            let next = self.repository.transition_task(
                task_id,
                task.task.state_revision,
                &self.run_id,
                Change::FinishExecution,
            )?;
            return Ok(GapRunOutcome::NotDecided(Box::new(next)));
        }
        if task.task.status == Status::Pending {
            task = self.repository.transition_task(
                task_id,
                task.task.state_revision,
                &self.run_id,
                Change::Claim,
            )?;
        }
        self.repository.transition_task(
            task_id,
            task.task.state_revision,
            &self.run_id,
            Change::StartItem(item_id.clone()),
        )?;
        let outcome = self.decide(&item.spec.frozen_inputs, &decision_id, verifier, now);
        match outcome {
            Ok(()) => self.close(task_id, &item_id, true, None, false),
            Err(error) => {
                // A response can be lost after commit. Never overwrite success.
                if self.repository.gap_decision(&decision_id)?.is_some() {
                    return self.close(task_id, &item_id, true, None, false);
                }
                if matches!(
                    error,
                    Error::OutcomeUnknown(_)
                        | Error::Storage(_)
                        | Error::Task(
                            crate::task_control::Error::StorageUnavailable(_)
                                | crate::task_control::Error::OutcomeUnknown(_)
                        )
                ) {
                    let current = self.repository.task(task_id)?;
                    if let Err(recording) = self.repository.transition_task(
                        task_id,
                        current.task.state_revision,
                        &self.run_id,
                        Change::RequireVerification {
                            item_id: item_id.clone(),
                            reason: "gap decision result not confirmed after verifier decision; committed decision not yet established".into(),
                        },
                    ) {
                        if self.repository.gap_decision(&decision_id)?.is_some() {
                            return self.close(task_id, &item_id, true, None, false);
                        }
                        return Err(recording);
                    }
                    return Err(error);
                }
                let current = self.repository.task(task_id)?;
                let cancelled_without_decision = matches!(error, Error::Conflict(_))
                    && current.task.status == Status::CancelRequested;
                self.close(
                    task_id,
                    &item_id,
                    false,
                    Some(error),
                    cancelled_without_decision,
                )
            }
        }
    }
    fn decide(
        &self,
        frozen: &serde_json::Value,
        decision_id: &str,
        verifier: &dyn SourceGapVerifier,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        let (target, subject) = parse_gap_frozen(frozen)?.parts()?;
        decide_gap(
            &self.repository,
            verifier,
            &target,
            &subject,
            decision_id,
            now,
        )?;
        Ok(())
    }
    /// Close the item and, once every item finished (or cancellation is
    /// pending), the task.
    fn close(
        &self,
        task_id: &str,
        item_id: &str,
        success: bool,
        error: Option<Error>,
        cancelled: bool,
    ) -> Result<GapRunOutcome, Error> {
        let current = self.repository.task(task_id)?;
        let next = self.repository.transition_task(
            task_id,
            current.task.state_revision,
            &self.run_id,
            Change::FinishItem(
                item_id.into(),
                ItemOutcome {
                    success,
                    error: error.map(|e| {
                        if cancelled {
                            format!("cancelled before decision: {e}")
                        } else {
                            e.to_string()
                        }
                    }),
                    evidence: None,
                },
            ),
        )?;
        let next = if next.items.iter().all(|i| i.stage == Stage::Finished)
            || next.task.status == Status::CancelRequested
        {
            self.repository.transition_task(
                task_id,
                next.task.state_revision,
                &self.run_id,
                if cancelled {
                    Change::FinishCancellation
                } else {
                    Change::FinishExecution
                },
            )?
        } else {
            next
        };
        Ok(if success {
            GapRunOutcome::Decided(Box::new(next))
        } else {
            GapRunOutcome::NotDecided(Box::new(next))
        })
    }
}
