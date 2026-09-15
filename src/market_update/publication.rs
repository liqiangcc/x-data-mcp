//! Application approval for an explicit daily range, separate from SQL conditions.
use super::{DateRange, Decision, Evidence, Reason};
use crate::{
    market_data::MarketDataBatch,
    market_storage::{
        Error, MarketRepository, Publication, PublishIntent, PublishPlan, Publisher, Receipt,
        Scope, compat::CheckedRecord,
    },
};
use serde::Serialize;
use serde_json::{Value, json};

/// Trusted verifier output, bound to the whole candidate, not just the segment.
/// This is not an MCP input or a claim that a real evidence verifier exists.
#[derive(Debug, Clone, Serialize)]
pub struct CombinedEvidence {
    pub base_integrity_sha256: String,
    pub segment_sha256: String,
    pub combined_integrity_sha256: String,
    pub complete_coverage_ref: String,
}
#[derive(Debug)]
pub enum Preparation {
    NeedsVerification(Reason),
    Ready(Box<RangePlan>),
}
/// Sealed decision. Infrastructure executes its conditions; it cannot decide to
/// replace a candidate with a no-change result or rebase onto another head.
#[derive(Debug, Clone)]
pub struct RangePlan {
    pub(crate) publication: PublishPlan,
    pub(crate) base: CheckedRecord,
    pub(crate) no_change: bool,
    pub(crate) audit: Value,
}
impl RangePlan {
    pub fn is_no_change(&self) -> bool {
        self.no_change
    }
    pub fn command(&self) -> &Publication {
        self.publication.publication()
    }
}
pub trait RangeRepository: MarketRepository {
    fn commit_range(&self, plan: &RangePlan) -> Result<Receipt, Error>;
    fn range_receipt(&self, id: &str) -> Result<Option<Receipt>, Error>;
    fn item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error>;
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<crate::task_control::TaskSnapshot, Error>;
    /// Append a covering binding for a proven gap the committed version
    /// satisfied in full. Append-only; re-binding the identical pair is an
    /// idempotent no-op.
    fn cover_gap(&self, covering: &crate::market_storage::GapCovering) -> Result<bool, Error>;
    /// Audit read of covering bindings for one gap.
    fn gap_coverings(&self, gap_id: &str)
    -> Result<Vec<crate::market_storage::GapCovering>, Error>;
}
impl<R: MarketRepository> Publisher<R> {
    pub fn prepare_range(
        &self,
        command: Publication,
        batch: &MarketDataBatch,
        range: &DateRange,
        evidence: &Evidence,
        combined: Option<&CombinedEvidence>,
    ) -> Result<Preparation, Error> {
        if command.publication_id.trim().is_empty()
            || command.intent
                != (PublishIntent::Daily {
                    expected_head: Some(evidence.base_version_id.clone()),
                })
        {
            return Err(Error::Invalid(
                "range update must bind its expected daily head".into(),
            ));
        }
        let base = self.repository.fixed(&evidence.base_version_id)?;
        let task = self.repository.task(&command.task_id)?;
        let range_task = matches!(
            task.task.key.operation.as_str(),
            "update_data" | "backfill_data"
        );
        let frozen_matches = if task.task.key.contract_version == super::tasks::CONTRACT_VERSION
            && task.task.key.dataset == "market.daily"
        {
            task.items.iter().any(|item| {
                item.spec.item_id == command.item_id
                    && item.spec.frozen_inputs
                        == super::tasks::frozen(
                            &evidence.base_version_id,
                            &evidence.base_integrity_sha256,
                            range,
                        )
            })
        } else if task.task.key.contract_version == super::tasks::DEFAULT_CONTRACT_VERSION
            && task.task.key.dataset == "market.daily"
        {
            // The executed range must equal the union of the item's frozen
            // required ranges (refresh + frozen gaps for update, frozen
            // gaps only for backfill) on the frozen base identity.
            task.items.iter().any(|item| {
                item.spec.item_id == command.item_id
                    && super::tasks::parse_default_frozen(&item.spec.frozen_inputs)
                        .ok()
                        .is_some_and(|frozen| {
                            frozen.base_version_id == evidence.base_version_id
                                && frozen.base_integrity_sha256 == evidence.base_integrity_sha256
                                && frozen.union().ok().as_ref() == Some(range)
                        })
            })
        } else {
            false
        };
        if range_task && !frozen_matches {
            return Err(Error::Conflict(
                "range publication differs from accepted frozen inputs".into(),
            ));
        }
        let decision = super::prepare(&base, batch, range, Some(evidence))?;
        let (publication, no_change) = match decision {
            Decision::NeedsVerification(reason) => {
                return Ok(Preparation::NeedsVerification(reason));
            }
            Decision::NoChange { version_id } => {
                if command.version_id != version_id || combined.is_some() {
                    return Err(Error::Invalid(
                        "no-change must reuse the fixed version".into(),
                    ));
                }
                let plan = PublishPlan {
                    command,
                    record: base.clone(),
                    required_heads: vec![(Scope::of(base.version()), Some(version_id))],
                    promote: None,
                    base_digest: None,
                };
                plan.finish_task(self.repository.task(&plan.command.task_id)?)?;
                (plan, true)
            }
            Decision::Candidate(prepared) => {
                if command.version_id == base.version().version_id {
                    return Err(Error::Invalid(
                        "changed data needs a new immutable version".into(),
                    ));
                }
                let candidate = self.repository.candidate(&command.version_id)?;
                let Some(whole) = combined else {
                    return Ok(Preparation::NeedsVerification(Reason::CoverageUnknown));
                };
                if whole.base_integrity_sha256 != prepared.base_integrity_sha256
                    || whole.segment_sha256 != prepared.candidate_sha256
                    || Some(whole.combined_integrity_sha256.as_str())
                        != candidate.raw()["integrity_sha256"].as_str()
                    || whole.complete_coverage_ref.trim().is_empty()
                {
                    return Ok(Preparation::NeedsVerification(Reason::CoverageUnknown));
                }
                let v = candidate.version();
                if prepared.metadata_changed
                    && candidate.batch.coverage == base.batch.coverage
                    && candidate.version().unit_currency == base.version().unit_currency
                {
                    return Err(Error::Invalid(
                        "combined candidate discards changed metadata".into(),
                    ));
                }
                if v.records != prepared.records
                    || v.symbol != batch.symbol
                    || v.period != batch.period
                    || v.adjustment != batch.adjustment
                    || v.source != batch.provenance.source
                    || v.adjustment_evidence != batch.adjustment_evidence
                {
                    return Err(Error::Invalid(
                        "combined candidate differs from approved range merge".into(),
                    ));
                }
                // A full candidate may enrich units but may not lose/change any
                // confirmed unit from either the preserved data or the segment.
                for units in [&base.version().unit_currency, &batch.unit_currency] {
                    let target = &v.unit_currency;
                    for (old, new) in [
                        (&units.price_currency, &target.price_currency),
                        (&units.price_unit, &target.price_unit),
                        (&units.volume_unit, &target.volume_unit),
                        (&units.amount_currency, &target.amount_currency),
                        (&units.amount_unit, &target.amount_unit),
                    ] {
                        if old.is_confirmed() && (!new.is_confirmed() || old.value != new.value) {
                            return Ok(Preparation::NeedsVerification(Reason::UnitsIncompatible));
                        }
                    }
                }
                let plan = self.prepare(command)?;
                if plan.record.raw() != candidate.raw() {
                    return Err(Error::Integrity(
                        "combined candidate changed during approval".into(),
                    ));
                }
                (plan, false)
            }
        };
        let audit = json!({
            "algorithm_version":"local-range-update-v1",
            "command":publication.command,
            "base_integrity_sha256":base.raw()["integrity_sha256"],
            "result_integrity_sha256":publication.record.raw()["integrity_sha256"],
            "segment":batch,"range":range,"evidence":evidence,
            "combined":combined,"no_change":no_change
        });
        Ok(Preparation::Ready(Box::new(RangePlan {
            publication,
            base,
            no_change,
            audit,
        })))
    }
}
