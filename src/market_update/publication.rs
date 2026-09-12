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
