//! Suspension intervals: the derived `unresolved_gap` rows and the
//! `confirmed` adapter seam (M2 PR-6, issue-391 §7.3/§7.4/§16).
//!
//! Layer position (separation points 1/2/3):
//!
//! - **Separation point 2 (parsed facts → derived)**: a run of missing bars is
//!   *derived* from two parsed facts — the trading calendar and the entity's
//!   bar days — plus the main table's member window. The derivation lives here
//!   as a pure function over those inputs, so the derived layer is recomputable
//!   from the parsed layer and carries its own `algorithm_version`
//!   ([`ALGORITHM_VERSION`]); it is not folded into the parsed-fact digest.
//! - **Separation point 1 (source → domain)**: `confirmed` rows need an
//!   announcement or GP29 event. The domain never sees a publisher: the
//!   adapter hands over [`ConfirmedSuspensionSnapshot`] and the whole seam is
//!   [`suspension_source`], one function plus a [`PendingSource`] value — the
//!   same shape as `assembly::exchange_source`. A pending source is a value,
//!   not an error: it yields **zero** confirmed rows and the gap rows stay
//!   `unresolved_gap` (AC-M2-8).
//! - **Separation point 3 (effective ↔ known time)**: a derived gap has no
//!   announcement, so its `known_at` is NULL and a point-in-time query excludes
//!   it (fail-closed). A confirmed row carries the announcement's day when the
//!   source has one.
//!
//! The rules that must not be relaxed (§7.1/§7.3):
//!
//! - **"No bar" is not evidence of a suspension.** A gap says "these trading
//!   days have no bar and we do not know why"; it never becomes `suspended`.
//! - A gap exists only **between two consecutive bars**. Nothing is derived
//!   before the first bar (that is a listing date or a missing start), after
//!   the last bar (that is a delisting or the end of the data), outside the
//!   member window (before listing / after delisting), or on a non-trading day
//!   (it is not in the calendar). Consecutive missing days merge into one row
//!   with `missing_days` (Q6); a day covered by a confirmed event is explained,
//!   so it splits the run instead of belonging to it.
//! - `missing_days = 1` rows and longer rows are kept apart in the summary
//!   ([`GapDerivation`]) so a diagnostic can tell a one-off data hole from a
//!   long outage.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;

use super::assembly::PendingSource;
use super::{
    Exchange, SecurityLifecycleFact, SuspensionCertainty, SuspensionInterval, SuspensionReason,
};

/// `derived_dataset_versions.dataset_kind` of the suspension dataset (§9.3).
pub const DATASET_KIND: &str = "suspensions";
/// `derived_dataset_versions.dataset_key`: one whole-market dataset, the same
/// key the assembled main table uses. A later security type is a row property,
/// not a second suspension dataset.
pub const DATASET_KEY: &str = "a_share";
/// Digest algorithm name in `digest_format` (§3.10).
pub const DIGEST_FORMAT: &str = "suspensions-v1";
/// Derivation label in `algorithm_version` for the gap rows.
pub const ALGORITHM_VERSION: &str = "suspension-gap-v1";
/// `source_kind` of a derived gap: the evidence is the bar sequence itself
/// (the same vocabulary `code_migrations` uses for bar-derived facts), never a
/// publisher.
pub const GAP_SOURCE_KIND: &str = "bar_evidence";

/// One derived gap run, before it becomes a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapRun {
    pub start_day: NaiveDate,
    pub end_day: NaiveDate,
    pub missing_days: i64,
}

/// What one entity's derivation produced, plus the diagnostic split of
/// §7.3: a single-day hole and a long outage are counted apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GapDerivation {
    /// The rows in calendar order; `certainty='unresolved_gap'`,
    /// `reason='missing_bar_no_event'`, `known_at=NULL`.
    pub rows: Vec<SuspensionInterval>,
    pub single_day_rows: usize,
    pub multi_day_rows: usize,
}

impl GapDerivation {
    pub fn missing_days(&self) -> i64 {
        self.rows.iter().map(|row| row.missing_days).sum()
    }

