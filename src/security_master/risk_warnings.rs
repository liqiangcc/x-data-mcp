//! Risk-warning (ST/*ST) adapter boundary and interval assembly
//! (issue-391 §6.3, §10.1; issue-446 PR-10, D-58).
//!
//! The domain owns the rules that turn dated source events into warning
//! intervals. It never fetches, freezes or writes: the source adapter
//! (`official_security::risk_warnings`) parses frozen bytes into
//! [`SzNameChange`]/[`RiskWarningStatusEvent`], this module closes and opens
//! intervals, and the storage writer persists the validated set.
//!
//! ## What absence means
//!
//! Zero rows are not "no warning". A pending source and an unobserved window
//! are separate states, and a missing interval is read as `unknown`, never
//! `normal` (§10.1). The interval set therefore carries its coverage windows:
//! a window with no rows can be recorded as `not_observed` instead of being
//! silently dropped. No function here ever synthesises a `normal` row from
//! absent evidence.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;

use super::assembly::PendingSource;
use super::{Exchange, RiskStatus, RiskWarningInterval};

/// One acquisition window and whether it produced rows. The shared shape lives
/// in [`super::coverage`] because the name history records the same windows
/// (PR-12); the re-export keeps this module's existing paths.
pub use super::coverage::CoverageWindow;

/// Dataset name the daily item reports and the read model consumes
/// (`derived_dataset_versions.dataset_kind`, §3.2).
pub const DATASET_KIND: &str = "risk_warnings";
/// One whole-market dataset, like the main table and the suspensions.
pub const DATASET_KEY: &str = "a_share";
/// Content-digest format registered with the dataset version.
pub const DIGEST_FORMAT: &str = "sha256-risk-warning-intervals-v1";
/// Algorithm version of the interval assembly.
///
/// `v2` (D-81 R5): a same-day conflict no longer fails the whole dataset. The
/// conflicting code gets one `marker_only_unresolved` row on the conflict day
/// and every other code is assembled as before. Datasets with no conflict
/// assemble to the same rows under `v1` and `v2`, so the content digest (and
/// the `version_id` derived from it) does not move when only the label does.
/// `v3` (D-81, task 49/#578): for Shenzhen A-shares only, write `normal`
/// intervals as the complement of ST/warning intervals within the name history
/// coverage. The rules are closed-world: PT prefix, "退" suffix, `*` prefix
/// without ST, and the state after a conflict until the next event remain
/// `unknown`.
/// `v4` (D-81 GP29 §7, #579): the title classifier rejects "存在被实施..."
/// forward-looking risk notices instead of misclassifying them as
/// implementations. This affects 14 SH announcements in 2014-12; the 7 codes
/// that had open intervals from those dates get them closed. `v3` and `v4`
/// were developed at the same time, so the numbers follow merge order: `v3`
/// went to #578, which merged first.
/// `v5` (D-84, this task): the SH effective day is the trading day after the
/// first trading day on or after the announcement day (or the explicit title
/// date when one is present). Implementation and revocation use the same rule;
/// an event the calendar cannot place yet is deferred and writes no row. The
/// rows move by one trading day (two when the announcement is not a trading
/// day), so the content digest and `version_id` move on the next write.
pub const ALGORITHM_VERSION: &str = "risk-warnings-v5";

/// `raw_status_code` of the row an SH implementation and revocation on one
/// effective day collapse to.
pub const RAW_SAME_DAY_ACTIONS: &str = "unresolved:same_day_action_conflict";
/// `raw_status_code` of the row two SZ short-name changes on one day collapse
/// to.
pub const RAW_SAME_DAY_NAME_CHANGES: &str = "unresolved:same_day_name_changes";
/// How many conflicts the evidence lists next to the count.
pub const CONFLICT_SAMPLE_LIMIT: usize = 20;

/// One dated short-name change from the SZSE `SSGSGMXX`/`tab2` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SzNameChange {
    pub code: String,
    pub day: NaiveDate,
    pub before_name: String,
    pub after_name: String,
    pub source_record_id: String,
    pub source_document_sha256: String,
    /// The day the first snapshot containing this row entered the source
    /// store (`issue-446` §5.3, Q4). The page carries the change day but no
    /// publication time, so the adapter fills this from the store instead of
    /// leaving the fact's knowledge time unknown: with `known_at` NULL, a
    /// `known_at` filter would hide every SZ row forever. `None` only when no
    /// store could answer (a `--scope` replay without a database).
    pub known_at: Option<NaiveDate>,
}

/// What a SH announcement index row says happened. The source is event-level;
/// the interval around the event is derived here, not claimed by the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskWarningAction {
    Implement,
    Revoke,
}

/// One SH announcement event: implementation or revocation plus the
/// announcement day. `status` is present for an implementation and `None` for
/// a revocation (the revoked status is the open interval's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskWarningStatusEvent {
    pub code: String,
    /// The day the event takes effect. A title with an explicit date sets this
    /// directly (`day_is_explicit`); every other event starts at its
    /// announcement day and [`place_sh_events`] applies the D-84 trading-day
    /// rule to it.
    pub day: NaiveDate,
    /// `true` when `day` came from an explicit date in the title. D-84 never
    /// moves such a day, so placement must not apply the calendar rule twice.
    pub day_is_explicit: bool,
    pub action: RiskWarningAction,
    pub status: Option<RiskStatus>,
    pub announced_at: Option<String>,
    pub known_at: Option<NaiveDate>,
    pub raw_status_code: String,
    pub source_kind: String,
    pub source_record_id: String,
    pub source_document_sha256: String,
}

/// One SH event whose D-84 effective day cannot be placed yet because the
/// trading calendar does not reach the day after the first trading day on or
/// after the announcement. It writes no row: the state stays `unknown` until a
/// later run has the calendar day, instead of guessing with a weekend rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredShEvent {
    pub code: String,
    pub announced: NaiveDate,
    pub action: RiskWarningAction,
    pub status: Option<RiskStatus>,
    pub source_record_id: String,
}

/// The complete admitted answer for one dataset version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskWarningSet {
    pub rows: Vec<RiskWarningInterval>,
    pub coverage: Vec<CoverageWindow>,
    pub pending_sources: Vec<PendingSource>,
    /// SH events the calendar cannot place yet (D-84). They write no row; the
    /// count and a sample travel in the item/registration evidence.
    pub sh_deferred: Vec<DeferredShEvent>,
}

impl RiskWarningSet {
    /// The rows that stand for a same-day conflict (D-81 R5): the code and the
    /// day whose state the source cannot settle.
    pub fn same_day_conflicts(&self) -> Vec<&RiskWarningInterval> {
        let mut conflicts: Vec<&RiskWarningInterval> = self
            .rows
            .iter()
            .filter(|row| row.status == RiskStatus::MarkerOnlyUnresolved)
            .collect();
        conflicts.sort_by(|left, right| {
            (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
                right.exchange,
                right.code.as_str(),
                right.effective_from,
            ))
        });
        conflicts
    }

    /// Item evidence of the conflicts: the count and the first
    /// [`CONFLICT_SAMPLE_LIMIT`] `(code, day)` pairs. Both the daily item and
    /// the registration evidence derive it from the rows, so a replay that
    /// rebuilds the rows rebuilds the evidence too.
    pub fn same_day_conflict_evidence(&self) -> serde_json::Value {
        let conflicts = self.same_day_conflicts();
        serde_json::json!({
            "count": conflicts.len(),
            "codes": conflicts
                .iter()
                .map(|row| (row.exchange, row.code.as_str()))
                .collect::<BTreeSet<_>>()
                .len(),
            "sample_limit": CONFLICT_SAMPLE_LIMIT,
            "sample": conflicts
                .iter()
                .take(CONFLICT_SAMPLE_LIMIT)
                .map(|row| serde_json::json!({
                    "exchange": row.exchange.as_str(),
                    "code": row.code,
                    "day": row.effective_from.to_string(),
                    "kind": row.raw_status_code,
                    "source_record_id": row.source_record_id,
                }))
                .collect::<Vec<_>>(),
            "policy": "D-81 R5: a same-day conflict is written as marker_only_unresolved \
                for that code on that day only; every other code is published and the \
                state around the conflict is unknown, never normal",
        })
    }

    pub fn observed_windows(&self) -> usize {
        self.coverage
            .iter()
            .filter(|window| window.observed)
            .count()
    }

    pub fn not_observed_windows(&self) -> usize {
        self.coverage
            .iter()
            .filter(|window| !window.observed)
            .count()
    }
}

/// The risk-warning adapter's answer for one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskWarningSource {
    Admitted(RiskWarningSet),
    Pending(PendingSource),
}

