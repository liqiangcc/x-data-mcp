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
        let mut binding = self.binding(task_id, item_id, "range")?;
        let default_contract =
            binding.head.key.contract_version == super::tasks::DEFAULT_CONTRACT_VERSION;
        let republish_contract =
            binding.head.key.contract_version == super::tasks::REPUBLISH_CONTRACT_VERSION;
        if !matches!(
            binding.head.key.operation.as_str(),
            "update_data" | "backfill_data" | "republish_data"
        ) || binding.head.key.dataset != "market.daily"
            || (!default_contract
                && !republish_contract
                && binding.head.key.contract_version != super::tasks::CONTRACT_VERSION)
        {
            return Err(Error::Invalid(
                "unsupported range execution contract".into(),
            ));
        }
        if let Some(receipt) = self.repository.item_receipt(task_id, item_id)? {
            // Covering write-back is idempotent and may lag the commit after
            // a crash; replay re-establishes it from the frozen gap list.
            if default_contract {
                self.ensure_default_coverings(
                    &binding.item.spec.frozen_inputs,
                    &receipt.version_id,
                )?;
            }
            return Ok(ItemExecution::Published(receipt));
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
                    "successful range item has no receipt".into(),
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
        let outcome = if default_contract {
            self.acquire_and_commit_default(&mut binding, item_id, provider)
        } else {
            self.acquire_and_commit(&mut binding, item_id, provider)
        };
        match outcome {
            Ok(receipt) => Ok(ItemExecution::Published(receipt)),
            Err(error) => {
                // A response can be lost after commit. Never overwrite success.
                if let Some(receipt) = self.repository.item_receipt(task_id, item_id)? {
                    if default_contract {
                        self.ensure_default_coverings(
                            &binding.item.spec.frozen_inputs,
                            &receipt.version_id,
                        )?;
                    }
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
                    let mut binding = self.binding(task_id, item_id, "range")?;
                    if let Err(recording) = self.repository.transition_task_head(
                        &mut binding.head, Some(&mut binding.item), &self.run_id,
                        Change::RequireVerification {
                            item_id: item_id.into(),
                            reason: "item execution result not confirmed after acquisition/derivation/publication; committed receipt not yet established".into(),
                        },
                    ) {
                        // A concurrent commit may have won before the marker.
                        // Consult its fact once; never overwrite that success.
                        if let Some(receipt) = self.repository.item_receipt(task_id, item_id)? {
                            return Ok(ItemExecution::Published(receipt));
                        }
                        return Err(recording);
                    }
                    return Err(error);
                }
                let mut binding = self.binding(task_id, item_id, "range")?;
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
    /// O(1) head+item binding for this execution step; a missing item
    /// produces the same invalid-command error the snapshot path did.
    fn binding(
        &self,
        task_id: &str,
        item_id: &str,
        kind: &str,
    ) -> Result<crate::task_control::ItemBinding, Error> {
        match self.repository.task_item_binding(task_id, item_id)? {
            Some(binding) => Ok(binding),
            None => {
                self.repository.task_head(task_id)?;
                Err(Error::Invalid(format!(
                    "{kind} item absent from accepted task"
                )))
            }
        }
    }
    fn not_published(&self, task_id: &str) -> Result<ItemExecution, Error> {
        Ok(ItemExecution::NotPublished(Box::new(
            self.repository.task(task_id)?,
        )))
    }
    fn acquire_and_commit(
        &self,
        binding: &mut crate::task_control::ItemBinding,
        item_id: &str,
        provider: &dyn RangeProvider,
    ) -> Result<Receipt, Error> {
        let frozen = &binding.item.spec.frozen_inputs;
        let base_id = frozen["base_version_id"]
            .as_str()
            .ok_or_else(|| Error::Integrity("frozen base missing".into()))?
            .to_string();
        let base_digest = frozen["base_integrity_sha256"]
            .as_str()
            .ok_or_else(|| Error::Integrity("frozen base digest missing".into()))?
            .to_string();
        let range = DateRange {
            start: serde_json::from_value(frozen["range"]["start"].clone())
                .map_err(|e| Error::Integrity(e.to_string()))?,
            end: serde_json::from_value(frozen["range"]["end"].clone())
                .map_err(|e| Error::Integrity(e.to_string()))?,
        };
        self.commit_acquired_range(binding, item_id, &base_id, &base_digest, &range, provider)
    }
    /// Default-range execution (#147): every required range — refresh plus
    /// each frozen gap range for `update`, frozen gap ranges only for
    /// `backfill` — is satisfied by one fetch/commit over their union,
    /// through the same verified pipeline as the explicit contract. One
    /// commit means `finish_task` completes the item only after the union
    /// (and therefore every required range) is verified.
    fn acquire_and_commit_default(
        &self,
        binding: &mut crate::task_control::ItemBinding,
        item_id: &str,
        provider: &dyn RangeProvider,
    ) -> Result<Receipt, Error> {
        let parsed = super::tasks::parse_default_frozen(&binding.item.spec.frozen_inputs)?;
        // Shape/operation consistency: `update` freezes a refresh range,
        // `backfill` never does; anything else is a frozen-shape violation.
        match binding.head.key.operation.as_str() {
            "update_data" if parsed.refresh_range.is_none() => {
                return Err(Error::Integrity(
                    "default update item missing frozen refresh range".into(),
                ));
            }
            "backfill_data" if parsed.refresh_range.is_some() => {
                return Err(Error::Integrity(
                    "default backfill item carries a refresh range".into(),
                ));
            }
            _ => {}
        }
        let union = parsed.union()?;
        let receipt = self.commit_acquired_range(
            binding,
            item_id,
            &parsed.base_version_id,
            &parsed.base_integrity_sha256,
            &union,
            provider,
        )?;
        self.ensure_default_coverings(&binding.item.spec.frozen_inputs, &receipt.version_id)?;
        Ok(receipt)
    }
    /// Append covering bindings for frozen gaps the committed version
    /// satisfied in full. The exact covering condition: the gap is listed in
    /// the item's frozen `proven_gap_ranges` AND its entire
    /// `[start_date, end_date]` is contained in the committed range whose
    /// evidence the verified pipeline checked end-to-end. Gaps not in the
    /// frozen list never gain a binding, whatever their overlap.
    fn ensure_default_coverings(&self, frozen: &Value, version_id: &str) -> Result<(), Error> {
        let parsed = super::tasks::parse_default_frozen(frozen)?;
        let union = parsed.union()?;
        for gap in &parsed.proven_gap_ranges {
            let range = gap.range.range()?;
            if range.start < union.start || range.end > union.end {
                return Err(Error::Integrity(
                    "frozen gap outside committed verified range".into(),
                ));
            }
            // Replay: the identical binding already exists — cover_gap is
            // append-only, so skip rather than conflict on covered_at.
            if self
                .repository
                .gap_coverings(&gap.gap_id)?
                .iter()
                .any(|c| c.version_id == version_id)
            {
                continue;
            }
            self.repository
                .cover_gap(&crate::market_storage::GapCovering {
                    gap_id: gap.gap_id.clone(),
                    version_id: version_id.into(),
                    covered_at: chrono::Utc::now(),
                })?;
        }
        Ok(())
    }
    /// Verified pipeline shared by both contracts: provider acquisition,
    /// base/derivation checks, combined-coverage staging, publication
    /// verification and the atomic commit/receipt + finish_item.
    fn commit_acquired_range(
        &self,
        binding: &mut crate::task_control::ItemBinding,
        item_id: &str,
        base_id: &str,
        base_digest: &str,
        range: &DateRange,
        provider: &dyn RangeProvider,
    ) -> Result<Receipt, Error> {
        let base = self.repository.fixed(base_id)?;
        if base.raw()["integrity_sha256"] != base_digest {
            return Err(Error::Integrity(
                "fixed base differs from accepted digest".into(),
            ));
        }
        let id = crate::hash::sha256(
            serde_json::json!([binding.head.task_id, item_id])
                .to_string()
                .as_bytes(),
        );
        let candidate_id = format!("range-version-{id}");
        let acquired = provider
            .acquire(&base, range, &candidate_id)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let current = self.repository.task_head(&binding.head.task_id)?;
        if current.status != Status::Running
            || current.state_revision != binding.head.state_revision
        {
            return Err(Error::Conflict(
                "task changed or cancelled during acquisition".into(),
            ));
        }
        let changed = match super::prepare(&base, &acquired.batch, range, Some(&acquired.evidence))?
        {
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
                task_id: binding.head.task_id.clone(),
                item_id: item_id.into(),
                run_id: self.run_id.clone(),
                expected_revision: binding.head.state_revision,
                intent: PublishIntent::Daily {
                    expected_head: Some(base_id.into()),
                },
            },
            &acquired.batch,
            range,
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
