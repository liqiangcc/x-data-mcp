//! Bounded publication-aware application execution. No MCP or source selection.
use super::{
    DateRange, Decision, Evidence,
    publication::{CombinedEvidence, Preparation, RangeRepository},
};
use crate::{
    market_data::{MarketDataBatch, MarketDataPipeline, SourceError},
    market_storage::{
        Error, Publication, PublishIntent, Publisher, Receipt, compat::CheckedRecord,
    },
    task_control::{Change, ItemOutcome, Stage, Status, TaskSnapshot},
};
use serde_json::Value;

pub struct CombinedCandidate {
    pub record: Value,
    pub evidence: CombinedEvidence,
}
pub struct Acquisition {
    pub batch: MarketDataBatch,
    pub evidence: Evidence,
    pub combined: Option<CombinedCandidate>,
}
/// Synchronous and read-only with respect to publication/task storage. Return
/// only after acquisition has stopped. Trusted fixture/verifier port, not MCP.
pub trait RangeProvider {
    fn acquire(
        &self,
        base: &CheckedRecord,
        range: &DateRange,
        candidate_id: &str,
    ) -> Result<Acquisition, SourceError>;
}
pub struct OfficialRangeProvider;
impl RangeProvider for OfficialRangeProvider {
    fn acquire(
        &self,
        base: &CheckedRecord,
        _range: &DateRange,
        _candidate_id: &str,
    ) -> Result<Acquisition, SourceError> {
        crate::official_files::OfficialFilePipeline
            .materialize_daily(&base.version().symbol, base.version().adjustment)?;
        Err(SourceError::NotVerified(
            "range evidence verifier and combined coverage are not implemented".into(),
        ))
    }
}
#[derive(Debug)]
pub enum ItemExecution {
    Published(Receipt),
    NotPublished(Box<TaskSnapshot>),
}
pub struct RangeRunner<R> {
    pub repository: R,
    pub run_id: String,
}
impl<R: RangeRepository + Clone> RangeRunner<R> {
    pub fn run_item(
        &self,
        task_id: &str,
        item_id: &str,
        provider: &dyn RangeProvider,
    ) -> Result<ItemExecution, Error> {
        let mut task = self.repository.task(task_id)?;
        if !matches!(
            task.task.key.operation.as_str(),
            "update_data" | "backfill_data"
        ) || task.task.key.dataset != "market.daily"
            || task.task.key.contract_version != super::tasks::CONTRACT_VERSION
        {
            return Err(Error::Invalid(
                "unsupported range execution contract".into(),
            ));
        }
        let item = task
            .items
            .iter()
            .find(|i| i.spec.item_id == item_id)
            .cloned()
            .ok_or_else(|| Error::Invalid("range item absent from accepted task".into()))?;
        if let Some(receipt) = self.repository.item_receipt(task_id, item_id)? {
            return Ok(ItemExecution::Published(receipt));
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
                    "successful range item has no receipt".into(),
                ));
            }
            return Ok(ItemExecution::NotPublished(Box::new(task)));
        }
        if task.task.status == Status::CancelRequested {
            let next = self.repository.transition_task(
                task_id,
                task.task.state_revision,
                &self.run_id,
                Change::FinishExecution,
            )?;
            return Ok(ItemExecution::NotPublished(Box::new(next)));
        }
        if task.task.status == Status::Pending {
            task = self.repository.transition_task(
                task_id,
                task.task.state_revision,
                &self.run_id,
                Change::Claim,
            )?;
        }
        task = self.repository.transition_task(
            task_id,
            task.task.state_revision,
            &self.run_id,
            Change::StartItem(item_id.into()),
        )?;
        let outcome = self.acquire_and_commit(&task, item_id, &item.spec.frozen_inputs, provider);
        match outcome {
            Ok(receipt) => Ok(ItemExecution::Published(receipt)),
            Err(error) => {
                // A response can be lost after commit. Never overwrite success.
                if let Some(receipt) = self.repository.item_receipt(task_id, item_id)? {
                    return Ok(ItemExecution::Published(receipt));
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
                    return Err(error);
                }
                let current = self.repository.task(task_id)?;
                let cancelled_without_publication = matches!(error, Error::Conflict(_))
                    && current.task.status == Status::CancelRequested;
                let next = self.repository.transition_task(
                    task_id,
                    current.task.state_revision,
                    &self.run_id,
                    Change::FinishItem(
                        item_id.into(),
                        ItemOutcome {
                            success: false,
                            error: Some(if cancelled_without_publication {
                                format!("cancelled before publication: {error}")
                            } else {
                                error.to_string()
                            }),
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
                        if cancelled_without_publication {
                            Change::FinishCancellation
                        } else {
                            Change::FinishExecution
                        },
                    )?
                } else {
                    next
                };
                Ok(ItemExecution::NotPublished(Box::new(next)))
            }
        }
    }
    fn acquire_and_commit(
        &self,
        task: &TaskSnapshot,
        item_id: &str,
        frozen: &Value,
        provider: &dyn RangeProvider,
    ) -> Result<Receipt, Error> {
        let base_id = frozen["base_version_id"]
            .as_str()
            .ok_or_else(|| Error::Integrity("frozen base missing".into()))?;
        let base = self.repository.fixed(base_id)?;
        if base.raw()["integrity_sha256"] != frozen["base_integrity_sha256"] {
            return Err(Error::Integrity(
                "fixed base differs from accepted digest".into(),
            ));
        }
        let range = DateRange {
            start: serde_json::from_value(frozen["range"]["start"].clone())
                .map_err(|e| Error::Integrity(e.to_string()))?,
            end: serde_json::from_value(frozen["range"]["end"].clone())
                .map_err(|e| Error::Integrity(e.to_string()))?,
        };
        let id = crate::hash::sha256(
            serde_json::json!([task.task.task_id, item_id])
                .to_string()
                .as_bytes(),
        );
        let candidate_id = format!("range-version-{id}");
        let acquired = provider
            .acquire(&base, &range, &candidate_id)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let current = self.repository.task(&task.task.task_id)?;
        if current.task.status != Status::Running
            || current.task.state_revision != task.task.state_revision
        {
            return Err(Error::Conflict(
                "task changed or cancelled during acquisition".into(),
            ));
        }
        let changed =
            match super::prepare(&base, &acquired.batch, &range, Some(&acquired.evidence))? {
                Decision::Candidate(_) => true,
                Decision::NoChange { .. } => false,
                Decision::NeedsVerification(reason) => {
                    return Err(Error::Invalid(format!("NEEDS_VERIFICATION: {reason:?}")));
                }
            };
        if changed {
            let whole = acquired.combined.as_ref().ok_or_else(|| {
                Error::Invalid("NEEDS_VERIFICATION: combined coverage absent".into())
            })?;
            if whole.record["version_id"] != candidate_id {
                return Err(Error::Invalid(
                    "unexpected candidate version identity".into(),
                ));
            }
            self.repository.stage(&whole.record)?;
        }
        let plan = (Publisher {
            repository: self.repository.clone(),
        })
        .prepare_range(
            Publication {
                publication_id: format!("range-result-{id}"),
                version_id: if changed {
                    candidate_id
                } else {
                    base_id.into()
                },
                task_id: task.task.task_id.clone(),
                item_id: item_id.into(),
                run_id: self.run_id.clone(),
                expected_revision: task.task.state_revision,
                intent: PublishIntent::Daily {
                    expected_head: Some(base_id.into()),
                },
            },
            &acquired.batch,
            &range,
            &acquired.evidence,
            acquired.combined.as_ref().map(|c| &c.evidence),
        )?;
        match plan {
            Preparation::Ready(plan) => self.repository.commit_range(&plan),
            Preparation::NeedsVerification(reason) => {
                Err(Error::Invalid(format!("NEEDS_VERIFICATION: {reason:?}")))
            }
        }
    }
}