/// The adapter boundary (§16 PR-3/PR-8): one function, no trait.
///
/// `Some(rows)` is an admitted, generation-frozen row set. `None` is not "no
/// warnings": it is the `pending_source` state, and the dataset then carries
/// zero rows.
pub fn risk_warning_source(rows: Option<Vec<RiskWarningInterval>>) -> RiskWarningSource {
    match rows {
        Some(rows) => RiskWarningSource::Admitted(RiskWarningSet {
            rows,
            coverage: Vec::new(),
            pending_sources: Vec::new(),
            sh_deferred: Vec::new(),
        }),
        None => RiskWarningSource::Pending(PendingSource {
            dataset: DATASET_KIND,
            source: "exchange_risk_warning_announcement",
            reason: "no admitted ST/risk-warning source is frozen for this generation \
                     (#308 PR-10); zero rows are written and no 'normal' state is inferred",
        }),
    }
}

/// Why a parsed event stream cannot become a stored interval set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("a risk-warning interval ends before it starts")]
    Range,
    #[error("two risk-warning intervals overlap for one code")]
    Overlap,
    #[error("two risk-warning rows share the identity (exchange, code, effective_from)")]
    Duplicate,
    #[error("a risk-warning row carries no source_kind")]
    SourceKind,
    #[error("a risk-warning row has a status that is not a warning interval")]
    Status,
    #[error("a short-name change chain does not connect before_name to the prior after_name")]
    BrokenChain,
    #[error("two implementation events on one day disagree about the warning status")]
    SameDayStatusConflict,
    #[error("a source event has no usable effective day")]
    UnknownEffectiveDay,
    #[error("a source event names no 6-digit A-share code")]
    Code,
}

/// Assemble SZ intervals from a dated short-name change chain.
///
/// Only a row's `after_name` opens or closes a state; the first row's
/// `before_name` is accepted as the chain's starting point but is never turned
/// into an interval because its start day is unknown. A warning that was
/// already open before the table's first row is therefore not fabricated: the
/// adapter counts it as an unbounded prefix instead.
///
/// D-81 task 49: Normal intervals are written as the complement of ST/warning
/// intervals, bounded by coverage and delisting dates. `delisting_dates` maps
/// code to effective_to from security_master; codes not in the map will not
/// write normal intervals (fail-closed).
pub fn assemble_sz(
    rows: &[SzNameChange],
    mut coverage: Vec<CoverageWindow>,
    delisting_dates: &BTreeMap<String, NaiveDate>,
) -> Result<RiskWarningSet, Error> {
    let mut by_code: BTreeMap<&str, Vec<&SzNameChange>> = BTreeMap::new();
    for row in rows {
        check_code(&row.code)?;
        by_code.entry(row.code.as_str()).or_default().push(row);
    }

    // D-81 task 49: Store changes for later use in normal interval computation
    let code_changes: Vec<(String, Vec<&SzNameChange>)> = by_code
        .iter()
        .map(|(code, changes)| (code.to_string(), changes.clone()))
        .collect();

    let mut intervals = Vec::new();
    for (code, mut changes) in by_code {
        changes.sort_by_key(|change| change.day);
        // `None` is "the chain's start" or "the name after a same-day
        // conflict": the next row's `before_name` is accepted as the anchor.
        let mut current_name: Option<&str> = None;
        let mut open: Option<RiskWarningInterval> = None;
        let mut index = 0usize;
        while index < changes.len() {
            let day = changes[index].day;
            let mut end = index + 1;
            while end < changes.len() && changes[end].day == day {
                end += 1;
            }
            let group = &changes[index..end];
            index = end;
            if group.len() > 1 {
                // D-81 R5: two changes on one day leave that day (and the name
                // after it) unresolved for this code only. A group that does
                // not even connect to the chain is still broken.
                if let Some(name) = current_name
                    && !group.iter().any(|change| change.before_name == name)
                {
                    return Err(Error::BrokenChain);
                }
                close_at(&mut open, &mut intervals, day);
                intervals.push(sz_conflict_row(code, group, day));
                current_name = None;
                continue;
            }
            let change = group[0];
            if current_name.is_some_and(|name| name != change.before_name) {
                return Err(Error::BrokenChain);
            }
            let before = warning_status(&change.before_name);
            let after = warning_status(&change.after_name);
            if (before != after || after.is_none())
                && let Some(mut current) = open.take()
                && change.day > current.effective_from
            {
                current.effective_to = Some(change.day);
                intervals.push(current);
            }
            if let Some(status) = after
                && open.is_none()
            {
                open = Some(interval_from_sz(code, change, status));
            }
            current_name = Some(change.after_name.as_str());
        }
        if let Some(interval) = open {
            intervals.push(interval);
        }
    }

    // D-81 task 49: Add Normal intervals for Shenzhen codes
    let warning_intervals_clone = intervals.clone();
    for (code, changes) in &code_changes {
        add_normal_intervals_sz(
            code,
            changes,
            &warning_intervals_clone,
            &coverage,
            delisting_dates,
            &mut intervals,
        );
    }

    coverage.sort_by(|left, right| (left.from, right.to).cmp(&(right.from, right.to)));
    Ok(RiskWarningSet {
        rows: intervals,
        coverage,
        pending_sources: Vec::new(),
        sh_deferred: Vec::new(),
    })
}

/// D-84: the SH effective day is the trading day after the first trading day on
/// or after the announcement day. It is `None` when the calendar does not reach
/// that second day: a weekend rule would guess wrong across holidays (the
/// 2020-04-29 May Day case), so the event waits for the calendar instead.
///
/// `calendar` is the stored `trading_days` list, strictly ascending. The rule is
/// owner-risk (`OWNER_RISK_ACCEPTED`), checked against GP29: 607/617 paired SH
/// implementations and 540/544 paired revocations match it; the revoke report
/// is `docs/tasks/revoke-effective-20261106.md`.
pub fn sh_effective_day(announced: NaiveDate, calendar: &[NaiveDate]) -> Option<NaiveDate> {
    let first = calendar.partition_point(|day| *day < announced);
    calendar.get(first.checked_add(1)?).copied()
}

/// Apply D-84 to the parsed SH events: an announcement-anchored event moves to
/// [`sh_effective_day`], an explicit title date stays as parsed, and an event
/// the calendar cannot place is returned as deferred (it writes no row).
///
/// `calendar` is `None` only for a `--scope` replay without a database, where no
/// SH row can be placed; every event is then deferred instead of being written
/// with the announcement day.
pub fn place_sh_events(
    events: Vec<RiskWarningStatusEvent>,
    calendar: Option<&[NaiveDate]>,
) -> (Vec<RiskWarningStatusEvent>, Vec<DeferredShEvent>) {
    let mut placed = Vec::with_capacity(events.len());
    let mut deferred = Vec::new();
    for mut event in events {
        if event.day_is_explicit {
            placed.push(event);
            continue;
        }
        match calendar.and_then(|days| sh_effective_day(event.day, days)) {
            Some(day) => {
                event.day = day;
                placed.push(event);
            }
            None => deferred.push(DeferredShEvent {
                code: event.code,
                announced: event.day,
                action: event.action,
                status: event.status,
                source_record_id: event.source_record_id,
            }),
        }
    }
    (placed, deferred)
}

/// Content digest of the database input the SH effective-day rule reads: the
/// trading calendar days. The source store alone cannot name them, so a replay
/// registers this digest next to the rows: when the calendar advances, a
/// `--verify-only` comparison can say `inputs_changed` instead of treating the
/// difference as store corruption.
pub fn inputs_digest(calendar: &[NaiveDate]) -> String {
    let mut payload = Vec::with_capacity(32 + calendar.len() * 8);
    payload.extend_from_slice(b"risk-warning-inputs-v1\0");
    payload.extend_from_slice(&(calendar.len() as u64).to_be_bytes());
    for day in calendar {
        payload.extend_from_slice(&crate::compact_market::epoch_day(*day).to_be_bytes());
    }
    crate::hash::sha256(&payload)
}

