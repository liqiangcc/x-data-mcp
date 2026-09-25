//! Fixed published daily -> locally derived yearly candidate. No source or MCP wiring.
pub mod results;
pub mod runner;
pub mod tasks;
use std::collections::BTreeSet;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    market_data::{CoverageEvidence, CoverageStatus, Period, YearlyDerivation},
    market_storage::{
        Error, MarketRepository, Publication, PublishIntent, PublishPlan, Publisher, base_error,
        compat::{self, CheckedRecord},
    },
    publish::{EffectiveVersion, validate_and_deduplicate},
    yearly,
};

#[derive(Debug, Clone)]
pub struct Derivation {
    pub symbol: String,
    pub base_daily_version_id: String,
    pub version_id: String,
    pub years: BTreeSet<i32>,
    /// Explicit execution instant; Shanghai date determines unfinished years.
    pub derived_at: DateTime<Utc>,
}

/// Trusted internal verifier output, not a public declaration of completeness.
/// No production verifier exists yet. Complete means the selected input range
/// was obtained, not that every natural year or all listing history is covered.
#[derive(Debug, Clone, Serialize)]
pub struct DerivationEvidence {
    pub base_integrity_sha256: String,
    pub years: BTreeSet<i32>,
    pub as_of: NaiveDate,
    pub coverage: CoverageEvidence,
}

#[derive(Debug, Clone)]
pub struct YearlyCandidate {
    raw: Value,
    checked: CheckedRecord,
    base_daily_version_id: String,
    base_integrity_sha256: String,
}
impl YearlyCandidate {
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}

impl<R: MarketRepository> Publisher<R> {
    /// Read only. Does not stage data, change heads, acquire scopes or accept a task.
    /// `base` is the caller's windowed read of the fixed daily version and
    /// `annual` the deterministic re-aggregation already performed for it —
    /// both are rechecked against the request and evidence here.
    pub fn derive_yearly(
        &self,
        request: &Derivation,
        evidence: &DerivationEvidence,
        base: &CheckedRecord,
        annual: &crate::yearly::AnnualSeries,
    ) -> Result<YearlyCandidate, Error> {
        if request.symbol.trim().is_empty()
            || request.version_id.trim().is_empty()
            || request.base_daily_version_id.trim().is_empty()
            || request.version_id == request.base_daily_version_id
            || request.years.is_empty()
        {
            return Err(Error::Invalid("explicit yearly identity/selection".into()));
        }
        let daily = base.version();
        if daily.version_id != request.base_daily_version_id {
            return Err(Error::Base("fixed daily identity binding".into()));
        }
        if daily.symbol != request.symbol || daily.period != Period::Day {
            return Err(Error::Base("fixed daily identity/period".into()));
        }
        let as_of = request
            .derived_at
            .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
            .date_naive();
        if base.raw()["integrity_sha256"].as_str() != Some(evidence.base_integrity_sha256.as_str())
            || evidence.years != request.years
            || evidence.as_of != as_of
            || evidence.coverage.status != CoverageStatus::Complete
            || evidence.coverage.reason.trim().is_empty()
        {
            return Err(Error::Base(
                "selected range coverage not verified for fixed daily/years/as_of".into(),
            ));
        }
        // `annual` was aggregated once by the caller over the same windowed
        // base — verify it really is that series before trusting it.
        if annual.base_daily_version_id != daily.version_id
            || annual.as_of != as_of
            || annual.records.len() != request.years.len()
            || !annual
                .records
                .iter()
                .map(|r| r.bar.date.year())
                .eq(request.years.iter().copied())
        {
            return Err(Error::Base(
                "aggregated annual series does not match the request".into(),
            ));
        }
        let lineage = YearlyDerivation {
            base_daily_version_id: daily.version_id.clone(),
            algorithm_version: yearly::ALGORITHM_VERSION.into(),
            first_trading_date: annual.records.first().unwrap().lineage.first_trading_date,
            last_trading_date: annual.records.last().unwrap().lineage.last_trading_date,
        };
        // Rebuild the yearly batch from metadata instead of cloning the daily
        // window's bars and immediately replacing them. This keeps provenance
        // and evidence identical while avoiding a second allocation of every
        // selected daily bar.
        let mut provenance = base.batch.provenance.clone();
        provenance.acquired_at = request.derived_at;
        provenance.adapter_version = "local-yearly-publication-v1".into();
        provenance.yearly_derivation = Some(lineage.clone());
        let batch = crate::market_data::MarketDataBatch {
            symbol: base.batch.symbol.clone(),
            period: Period::Year,
            adjustment: base.batch.adjustment,
            bars: annual.records.iter().map(|r| r.bar.clone()).collect(),
            provenance,
            coverage: evidence.coverage.clone(),
            unit_currency: base.batch.unit_currency.clone(),
            adjustment_evidence: base.batch.adjustment_evidence.clone(),
        };
        let (records, validation) =
            validate_and_deduplicate(&batch).map_err(|e| Error::Base(e.to_string()))?;
        let version = EffectiveVersion {
            version_id: request.version_id.clone(),
            symbol: daily.symbol.clone(),
            period: Period::Year,
            adjustment: daily.adjustment,
            source: daily.source.clone(),
            records,
            published_at: request.derived_at,
            validation,
            unit_currency: daily.unit_currency.clone(),
            adjustment_evidence: daily.adjustment_evidence.clone(),
            yearly_derivation: Some(lineage),
        };
        let mut raw = serde_json::to_value(version).map_err(|e| Error::Invalid(e.to_string()))?;
        raw["batch"] = json!(batch);
        // Preserve per-year participation and unfinished flags without claiming
        // analysis_coverage_v1 or modifying old compatibility records.
        raw["local_yearly_derivation_v1"] = json!({
            "annual": annual,
            "evidence": evidence,
            "base_acquired_at": base.batch.provenance.acquired_at,
        });
        raw["integrity_sha256"] = json!(compat::legacy_digest(&raw)?);
        let checked = compat::check(&raw)?;
        Ok(YearlyCandidate {
            raw,
            checked,
            base_daily_version_id: daily.version_id.clone(),
            base_integrity_sha256: evidence.base_integrity_sha256.clone(),
        })
    }

