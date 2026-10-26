//! Application approval for an explicit daily range, separate from SQL conditions.
use super::{DateRange, Decision, Evidence, Reason};
use crate::{
    market_data::MarketDataBatch,
    market_storage::{
        Error, MarketRepository, Publication, PublishIntent, PublishPlan, Publisher, Receipt,
        Scope,
        compat::{self, CheckedRecord},
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
    pub(crate) audit_payloads: Vec<crate::sqlite_market::envelope::Payload>,
}
impl RangePlan {
    pub fn is_no_change(&self) -> bool {
        self.no_change
    }
    pub fn command(&self) -> &Publication {
        self.publication.publication()
    }
}
/// Execution state of one accepted range item (XP-025 H4).
pub struct RangeItemState {
    pub binding: Option<crate::task_control::ItemBinding>,
    pub has_receipt: bool,
}
/// One base read request: version id and the bar window the diff needs.
pub struct BaseWant {
    pub base_version_id: String,
    pub start: chrono::NaiveDate,
    pub end: chrono::NaiveDate,
}
/// Base facts and windowed bars for one `BaseWant`.
pub struct BaseRead {
    pub facts: crate::market_update::UpdateBaseFacts,
    pub bars: Vec<crate::market_data::Bar>,
}
pub trait RangeRepository: MarketRepository {
    /// Bindings and receipts for `item_ids`; backends that can serve them
    /// from one read transaction override this. The default is the
    /// per-item reads the runner always did. A tx-level `Err` makes the
    /// caller fall back to the per-item path.
    fn range_item_states(
        &self,
        task_id: &str,
        item_ids: &[String],
    ) -> Result<Vec<Result<RangeItemState, Error>>, Error> {
        Ok(item_ids
            .iter()
            .map(|item_id| {
                Ok(RangeItemState {
                    binding: self.task_item_binding(task_id, item_id)?,
                    has_receipt: self.item_receipt(task_id, item_id)?.is_some(),
                })
            })
            .collect())
    }
    /// Base facts and window bars for many items; `Ok(None)` = the backend
    /// cannot serve typed facts/bars for that base (caller falls back).
    fn range_base_reads(
        &self,
        wants: &[BaseWant],
    ) -> Result<Vec<Result<Option<BaseRead>, Error>>, Error> {
        Ok(wants
            .iter()
            .map(|want| {
                let Some(facts) = self.update_base_facts(&want.base_version_id)? else {
                    return Ok(None);
                };
                let Some(bars) =
                    self.base_bars_in_range(&want.base_version_id, want.start, want.end)?
                else {
                    return Ok(None);
                };
                Ok(Some(BaseRead { facts, bars }))
            })
            .collect())
    }
    fn commit_range(&self, plan: &RangePlan) -> Result<Receipt, Error>;
    /// Ops-first daily commit for backends with parent-delta staging: the
    /// candidate is staged as a delta on `base_version_id` and the staged
    /// leaf must equal `expected` bar-for-bar (the content binding that
    /// replaces the envelope `combined_integrity_sha256` digest check).
    /// `Ok(None)` means the backend lacks an ops path; the caller falls
    /// back to `commit_range`.
    #[allow(clippy::too_many_arguments)]
    fn commit_range_ops(
        &self,
        _command: &Publication,
        _scope: &Scope,
        _base_version_id: &str,
        _upserts: &[crate::market_data::Bar],
        _removes: &[chrono::NaiveDate],
        _expected: &[crate::market_data::Bar],
        _raw: Option<&crate::market_data::RawBatch>,
    ) -> Result<Option<Receipt>, Error> {
        Ok(None)
    }
    /// Typed base facts without bar materialization (#259 S5G). `None`
    /// means the backend cannot serve metadata-only facts; the caller
    /// falls back to `fixed()`.
    fn update_base_facts(
        &self,
        _base_version_id: &str,
    ) -> Result<Option<crate::market_update::UpdateBaseFacts>, Error> {
        Ok(None)
    }
    /// Windowed base bars for the ops-diff path: leaf bars restricted to
    /// `[start, end]` without full-history materialization. `None` means
    /// unsupported.
    fn base_bars_in_range(
        &self,
        _base_version_id: &str,
        _start: chrono::NaiveDate,
        _end: chrono::NaiveDate,
    ) -> Result<Option<Vec<crate::market_data::Bar>>, Error> {
        Ok(None)
    }
    /// Typed ops commit (#259 S5G): stages the delta and verifies the
    /// staged ops by read-back comparison — O(ops), no expected leaf
    /// bars required. `metadata` carries the checked cross-generation
    /// version/source-binding update for append commits that need it.
    /// `continuity_evidence` is the probe's proof record, stored verbatim
    /// alongside the commit. `Ok(None)` means unsupported.
    #[allow(clippy::too_many_arguments)]
    fn commit_range_ops_typed(
        &self,
        _command: &Publication,
        _scope: &Scope,
        _base_version_id: &str,
        _upserts: &[crate::market_data::Bar],
        _removes: &[chrono::NaiveDate],
        _metadata: Option<&crate::market_update::TypedVersionMetadata>,
        _continuity_evidence: Option<&serde_json::Value>,
        _raw: Option<&crate::market_data::RawBatch>,
    ) -> Result<Option<Receipt>, Error> {
        Ok(None)
    }
    /// No-change publication for the typed update path (#259 S5G): the
    /// continuity probe proved the source bytes identical to the base's
    /// pinned inputs, so the result reuses the base version and the head
    /// stays put. `Ok(None)` means unsupported.
    fn commit_range_no_change_typed(
        &self,
        _command: &Publication,
        _base_version_id: &str,
        _range: &crate::market_update::DateRange,
        _continuity_evidence: Option<&serde_json::Value>,
        _raw: Option<&crate::market_data::RawBatch>,
    ) -> Result<Option<Receipt>, Error> {
        Ok(None)
    }
    /// Task-level batched update commit (#259 S5G-6): one write
    /// transaction covers every prepared item — claim, per-item start,
    /// staging, publication commit and item finish. Content-level item
    /// failures are recorded as item outcomes inside the batch;
    /// structural errors roll the whole transaction back and the caller
    /// re-runs each item individually. `Ok(None)` means the backend lacks
    /// a typed update path; the caller keeps the per-item loop.
    fn commit_update_batch(
        &self,
        _task_id: &str,
        _items: &[crate::market_update::UpdateBatchItem],
    ) -> Result<Option<Vec<Result<Receipt, Error>>>, Error> {
        Ok(None)
    }
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
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_range(
        &self,
        command: Publication,
        batch: &MarketDataBatch,
        range: &DateRange,
        evidence: &Evidence,
        combined: Option<&super::runner::CombinedCandidate>,
        raw: Option<&crate::market_data::RawBatch>,
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
        self.check_frozen_inputs(&command, evidence, range)?;
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
                    raw: raw.cloned(),
                };
                let mut binding = self
                    .repository
                    .task_item_binding(&plan.command.task_id, &plan.command.item_id)?
                    .ok_or(crate::task_control::Error::TaskNotFound)?;
                plan.finish_task(&mut binding)?;
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
                if whole.evidence.base_integrity_sha256 != prepared.base_integrity_sha256
                    || whole.evidence.segment_sha256 != prepared.candidate_sha256
                    || whole.evidence.complete_coverage_ref.trim().is_empty()
                    || compat::legacy_digest(&whole.record).ok().as_deref()
                        != Some(whole.evidence.combined_integrity_sha256.as_str())
                    || !self
                        .repository
                        .staged_matches(&command.version_id, &whole.record)?
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
                let rewrite = matches!(
                    evidence.continuity.as_ref().map(|c| c.mode.as_str()),
                    Some("full")
                );
                if rewrite {
                    // Corporate-action rewrite (#290): the approved merge is
                    // meaningless under a history rewrite — the staged record
                    // must be the full new materialization instead. Bind it
                    // fail-closed: the coverage claim is complete, every
                    // acquired segment bar appears verbatim, and no base
                    // record date silently disappears.
                    use crate::market_data::CoverageStatus;
                    if v.records.is_empty()
                        || !matches!(candidate.batch.coverage.status, CoverageStatus::Complete)
                    {
                        return Ok(Preparation::NeedsVerification(Reason::CoverageUnknown));
                    }
                    let staged: std::collections::BTreeMap<_, _> =
                        v.records.iter().map(|b| (b.date, b)).collect();
                    if batch.bars.iter().any(|b| staged.get(&b.date) != Some(&b)) {
                        return Err(Error::Invalid(
                            "rewrite candidate omits acquired segment bars".into(),
                        ));
                    }
                    if base
                        .version()
                        .records
                        .iter()
                        .any(|b| !staged.contains_key(&b.date))
                    {
                        return Ok(Preparation::NeedsVerification(Reason::CoverageUnknown));
                    }
                } else if v.records != prepared.records {
                    return Err(Error::Invalid(
                        "combined candidate differs from approved range merge".into(),
                    ));
                }
                if v.symbol != batch.symbol
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
                let mut plan = self.prepare(command)?;
                plan.raw = raw.cloned();
                if plan.record.raw() != candidate.raw() {
                    return Err(Error::Integrity(
                        "combined candidate changed during approval".into(),
                    ));
                }
                (plan, false)
            }
        };
        let (segment, audit_payloads) = if no_change {
            // No-change reuses the base version: its range bars are exactly the
            // base's, so the audit references them instead of storing a second
            // payload copy (#259 P0-B). Shared evidence blocks keep the normal
            // content-addressed refs — they dedup against the base's payloads.
            let mut segment = serde_json::to_value(batch)
                .map_err(|e| Error::Integrity(format!("audit segment serialization: {e}")))?;
            segment["bars"] = json!({"$ref": "base"});
            let mut payloads = Vec::new();
            crate::sqlite_market::envelope::pack_shared(&mut segment, &mut payloads)?;
            (segment, payloads)
        } else {
            crate::sqlite_market::envelope::pack_batch(&json!(batch))?
        };
        let audit = json!({
            "algorithm_version":"local-range-update-v1",
            "command":publication.command,
            "base_integrity_sha256":base.raw()["integrity_sha256"],
            "result_integrity_sha256":publication.record.raw()["integrity_sha256"],
            "segment":segment,"range":range,"evidence":evidence,
            "combined":combined.map(|c| &c.evidence),"no_change":no_change
        });
        Ok(Preparation::Ready(Box::new(RangePlan {
            publication,
            base,
            no_change,
            audit,
            audit_payloads,
        })))
    }
    /// Accepted-inputs binding shared by the envelope and ops-first commits:
    /// for accepted update/backfill tasks the executed base + range must equal
    /// the item's frozen inputs.
    pub(crate) fn check_frozen_inputs(
        &self,
        command: &Publication,
        evidence: &Evidence,
        range: &DateRange,
    ) -> Result<(), Error> {
        let head = self.repository.task_head(&command.task_id)?;
        let range_task = matches!(head.key.operation.as_str(), "update_data" | "backfill_data");
        let item_frozen = self
            .repository
            .task_item_binding(&command.task_id, &command.item_id)?
            .map(|binding| binding.item.spec.frozen_inputs);
        let frozen_matches = frozen_inputs_match(
            &head.key.contract_version,
            &head.key.dataset,
            item_frozen.as_ref(),
            evidence,
            range,
        );
        if range_task && !frozen_matches {
            return Err(Error::Conflict(
                "range publication differs from accepted frozen inputs".into(),
            ));
        }
        Ok(())
    }
}