/// Assemble SH intervals from event-level announcement metadata.
///
/// D-58 admits only implementation/revocation plus the announcement day for
/// SH. The effective day is the D-84 trading day after the first trading day on
/// or after the announcement day (`OWNER_RISK_ACCEPTED`); an explicit date in
/// the title wins and is never moved. Placement happens in [`place_sh_events`]
/// before this function, which never invents a dated name history. A
/// `not_observed` coverage window cuts every open interval at its start, so a
/// missing announcement can only produce an unknown gap, never a warning claimed
/// across it.
pub fn assemble_sh(
    events: &[RiskWarningStatusEvent],
    gaps: &[(NaiveDate, NaiveDate)],
    mut coverage: Vec<CoverageWindow>,
) -> Result<RiskWarningSet, Error> {
    let mut gap_starts = BTreeSet::new();
    for (from, _) in gaps {
        gap_starts.insert(*from);
    }
    let mut by_code: BTreeMap<&str, Vec<&RiskWarningStatusEvent>> = BTreeMap::new();
    for event in events {
        check_code(&event.code)?;
        by_code.entry(event.code.as_str()).or_default().push(event);
    }
    let mut intervals = Vec::new();
    for (_code, mut events) in by_code {
        events.sort_by(|left, right| {
            (left.day, action_rank(left.action)).cmp(&(right.day, action_rank(right.action)))
        });
        let steps = sh_steps(&events)?;
        let mut open: Option<RiskWarningInterval> = None;
        let mut cursor = 0usize;
        let code_gaps: Vec<NaiveDate> = gap_starts
            .iter()
            .copied()
            .filter(|day| steps.iter().any(|step| step.day() > *day))
            .collect();
        for day in code_gaps {
            while cursor < steps.len() && steps[cursor].day() <= day {
                apply_sh_step(&mut open, &mut intervals, &steps[cursor]);
                cursor += 1;
            }
            close_at(&mut open, &mut intervals, day);
        }
        for step in &steps[cursor..] {
            apply_sh_step(&mut open, &mut intervals, step);
        }
        if let Some(interval) = open {
            intervals.push(interval);
        }
    }
    coverage.sort_by_key(|left| (left.from, left.to));
    Ok(RiskWarningSet {
        rows: intervals,
        coverage,
        pending_sources: Vec::new(),
        sh_deferred: Vec::new(),
    })
}

/// One thing that happens to a code's warning on one day: a single event, or
/// the several events of a day the source contradicts itself on.
enum ShStep<'a> {
    Event(&'a RiskWarningStatusEvent),
    /// D-81 R5: an implementation and a revocation on one effective day. The
    /// state on that day is unknown, and so is the one after it until a later
    /// event states one.
    Conflict {
        day: NaiveDate,
        events: Vec<&'a RiskWarningStatusEvent>,
    },
}

impl ShStep<'_> {
    fn day(&self) -> NaiveDate {
        match self {
            Self::Event(event) => event.day,
            Self::Conflict { day, .. } => *day,
        }
    }
}

/// Group one code's day-sorted events into steps. Two implementations of
/// different statuses on one day, and an implementation without a status, are
/// still errors for the whole set; an implementation next to a revocation
/// isolates the code (D-81 R5, revising D-68 item 4).
fn sh_steps<'a>(events: &[&'a RiskWarningStatusEvent]) -> Result<Vec<ShStep<'a>>, Error> {
    let mut steps = Vec::with_capacity(events.len());
    let mut index = 0usize;
    while index < events.len() {
        let day = events[index].day;
        let mut end = index + 1;
        while end < events.len() && events[end].day == day {
            end += 1;
        }
        let mut implement = None;
        let mut revoke = false;
        for event in &events[index..end] {
            match event.action {
                RiskWarningAction::Implement => {
                    let Some(status) = event.status else {
                        return Err(Error::Status);
                    };
                    if implement.is_some_and(|current| current != status) {
                        return Err(Error::SameDayStatusConflict);
                    }
                    implement = Some(status);
                }
                RiskWarningAction::Revoke => revoke = true,
            }
        }
        if implement.is_some() && revoke {
            steps.push(ShStep::Conflict {
                day,
                events: events[index..end].to_vec(),
            });
        } else {
            steps.extend(events[index..end].iter().map(|event| ShStep::Event(event)));
        }
        index = end;
    }
    Ok(steps)
}

fn apply_sh_step(
    open: &mut Option<RiskWarningInterval>,
    intervals: &mut Vec<RiskWarningInterval>,
    step: &ShStep<'_>,
) {
    match step {
        ShStep::Event(event) => apply_sh_event(open, intervals, event),
        ShStep::Conflict { day, events } => {
            // Whatever was open ran up to the conflict day; from there the
            // state is unknown, so the warning is not carried across it.
            close_at(open, intervals, *day);
            intervals.push(sh_conflict_row(*day, events));
        }
    }
}

fn apply_sh_event(
    open: &mut Option<RiskWarningInterval>,
    intervals: &mut Vec<RiskWarningInterval>,
    event: &RiskWarningStatusEvent,
) {
    match event.action {
        RiskWarningAction::Implement => {
            let Some(status) = event.status else {
                return;
            };
            if open
                .as_ref()
                .is_some_and(|current| current.status == status)
            {
                return;
            }
            close_at(open, intervals, event.day);
            *open = Some(interval_from_event(event, status));
        }
        RiskWarningAction::Revoke => close_at(open, intervals, event.day),
    }
}

fn close_at(
    open: &mut Option<RiskWarningInterval>,
    intervals: &mut Vec<RiskWarningInterval>,
    day: NaiveDate,
) {
    let Some(mut current) = open.take() else {
        return;
    };
    if day > current.effective_from {
        current.effective_to = Some(day);
        intervals.push(current);
    }
}

/// The latest of the rows' knowledge times, `None` when any row has none: a
/// conflict is known once every row that makes it is known.
fn latest_known_at(days: impl Iterator<Item = Option<NaiveDate>>) -> Option<NaiveDate> {
    let mut latest: Option<NaiveDate> = None;
    for day in days {
        let day = day?;
        latest = Some(latest.map_or(day, |current| current.max(day)));
    }
    latest
}

/// One day of `marker_only_unresolved` for a code the source contradicts
/// itself on. It lasts one calendar day: the days after it carry no row, which
/// reads as `unknown` like every other uncovered day.
#[allow(clippy::too_many_arguments)]
fn conflict_row(
    exchange: Exchange,
    code: &str,
    day: NaiveDate,
    raw_status_code: &str,
    announced_at: Option<String>,
    known_at: Option<NaiveDate>,
    source_kind: &str,
    mut records: Vec<(String, String)>,
) -> RiskWarningInterval {
    // Order independent: the row must not depend on how the source listed the
    // day's rows, or a replay could write a different row than the daily item.
    records.sort();
    let source_document_sha256 = records.first().map(|(_, sha)| sha.clone());
    let ids: Vec<String> = records.into_iter().map(|(id, _)| id).collect();
    RiskWarningInterval {
        exchange,
        code: code.to_string(),
        effective_from: day,
        effective_to: day.succ_opt(),
        raw_status_code: raw_status_code.to_string(),
        status: RiskStatus::MarkerOnlyUnresolved,
        announced_at,
        known_at,
        source_kind: source_kind.to_string(),
        source_record_id: Some(ids.join(",")),
        source_document_sha256,
    }
}

fn sz_conflict_row(code: &str, group: &[&SzNameChange], day: NaiveDate) -> RiskWarningInterval {
    conflict_row(
        Exchange::Sz,
        code,
        day,
        RAW_SAME_DAY_NAME_CHANGES,
        None,
        latest_known_at(group.iter().map(|change| change.known_at)),
        "szse_name_history",
        group
            .iter()
            .map(|change| {
                (
                    change.source_record_id.clone(),
                    change.source_document_sha256.clone(),
                )
            })
            .collect(),
    )
}

fn sh_conflict_row(day: NaiveDate, events: &[&RiskWarningStatusEvent]) -> RiskWarningInterval {
    let announced_at = events
        .iter()
        .map(|event| event.announced_at.clone())
        .collect::<Option<Vec<String>>>()
        .and_then(|days| days.into_iter().max());
    conflict_row(
        Exchange::Sh,
        &events[0].code,
        day,
        RAW_SAME_DAY_ACTIONS,
        announced_at,
        latest_known_at(events.iter().map(|event| event.known_at)),
        "exchange_announcement",
        events
            .iter()
            .map(|event| {
                (
                    event.source_record_id.clone(),
                    event.source_document_sha256.clone(),
                )
            })
            .collect(),
    )
}

fn interval_from_sz(code: &str, change: &SzNameChange, status: RiskStatus) -> RiskWarningInterval {
    RiskWarningInterval {
        exchange: Exchange::Sz,
        code: code.to_string(),
        effective_from: change.day,
        effective_to: None,
        raw_status_code: raw_status(status).to_string(),
        status,
        announced_at: None,
        // §5.3/Q4: the knowledge time is the snapshot day, not the change
        // day; the adapter resolves it, this module never guesses it.
        known_at: change.known_at,
        source_kind: "szse_name_history".to_string(),
        source_record_id: Some(change.source_record_id.clone()),
        source_document_sha256: Some(change.source_document_sha256.clone()),
    }
}

fn interval_from_event(event: &RiskWarningStatusEvent, status: RiskStatus) -> RiskWarningInterval {
    RiskWarningInterval {
        exchange: Exchange::Sh,
        code: event.code.clone(),
        effective_from: event.day,
        effective_to: None,
        raw_status_code: event.raw_status_code.clone(),
        status,
        announced_at: event.announced_at.clone(),
        known_at: event.known_at,
        source_kind: event.source_kind.clone(),
        source_record_id: Some(event.source_record_id.clone()),
        source_document_sha256: Some(event.source_document_sha256.clone()),
    }
}