    fn push(&mut self, fact: &SecurityLifecycleFact, run: GapRun) {
        if run.missing_days == 1 {
            self.single_day_rows += 1;
        } else {
            self.multi_day_rows += 1;
        }
        self.rows.push(SuspensionInterval {
            exchange: fact.exchange,
            code: fact.code.clone(),
            start_day: run.start_day,
            end_day: run.end_day,
            certainty: SuspensionCertainty::UnresolvedGap,
            missing_days: run.missing_days,
            reason: SuspensionReason::MissingBarNoEvent,
            known_at: None,
            source_kind: GAP_SOURCE_KIND.into(),
            source_record_id: None,
        });
    }
}

/// Derive one `security_master` interval's `unresolved_gap` rows (§7.3).
///
/// Inputs are the parsed facts only: `calendar` (ascending trading days),
/// `bar_days` (ascending days with a 不复权 daily bar for this entity) and
/// `explained` (inclusive confirmed suspension ranges for this entity, sorted).
/// A confirmed range is an *explanation*: its days are missing for a known
/// reason, so they break a run rather than extending it, and the row's
/// `missing_days` counts only unexplained days.
///
/// The member window is §11.1's: `effective_from ≤ T < effective_to`,
/// `listing_date ≤ T`, `T < delisting_date`. A code that is not an A share
/// (`instrument_class != 0`) produces nothing — M2 derives gaps for A shares
/// only.
pub fn derive_gaps(
    fact: &SecurityLifecycleFact,
    calendar: &[NaiveDate],
    bar_days: &[NaiveDate],
    explained: &[(NaiveDate, NaiveDate)],
) -> GapDerivation {
    let mut derivation = GapDerivation::default();
    if fact.instrument_class != 0 {
        return derivation;
    }
    let start = match fact.listing_date {
        Some(listing) if listing > fact.effective_from => listing,
        _ => fact.effective_from,
    };
    let end = match (fact.effective_to, fact.delisting_date) {
        (Some(interval_end), Some(delisting)) => Some(interval_end.min(delisting)),
        (interval_end, delisting) => interval_end.or(delisting),
    };
    let mut run: Option<GapRun> = None;
    let mut seen_bar = false;
    for &day in calendar {
        if day < start || end.is_some_and(|end| day >= end) {
            continue;
        }
        if bar_days.binary_search(&day).is_ok() {
            seen_bar = true;
            flush(&mut run, fact, &mut derivation);
            continue;
        }
        // Before the first bar there is no previous bar to bracket a gap.
        if !seen_bar {
            continue;
        }
        if explained
            .iter()
            .any(|(from, to)| *from <= day && day <= *to)
        {
            flush(&mut run, fact, &mut derivation);
            continue;
        }
        run = Some(match run {
            Some(open) => GapRun {
                end_day: day,
                missing_days: open.missing_days + 1,
                ..open
            },
            None => GapRun {
                start_day: day,
                end_day: day,
                missing_days: 1,
            },
        });
    }
    // A run still open at the end of the window lies after the last bar: that
    // is a delisting or the end of the stored history, never a gap (§7.3).
    derivation
}

fn flush(run: &mut Option<GapRun>, fact: &SecurityLifecycleFact, derivation: &mut GapDerivation) {
    if let Some(run) = run.take() {
        derivation.push(fact, run);
    }
}

/// One `confirmed` suspension fact from an admitted source (A-3). The domain
/// keeps only what the table needs; `source_kind` is the evidence vocabulary
/// (`exchange_announcement`, `gp29_resume`), never a publisher name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedSuspensionFact {
    pub exchange: Exchange,
    pub code: String,
    pub start_day: NaiveDate,
    /// Inclusive, like `suspension_intervals.end_day`. An exchange interval
    /// whose `endStopDate` is "the day before resumption" is converted by the
    /// adapter, not here (#404 A-3).
    pub end_day: NaiveDate,
    /// `exchange_announcement` or `gp29_resume`; `missing_bar_no_event` is the
    /// derived reason and is rejected here.
    pub reason: SuspensionReason,
    /// The announcement day when the source carries one; NULL otherwise. Never
    /// proxied by the suspension day (BR-M2-2).
    pub known_at: Option<NaiveDate>,
    pub source_kind: String,
    pub source_record_id: Option<String>,
}