/// Whether an item's accepted frozen inputs bind the executed base and range
/// for the task's contract. Pure so the contract whitelist is testable
/// without a repository: contracts outside the three known shapes never
/// match.
fn frozen_inputs_match(
    contract_version: &str,
    dataset: &str,
    item_frozen: Option<&serde_json::Value>,
    evidence: &Evidence,
    range: &DateRange,
) -> bool {
    if contract_version == super::tasks::CONTRACT_VERSION && dataset == "market.daily" {
        // Generation mode stamps `source_generation_id` at accept;
        // carry it through — its value is enforced by the worker's
        // bound-session resolution, not by this input check.
        let mut expected = super::tasks::frozen(
            &evidence.base_version_id,
            &evidence.base_integrity_sha256,
            range,
        );
        if let Some(stamped) = item_frozen.and_then(|f| f.get("source_generation_id")) {
            expected["source_generation_id"] = stamped.clone();
        }
        item_frozen == Some(&expected)
    } else if matches!(
        contract_version,
        super::tasks::DEFAULT_CONTRACT_VERSION | crate::data_supply::plan::RUN_CONTRACT_VERSION
    ) && dataset == "market.daily"
    {
        // Plan-run items carry the default frozen shape (see
        // `data_supply::plan::item_spec`), so they bind the same way.
        // The executed range must equal the union of the item's frozen
        // required ranges (refresh + frozen gaps for update, frozen
        // gaps only for backfill) on the frozen base identity.
        item_frozen
            .and_then(|frozen| super::tasks::parse_default_frozen(frozen).ok())
            .is_some_and(|frozen| {
                frozen.base_version_id == evidence.base_version_id
                    && frozen.base_integrity_sha256 == evidence.base_integrity_sha256
                    && frozen.union().ok().as_ref() == Some(range)
            })
    } else {
        false
    }
}

