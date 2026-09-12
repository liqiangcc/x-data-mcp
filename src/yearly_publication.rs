//! Fixed published daily -> locally derived yearly candidate. No source or MCP wiring.
pub mod tasks;
use std::collections::BTreeSet;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    market_data::{CoverageEvidence, CoverageStatus, Period, YearlyDerivation},
    market_storage::{
        Error, MarketRepository, Publication, PublishIntent, PublishPlan, Publisher, base_error,
        compat,
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
    pub fn derive_yearly(
        &self,
        request: &Derivation,
        evidence: &DerivationEvidence,
    ) -> Result<YearlyCandidate, Error> {
        if request.symbol.trim().is_empty()
            || request.version_id.trim().is_empty()
            || request.base_daily_version_id.trim().is_empty()
            || request.version_id == request.base_daily_version_id
            || request.years.is_empty()
        {
            return Err(Error::Invalid("explicit yearly identity/selection".into()));
        }
        let base = self
            .repository
            .fixed(&request.base_daily_version_id)
            .map_err(base_error)?;
        let daily = base.version();
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
        let annual = yearly::aggregate(daily, Some(&request.years), as_of)
            .map_err(|e| Error::Base(e.to_string()))?;
        let lineage = YearlyDerivation {
            base_daily_version_id: daily.version_id.clone(),
            algorithm_version: yearly::ALGORITHM_VERSION.into(),
            first_trading_date: annual.records.first().unwrap().lineage.first_trading_date,
            last_trading_date: annual.records.last().unwrap().lineage.last_trading_date,
        };
        let mut batch = base.batch.clone();
        batch.period = Period::Year;
        batch.bars = annual.records.iter().map(|r| r.bar.clone()).collect();
        batch.coverage = evidence.coverage.clone();
        batch.provenance.acquired_at = request.derived_at;
        batch.provenance.adapter_version = "local-yearly-publication-v1".into();
        batch.provenance.yearly_derivation = Some(lineage.clone());
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
        compat::check(&raw)?;
        Ok(YearlyCandidate {
            raw,
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
        if candidate.raw["version_id"].as_str() != Some(command.version_id.as_str()) {
            return Err(Error::Invalid("yearly candidate/version binding".into()));
        }
        tasks::validate_binding(
            &self.repository.task(&command.task_id)?,
            &command,
            candidate,
        )?;
        let base = self
            .repository
            .fixed(&candidate.base_daily_version_id)
            .map_err(base_error)?;
        if base.raw()["integrity_sha256"].as_str() != Some(candidate.base_integrity_sha256.as_str())
        {
            return Err(Error::Base("fixed daily changed after derivation".into()));
        }
        if let PublishIntent::CurrentHead {
            expected_yearly_head: Some(id),
            ..
        } = &command.intent
        {
            let old = self.repository.fixed(id).map_err(base_error)?;
            let next = compat::check(&candidate.raw)?;
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
        self.repository.stage(&candidate.raw)?;
        let plan = self.prepare(command)?;
        if plan.record.raw() != candidate.raw()
            || plan.base_digest.as_deref() != Some(candidate.base_integrity_sha256.as_str())
        {
            return Err(Error::Base(
                "yearly candidate/base changed during approval".into(),
            ));
        }
        Ok(plan)
    }
}
