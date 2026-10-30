//! `market.industry_assignments` — the historical industry read model
//! (M3 PR-7, `#446` §5.5, AC-M3-3).
//!
//! Layer position (separation points 3/4): this module is the read model. It
//! reads the interval rows through the storage adapter
//! (`sqlite_market::industry::read_assignments`), applies the two-time
//! semantics — information set first, record set second — and answers with
//! assignments, never with table rows. The v8 entry adapter and the export
//! (PR-9) are projections on top of it; neither writes SQL.
//!
//! The three answers are fixed by §5.4/§5.5:
//!
//! ```text
//! as_of < coverage_start       → unknown{before_coverage}
//! a covering interval exists   → assigned
//! no covering interval         → unknown{missing_in_snapshot}
//! ```
//!
//! `before_coverage` is never the current catalog industry and never an empty
//! answer a caller could read as "this code has no industry": the accumulation
//! starts at the first interval-defining snapshot (2026-09-30 for the admitted
//! A-graded history), and anything earlier is honestly unknown. Inside
//! coverage, a code absent from the snapshot covering `as_of` is
//! `missing_in_snapshot`; nothing is interpolated between snapshots.
//!
//! `known_at` is the information-set cutoff. Every stored interval carries one
//! (schema 25 makes the column `NOT NULL`), but a backfilled row's time is the
//! backfill import day, so a query before that day cannot see it. Such a row is
//! excluded from the info set and **counted** by [`TemporalReport`]; because the
//! code is in the snapshot but was not knowable yet, it answers
//! `unknown{not_known_at}` rather than claiming the snapshot lacked it. A NULL
//! `known_at` would be excluded and counted the same way (the temporal layer
//! owns that vocabulary); this table cannot produce one, and the read model
//! does not invent one.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use rusqlite::Connection;

use super::Error;
use super::temporal::{Temporal, TemporalReport};
use crate::industry::{AssignmentRow, SCHEME};
use crate::sqlite_market::industry as store;

/// `derived_dataset_versions.dataset_kind`-style name the temporal block uses
/// for this dataset. The M3 industry tables have no registration row; the name
/// is the read model's dataset name, not a storage key.
pub const DATASET_INDUSTRY_ASSIGNMENTS: &str = "industry_assignments";

/// The industry question: a point in time, the information-set cutoff and the
/// entities the caller selected. An empty entity list means "every entity the
/// interval table holds", in `(exchange, code)` order (the export/ops shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndustryRequest {
    pub temporal: Temporal,
    pub entities: Vec<String>,
}

impl IndustryRequest {
    /// `as_of` only; no entity filter.
    pub fn new(as_of: NaiveDate) -> Self {
        Self {
            temporal: Temporal::effective_only(as_of),
            entities: Vec::new(),
        }
    }

    /// `as_of` plus the exact entity selection a `query_data` request carries.
    pub fn for_entities(as_of: NaiveDate, entities: impl IntoIterator<Item = String>) -> Self {
        Self {
            temporal: Temporal::effective_only(as_of),
            entities: entities.into_iter().collect(),
        }
    }

    pub fn with_known_at(mut self, known_at: NaiveDate) -> Self {
        self.temporal.known_at = Some(known_at);
        self
    }
}

/// Why an entity has no assignment on the queried day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// `as_of` predates the first interval-defining snapshot (§5.4).
    BeforeCoverage,
    /// The snapshot covering `as_of` does not contain the code; no
    /// interpolation between snapshots (§5.4).
    MissingInSnapshot,
    /// A covering interval exists but its `known_at` is later than the
    /// information-set cutoff, so a point-in-time answer may not use it.
    NotKnownAt,
}

impl UnknownReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeforeCoverage => "before_coverage",
            Self::MissingInSnapshot => "missing_in_snapshot",
            Self::NotKnownAt => "not_known_at",
        }
    }
}

/// One assigned interval, projected to the read-model identity and the §5.5
/// fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedIndustry {
    pub entity: String,
    pub exchange: String,
    pub code: String,
    pub scheme: &'static str,
    pub level: i64,
    pub industry_code: String,
    pub name: Option<String>,
    pub name_status: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub known_at: NaiveDate,
    pub effective_from_basis: String,
    pub evidence_grade: String,
    pub source_snapshot_sha256: String,
}

/// One code with no usable assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownIndustry {
    pub entity: String,
    pub exchange: String,
    pub code: String,
    pub reason: UnknownReason,
}