fn warning_status(name: &str) -> Option<RiskStatus> {
    let compact: String = name.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.contains("*ST") {
        Some(RiskStatus::DelistingRiskWarning)
    } else if compact.contains("ST") {
        Some(RiskStatus::OtherRiskWarning)
    } else {
        None
    }
}

/// D-81 task 49: Normalize and strip whitespace from a name.
/// Uses NFKC normalization to handle full-width characters.
fn normalize_name(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfkc().filter(|ch| !ch.is_whitespace()).collect()
}

/// D-81 task 49: Check if a name has PT prefix (after normalization and stripping whitespace).
/// Examples: "PT凯地", "PT 凯 地", "PT 南 洋", "ＰＴ凯地" (full-width)
fn has_pt_prefix(name: &str) -> bool {
    let normalized = normalize_name(name);
    normalized.starts_with("PT")
}

/// D-81 task 49: Check if a name ends with "退" or "退市".
/// Examples: "国华退", "*ST海润退市", "国华退 " (with trailing space)
fn has_tui_suffix(name: &str) -> bool {
    let normalized = normalize_name(name);
    normalized.ends_with("退市") || normalized.ends_with("退")
}

/// D-81 task 49: Check if a name starts with * but has no ST marker.
/// Examples: "*金亚", "*欣泰" (60 bar-days in data), "＊金亚" (full-width *)
/// These are not ST and not plain Normal either.
fn has_star_without_st(name: &str) -> bool {
    let normalized = normalize_name(name);
    normalized.starts_with('*') && !normalized.contains("ST")
}

/// D-81 task 49: Does the (normalized) name carry an ST marker?
/// Separate from `warning_status` on purpose: `warning_status` classifies the
/// ST interval rows and must not change; this only decides `normal`.
fn has_st_marker(name: &str) -> bool {
    normalize_name(name).contains("ST")
}

/// D-81 task 49: Check if a name is "plain" - eligible for Normal status.
/// A plain name has no ST, no PT prefix, no 退 suffix, and no * without ST.
/// Each predicate normalizes via NFKC, so full-width markers count too.
fn is_plain_name(name: &str) -> bool {
    !has_st_marker(name)
        && !has_pt_prefix(name)
        && !has_tui_suffix(name)
        && !has_star_without_st(name)
}

/// D-81 task 49: For Shenzhen codes only, write `normal` as the complement
/// of ST/warning intervals within the name history coverage.
///
/// Rules:
/// - Only write for codes that have at least one name row
/// - Start from the first name row date (not before)
/// - End at the coverage endpoint (from coverage windows) or delisting date, whichever is earlier
/// - Exclude days with ST/warning intervals
/// - Exclude PT prefix days, 退 suffix days, * without ST days
/// - Exclude conflict days and days after conflict until next event
/// - Known_at matches the name row that covers each normal interval
///
/// `delisting_dates`: map from code to delisting date (effective_to from security_master).
/// Codes not in this map will not write normal intervals (fail-closed).
fn add_normal_intervals_sz(
    code: &str,
    changes: &[&SzNameChange],
    warning_intervals: &[RiskWarningInterval],
    coverage: &[CoverageWindow],
    delisting_dates: &BTreeMap<String, NaiveDate>,
    all_intervals: &mut Vec<RiskWarningInterval>,
) {
    if changes.is_empty() {
        return; // No name rows -> no normal
    }

    // The caller's `code_changes` preserves source order; the segments below
    // need chronological order, so sort a local copy.
    let mut changes: Vec<&SzNameChange> = changes.to_vec();
    changes.sort_by_key(|change| change.day);

    // Fail-closed: only write normal if we have delisting date for this code
    let delisting_date = match delisting_dates.get(code) {
        Some(&date) => date,
        None => return, // No delisting date -> no normal (fail-closed)
    };

    // Find coverage endpoint for Shenzhen from coverage windows
    let coverage_end = coverage
        .iter()
        .filter(|w| w.exchange == Exchange::Sz && w.source_kind == "szse_name_history")
        .map(|w| w.to)
        .max();
    let Some(coverage_end) = coverage_end else {
        return; // No coverage -> no normal
    };

    // Normal intervals are half-open `[from, to)`.
    // `coverage_end` is inclusive (the last rename day), so its exclusive
    // bound is `coverage_end + 1`. `delisting_date` (security_master
    // `effective_to`) is already exclusive: the delisting day is not a member
    // day. Normal therefore stops at the earlier of the two exclusive bounds.
    let coverage_end_exclusive = coverage_end.succ_opt().unwrap_or(coverage_end);
    let effective_end = coverage_end_exclusive.min(delisting_date);

    // Build name segments: periods where a specific name is active
    // and track which ones are plain (eligible for normal)
    #[derive(Debug, Clone)]
    struct NameSegment {
        start: NaiveDate,
        end: Option<NaiveDate>, // None = until next change or coverage end
        is_plain: bool,
        known_at: Option<NaiveDate>,
        // D-81 task 49 审查意见4: 来源于打开这段的那次变更
        source_record_id: String,
        source_document_sha256: String,
    }

    let mut segments: Vec<NameSegment> = Vec::new();
    let first_day = changes.first().unwrap().day;
    let mut index = 0usize;

    while index < changes.len() {
        let day = changes[index].day;
        let mut end = index + 1;
        while end < changes.len() && changes[end].day == day {
            end += 1;
        }
        let group = &changes[index..end];
        index = end;

        if group.len() > 1 {
            // Conflict: close any open segment at the conflict day
            if let Some(last) = segments.last_mut()
                && last.end.is_none()
            {
                last.end = Some(day);
            }
            continue;
        }

        let change = group[0];
        let after_name = change.after_name.as_str();
        let is_plain = is_plain_name(after_name);

        segments.push(NameSegment {
            start: day,
            end: None,
            is_plain,
            known_at: change.known_at,
            source_record_id: change.source_record_id.clone(),
            source_document_sha256: change.source_document_sha256.clone(),
        });

        // Close previous segment if it's still open
        let len = segments.len();
        if len > 1 && segments[len - 2].end.is_none() {
            segments[len - 2].end = Some(day);
        }
    }

    // Close the last segment at effective end (min of coverage and delisting), but only if it's still open
    if let Some(last) = segments.last_mut()
        && last.end.is_none()
    {
        last.end = Some(effective_end);
    }

    // Build occupied days from warning/conflict intervals for this code
    let mut occupied_intervals: Vec<(NaiveDate, NaiveDate)> = Vec::new();
    for interval in warning_intervals {
        if interval.code != code {
            continue;
        }
        let start = interval.effective_from;
        let end = interval.effective_to.unwrap_or(effective_end);
        occupied_intervals.push((start, end));
    }
    occupied_intervals.sort();

    // For each plain name segment, write Normal intervals in the gaps
    for segment in &segments {
        if !segment.is_plain {
            continue; // Skip non-plain names
        }

        // If this segment starts right after a conflict, the period from
        // the previous segment to this one should remain unknown (no normal row).
        // So we only write normal intervals starting from this segment's start.
        let seg_start = segment.start.max(first_day);
        let seg_end = segment.end.unwrap_or(effective_end);
        if seg_end <= seg_start {
            continue;
        }

        // Find gaps in this segment that are not occupied by warnings
        let mut normal_start = seg_start;

        for (occ_start, occ_end) in &occupied_intervals {
            if *occ_end <= seg_start || *occ_start >= seg_end {
                continue; // Occupied interval doesn't overlap this segment
            }

            // Write normal interval before this occupied interval
            let gap_end = (*occ_start).min(seg_end);
            if normal_start < gap_end {
                all_intervals.push(RiskWarningInterval {
                    exchange: Exchange::Sz,
                    code: code.to_string(),
                    effective_from: normal_start,
                    effective_to: Some(gap_end),
                    raw_status_code: "plain".to_string(),
                    status: RiskStatus::Normal,
                    announced_at: None,
                    known_at: segment.known_at,
                    source_kind: "szse_name_history".to_string(),
                    source_record_id: Some(segment.source_record_id.clone()),
                    source_document_sha256: Some(segment.source_document_sha256.clone()),
                });
            }

            // Move start to after this occupied interval
            normal_start = (*occ_end).max(normal_start);
        }

        // Write any remaining normal interval after all occupied intervals
        if normal_start < seg_end {
            all_intervals.push(RiskWarningInterval {
                exchange: Exchange::Sz,
                code: code.to_string(),
                effective_from: normal_start,
                effective_to: Some(seg_end),
                raw_status_code: "plain".to_string(),
                status: RiskStatus::Normal,
                announced_at: None,
                known_at: segment.known_at,
                source_kind: "szse_name_history".to_string(),
                source_record_id: Some(segment.source_record_id.clone()),
                source_document_sha256: Some(segment.source_document_sha256.clone()),
            });
        }
    }
}

