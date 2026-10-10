//! Receipt-aware execution of accepted yearly tasks; no online source selection.
use super::{Derivation, DerivationEvidence, results::YearlyRepository};
use crate::{
    market_data::{CoverageEvidence, CoverageStatus, SourceError},
    market_storage::{
        Error, Publication, PublishIntent, Publisher, Receipt, compat::CheckedRecord,
    },
    task_control::{Change, ItemOutcome, Stage, Status, TaskSnapshot},
};
use chrono::{DateTime, Datelike, Utc};
use std::collections::BTreeSet;
/// Synchronous read-only verifier. Test injection never becomes a protocol input.
/// The runner re-aggregates the fixed base once per item and hands the result
/// in: a successful aggregate is itself the fail-closed coverage proof
/// (missing years, invalid input and bars beyond as_of abort before this
/// call). The verifier contributes the source gate and binds the proof into
/// evidence.
pub trait YearlyVerifier {
    fn verify(
        &self,
        base: &CheckedRecord,
        years: &BTreeSet<i32>,
        annual: &crate::yearly::AnnualSeries,
        derived_at: DateTime<Utc>,
    ) -> Result<DerivationEvidence, SourceError>;
}
/// Real verifier for the derived-yearly path: gated on an env-installed
/// inspected official-file source, coverage-proven by the runner's
/// re-aggregation of the fixed daily base for the requested years.
pub struct OfficialYearlyVerifier;
impl YearlyVerifier for OfficialYearlyVerifier {
    fn verify(
        &self,
        base: &CheckedRecord,
        years: &BTreeSet<i32>,
        annual: &crate::yearly::AnnualSeries,
        derived_at: DateTime<Utc>,
    ) -> Result<DerivationEvidence, SourceError> {
        if crate::official_market::TdxMarketSource::from_env().is_none() {
            return Err(SourceError::NotVerified(
                "official-file source not installed".into(),
            ));
        }
        bound_evidence(
            base,
            years,
            annual,
            derived_at,
            "env-installed official-file source".into(),
        )
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
        annual: &crate::yearly::AnnualSeries,
        derived_at: DateTime<Utc>,
    ) -> Result<DerivationEvidence, SourceError> {
        let Some(bound) = self.bound else {
            return Err(SourceError::NotVerified(
                "no bound generation session".into(),
            ));
        };
        bound_evidence(
            base,
            years,
            annual,
            derived_at,
            format!("bound generation {}", bound.generation_id),
        )
    }
}
fn bound_evidence(
    base: &CheckedRecord,
    years: &BTreeSet<i32>,
    annual: &crate::yearly::AnnualSeries,
    derived_at: DateTime<Utc>,
    source_label: String,
) -> Result<DerivationEvidence, SourceError> {
    let as_of = derived_at
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive();
    if annual.base_daily_version_id != base.version().version_id
        || annual.as_of != as_of
        || annual.records.len() != years.len()
        || !annual
            .records
            .iter()
            .map(|r| r.bar.date.year())
            .eq(years.iter().copied())
    {
        return Err(SourceError::NotVerified(
            "aggregated annual series does not cover the requested years".into(),
        ));
    }
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
                "selected years verified by re-aggregating fixed daily {} against {source_label} as of {as_of}; not a listing-history claim",
                base.version().version_id
            ),
        },
    })
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
        let _total = crate::stage_trace::span("yearly.exec.item");
        let binding_span = crate::stage_trace::span("yearly.exec.binding");
        let mut binding = match self.repository.task_item_binding(task_id, item_id)? {
            Some(binding) => binding,
            None => {
                self.repository.task_head(task_id)?;
                return Err(Error::Invalid(
                    "yearly item absent from accepted task".into(),
                ));
            }
        };
        drop(binding_span);
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
        let receipt_span = crate::stage_trace::span("yearly.exec.item_receipt");
        let receipt = self.repository.yearly_item_receipt(task_id, item_id)?;
        drop(receipt_span);
        if let Some(receipt) = receipt {
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
        let start_span = crate::stage_trace::span("yearly.exec.start_item");
        self.repository.transition_task_head(
            &mut binding.head,
            Some(&mut binding.item),
            &self.run_id,
            Change::StartItem(item_id.into()),
        )?;
        drop(start_span);
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
        let years: BTreeSet<i32> = serde_json::from_value(frozen["years"].clone())
            .map_err(|e| Error::Integrity(e.to_string()))?;
        let (first_year, last_year) = match (years.first(), years.last()) {
            (Some(first), Some(last)) => (*first, *last),
            _ => return Err(Error::Integrity("frozen years empty".into())),
        };
        let (first_day, last_day) = match (
            chrono::NaiveDate::from_ymd_opt(first_year, 1, 1),
            chrono::NaiveDate::from_ymd_opt(last_year, 12, 31),
        ) {
            (Some(first), Some(last)) => (first, last),
            _ => return Err(Error::Integrity("frozen years out of range".into())),
        };
        // Windowed base read: the verifier and derivation only consume bars
        // inside the selected years. `raw()["integrity_sha256"]` still
        // carries the stored digest of the complete version.
        let window_span = crate::stage_trace::span("yearly.exec.fixed_window");
        let base = self.repository.fixed_window(base_id, first_day, last_day)?;
        crate::stage_trace::note(
            "yearly.exec.fixed_window_bars",
            &[("bars", base.version().records.len() as u64)],
        );
        drop(window_span);
        if base.raw()["integrity_sha256"] != frozen["base_integrity_sha256"] {
            return Err(Error::Integrity(
                "fixed base differs from accepted digest".into(),
            ));
        }
        let intent: PublishIntent = serde_json::from_value(frozen["intent"].clone())
            .map_err(|e| Error::Integrity(e.to_string()))?;
        let prior = frozen["comparison"]["version_id"].as_str();
        if let Some(id) = prior
            && self.repository.fixed_facts(id)?.integrity_sha256
                != frozen["comparison"]["integrity_sha256"]
        {
            return Err(Error::Integrity(
                "fixed comparison differs from accepted digest".into(),
            ));
        }
        // One re-aggregation per item: success is the fail-closed coverage
        // proof the verifier binds into evidence, and the same series feeds
        // the derivation — the computation is deterministic. The memo key
        // binds the base's stored integrity digest so commit-time lineage
        // verification reuses this series instead of re-decoding the base
        // window.
        let as_of = self
            .derived_at
            .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .date_naive();
        let key = crate::yearly::aggregate_key(
            base.raw()["integrity_sha256"]
                .as_str()
                .ok_or_else(|| Error::Integrity("fixed base lacks integrity digest".into()))?,
            &years,
            as_of,
        );
        let aggregate_span = crate::stage_trace::span("yearly.exec.aggregate");
        let annual = crate::yearly::aggregate_memoized(key, || {
            crate::yearly::aggregate(base.version(), Some(&years), as_of)
        })
        .map_err(|e| Error::Base(e.to_string()))?;
        drop(aggregate_span);
        let verify_span = crate::stage_trace::span("yearly.exec.verify");
        let evidence = verifier
            .verify(&base, &years, &annual, self.derived_at)
            .map_err(|e| Error::Base(e.to_string()))?;
        drop(verify_span);
        let head_span = crate::stage_trace::span("yearly.exec.task_head");
        let current = self.repository.task_head(&binding.head.task_id)?;
        drop(head_span);
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
        let derive_span = crate::stage_trace::span("yearly.exec.derive");
        let candidate = publisher.derive_yearly(&request, &evidence, &base, &annual)?;
        drop(derive_span);
        // Pass the binding and base window this runner already loaded — the
        // prepare chain would otherwise re-read them several times per item.
        let prepare_span = crate::stage_trace::span("yearly.exec.prepare");
        let plan = publisher.prepare_yearly_result_prefetched(
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
            Some(&*binding),
            Some(&base),
        )?;
        drop(prepare_span);
        let _commit = crate::stage_trace::span("yearly.exec.commit");
        self.repository.commit_yearly(&plan)
    }
}
