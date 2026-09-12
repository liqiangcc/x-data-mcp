//! Pure daily range-update decisions (#4). No source, task, MCP or commit wiring.
//! Evidence references are trusted application inputs, never user-controlled approval flags.
use crate::{
    market_data::{Bar, CoverageStatus, EvidenceValue, MarketDataBatch, Period},
    market_storage::{Error, compat::CheckedRecord},
};
use chrono::{Datelike, NaiveDate};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
}
impl DateRange {
    fn contains(&self, date: NaiveDate) -> bool {
        self.start <= date && date <= self.end
    }
}
/// A future evidence verifier must construct this; a nonempty reference alone is
/// not proof of official semantics. This type is not deserializable or on MCP wire.
pub struct Evidence {
    pub base_version_id: String,
    pub base_integrity_sha256: String,
    pub candidate_sha256: String,
    pub range: DateRange,
    pub same_qfq_basis_ref: String,
    pub revision_ref: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    CoverageUnknown,
    BasisUnknown,
    UnitsIncompatible,
    MissingOldDates(Vec<NaiveDate>),
    KnownValueLost(Vec<NaiveDate>),
    RevisionUnverified(Vec<NaiveDate>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    NoChange { version_id: String },
    NeedsVerification(Reason),
    Candidate(PreparedRange),
}
/// Not a publish approval: combined coverage and provenance must be established
/// before the existing Publisher/conditional SQLite transaction may commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRange {
    pub records: Vec<Bar>,
    pub changed_dates: Vec<NaiveDate>,
    pub affected_years: BTreeSet<i32>,
    pub base_version_id: String,
}
pub fn candidate_digest(batch: &MarketDataBatch) -> Result<String, Error> {
    serde_json::to_vec(batch)
        .map(|b| crate::hash::sha256(&b))
        .map_err(|e| Error::Invalid(e.to_string()))
}
fn conflicting_units(a: &EvidenceValue, b: &EvidenceValue) -> bool {
    a.is_confirmed() && b.is_confirmed() && a.value != b.value
}
pub fn prepare(
    existing: &CheckedRecord,
    batch: &MarketDataBatch,
    range: &DateRange,
    evidence: Option<&Evidence>,
) -> Result<Decision, Error> {
    let old = existing.version();
    if range.start > range.end || range.start.year() <= 0 || range.end.year() > 9999 {
        return Err(Error::Invalid("daily update range".into()));
    }
    if old.period != Period::Day || batch.period != Period::Day {
        return Err(Error::Invalid(
            "yearly must derive from one fixed daily version".into(),
        ));
    }
    if batch.symbol != old.symbol
        || batch.provenance.symbol != old.symbol
        || batch.adjustment != old.adjustment
        || batch.provenance.source != old.source
        || batch.provenance.source_period != "day"
        || batch.provenance.source_adjustment != "qfq"
        || batch.provenance.yearly_derivation.is_some()
    {
        return Err(Error::Invalid(
            "update identity/source/adjustment mismatch".into(),
        ));
    }
    if batch.bars.iter().any(|b| !range.contains(b.date)) {
        return Err(Error::Invalid("candidate outside explicit range".into()));
    }
    let (bars, _) = crate::publish::validate_and_deduplicate(batch)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    crate::publish::validate_unit_currency_evidence(batch)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    crate::publish::validate_adjustment_evidence(batch)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let incoming: BTreeMap<_, _> = bars.iter().map(|b| (b.date, b)).collect();
    let missing: Vec<_> = old
        .records
        .iter()
        .filter(|b| range.contains(b.date) && !incoming.contains_key(&b.date))
        .map(|b| b.date)
        .collect();
    if !missing.is_empty() {
        return Ok(Decision::NeedsVerification(Reason::MissingOldDates(
            missing,
        )));
    }
    if batch.coverage.status != CoverageStatus::Complete {
        return Ok(Decision::NeedsVerification(Reason::CoverageUnknown));
    }
    let a = &old.unit_currency;
    let b = &batch.unit_currency;
    if [
        (&a.price_currency, &b.price_currency),
        (&a.price_unit, &b.price_unit),
        (&a.volume_unit, &b.volume_unit),
        (&a.amount_currency, &b.amount_currency),
        (&a.amount_unit, &b.amount_unit),
    ]
    .into_iter()
    .any(|(a, b)| conflicting_units(a, b))
    {
        return Ok(Decision::NeedsVerification(Reason::UnitsIncompatible));
    }
    let Some(proof) = evidence else {
        return Ok(Decision::NeedsVerification(Reason::BasisUnknown));
    };
    if proof.base_version_id != old.version_id
        || Some(proof.base_integrity_sha256.as_str()) != existing.raw()["integrity_sha256"].as_str()
        || proof.candidate_sha256 != candidate_digest(batch)?
        || proof.range != *range
        || proof.same_qfq_basis_ref.trim().is_empty()
    {
        return Ok(Decision::NeedsVerification(Reason::BasisUnknown));
    }
    let mut combined: BTreeMap<_, _> = old.records.iter().map(|b| (b.date, b.clone())).collect();
    let mut changed = vec![];
    let mut revised = vec![];
    let mut lost = vec![];
    for bar in bars {
        if let Some(previous) = combined.get(&bar.date) {
            if (previous.volume.is_some() && bar.volume.is_none())
                || (previous.amount.is_some() && bar.amount.is_none())
            {
                lost.push(bar.date);
            }
            if previous != &bar {
                revised.push(bar.date);
            }
        }
        if combined.get(&bar.date) != Some(&bar) {
            changed.push(bar.date);
        }
        combined.insert(bar.date, bar);
    }
    if !lost.is_empty() {
        return Ok(Decision::NeedsVerification(Reason::KnownValueLost(lost)));
    }
    if !revised.is_empty()
        && proof
            .revision_ref
            .as_ref()
            .is_none_or(|r| r.trim().is_empty())
    {
        return Ok(Decision::NeedsVerification(Reason::RevisionUnverified(
            revised,
        )));
    }
    if changed.is_empty() {
        return Ok(Decision::NoChange {
            version_id: old.version_id.clone(),
        });
    }
    Ok(Decision::Candidate(PreparedRange {
        records: combined.into_values().collect(),
        affected_years: changed.iter().map(|d| d.year()).collect(),
        changed_dates: changed,
        base_version_id: old.version_id.clone(),
    }))
}