#[cfg(test)]
mod frozen_inputs_tests {
    use super::*;
    use chrono::NaiveDate;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// The real plan-run item of sh.600026 from the market-wide private-stack
    /// run (354 entities were rejected with `PUBLISH_PRECONDITION` because the
    /// plan-run contract was missing from this check's whitelist).
    fn plan_run_item() -> serde_json::Value {
        serde_json::json!({
            "base_integrity_sha256": "1507c411a0563fc1a7f03f8ed9cf521ab21e37673afeed866a8704ceec37ba08",
            "base_version_id": "1507c411a0563fc1a7f03f8ed9cf521ab21e37673afeed866a8704ceec37ba08",
            "proven_gap_ranges": [],
            "refresh_range": {"start_date": "2002-05-23", "end_date": "2026-10-13"},
            "selection_evidence": {"evidence_digest": "8e900d2e83fe07897056efe061c4673f8fac52e1be1b32881debda264680e5f1"},
            "source_generation_id": "52fe6b77e031548d6c92e83fb387580669c10f257b770b3968ed573f04f000ae"
        })
    }

    fn evidence(range: &DateRange) -> Evidence {
        Evidence {
            base_version_id: "1507c411a0563fc1a7f03f8ed9cf521ab21e37673afeed866a8704ceec37ba08"
                .into(),
            base_integrity_sha256:
                "1507c411a0563fc1a7f03f8ed9cf521ab21e37673afeed866a8704ceec37ba08".into(),
            candidate_sha256: "candidate".into(),
            range: range.clone(),
            same_qfq_basis_ref: "basis".into(),
            revision_ref: None,
            continuity: None,
        }
    }

