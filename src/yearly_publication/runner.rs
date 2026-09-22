//! Receipt-aware execution of accepted yearly tasks; no online source selection.
use super::{Derivation, DerivationEvidence, results::YearlyRepository};
use crate::{
    market_data::{CoverageEvidence, CoverageStatus, SourceError},
    market_storage::{
        Error, Publication, PublishIntent, Publisher, Receipt, compat::CheckedRecord,
    },
    task_control::{Change, ItemOutcome, Stage, Status, TaskSnapshot},
};
use chrono::{DateTime, Utc};
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
/// Real verifier for the derived-yearly path: gated on an env-installed
/// inspected official-file source, coverage-proven by re-aggregating the
/// fixed daily base for the requested years. Missing years, invalid input
/// and bars beyond as_of all fail closed.
pub struct OfficialYearlyVerifier;
impl YearlyVerifier for OfficialYearlyVerifier {
    fn verify(
        &self,
        base: &CheckedRecord,
        years: &BTreeSet<i32>,
        derived_at: DateTime<Utc>,
    ) -> Result<DerivationEvidence, SourceError> {
        if crate::official_market::TdxMarketSource::from_env().is_none() {
            return Err(SourceError::NotVerified(
                "official-file source not installed".into(),
            ));
        }
        let as_of = derived_at
            .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .date_naive();
        crate::yearly::aggregate(base.version(), Some(years), as_of)
            .map_err(|e| SourceError::NotVerified(e.to_string()))?;
        Ok(DerivationEvidence {
            base_integrity_sha256: base.raw()["integrity_sha256"]
                .as_str()
                .ok_or_else(|| {
                    SourceError::NotVerified("fixed base carries no integrity digest".into())
                })?
                .into(),
            years: years.clone(),
            as_of,
            coverage: CoverageEvidence {
                status: CoverageStatus::Complete,
                reason: format!(
                    "selected years verified by re-aggregating fixed daily {} from the env-installed official-file source as of {as_of}; not a listing-history claim",
                    base.version().version_id
                ),
            },
        })
    }
}
/// Generation-mode verifier: the bound session's presence already proves a
/// verified, usable, pinned generation — the same fail-closed gate the
/// env-installed source provides in legacy mode. A missing or unpinned
/// session keeps the source refusal; there is no mutable-root fallback.
pub struct BoundYearlyVerifier<'a> {
    pub bound: Option<&'a crate::data_supply::generation::BoundSource>,
}
impl YearlyVerifier for BoundYearlyVerifier<'_> {
    fn verify(
        &self,
        base: &CheckedRecord,
        years: &BTreeSet<i32>,
        derived_at: DateTime<Utc>,
    ) -> Result<DerivationEvidence, SourceError> {
        let Some(bound) = self.bound else {
            return Err(SourceError::NotVerified(
                "no bound generation session".into(),
            ));
        };
        let as_of = derived_at
            .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .date_naive();
        crate::yearly::aggregate(base.version(), Some(years), as_of)
            .map_err(|e| SourceError::NotVerified(e.to_string()))?;
        Ok(DerivationEvidence {
            base_integrity_sha256: base.raw()["integrity_sha256"]
                .as_str()
                .ok_or_else(|| {
                    SourceError::NotVerified("fixed base carries no integrity digest".into())
                })?
                .into(),
            years: years.clone(),
            as_of,
            coverage: CoverageEvidence {
                status: CoverageStatus::Complete,
                reason: format!(
                    "selected years verified by re-aggregating fixed daily {} against bound generation {} as of {as_of}; not a listing-history claim",
                    base.version().version_id,
                    bound.generation_id
                ),
            },
        })
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
        let mut binding = match self.repository.task_item_binding(task_id, item_id)? {
            Some(binding) => binding,
            None => {
                self.repository.task_head(task_id)?;
                return Err(Error::Invalid(
                    "yearly item absent from accepted task".into(),
                ));
            }
        };
        if !matches!(
            binding.head.key.operation.as_str(),
            "update_data" | "backfill_data"
        ) || binding.head.key.dataset != "market.yearly"
            || binding.head.key.contract_version != super::tasks::CONTRACT_VERSION
        {
            return Err(Error::Invalid(
                "unsupported yearly execution contract".into(),
            ));
        }
        if let Some(receipt) = self.repository.yearly_item_receipt(task_id, item_id)? {
            return Ok(YearlyExecution::Published(receipt));
        }
        if binding.head.visible_status(&self.run_id) == Status::NeedsVerification {
            return Err(Error::Task(crate::task_control::Error::NeedsVerification(
                "old run or unresolved execution".into(),
            )));
        }
        if binding.item.stage == Stage::Running {
            return Err(Error::OutcomeUnknown(
                "running item has no verified result; inspect before any retry".into(),
            ));
        }
        if binding.head.status.terminal() || binding.item.stage == Stage::Finished {
            if binding.item.outcome.as_ref().is_some_and(|o| o.success) {
                return Err(Error::Integrity(
                    "successful yearly item has no receipt".into(),
                ));
            }
            return self.not_published(task_id);
        }
        if binding.head.status == Status::CancelRequested {
            self.repository.transition_task_head(
                &mut binding.head,
                None,
                &self.run_id,
                Change::FinishExecution,
            )?;
            return self.not_published(task_id);
        }
        if binding.head.status == Status::Pending {
            self.repository.transition_task_head(
                &mut binding.head,
                None,
                &self.run_id,
                Change::Claim,
            )?;
        }
        self.repository.transition_task_head(
            &mut binding.head,
            Some(&mut binding.item),
            &self.run_id,
            Change::StartItem(item_id.into()),
        )?;
        let outcome = self.derive_and_commit(&mut binding, item_id, provider);
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
                    let mut binding = self
                        .repository
                        .task_item_binding(task_id, item_id)?
                        .ok_or(Error::Integrity("bound task item missing".into()))?;
                    if let Err(recording) = self.repository.transition_task_head(
                        &mut binding.head, Some(&mut binding.item), &self.run_id,
                        Change::RequireVerification {
                            item_id: item_id.into(),
                            reason: "item execution result not confirmed after acquisition/derivation/publication; committed receipt not yet established".into(),
                        },
                    ) {
                        // A concurrent commit may have won before the marker.
                        // Consult its fact once; never overwrite that success.
                        if let Some(receipt) = self.repository.yearly_item_receipt(task_id, item_id)? {
                            return Ok(YearlyExecution::Published(receipt));
                        }
                        return Err(recording);
                    }
                    return Err(error);
                }
                let mut binding = self
                    .repository
                    .task_item_binding(task_id, item_id)?
                    .ok_or(Error::Integrity("bound task item missing".into()))?;
                let cancelled_without_publication = matches!(error, Error::Conflict(_))
                    && binding.head.status == Status::CancelRequested;
                self.repository.transition_task_head(
                    &mut binding.head,
                    Some(&mut binding.item),
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
                            evidence: None,
                        },
                    ),
                )?;
                if binding.head.counts.finished == binding.head.counts.total
                    || binding.head.status == Status::CancelRequested
                {
                    self.repository.transition_task_head(
                        &mut binding.head,
                        None,
                        &self.run_id,
                        if cancelled_without_publication {
                            Change::FinishCancellation
                        } else {
                            Change::FinishExecution
                        },
                    )?;
                }
                self.not_published(task_id)
            }
        }
    }
    fn not_published(&self, task_id: &str) -> Result<YearlyExecution, Error> {
        Ok(YearlyExecution::NotPublished(Box::new(
            self.repository.task(task_id)?,
        )))
    }
    fn derive_and_commit(
        &self,
        binding: &mut crate::task_control::ItemBinding,
        item_id: &str,
        verifier: &dyn YearlyVerifier,
    ) -> Result<Receipt, Error> {
        let frozen = &binding.item.spec.frozen_inputs;
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
        let current = self.repository.task_head(&binding.head.task_id)?;
        if current.status != Status::Running
            || current.state_revision != binding.head.state_revision
        {
            return Err(Error::Conflict(
                "task changed or cancelled during yearly verification".into(),
            ));
        }
        let id = crate::hash::sha256(
            serde_json::json!([binding.head.task_id, item_id])
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
                task_id: binding.head.task_id.clone(),
                item_id: item_id.into(),
                run_id: self.run_id.clone(),
                expected_revision: binding.head.state_revision,
                intent,
            },
            &candidate,
            prior,
        )?;
        self.repository.commit_yearly(&plan)
    }
}