fn raw_status(status: RiskStatus) -> &'static str {
    match status {
        RiskStatus::OtherRiskWarning => "ST",
        RiskStatus::DelistingRiskWarning => "*ST",
        _ => "unknown",
    }
}

fn action_rank(action: RiskWarningAction) -> u8 {
    match action {
        RiskWarningAction::Implement => 0,
        RiskWarningAction::Revoke => 1,
    }
}

fn check_code(code: &str) -> Result<(), Error> {
    if code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        Err(Error::Code)
    }
}

/// Validate a row set before it is stored. The DDL already carries the range
/// and status vocabularies; these are the invariants it cannot state.
pub fn validate_set(rows: &[RiskWarningInterval]) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    let mut sorted: Vec<&RiskWarningInterval> = rows.iter().collect();
    sorted.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
    for row in &sorted {
        check_code(&row.code)?;
        if row
            .effective_to
            .is_some_and(|end| end <= row.effective_from)
        {
            return Err(Error::Range);
        }
        if row.source_kind.trim().is_empty() {
            return Err(Error::SourceKind);
        }
        // Normal is admitted only for szse_name_history source (D-81, task 49)
        if row.status == RiskStatus::Normal {
            if row.source_kind != "szse_name_history" {
                return Err(Error::Status);
            }
        } else if !matches!(
            row.status,
            RiskStatus::OtherRiskWarning
                | RiskStatus::DelistingRiskWarning
                | RiskStatus::DelistingArrangement
                | RiskStatus::TerminatedOrDelisted
                | RiskStatus::MarkerOnlyUnresolved
        ) {
            return Err(Error::Status);
        }
        if !seen.insert((row.exchange, row.code.as_str(), row.effective_from)) {
            return Err(Error::Duplicate);
        }
    }
    for pair in sorted.windows(2) {
        let (left, right) = (pair[0], pair[1]);
        if left.exchange != right.exchange || left.code != right.code {
            continue;
        }
        let end = left
            .effective_to
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(9999, 12, 31).expect("9999-12-31 is valid"));
        if right.effective_from < end {
            return Err(Error::Overlap);
        }
    }
    Ok(())
}