    fn full_range() -> DateRange {
        DateRange {
            start: day(2002, 5, 23),
            end: day(2026, 10, 13),
        }
    }

    #[test]
    fn plan_run_contract_binds_like_the_default_contract() {
        let range = full_range();
        let item = plan_run_item();
        for contract in [
            super::super::tasks::DEFAULT_CONTRACT_VERSION,
            crate::data_supply::plan::RUN_CONTRACT_VERSION,
        ] {
            assert!(
                frozen_inputs_match(
                    contract,
                    "market.daily",
                    Some(&item),
                    &evidence(&range),
                    &range
                ),
                "{contract} must accept an item whose frozen inputs equal the executed base and range"
            );
        }
    }

    #[test]
    fn plan_run_contract_still_fails_closed_on_any_drift() {
        let range = full_range();
        let item = plan_run_item();
        let run = crate::data_supply::plan::RUN_CONTRACT_VERSION;
        // executed range differs from the frozen union
        let narrower = DateRange {
            start: day(2026, 10, 9),
            end: day(2026, 10, 13),
        };
        assert!(!frozen_inputs_match(
            run,
            "market.daily",
            Some(&item),
            &evidence(&narrower),
            &narrower
        ));
        // executed base differs from the frozen base
        let mut other_base = evidence(&range);
        other_base.base_version_id = "f".repeat(64);
        assert!(!frozen_inputs_match(
            run,
            "market.daily",
            Some(&item),
            &other_base,
            &range
        ));
        // no accepted item, malformed item, or a different dataset
        assert!(!frozen_inputs_match(
            run,
            "market.daily",
            None,
            &evidence(&range),
            &range
        ));
        assert!(!frozen_inputs_match(
            run,
            "market.daily",
            Some(&serde_json::json!({"base_version_id": "x"})),
            &evidence(&range),
            &range
        ));
        assert!(!frozen_inputs_match(
            run,
            "market.yearly",
            Some(&item),
            &evidence(&range),
            &range
        ));
        // an unknown contract never matches
        assert!(!frozen_inputs_match(
            "unknown-v9",
            "market.daily",
            Some(&item),
            &evidence(&range),
            &range
        ));
    }
}
