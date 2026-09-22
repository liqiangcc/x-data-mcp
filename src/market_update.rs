//! Daily range-update application decisions (#4). No source or MCP wiring.
//! Evidence references are trusted application inputs, never user-controlled approval flags.
use crate::{
    market_data::{Bar, CoverageStatus, EvidenceValue, MarketDataBatch, Period},
    market_storage::{Error, compat::CheckedRecord},
};
pub mod publication;
pub mod runner;
pub mod tasks;
use chrono::{Datelike, NaiveDate};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    pub base_version_id: String,
    pub base_integrity_sha256: String,
    pub candidate_sha256: String,
    pub range: DateRange,
    pub same_qfq_basis_ref: String,
    pub revision_ref: Option<String>,
    /// Recorded continuity proof for generation-bound updates (#262):
    /// how the bytes read for this acquisition relate to the fixed base's
    /// pinned snapshot inputs. Audit metadata — the fail-closed value
    /// checks in `prepare` do not depend on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityEvidence>,
}
/// How the security's snapshot inputs at update time relate to the fixed
/// base's pinned bindings. `mode`: `unchanged` (member digest and event
/// digest both identical), `append` (the base's member bytes are an exact
/// prefix and the event set is unchanged, so only strictly-newer records
/// were materialized), `full` (no usable digest proof — complete
/// materialization and value-level verification).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContinuityEvidence {
    pub mode: String,
    /// sha256 of the `.day` member bytes actually read for this update.
    pub day_member_sha256: String,
    /// Byte length of the prefix proven identical to the base's member;
    /// equals the whole member for `unchanged`, 0 for `full`.
    pub day_prefix_bytes: u64,
    /// Digest of the security's category-1 event set plus excluded count.
    pub gbbq_events_sha256: String,
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
    pub metadata_changed: bool,
    pub affected_years: BTreeSet<i32>,
    pub base_version_id: String,
    pub base_integrity_sha256: String,
    pub candidate_sha256: String,
    pub requested_range: DateRange,
    pub same_qfq_basis_ref: String,
    pub revision_ref: Option<String>,
}
/// Typed base-version facts for the ops-first update path (#259 S5G):
/// everything acquire/prepare needs about the base — bindings, coverage,
/// units, integrity digest, record bounds — without materializing bars.
/// `day_member_sha256`/`security_event_sha256` are the source bindings the
/// base was published from; `integrity_sha256` is the published integrity
/// digest in the same hex form the envelope `raw()["integrity_sha256"]`
/// carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateBaseFacts {
    pub version_id: String,
    pub symbol: String,
    pub record_count: u32,
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    pub integrity_sha256: String,
    pub coverage: crate::market_data::CoverageEvidence,
    pub unit_currency: crate::market_data::UnitCurrencyEvidence,
    pub source: String,
    pub generation_id: String,
    pub generation_manifest_sha256: String,
    pub day_member_sha256: String,
    pub security_event_sha256: String,
}
/// One prepared item for `RangeRepository::commit_update_batch`
/// (#259 S5G-6): acquisition and continuity probing already ran outside
/// the transaction, so the batch writer only replays, transitions, stages
/// and commits.
#[derive(Debug)]
pub struct UpdateBatchItem {
    pub task_id: String,
    pub item_id: String,
    pub publication_id: String,
    pub run_id: String,
    pub symbol: String,
    pub occupancy_key: String,
    pub prepared: UpdateBatchPrepared,
}
#[derive(Debug)]
pub enum UpdateBatchPrepared {
    /// Continuity-proven unchanged member: reuse the base version, the
    /// published head stays put.
    NoChange {
        base_version_id: String,
        range_start_day: i64,
        range_end_day: i64,
        continuity: serde_json::Value,
    },
    /// Continuity-proven append: stage `upserts` as a delta on the base
    /// version, then commit.
    Append {
        base_version_id: String,
        result_version_id: String,
        upserts: Vec<crate::market_data::Bar>,
        continuity: serde_json::Value,
    },
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
    let incoming: BTreeMap<_, _> = batch.bars.iter().map(|b| (b.date, b)).collect();
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
    let (bars, _) = crate::publish::validate_and_deduplicate(batch)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    crate::publish::validate_unit_currency_evidence(batch)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    crate::publish::validate_adjustment_evidence(batch)
        .map_err(|e| Error::Invalid(e.to_string()))?;
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
                || (previous.r7_raw.is_some() && bar.r7_raw.is_none())
            {
                lost.push(bar.date);
            }
            // #4 AC-71: filling an unknown optional field is not a correction
            // to a previously known value. Other simultaneous changes still are.
            if previous.open != bar.open
                || previous.high != bar.high
                || previous.low != bar.low
                || previous.close != bar.close
                || previous.volume.zip(bar.volume).is_some_and(|(a, b)| a != b)
                || previous.amount.zip(bar.amount).is_some_and(|(a, b)| a != b)
                || previous.r7_raw.zip(bar.r7_raw).is_some_and(|(a, b)| a != b)
            {
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
    // #4 AC-84/87: unchanged prices alone do not prove unchanged data.
    // Acquired-at is execution provenance; changed coverage is version content.
    let units_enriched = [
        (&a.price_currency, &b.price_currency),
        (&a.price_unit, &b.price_unit),
        (&a.volume_unit, &b.volume_unit),
        (&a.amount_currency, &b.amount_currency),
        (&a.amount_unit, &b.amount_unit),
    ]
    .into_iter()
    .any(|(old, new)| !old.is_confirmed() && new.is_confirmed());
    let metadata_changed = batch.coverage != existing.batch.coverage || units_enriched;
    if changed.is_empty() && !metadata_changed {
        return Ok(Decision::NoChange {
            version_id: old.version_id.clone(),
        });
    }
    Ok(Decision::Candidate(PreparedRange {
        // Coverage/units can affect more than numerically changed dates. Do not
        // promise that unrelated old yearly metadata remains valid.
        affected_years: if metadata_changed {
            combined.keys().map(|d| d.year()).collect()
        } else {
            changed.iter().map(|d| d.year()).collect()
        },
        records: combined.into_values().collect(),
        changed_dates: changed,
        metadata_changed,
        base_version_id: old.version_id.clone(),
        base_integrity_sha256: proof.base_integrity_sha256.clone(),
        candidate_sha256: proof.candidate_sha256.clone(),
        requested_range: range.clone(),
        same_qfq_basis_ref: proof.same_qfq_basis_ref.clone(),
        revision_ref: proof.revision_ref.clone(),
    }))
}