    /// Caller supplies the application intent; no automatic promotion or rebase.
    /// Existing atomic commit rechecks this plan's base digest, heads and task.
    pub fn prepare_derived_yearly(
        &self,
        command: Publication,
        candidate: &YearlyCandidate,
    ) -> Result<PublishPlan, Error> {
        self.prepare_derived_yearly_prefetched(command, candidate, None, None)
    }
    /// `prepare_derived_yearly` with the caller's already-loaded binding and
    /// base facts/meta window — identical checks in the same order, minus
    /// the redundant storage reads. Used by `prepare_yearly_result`.
    pub(crate) fn prepare_derived_yearly_prefetched(
        &self,
        command: Publication,
        candidate: &YearlyCandidate,
        binding: Option<&crate::task_control::ItemBinding>,
        base_view: Option<(
            &crate::market_storage::FixedFacts,
            &crate::market_storage::compat::CheckedRecord,
        )>,
    ) -> Result<PublishPlan, Error> {
        if candidate.raw["version_id"].as_str() != Some(command.version_id.as_str()) {
            return Err(Error::Invalid("yearly candidate/version binding".into()));
        }
        let fetched_binding;
        let binding = match binding {
            Some(binding) => Some(binding),
            None => {
                fetched_binding = self
                    .repository
                    .task_item_binding(&command.task_id, &command.item_id)?;
                fetched_binding.as_ref()
            }
        };
        match binding {
            Some(binding) => tasks::validate_binding(binding, &command, candidate)?,
            None => {
                let head = self.repository.task_head(&command.task_id)?;
                if matches!(head.key.operation.as_str(), "update_data" | "backfill_data") {
                    return Err(Error::Conflict(
                        "yearly publication differs from accepted frozen inputs".into(),
                    ));
                }
            }
        }
        let fetched_facts;
        let base = match base_view {
            Some((facts, _)) => facts,
            None => {
                fetched_facts = self
                    .repository
                    .fixed_facts(&candidate.base_daily_version_id)
                    .map_err(base_error)?;
                &fetched_facts
            }
        };
        if base.integrity_sha256 != candidate.base_integrity_sha256 {
            return Err(Error::Base("fixed daily changed after derivation".into()));
        }
        if let PublishIntent::CurrentHead {
            expected_yearly_head: Some(id),
            ..
        } = &command.intent
        {
            let old = self.repository.fixed(id).map_err(base_error)?;
            let next = &candidate.checked;
            if old.version().symbol != next.version().symbol || old.version().period != Period::Year
            {
                return Err(Error::Base("expected yearly identity/period".into()));
            }
            let selected: BTreeSet<_> = next
                .version()
                .records
                .iter()
                .map(|r| r.date.year())
                .collect();
            if old
                .version()
                .records
                .iter()
                .any(|r| !selected.contains(&r.date.year()))
            {
                return Err(Error::Conflict(
                    "selected years cannot replace a head containing other years".into(),
                ));
            }
        }
        // The materialized staged record serves both the content check here
        // and `plan_record`/`mark_validated` below — one read instead of two.
        let staged = match self.repository.candidate(&command.version_id) {
            Ok(existing) if existing.raw() == candidate.raw() => existing,
            // Compact backends synthesize the staged raw from typed facts,
            // adding `compact_source` and dropping the derived
            // `local_yearly_derivation_v1` projection — compare the typed
            // version content instead of the envelope bytes.
            Ok(existing)
                if existing.raw().get("compact_source").is_some()
                    && existing.version() == candidate.checked.version() =>
            {
                existing
            }
            Ok(_) => {
                return Err(Error::Conflict(
                    "yearly candidate id has different content".into(),
                ));
            }
            Err(Error::NotFound) => {
                self.repository.stage(&candidate.raw)?;
                self.repository.candidate(&command.version_id)?
            }
            Err(error) => return Err(error),
        };
        let plan = self.plan_record_prefetched(command, staged, binding, base_view)?;
        self.repository.mark_validated(&plan.record)?;
        // `derive_yearly` already authenticated and parsed this immutable
        // candidate; reuse the checked record instead of repeating the full
        // JSON digest, deserialization and bar validation pass.
        let next = &candidate.checked;
        let same_record = plan.record.raw() == candidate.raw()
            || (plan.record.raw().get("compact_source").is_some()
                && plan.record.version == next.version);
        if !same_record
            || plan.base_digest.as_deref() != Some(candidate.base_integrity_sha256.as_str())
        {
            return Err(Error::Base(
                "yearly candidate/base changed during approval".into(),
            ));
        }
        Ok(plan)
    }
}