impl ConfirmedSuspensionFact {
    pub fn entity(&self) -> String {
        format!("{}.{}", self.exchange.as_str(), self.code)
    }

    /// The table row. `missing_days` is the count of calendar trading days the
    /// interval covers, which only the caller holding the calendar can supply.
    pub fn to_interval(&self, missing_days: i64) -> SuspensionInterval {
        SuspensionInterval {
            exchange: self.exchange,
            code: self.code.clone(),
            start_day: self.start_day,
            end_day: self.end_day,
            certainty: SuspensionCertainty::Confirmed,
            missing_days,
            reason: self.reason,
            known_at: self.known_at,
            source_kind: self.source_kind.clone(),
            source_record_id: self.source_record_id.clone(),
        }
    }
}

/// An admitted, generation-frozen suspension-announcement snapshot for one
/// generation. `as_of` is the snapshot date (evidence only); it is never used
/// as a `known_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedSuspensionSnapshot {
    pub as_of: NaiveDate,
    pub facts: Vec<ConfirmedSuspensionFact>,
}

/// The suspension-announcement adapter's answer for one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuspensionSource {
    Admitted(ConfirmedSuspensionSnapshot),
    Pending(PendingSource),
}

/// The suspension-announcement adapter boundary (§16 PR-6): one function, no
/// trait.
///
/// `Some(snapshot)` is an admitted snapshot (A-3: the SH structured
/// suspension/resumption table since 2008, the SZ announcement index). `None`
/// is not "no suspensions": it is the `pending_source` state, and the dataset
/// then carries **zero** confirmed rows — the gaps stay `unresolved_gap` and
/// tradability stays `unknown`, never `suspended` (AC-M2-8). The closed world
/// does not hold even with the source admitted: "no announcement found" is not
/// "no suspension" (#404 A-3).
pub fn suspension_source(snapshot: Option<ConfirmedSuspensionSnapshot>) -> SuspensionSource {
    match snapshot {
        Some(snapshot) => SuspensionSource::Admitted(snapshot),
        None => SuspensionSource::Pending(PendingSource {
            dataset: DATASET_KIND,
            source: "exchange_suspension_announcement",
            reason: "no admitted suspension/resumption snapshot is frozen for this \
                     generation (A-3); every missing bar stays an unresolved_gap",
        }),
    }
}

/// The whole suspension dataset: confirmed facts from the admitted source plus
/// the derived gaps, in row identity order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuspensionSet {
    /// Both certainties, sorted by `(exchange, code, start_day)`.
    pub rows: Vec<SuspensionInterval>,
    pub confirmed_rows: usize,
    pub gap_rows: usize,
    pub single_day_gap_rows: usize,
    pub multi_day_gap_rows: usize,
    /// Source branches that produced no row this run (`pending_source`, §10.1).
    pub pending_sources: Vec<PendingSource>,
}