/// The read-model answer for one entity: `assigned` or `unknown{reason}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndustryAssignment {
    Assigned(AssignedIndustry),
    Unknown(UnknownIndustry),
}

impl IndustryAssignment {
    pub fn entity(&self) -> &str {
        match self {
            Self::Assigned(row) => &row.entity,
            Self::Unknown(row) => &row.entity,
        }
    }

    pub fn status(&self) -> &'static str {
        match self {
            Self::Assigned(_) => "assigned",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// One read-model answer plus the counts behind it (§5.5, §12.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndustryReport {
    pub as_of: NaiveDate,
    pub known_at: Option<NaiveDate>,
    /// Earliest interval start in the whole table; `None` when no
    /// interval-defining snapshot exists (§5.4).
    pub coverage_start: Option<NaiveDate>,
    pub assignments: Vec<IndustryAssignment>,
    pub temporal: TemporalReport,
}

/// Storage-independent identity of the whole interval table: the §9.4.2
/// content summary over `industry_snapshots` + `industry_assignments`, the
/// latest ingestion time, the coverage start and the row counts.
///
/// The M3 tables carry no `derived_dataset_versions` registration (§5.1), so
/// this summary is the dataset version the read surface names in
/// `version_ref`. Recomputing it from the same rows always gives the same id;
/// a pinned id that no longer matches is reported as changed, not silently
/// served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndustryIdentity {
    pub version_id: String,
    pub latest_ingested_at: Option<String>,
    pub coverage_start: Option<NaiveDate>,
    pub snapshot_rows: usize,
    pub assignment_rows: usize,
}

