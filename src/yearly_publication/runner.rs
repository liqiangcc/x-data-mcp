//! Receipt-aware execution of accepted yearly tasks; no online source selection.
use super::{Derivation, DerivationEvidence, results::YearlyRepository};
use crate::{
    market_data::{MarketDataPipeline, SourceError},
    market_storage::{
        Error, Publication, PublishIntent, Publisher, Receipt, compat::CheckedRecord,
    },
    task_control::{Change, ItemOutcome, Stage, Status, TaskSnapshot},
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeSet;
/// Synchronous read-only verifier. Test injection never becomes a protocol input.
pub trait YearlyVerifier {
    fn verify(
        &self,
        base: &CheckedRecord,
        years: &BTreeSet<i32>,
        derived_at: DateTime<Utc>,
    ) -> Result<DerivationEvidence, SourceError>;
}
pub struct OfficialYearlyVerifier;
impl YearlyVerifier for OfficialYearlyVerifier {
    fn verify(
        &self,
        base: &CheckedRecord,
        _years: &BTreeSet<i32>,
        _derived_at: DateTime<Utc>,
    ) -> Result<DerivationEvidence, SourceError> {
        crate::official_files::OfficialFilePipeline.derive_yearly(base.version())?;
        Err(SourceError::NotVerified(
            "fixed daily yearly coverage verifier is not implemented".into(),
        ))
    }
}
#[derive(Debug)]
pub enum YearlyExecution {
    Published(Receipt),
    NotPublished(Box<TaskSnapshot>),
}
pub struct YearlyRunner<R> {
    pub repository: R,
    pub run_id: String,
    pub derived_at: DateTime<Utc>,
}
impl<R: YearlyRepository + Clone> YearlyRunner<R> {
    pub fn run_item(
        &self,
        task_id: &str,
        item_id: &str,
        provider: &dyn YearlyVerifier,
    ) -> Result<YearlyExecution, Error> {
        let mut task = self.repository.task(task_id)?;
        if !matches!(
            task.task.key.operation.as_str(),
            "update_data" | "backfill_data"
        ) || task.task.key.dataset != "market.yearly"
            || task.task.key.contract_version != super::tasks::CONTRACT_VERSION
        {
            return Err(Error::Invalid(
                "unsupported yearly execution contract".into(),
            ));
        }
        let item = task
            .items
            .iter()
            .find(|i| i.spec.item_id == item_id)
            .cloned()
            .ok_or_else(|| Error::Invalid("yearly item absent from accepted task".into()))?;
        if let Some(receipt) = self.repository.yearly_item_receipt(task_id, item_id)? {
            return Ok(YearlyExecution::Published(receipt));
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
                    "successful yearly item has no receipt".into(),
                ));
            }
            return Ok(YearlyExecution::NotPublished(Box::new(task)));
        }
        if task.task.status == Status::CancelRequested {
            let next = self.repository.transition_task(
                task_id,
                task.task.state_revision,
                &self.run_id,
                Change::FinishExecution,
            )?;
            return Ok(YearlyExecution::NotPublished(Box::new(next)));
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
        let outcome = self.derive_and_commit(&task, item_id, &item.spec.frozen_inputs, provider);
        match outcome {
            Ok(receipt) => Ok(YearlyExecution::Published(receipt)),
            Err(error) => {
                // A response can be lost after commit. Never overwrite success.
                if let Some(receipt) = self.repository.yearly_item_receipt(task_id, item_id)? {
                    return Ok(YearlyExecution::Published(receipt));
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
                Ok(YearlyExecution::NotPublished(Box::new(next)))
            }
        }
    }
    fn derive_and_commit(
        &self,
        task: &TaskSnapshot,
        item_id: &str,
        frozen: &Value,
        verifier: &dyn YearlyVerifier,
    ) -> Result<Receipt, Error> {
        let base_id = frozen["base_daily_version_id"]
            .as_str()
            .ok_or_else(|| Error::Integrity("frozen base missing".into()))?;
        let base = self.repository.fixed(base_id)?;
        if base.raw()["integrity_sha256"] != frozen["base_integrity_sha256"] {
            return Err(Error::Integrity(
                "fixed base differs from accepted digest".into(),
            ));
        }
        let years: BTreeSet<i32> = serde_json::from_value(frozen["years"].clone())
            .map_err(|e| Error::Integrity(e.to_string()))?;
        let intent: PublishIntent = serde_json::from_value(frozen["intent"].clone())
            .map_err(|e| Error::Integrity(e.to_string()))?;
        let prior = frozen["comparison"]["version_id"].as_str();
        if let Some(id) = prior
            && self.repository.fixed(id)?.raw()["integrity_sha256"]
                != frozen["comparison"]["integrity_sha256"]
        {
            return Err(Error::Integrity(
                "fixed comparison differs from accepted digest".into(),
            ));
        }
        let evidence = verifier
            .verify(&base, &years, self.derived_at)
            .map_err(|e| Error::Base(e.to_string()))?;
        let current = self.repository.task(&task.task.task_id)?;
        if current.task.status != Status::Running
            || current.task.state_revision != task.task.state_revision
        {
            return Err(Error::Conflict(
                "task changed or cancelled during yearly verification".into(),
            ));
        }
        let id = crate::hash::sha256(
            serde_json::json!([task.task.task_id, item_id])
                .to_string()
                .as_bytes(),
        );
        let request = Derivation {
            symbol: base.version().symbol.clone(),
            base_daily_version_id: base_id.into(),
            version_id: format!("yearly-version-{id}"),
            years,
            derived_at: self.derived_at,
        };
        let publisher = Publisher {
            repository: self.repository.clone(),
        };
        let candidate = publisher.derive_yearly(&request, &evidence)?;
        let plan = publisher.prepare_yearly_result(
            Publication {
                publication_id: format!("yearly-result-{id}"),
                version_id: request.version_id,
                task_id: task.task.task_id.clone(),
                item_id: item_id.into(),
                run_id: self.run_id.clone(),
                expected_revision: task.task.state_revision,
                intent,
            },
            &candidate,
            prior,
        )?;
        self.repository.commit_yearly(&plan)
    }
}