impl SuspensionSet {
    pub fn missing_days(&self) -> i64 {
        self.rows.iter().map(|row| row.missing_days).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// One whole-market derivation (§16 PR-6).
///
/// The adapter seam decides how many confirmed rows exist ([`SuspensionSource`]);
/// this function only turns facts into rows. Everything it returns is either
/// the admitted snapshot verbatim (plus the calendar-derived `missing_days`) or
/// derived from parsed facts, so a later run with the same inputs produces the
/// same rows and digest.
///
/// `facts` are the main table's intervals (PR-3's assembly, any order),
/// `calendar` the ascending derived trading days, and `bar_days` the ascending
/// 不复权 daily bar days per `exchange.code` (a missing entity has no bars).
/// Kept as plain parameters: the daily item (PR-8) already holds these three
/// values, and the domain must not learn where they were stored.
pub fn assemble_suspensions(
    source: &SuspensionSource,
    facts: &[SecurityLifecycleFact],
    calendar: &[NaiveDate],
    bar_days: &BTreeMap<String, Vec<NaiveDate>>,
) -> Result<SuspensionSet, Error> {
    let (confirmed_facts, pending) = match source {
        SuspensionSource::Admitted(snapshot) => (snapshot.facts.as_slice(), None),
        SuspensionSource::Pending(pending) => (&[][..], Some(*pending)),
    };
    validate_confirmed(confirmed_facts)?;

    let mut set = SuspensionSet::default();
    if let Some(pending) = pending {
        set.pending_sources.push(pending);
    }

    // A confirmed fact names an entity; an entity the main table does not
    // carry cannot be placed in a member window, so the adapter/seam is wrong
    // (fail-closed instead of writing an unattributable row).
    let mut explained: BTreeMap<String, Vec<(NaiveDate, NaiveDate)>> = BTreeMap::new();
    for fact in confirmed_facts {
        if !facts.iter().any(|row| row.entity() == fact.entity()) {
            return Err(Error::UnknownEntity);
        }
        let missing_days = trading_days_within(calendar, fact.start_day, fact.end_day);
        if missing_days == 0 {
            return Err(Error::TradingDayRange);
        }
        explained
            .entry(fact.entity())
            .or_default()
            .push((fact.start_day, fact.end_day));
        set.rows.push(fact.to_interval(missing_days));
    }
    for ranges in explained.values_mut() {
        ranges.sort_unstable();
    }

    let no_bars: Vec<NaiveDate> = Vec::new();
    let no_ranges: Vec<(NaiveDate, NaiveDate)> = Vec::new();
    for fact in facts {
        let bars = bar_days
            .get(&fact.entity())
            .map(Vec::as_slice)
            .unwrap_or(no_bars.as_slice());
        let ranges = explained
            .get(&fact.entity())
            .map(Vec::as_slice)
            .unwrap_or(no_ranges.as_slice());
        let derivation = derive_gaps(fact, calendar, bars, ranges);
        set.single_day_gap_rows += derivation.single_day_rows;
        set.multi_day_gap_rows += derivation.multi_day_rows;
        set.rows.extend(derivation.rows);
    }

    set.rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.start_day).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.start_day,
        ))
    });
    set.confirmed_rows = set
        .rows
        .iter()
        .filter(|row| row.certainty == SuspensionCertainty::Confirmed)
        .count();
    set.gap_rows = set.rows.len() - set.confirmed_rows;
    validate_set(&set.rows)?;
    Ok(set)
}

/// Count the calendar days in the closed interval `[start, end]`.
fn trading_days_within(calendar: &[NaiveDate], start: NaiveDate, end: NaiveDate) -> i64 {
    i64::try_from(
        calendar
            .iter()
            .filter(|day| start <= **day && **day <= end)
            .count(),
    )
    .unwrap_or(i64::MAX)
}

/// What is wrong with a suspension fact/row set. All variants are integrity
/// failures: a source that produces them cannot be stored as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("a suspension interval ends before it starts")]
    Range,
    #[error("missing_days must be positive")]
    MissingDays,
    #[error(
        "missing_bar_no_event is the derived gap reason; a confirmed row needs an \
         announcement or a resume event"
    )]
    Reason,
    #[error("an unresolved_gap row must carry missing_bar_no_event")]
    GapReason,
    #[error("a suspension row carries no source_kind")]
    SourceKind,
    #[error("a derived gap row must not carry a known_at")]
    DerivedKnownAt,
    #[error("confirmed suspension intervals overlap for one code")]
    Overlap,
    #[error("two suspension rows share the identity (exchange, code, start_day)")]
    Duplicate,
    #[error("a confirmed suspension interval covers no calendar trading day")]
    TradingDayRange,
    #[error("a confirmed suspension names an entity that is not in the main table")]
    UnknownEntity,
}