/// `market.industry_assignments` for the selected entities at `as_of`.
///
/// Rows are observed through [`TemporalReport`] before the record set is
/// selected, so a `known_at` cutoff can never see a later-known interval. The
/// answer has one entry per selected entity — `assigned` or `unknown` — which
/// is what keeps `unknown` observable instead of turning it into a missing row.
pub fn assignments(c: &Connection, request: &IndustryRequest) -> Result<IndustryReport, Error> {
    let rows = store::read_assignments(c)?;
    let coverage_start = coverage_start(&rows)?;
    let mut report = TemporalReport::new(request.temporal);
    let entities = selected_entities(request, &rows)?;
    let index = index_by_entity(&rows);

    let mut assignments = Vec::with_capacity(entities.len());
    for entity in entities {
        let (exchange, code) = split_entity(&entity)?;
        let candidates = index
            .get(&(exchange.clone(), code.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut admitted = Vec::with_capacity(candidates.len());
        for row in candidates {
            if report.observe(DATASET_INDUSTRY_ASSIGNMENTS, Some(row_known_at(row)?)) {
                admitted.push(*row);
            }
        }
        assignments.push(answer(
            &entity,
            &exchange,
            &code,
            request.temporal.as_of,
            coverage_start,
            &admitted,
            candidates,
        )?);
    }
    Ok(IndustryReport {
        as_of: request.temporal.as_of,
        known_at: request.temporal.known_at,
        coverage_start,
        assignments,
        temporal: report,
    })
}

/// The dataset identity used by the entry adapter's manifest and pin check.
pub fn dataset_identity(c: &Connection) -> Result<IndustryIdentity, Error> {
    let snapshots = store::read_snapshots(c)?;
    let assignments = store::read_assignments(c)?;
    Ok(IndustryIdentity {
        version_id: crate::industry::summary(&snapshots, &assignments),
        latest_ingested_at: snapshots
            .iter()
            .map(|snapshot| snapshot.ingested_at.as_str())
            .max()
            .map(str::to_owned),
        coverage_start: coverage_start(&assignments)?,
        snapshot_rows: snapshots.len(),
        assignment_rows: assignments.len(),
    })
}

/// Every stored interval, projected to [`AssignedIndustry`], in the storage
/// adapter's `(exchange, code, effective_from)` order.
///
/// This is the export shape (#446 §10.4 `industry_assignments`): the whole
/// interval table, not a per-`as_of` answer. Exporting the answers would freeze
/// today's `coverage_start` into the file and lose the intervals a consumer
/// needs to rebuild an earlier point in time. The `entity` comes from
/// `(exchange, code)`, the same identity the read model joins to the main
/// table.
pub fn interval_rows(c: &Connection) -> Result<Vec<AssignedIndustry>, Error> {
    let rows = store::read_assignments(c)?;
    rows.iter()
        .map(|row| assigned(row, &format!("{}.{}", row.exchange, row.code)))
        .collect()
}

fn coverage_start(rows: &[AssignmentRow]) -> Result<Option<NaiveDate>, Error> {
    crate::industry::coverage_start(rows)
        .map(|day| epoch_date(day, "effective_from"))
        .transpose()
}

fn selected_entities(
    request: &IndustryRequest,
    rows: &[AssignmentRow],
) -> Result<Vec<String>, Error> {
    let mut entities: BTreeSet<String> = if request.entities.is_empty() {
        rows.iter()
            .map(|row| format!("{}.{}", row.exchange, row.code))
            .collect()
    } else {
        request.entities.iter().cloned().collect()
    };
    for entity in &entities {
        split_entity(entity)?;
    }
    entities.retain(|entity| !entity.is_empty());
    Ok(entities.into_iter().collect())
}

/// The interval rows grouped by `(exchange, code)`, each group in the storage
/// adapter's `effective_from` order.
fn index_by_entity(rows: &[AssignmentRow]) -> BTreeMap<(String, String), Vec<&AssignmentRow>> {
    let mut index: BTreeMap<(String, String), Vec<&AssignmentRow>> = BTreeMap::new();
    for row in rows {
        index
            .entry((row.exchange.clone(), row.code.clone()))
            .or_default()
            .push(row);
    }
    index
}

fn answer(
    entity: &str,
    exchange: &str,
    code: &str,
    as_of: NaiveDate,
    coverage_start: Option<NaiveDate>,
    admitted: &[&AssignmentRow],
    candidates: &[&AssignmentRow],
) -> Result<IndustryAssignment, Error> {
    if coverage_start.is_none_or(|start| as_of < start) {
        return Ok(unknown(
            entity,
            exchange,
            code,
            UnknownReason::BeforeCoverage,
        ));
    }
    if let Some(row) = admitted.iter().find(|row| covers(row, as_of)) {
        return Ok(IndustryAssignment::Assigned(assigned(row, entity)?));
    }
    let reason = if candidates.iter().any(|row| covers(row, as_of)) {
        UnknownReason::NotKnownAt
    } else {
        UnknownReason::MissingInSnapshot
    };
    Ok(unknown(entity, exchange, code, reason))
}

fn unknown(entity: &str, exchange: &str, code: &str, reason: UnknownReason) -> IndustryAssignment {
    IndustryAssignment::Unknown(UnknownIndustry {
        entity: entity.to_owned(),
        exchange: exchange.to_owned(),
        code: code.to_owned(),
        reason,
    })
}

fn assigned(row: &AssignmentRow, entity: &str) -> Result<AssignedIndustry, Error> {
    Ok(AssignedIndustry {
        entity: entity.to_owned(),
        exchange: row.exchange.clone(),
        code: row.code.clone(),
        scheme: SCHEME,
        level: row.level,
        industry_code: row.industry_code.clone(),
        name: row.name.clone(),
        name_status: row.name_status.clone(),
        effective_from: epoch_date(row.effective_from, "effective_from")?,
        effective_to: row
            .effective_to
            .map(|day| epoch_date(day, "effective_to"))
            .transpose()?,
        known_at: row_known_at(row)?,
        effective_from_basis: row.basis.clone(),
        evidence_grade: row.evidence_grade.clone(),
        source_snapshot_sha256: row.snapshot_member_sha256.clone(),
    })
}

fn covers(row: &AssignmentRow, as_of: NaiveDate) -> bool {
    let day = crate::industry::epoch_day(as_of);
    row.effective_from <= day && row.effective_to.is_none_or(|end| day < end)
}

fn row_known_at(row: &AssignmentRow) -> Result<NaiveDate, Error> {
    crate::industry::parse_day_prefix(&row.known_at)
        .map_err(|error| Error::Integrity(error.to_string()))
}

fn epoch_date(day: i64, field: &str) -> Result<NaiveDate, Error> {
    crate::compact_market::epoch_date(day)
        .map_err(|_| Error::Integrity(format!("industry {field} {day} is outside the date range")))
}

fn split_entity(entity: &str) -> Result<(String, String), Error> {
    crate::compact_market::canonical_entity(entity)
        .map_err(|error| Error::Invalid(error.to_string()))?;
    let (exchange, code) = entity
        .split_once('.')
        .ok_or_else(|| Error::Invalid(format!("non-canonical entity {entity}")))?;
    Ok((exchange.to_owned(), code.to_owned()))
}

#[cfg(test)]
mod tests;