/// Content digest of the dataset: `SHA256("risk-warnings-v1" ‖ row_count ‖
/// rows…)`. Source bindings and coverage evidence describe provenance, not the
/// dataset identity; the caller sorts the rows first.
pub fn dataset_digest(rows: &[RiskWarningInterval]) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    let mut p = Vec::with_capacity(64 + rows.len() * 96);
    p.extend_from_slice(b"risk-warnings-v1\0");
    p.extend_from_slice(&(rows.len() as u64).to_be_bytes());
    let mut sorted: Vec<&RiskWarningInterval> = rows.iter().collect();
    sorted.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
    for row in sorted {
        push_text(&mut p, row.exchange.as_str());
        push_text(&mut p, &row.code);
        push_day(&mut p, row.effective_from);
        push_opt_day(&mut p, row.effective_to);
        push_text(&mut p, &row.raw_status_code);
        push_text(&mut p, row.status.as_str());
        push_opt_text(&mut p, row.announced_at.as_deref());
        push_opt_day(&mut p, row.known_at);
        push_text(&mut p, &row.source_kind);
        push_opt_text(&mut p, row.source_record_id.as_deref());
        push_opt_text(&mut p, row.source_document_sha256.as_deref());
    }
    Sha256::digest(&p).into()
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
mod tests {
    use super::*;
    use chrono::Datelike;

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid test day")
    }

    fn change(code: &str, when: &str, before: &str, after: &str) -> SzNameChange {
        SzNameChange {
            code: code.into(),
            day: NaiveDate::parse_from_str(when, "%Y-%m-%d").expect("valid test day"),
            before_name: before.into(),
            after_name: after.into(),
            source_record_id: format!("{when}|{code}|{after}"),
            source_document_sha256: "a".repeat(64),
            known_at: None,
        }
    }

    // Helper to create delisting dates for tests
    // Default: far future (9999-12-31) means "not delisted"
    fn delisting_dates(codes: &[(&str, Option<NaiveDate>)]) -> BTreeMap<String, NaiveDate> {
        codes
            .iter()
            .map(|(code, date)| (code.to_string(), date.unwrap_or_else(|| day(9999, 12, 31))))
            .collect()
    }

    #[test]
    fn sz_chain_opens_and_closes_st_interval() {
        let rows = vec![
            change("000001", "1998-04-28", "深发展A", "ST深发展"),
            change("000001", "1999-04-28", "ST深发展", "深发展A"),
        ];
        let set = assemble_sz(&rows, Vec::new(), &BTreeMap::new()).expect("chain assembles");
        assert_eq!(set.rows.len(), 1);
        assert_eq!(set.rows[0].status, RiskStatus::OtherRiskWarning);
        assert_eq!(set.rows[0].effective_from, day(1998, 4, 28));
        assert_eq!(set.rows[0].effective_to, Some(day(1999, 4, 28)));
    }

    /// D-81 R5: two changes of one code on one day no longer fail the set.
    /// The code is `marker_only_unresolved` on that day, the ST interval that
    /// was open ends there, and another code is assembled as usual.
    #[test]
    fn sz_same_day_conflict_isolates_the_code() {
        let rows = vec![
            change("000001", "1998-01-05", "深发展A", "ST深发展"),
            change("000001", "1998-04-28", "ST深发展", "深发展A"),
            change("000001", "1998-04-28", "深发展A", "ST深发展"),
            change("000002", "1998-02-02", "万科A", "ST万科"),
        ];
        let set = assemble_sz(&rows, Vec::new(), &BTreeMap::new())
            .expect("a conflict is isolated, not fatal");
        validate_set(&set.rows).expect("the rows are a valid interval set");
        let by_code = |code: &str| -> Vec<&RiskWarningInterval> {
            set.rows.iter().filter(|row| row.code == code).collect()
        };
        let conflicted = by_code("000001");
        assert_eq!(conflicted.len(), 2);
        assert_eq!(conflicted[0].status, RiskStatus::OtherRiskWarning);
        assert_eq!(conflicted[0].effective_to, Some(day(1998, 4, 28)));
        assert_eq!(conflicted[1].status, RiskStatus::MarkerOnlyUnresolved);
        assert_eq!(conflicted[1].effective_from, day(1998, 4, 28));
        assert_eq!(conflicted[1].effective_to, Some(day(1998, 4, 29)));
        assert_eq!(conflicted[1].raw_status_code, RAW_SAME_DAY_NAME_CHANGES);
        let normal = by_code("000002");
        assert_eq!(normal.len(), 1);
        assert_eq!(normal[0].status, RiskStatus::OtherRiskWarning);
        assert_eq!(normal[0].effective_to, None);
        assert_eq!(set.same_day_conflicts().len(), 1);
    }

    /// The conflict does not make the chain after it unreadable: the next row
    /// anchors the name again, and a group that does not connect to the chain
    /// is still `BrokenChain`.
    #[test]
    fn sz_chain_continues_after_a_conflict_and_still_breaks_when_unconnected() {
        let rows = vec![
            change("000001", "1998-04-28", "深发展A", "ST深发展"),
            change("000001", "1998-04-28", "深发展A", "*ST深发展"),
            change("000001", "1998-09-01", "*ST深发展", "深发展A"),
        ];
        let set = assemble_sz(&rows, Vec::new(), &BTreeMap::new()).expect("chain continues");
        assert_eq!(set.rows.len(), 1);
        assert_eq!(set.rows[0].status, RiskStatus::MarkerOnlyUnresolved);

        let unconnected = vec![
            change("000001", "1998-01-05", "深发展A", "深发展B"),
            change("000001", "1998-04-28", "甲", "ST乙"),
            change("000001", "1998-04-28", "丙", "丁"),
        ];
        assert_eq!(
            assemble_sz(&unconnected, Vec::new(), &BTreeMap::new()),
            Err(Error::BrokenChain)
        );
    }

    #[test]
    fn no_events_is_not_a_normal_row() {
        let set = assemble_sz(&[], Vec::new(), &BTreeMap::new()).expect("empty input is valid");
        assert!(set.rows.is_empty());
        assert_eq!(set.not_observed_windows(), 0);
    }

    #[test]
    fn sh_events_make_an_interval_and_a_gap_cuts_it() {
        let events = vec![
            RiskWarningStatusEvent {
                code: "600001".into(),
                day: day(2020, 1, 2),
                day_is_explicit: false,
                action: RiskWarningAction::Implement,
                status: Some(RiskStatus::OtherRiskWarning),
                announced_at: Some("2020-01-02".into()),
                known_at: Some(day(2020, 1, 2)),
                raw_status_code: "implement:other_risk_warning".into(),
                source_kind: "exchange_announcement".into(),
                source_record_id: "1".into(),
                source_document_sha256: "b".repeat(64),
            },
            RiskWarningStatusEvent {
                code: "600001".into(),
                day: day(2020, 5, 6),
                day_is_explicit: false,
                action: RiskWarningAction::Revoke,
                status: None,
                announced_at: Some("2020-05-06".into()),
                known_at: Some(day(2020, 5, 6)),
                raw_status_code: "revoke".into(),
                source_kind: "exchange_announcement".into(),
                source_record_id: "2".into(),
                source_document_sha256: "b".repeat(64),
            },
        ];
        let set = assemble_sh(&events, &[], Vec::new()).expect("events assemble");
        assert_eq!(set.rows.len(), 1);
        assert_eq!(set.rows[0].effective_to, Some(day(2020, 5, 6)));
        let cut = assemble_sh(&events, &[(day(2020, 3, 1), day(2020, 3, 31))], Vec::new())
            .expect("gap cuts the interval");
        assert_eq!(cut.rows[0].effective_to, Some(day(2020, 3, 1)));
    }

    fn weekdays(from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
        let mut days = Vec::new();
        let mut day = from;
        while day <= to {
            if day.weekday().number_from_monday() <= 5 {
                days.push(day);
            }
            day = day.succ_opt().unwrap();
        }
        days
    }

    /// D-84: Thursday announcement → Friday, Friday → Monday, Saturday →
    /// Tuesday. The rule is not "announcement + 1 calendar day".
    #[test]
    fn sh_effective_day_uses_the_next_trading_day() {
        let calendar = weekdays(day(2024, 5, 1), day(2024, 5, 31));
        assert_eq!(
            sh_effective_day(day(2024, 5, 9), &calendar),
            Some(day(2024, 5, 10))
        );
        assert_eq!(
            sh_effective_day(day(2024, 5, 10), &calendar),
            Some(day(2024, 5, 13))
        );
        assert_eq!(
            sh_effective_day(day(2024, 5, 11), &calendar),
            Some(day(2024, 5, 14))
        );
    }

    /// D-84: a holiday is read from the calendar, not guessed from the weekend.
    /// 2020-04-30 is the last trading day before May Day; the next trading day
    /// is 2020-05-06, so the effective day is 2020-05-06.
    #[test]
    fn sh_effective_day_crosses_a_holiday_from_the_calendar() {
        let calendar = vec![
            day(2020, 4, 28),
            day(2020, 4, 29),
            day(2020, 4, 30),
            day(2020, 5, 6),
            day(2020, 5, 7),
        ];
        assert_eq!(
            sh_effective_day(day(2020, 4, 30), &calendar),
            Some(day(2020, 5, 6))
        );
        // Saturday before a Monday holiday: the first trading day is Tuesday,
        // the effective day is Wednesday. A weekend rule would say Tuesday.
        let holiday_monday = vec![day(2024, 5, 10), day(2024, 5, 14), day(2024, 5, 15)];
        assert_eq!(
            sh_effective_day(day(2024, 5, 11), &holiday_monday),
            Some(day(2024, 5, 15))
        );
    }

    /// D-84: the day after the first trading day must be on the calendar. A
    /// calendar that ends on the announcement day (or before it) cannot place
    /// the event; it is deferred, never written on the announcement day.
    #[test]
    fn sh_effective_day_is_unknown_past_the_calendar_end() {
        let calendar = weekdays(day(2024, 5, 1), day(2024, 5, 10));
        assert_eq!(sh_effective_day(day(2024, 5, 10), &calendar), None);
        assert_eq!(sh_effective_day(day(2024, 5, 11), &calendar), None);
        assert_eq!(sh_effective_day(day(2024, 6, 1), &calendar), None);
    }

    /// D-84: implementation and revocation move by the same rule; an explicit
    /// title date is kept; an event past the calendar is deferred and writes no
    /// row.
    #[test]
    fn place_sh_events_shifts_both_actions_and_defers_past_the_calendar() {
        let calendar = weekdays(day(2024, 5, 1), day(2024, 5, 31));
        let mut explicit = sh_event(
            "600003",
            day(2024, 5, 6),
            RiskWarningAction::Implement,
            Some(RiskStatus::OtherRiskWarning),
            "explicit",
        );
        explicit.day_is_explicit = true;
        let events = vec![
            sh_event(
                "600001",
                day(2024, 5, 9),
                RiskWarningAction::Implement,
                Some(RiskStatus::OtherRiskWarning),
                "impl",
            ),
            sh_event(
                "600001",
                day(2024, 5, 10),
                RiskWarningAction::Revoke,
                None,
                "revoke",
            ),
            explicit,
            sh_event(
                "600004",
                day(2024, 6, 3),
                RiskWarningAction::Implement,
                Some(RiskStatus::OtherRiskWarning),
                "late",
            ),
        ];
        let (placed, deferred) = place_sh_events(events, Some(&calendar));
        assert_eq!(placed.len(), 3);
        assert_eq!(placed[0].day, day(2024, 5, 10));
        assert_eq!(placed[1].day, day(2024, 5, 13));
        assert_eq!(placed[2].day, day(2024, 5, 6));
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].code, "600004");
        assert_eq!(deferred[0].announced, day(2024, 6, 3));

        // `known_at` stays the announcement day: the rule moves when the state
        // starts, not when we knew about it.
        assert_eq!(placed[0].known_at, Some(day(2024, 5, 9)));

        let set = assemble_sh(&placed, &[], Vec::new()).expect("placed events assemble");
        let row = &set.rows[0];
        assert_eq!(row.effective_from, day(2024, 5, 10));
        assert_eq!(row.effective_to, Some(day(2024, 5, 13)));
    }

    /// D-84: without a calendar (a `--scope` replay without a database) no SH
    /// row is placed; the events are deferred rather than written on the
    /// announcement day.
    #[test]
    fn place_sh_events_without_a_calendar_defers_every_announcement() {
        let events = vec![sh_event(
            "600001",
            day(2024, 5, 9),
            RiskWarningAction::Implement,
            Some(RiskStatus::OtherRiskWarning),
            "impl",
        )];
        let (placed, deferred) = place_sh_events(events, None);
        assert!(placed.is_empty());
        assert_eq!(deferred.len(), 1);
    }

    fn sh_event(
        code: &str,
        when: NaiveDate,
        action: RiskWarningAction,
        status: Option<RiskStatus>,
        id: &str,
    ) -> RiskWarningStatusEvent {
        RiskWarningStatusEvent {
            code: code.into(),
            day: when,
            day_is_explicit: false,
            action,
            status,
            announced_at: Some(when.to_string()),
            known_at: Some(when),
            raw_status_code: match action {
                RiskWarningAction::Implement => "implement:other_risk_warning".into(),
                RiskWarningAction::Revoke => "revoke".into(),
            },
            source_kind: "exchange_announcement".into(),
            source_record_id: id.into(),
            source_document_sha256: "b".repeat(64),
        }
    }

    /// D-81 R5 (revising D-68 item 4): a conflicting code is
    /// `marker_only_unresolved` on the conflict day and a normal code in the
    /// same set is published.
    #[test]
    fn sh_same_day_implement_and_revoke_isolates_the_code() {
        let other = Some(RiskStatus::OtherRiskWarning);
        let events = vec![
            sh_event(
                "600001",
                day(2020, 1, 2),
                RiskWarningAction::Implement,
                other,
                "a1",
            ),
            sh_event(
                "600001",
                day(2022, 1, 29),
                RiskWarningAction::Implement,
                other,
                "a2",
            ),
            sh_event(
                "600001",
                day(2022, 1, 29),
                RiskWarningAction::Revoke,
                None,
                "a3",
            ),
            sh_event(
                "600001",
                day(2022, 3, 1),
                RiskWarningAction::Implement,
                other,
                "a4",
            ),
            sh_event(
                "600002",
                day(2021, 6, 1),
                RiskWarningAction::Implement,
                other,
                "b1",
            ),
            sh_event(
                "600002",
                day(2021, 9, 1),
                RiskWarningAction::Revoke,
                None,
                "b2",
            ),
        ];
        let set = assemble_sh(&events, &[], Vec::new()).expect("the dataset is published");
        validate_set(&set.rows).expect("the rows are a valid interval set");
        let of = |code: &str| -> Vec<(NaiveDate, Option<NaiveDate>, RiskStatus)> {
            set.rows
                .iter()
                .filter(|row| row.code == code)
                .map(|row| (row.effective_from, row.effective_to, row.status))
                .collect()
        };
        assert_eq!(
            of("600001"),
            vec![
                (
                    day(2020, 1, 2),
                    Some(day(2022, 1, 29)),
                    RiskStatus::OtherRiskWarning
                ),
                (
                    day(2022, 1, 29),
                    Some(day(2022, 1, 30)),
                    RiskStatus::MarkerOnlyUnresolved
                ),
                (day(2022, 3, 1), None, RiskStatus::OtherRiskWarning),
            ],
            "the warning is not carried across the conflict day, and the days after \
             it are uncovered (unknown) until the next event"
        );
        assert_eq!(
            of("600002"),
            vec![(
                day(2021, 6, 1),
                Some(day(2021, 9, 1)),
                RiskStatus::OtherRiskWarning
            )]
        );
        let conflicts = set.same_day_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].code, "600001");
        assert_eq!(conflicts[0].raw_status_code, RAW_SAME_DAY_ACTIONS);
        assert_eq!(conflicts[0].source_record_id.as_deref(), Some("a2,a3"));
        assert_eq!(conflicts[0].known_at, Some(day(2022, 1, 29)));
        let evidence = set.same_day_conflict_evidence();
        assert_eq!(evidence["count"], 1);
        assert_eq!(evidence["sample"][0]["code"], "600001");
        assert_eq!(evidence["sample"][0]["day"], "2022-01-29");
    }

    /// The conflict row must not depend on the order the events arrive in, or
    /// replay could write a different row than the daily item.
    #[test]
    fn the_conflict_row_is_independent_of_event_order() {
        let other = Some(RiskStatus::OtherRiskWarning);
        let events = vec![
            sh_event(
                "600001",
                day(2022, 1, 29),
                RiskWarningAction::Implement,
                other,
                "z9",
            ),
            sh_event(
                "600001",
                day(2022, 1, 29),
                RiskWarningAction::Revoke,
                None,
                "a1",
            ),
        ];
        let mut reversed = events.clone();
        reversed.reverse();
        let forward = assemble_sh(&events, &[], Vec::new()).unwrap();
        let backward = assemble_sh(&reversed, &[], Vec::new()).unwrap();
        assert_eq!(forward.rows, backward.rows);
        assert_eq!(
            dataset_digest(&forward.rows),
            dataset_digest(&backward.rows)
        );
        assert_eq!(forward.rows[0].source_record_id.as_deref(), Some("a1,z9"));
    }

    /// R5 isolates only the implementation-versus-revocation case. Two
    /// implementations of different statuses on one day, and an
    /// implementation without a status, still fail the set.
    #[test]
    fn other_same_day_failures_stay_fail_closed() {
        let events = vec![
            sh_event(
                "600001",
                day(2020, 1, 2),
                RiskWarningAction::Implement,
                Some(RiskStatus::OtherRiskWarning),
                "1",
            ),
            sh_event(
                "600001",
                day(2020, 1, 2),
                RiskWarningAction::Implement,
                Some(RiskStatus::DelistingRiskWarning),
                "2",
            ),
        ];
        assert_eq!(
            assemble_sh(&events, &[], Vec::new()),
            Err(Error::SameDayStatusConflict)
        );
        let statusless = vec![sh_event(
            "600001",
            day(2020, 1, 2),
            RiskWarningAction::Implement,
            None,
            "1",
        )];
        assert_eq!(
            assemble_sh(&statusless, &[], Vec::new()),
            Err(Error::Status)
        );
    }

    /// A conflict row reads as a status that is not `normal` and not a warning
    /// class: the price-limit mapping and the member state treat it as unknown.
    #[test]
    fn the_conflict_status_is_a_valid_stored_status() {
        assert_eq!(
            RiskStatus::parse("marker_only_unresolved"),
            Some(RiskStatus::MarkerOnlyUnresolved)
        );
        assert_ne!(RiskStatus::MarkerOnlyUnresolved, RiskStatus::Normal);
    }

    // D-81 task 49: Normal interval tests

    #[test]
    fn sz_writes_normal_when_no_st_and_plain_name() {
        let rows = vec![
            change("000001", "1998-01-05", "万科A", "万科B"),
            change("000001", "2000-01-10", "万科B", "万科C"),
        ];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2001, 12, 31),
            observed: true,
            rows: 2,
            reason: String::new(),
        }];
        let set = assemble_sz(
            &rows,
            coverage,
            &delisting_dates(&[("000001", None), ("000002", None)]),
        )
        .expect("plain names");
        validate_set(&set.rows).expect("valid set");

        // Should have normal intervals
        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();
        assert!(
            !normals.is_empty(),
            "should write normal intervals for plain names"
        );

        // Normal intervals should be from szse_name_history source
        for normal in &normals {
            assert_eq!(normal.source_kind, "szse_name_history");
            assert_eq!(normal.exchange, Exchange::Sz);
        }
    }

    #[test]
    fn sz_normal_does_not_write_for_st_periods() {
        let rows = vec![
            change("000001", "1998-01-05", "深发展A", "深发展B"),
            change("000001", "1999-01-01", "深发展B", "ST深发展"),
            change("000001", "2000-01-01", "ST深发展", "深发展C"),
        ];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2001, 12, 31),
            observed: true,
            rows: 3,
            reason: String::new(),
        }];
        let set = assemble_sz(
            &rows,
            coverage,
            &delisting_dates(&[("000001", None), ("000002", None)]),
        )
        .expect("mixed periods");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();
        let warnings: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::OtherRiskWarning)
            .collect();

        assert!(!warnings.is_empty(), "should have ST interval");
        // Normal intervals should not overlap with ST intervals
        for normal in &normals {
            for warning in &warnings {
                let normal_start = normal.effective_from;
                let normal_end = normal.effective_to.unwrap();
                let warning_start = warning.effective_from;
                let warning_end = warning.effective_to.unwrap_or(day(9999, 12, 31));

                // Check no overlap
                assert!(
                    normal_end <= warning_start || normal_start >= warning_end,
                    "normal and warning should not overlap"
                );
            }
        }
    }

    #[test]
    fn sz_normal_excludes_pt_prefix_days() {
        let rows = vec![
            change("000001", "1998-01-05", "万科A", "PT万科"),
            change("000001", "2000-01-01", "PT万科", "万科B"),
        ];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2001, 12, 31),
            observed: true,
            rows: 2,
            reason: String::new(),
        }];
        let set = assemble_sz(
            &rows,
            coverage,
            &delisting_dates(&[("000001", None), ("000002", None)]),
        )
        .expect("PT prefix");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();

        // No normal interval should cover the PT period (1998-01-05 to 2000-01-01)
        for normal in &normals {
            assert!(
                normal.effective_to.unwrap() <= day(1998, 1, 5)
                    || normal.effective_from >= day(2000, 1, 1),
                "normal should not cover PT period"
            );
        }
    }

    #[test]
    fn sz_normal_excludes_tui_suffix_days() {
        let rows = vec![
            change("000001", "1998-01-05", "万科A", "万科退"),
            // After delisting marker, typically no more changes
        ];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2001, 12, 31),
            observed: true,
            rows: 1,
            reason: String::new(),
        }];
        let set = assemble_sz(
            &rows,
            coverage,
            &delisting_dates(&[("000001", None), ("000002", None)]),
        )
        .expect("退 suffix");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();

        // No normal interval should cover the period after 退 appears
        for normal in &normals {
            assert!(
                normal.effective_to.unwrap() <= day(1998, 1, 5),
                "normal should end before 退 suffix"
            );
        }
    }

    #[test]
    fn sz_normal_excludes_star_without_st() {
        let rows = vec![
            change("000001", "1998-01-05", "万科A", "*金亚"),
            change("000001", "2000-01-01", "*金亚", "万科B"),
        ];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2001, 12, 31),
            observed: true,
            rows: 2,
            reason: String::new(),
        }];
        let set = assemble_sz(
            &rows,
            coverage,
            &delisting_dates(&[("000001", None), ("000002", None)]),
        )
        .expect("* without ST");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();

        // No normal interval should cover the * period (1998-01-05 to 2000-01-01)
        for normal in &normals {
            assert!(
                normal.effective_to.unwrap() <= day(1998, 1, 5)
                    || normal.effective_from >= day(2000, 1, 1),
                "normal should not cover * without ST period"
            );
        }
    }

    #[test]
    fn sz_normal_does_not_write_before_first_name_row() {
        let rows = vec![change("000001", "2000-01-05", "万科A", "万科B")];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1), // Coverage starts before first name
            to: day(2001, 12, 31),
            observed: true,
            rows: 1,
            reason: String::new(),
        }];
        let set = assemble_sz(
            &rows,
            coverage,
            &delisting_dates(&[("000001", None), ("000002", None)]),
        )
        .expect("late first name");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();

        // All normal intervals should start on or after the first name row date
        for normal in &normals {
            assert!(
                normal.effective_from >= day(2000, 1, 5),
                "normal should not start before first name row"
            );
        }
    }

    #[test]
    fn sz_normal_does_not_write_for_codes_with_no_name_rows() {
        let rows = vec![]; // Empty
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2001, 12, 31),
            observed: true,
            rows: 0,
            reason: String::new(),
        }];
        let set = assemble_sz(
            &rows,
            coverage,
            &delisting_dates(&[("000001", None), ("000002", None)]),
        )
        .expect("no names");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();
        assert!(normals.is_empty(), "no normal for codes without name rows");
    }

    #[test]
    fn sz_normal_after_conflict_until_next_event_is_unknown() {
        let rows = vec![
            change("000001", "1998-01-05", "深发展A", "深发展B"),
            change("000001", "1999-04-28", "深发展B", "深发展C"),
            change("000001", "1999-04-28", "深发展B", "深发展D"), // Conflict
            change("000001", "2000-01-01", "深发展D", "深发展E"),
        ];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2001, 12, 31),
            observed: true,
            rows: 4,
            reason: String::new(),
        }];
        let set = assemble_sz(
            &rows,
            coverage,
            &delisting_dates(&[("000001", None), ("000002", None)]),
        )
        .expect("conflict recovery");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();
        let conflicts: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::MarkerOnlyUnresolved)
            .collect();

        assert_eq!(conflicts.len(), 1, "should have one conflict marker");
        assert_eq!(conflicts[0].effective_from, day(1999, 4, 28));

        // No normal interval should cover the period from conflict to next event
        // (1999-04-28 to 2000-01-01)
        for normal in &normals {
            let covers_conflict_gap = normal.effective_from < day(2000, 1, 1)
                && normal.effective_to.unwrap() > day(1999, 4, 28);
            assert!(
                !covers_conflict_gap,
                "normal should not cover post-conflict gap: {} to {:?}",
                normal.effective_from, normal.effective_to
            );
        }
    }

    #[test]
    fn sz_normal_stops_at_coverage_endpoint() {
        let rows = vec![
            change("000001", "1998-01-05", "万科A", "万科B"),
            // Name continues but coverage ends
        ];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2000, 12, 31), // Ends before present
            observed: true,
            rows: 1,
            reason: String::new(),
        }];
        let set = assemble_sz(
            &rows,
            coverage,
            &delisting_dates(&[("000001", None), ("000002", None)]),
        )
        .expect("coverage limit");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();

        // No normal interval should extend beyond coverage endpoint
        for normal in &normals {
            assert!(
                normal.effective_to.unwrap() <= day(2001, 1, 1), // succ of coverage end
                "normal should not extend beyond coverage endpoint"
            );
        }
    }

    #[test]
    fn sz_normal_conflict_with_no_subsequent_event_stays_unknown_until_coverage_end() {
        let rows = vec![
            change("000001", "1998-01-05", "深发展A", "深发展B"),
            change("000001", "1999-04-28", "深发展B", "深发展C"),
            change("000001", "1999-04-28", "深发展B", "深发展D"), // Conflict, no more events
        ];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2001, 12, 31),
            observed: true,
            rows: 3,
            reason: String::new(),
        }];
        let set = assemble_sz(
            &rows,
            coverage,
            &delisting_dates(&[("000001", None), ("000002", None)]),
        )
        .expect("conflict at end");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();
        let conflicts: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::MarkerOnlyUnresolved)
            .collect();

        assert_eq!(conflicts.len(), 1, "should have one conflict marker");
        assert_eq!(conflicts[0].effective_from, day(1999, 4, 28));

        // No normal interval should extend beyond the conflict day
        for normal in &normals {
            assert!(
                normal.effective_to.unwrap() <= day(1999, 4, 28),
                "normal should end at or before conflict day: {} to {:?}",
                normal.effective_from,
                normal.effective_to
            );
        }

        // Verify no normal interval covers the period after conflict to coverage end
        for normal in &normals {
            assert!(
                !(normal.effective_from >= day(1999, 4, 29)
                    && normal.effective_to.unwrap() <= day(2002, 1, 1)),
                "should not have normal interval after conflict with no subsequent event"
            );
        }
    }

    #[test]
    fn sz_normal_stops_at_delisting_date() {
        // 退市代码最后简称是 plain，退市日及之后不应有 normal。
        let rows = vec![change("000001", "1998-01-05", "万科A", "万科B")];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2005, 12, 31),
            observed: true,
            rows: 1,
            reason: String::new(),
        }];
        // Delisted on 2000-06-30
        let dates = delisting_dates(&[("000001", Some(day(2000, 6, 30)))]);
        let set = assemble_sz(&rows, coverage, &dates).expect("delisted");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();
        assert!(!normals.is_empty(), "should write normal before delisting");
        for normal in &normals {
            assert!(
                normal.effective_to.unwrap() <= day(2000, 6, 30),
                "normal must not extend past the delisting date: {} to {:?}",
                normal.effective_from,
                normal.effective_to
            );
        }
    }

    #[test]
    fn sz_normal_not_written_when_code_absent_from_security_master() {
        // 主表里没有的代码不写 normal（fail-closed）。
        let rows = vec![change("000001", "1998-01-05", "万科A", "万科B")];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2005, 12, 31),
            observed: true,
            rows: 1,
            reason: String::new(),
        }];
        let set = assemble_sz(&rows, coverage, &BTreeMap::new()).expect("no master row");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();
        assert!(
            normals.is_empty(),
            "a code absent from security_master must not get a normal row"
        );
    }

    #[test]
    fn sz_normal_rows_carry_the_opening_change_provenance() {
        // normal 行必须带来源：打开该段的简称变更的 record id 与 sha。
        let rows = vec![change("000001", "1998-01-05", "万科A", "万科B")];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2005, 12, 31),
            observed: true,
            rows: 1,
            reason: String::new(),
        }];
        let dates = delisting_dates(&[("000001", None)]);
        let set = assemble_sz(&rows, coverage, &dates).expect("plain");
        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();
        assert!(!normals.is_empty());
        for normal in &normals {
            assert_eq!(
                normal.source_record_id.as_deref(),
                Some("1998-01-05|000001|万科B")
            );
            assert_eq!(
                normal.source_document_sha256.as_deref(),
                Some("a".repeat(64).as_str())
            );
        }
    }

    #[test]
    fn sz_normal_treats_full_width_markers_as_not_plain() {
        // 全角字符（＊ＳＴ、ＰＴ、＊）在 NFKC 规范化后不算 plain。
        let rows = vec![
            change("000001", "1998-01-05", "万科A", "＊ＳＴ某某"),
            change("000001", "1999-01-05", "＊ＳＴ某某", "ＰＴ凯地"),
            change("000001", "2000-01-05", "ＰＴ凯地", "＊金亚"),
            change("000001", "2001-01-05", "＊金亚", "万科B"),
        ];
        let coverage = vec![CoverageWindow {
            source_kind: "szse_name_history".to_string(),
            exchange: Exchange::Sz,
            from: day(1998, 1, 1),
            to: day(2002, 12, 31),
            observed: true,
            rows: 4,
            reason: String::new(),
        }];
        let dates = delisting_dates(&[("000001", None)]);
        let set = assemble_sz(&rows, coverage, &dates).expect("full-width markers");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();
        // Only the segment after 2001-01-05 (万科B) may be normal.
        for normal in &normals {
            assert!(
                normal.effective_from >= day(2001, 1, 5),
                "full-width marker periods must not be normal: {} to {:?}",
                normal.effective_from,
                normal.effective_to
            );
        }
    }

    #[test]
    fn sz_normal_does_not_write_for_shanghai() {
        // This test verifies that assemble_sh does NOT write normal intervals
        // (only assemble_sz should)
        let events = vec![
            sh_event(
                "600000",
                day(2000, 1, 1),
                RiskWarningAction::Implement,
                Some(RiskStatus::OtherRiskWarning),
                "test-1",
            ),
            sh_event(
                "600000",
                day(2001, 1, 1),
                RiskWarningAction::Revoke,
                None,
                "test-2",
            ),
        ];
        let coverage = vec![CoverageWindow {
            source_kind: "exchange_announcement".to_string(),
            exchange: Exchange::Sh,
            from: day(1998, 1, 1),
            to: day(2005, 12, 31),
            observed: true,
            rows: 2,
            reason: String::new(),
        }];
        let set = assemble_sh(&events, &[], coverage).expect("shanghai");
        validate_set(&set.rows).expect("valid set");

        let normals: Vec<_> = set
            .rows
            .iter()
            .filter(|r| r.status == RiskStatus::Normal)
            .collect();
        assert!(
            normals.is_empty(),
            "Shanghai should never write normal intervals"
        );
    }
}