/// Validate the admitted confirmed facts before they are unioned with the gaps.
///
/// Two rules beyond the DDL: the reason must not be the derived one (a
/// confirmed row without an event is a gap wearing the wrong label), and two
/// intervals of one code must neither overlap nor share a start day — the row
/// identity is `(exchange, code, start_day)` and overlapping suspensions would
/// make tradability ambiguous instead of fail-closed.
pub fn validate_confirmed(facts: &[ConfirmedSuspensionFact]) -> Result<(), Error> {
    for fact in facts {
        if fact.end_day < fact.start_day {
            return Err(Error::Range);
        }
        if fact.reason == SuspensionReason::MissingBarNoEvent {
            return Err(Error::Reason);
        }
        if fact.source_kind.trim().is_empty() {
            return Err(Error::SourceKind);
        }
    }
    for (index, fact) in facts.iter().enumerate() {
        let entity = fact.entity();
        for other in &facts[index + 1..] {
            if other.entity() != entity {
                continue;
            }
            if other.start_day <= fact.end_day && fact.start_day <= other.end_day {
                return Err(Error::Overlap);
            }
        }
    }
    Ok(())
}

/// Validate a row set before it is stored: the invariants the DDL cannot state
/// (certainty ↔ reason, a derived row's `known_at`, row identity) plus the
/// DDL's own `missing_days > 0` / `end_day >= start_day`.
pub fn validate_set(rows: &[SuspensionInterval]) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for row in rows {
        if row.end_day < row.start_day {
            return Err(Error::Range);
        }
        if row.missing_days <= 0 {
            return Err(Error::MissingDays);
        }
        if row.source_kind.trim().is_empty() {
            return Err(Error::SourceKind);
        }
        match row.certainty {
            SuspensionCertainty::UnresolvedGap => {
                if row.reason != SuspensionReason::MissingBarNoEvent {
                    return Err(Error::GapReason);
                }
                if row.known_at.is_some() {
                    return Err(Error::DerivedKnownAt);
                }
            }
            SuspensionCertainty::Confirmed => {
                if row.reason == SuspensionReason::MissingBarNoEvent {
                    return Err(Error::Reason);
                }
            }
        }
        if !seen.insert((row.exchange, row.code.as_str(), row.start_day)) {
            return Err(Error::Duplicate);
        }
    }
    Ok(())
}

/// Content digest of the dataset: `SHA256("suspensions-v1" ‖ row_count ‖
/// rows…)`. The source binding and the evidence bundle are excluded: they
/// describe where the facts came from, not the dataset identity (§3.10). The
/// caller sorts the rows first (row identity order).
pub fn dataset_digest(rows: &[SuspensionInterval]) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    let mut payload = Vec::with_capacity(64 + rows.len() * 64);
    payload.extend_from_slice(b"suspensions-v1\0");
    payload.extend_from_slice(&(rows.len() as u64).to_be_bytes());
    for row in rows {
        push_text(&mut payload, row.exchange.as_str());
        push_text(&mut payload, &row.code);
        push_day(&mut payload, row.start_day);
        push_day(&mut payload, row.end_day);
        push_text(&mut payload, row.certainty.as_str());
        payload.extend_from_slice(&row.missing_days.to_be_bytes());
        push_text(&mut payload, row.reason.as_str());
        push_opt_day(&mut payload, row.known_at);
        push_text(&mut payload, &row.source_kind);
        push_opt_text(&mut payload, row.source_record_id.as_deref());
    }
    Sha256::digest(&payload).into()
}

fn push_text(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn push_opt_text(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            out.push(1);
            push_text(out, value);
        }
        None => out.push(0),
    }
}

fn push_day(out: &mut Vec<u8>, day: NaiveDate) {
    out.extend_from_slice(&crate::compact_market::epoch_day(day).to_be_bytes());
}

fn push_opt_day(out: &mut Vec<u8>, day: Option<NaiveDate>) {
    match day {
        Some(day) => {
            out.push(1);
            push_day(out, day);
        }
        None => out.push(0),
    }
}

#[cfg(test)]
mod tests;
