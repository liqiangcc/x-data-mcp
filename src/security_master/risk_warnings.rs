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
pub const ALGORITHM_VERSION: &str = "risk-warnings-v1";

/// One dated short-name change from the SZSE `SSGSGMXX`/`tab2` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SzNameChange {
    pub code: String,
    pub day: NaiveDate,
    pub before_name: String,
    pub after_name: String,
    pub source_record_id: String,
    pub source_document_sha256: String,
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
    pub day: NaiveDate,
    pub action: RiskWarningAction,
    pub status: Option<RiskStatus>,
    pub announced_at: Option<String>,
    pub known_at: Option<NaiveDate>,
    pub raw_status_code: String,
    pub source_kind: String,
    pub source_record_id: String,
    pub source_document_sha256: String,
}

/// The complete admitted answer for one dataset version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskWarningSet {
    pub rows: Vec<RiskWarningInterval>,
    pub coverage: Vec<CoverageWindow>,
    pub pending_sources: Vec<PendingSource>,
}

impl RiskWarningSet {
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
    #[error("a short-name change chain has two changes on the same day")]
    SameDayConflict,
    #[error("a short-name change chain does not connect before_name to the prior after_name")]
    BrokenChain,
    #[error("an implementation and a revocation share one announcement day")]
    SameDayActionConflict,
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
pub fn assemble_sz(
    rows: &[SzNameChange],
    mut coverage: Vec<CoverageWindow>,
) -> Result<RiskWarningSet, Error> {
    let mut by_code: BTreeMap<&str, Vec<&SzNameChange>> = BTreeMap::new();
    for row in rows {
        check_code(&row.code)?;
        by_code.entry(row.code.as_str()).or_default().push(row);
    }
    let mut intervals = Vec::new();
    for (code, mut changes) in by_code {
        changes.sort_by_key(|change| change.day);
        for pair in changes.windows(2) {
            if pair[0].day == pair[1].day {
                return Err(Error::SameDayConflict);
            }
        }
        let mut current_name = changes[0].before_name.clone();
        let mut open: Option<RiskWarningInterval> = None;
        for change in changes {
            if change.before_name != current_name {
                return Err(Error::BrokenChain);
            }
            let before = warning_status(&current_name);
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
            current_name = change.after_name.clone();
        }
        if let Some(interval) = open {
            intervals.push(interval);
        }
    }
    coverage.sort_by(|left, right| (left.from, right.to).cmp(&(right.from, right.to)));
    Ok(RiskWarningSet {
        rows: intervals,
        coverage,
        pending_sources: Vec::new(),
    })
}

/// Assemble SH intervals from event-level announcement metadata.
///
/// D-58 admits only implementation/revocation plus the announcement day for
/// SH. The effective day is anchored to the announcement day with
/// `OWNER_RISK_ACCEPTED` (the caller records that basis in the item evidence);
/// this function never invents a dated name history. A `not_observed` coverage
/// window cuts every open interval at its start, so a missing announcement can
/// only produce an unknown gap, never a warning claimed across it.
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
        validate_sh_events(&events)?;
        let mut open: Option<RiskWarningInterval> = None;
        let mut cursor = 0usize;
        let code_gaps: Vec<NaiveDate> = gap_starts
            .iter()
            .copied()
            .filter(|day| events.iter().any(|event| event.day > *day))
            .collect();
        for day in code_gaps {
            while cursor < events.len() && events[cursor].day <= day {
                apply_sh_event(&mut open, &mut intervals, events[cursor]);
                cursor += 1;
            }
            close_at(&mut open, &mut intervals, day);
        }
        for event in &events[cursor..] {
            apply_sh_event(&mut open, &mut intervals, event);
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
    })
}

fn validate_sh_events(events: &[&RiskWarningStatusEvent]) -> Result<(), Error> {
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
            return Err(Error::SameDayActionConflict);
        }
        index = end;
    }
    Ok(())
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

fn interval_from_sz(code: &str, change: &SzNameChange, status: RiskStatus) -> RiskWarningInterval {
    RiskWarningInterval {
        exchange: Exchange::Sz,
        code: code.to_string(),
        effective_from: change.day,
        effective_to: None,
        raw_status_code: raw_status(status).to_string(),
        status,
        announced_at: None,
        known_at: None,
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
        if !matches!(
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
        }
    }

    #[test]
    fn sz_chain_opens_and_closes_st_interval() {
        let rows = vec![
            change("000001", "1998-04-28", "深发展A", "ST深发展"),
            change("000001", "1999-04-28", "ST深发展", "深发展A"),
        ];
        let set = assemble_sz(&rows, Vec::new()).expect("chain assembles");
        assert_eq!(set.rows.len(), 1);
        assert_eq!(set.rows[0].status, RiskStatus::OtherRiskWarning);
        assert_eq!(set.rows[0].effective_from, day(1998, 4, 28));
        assert_eq!(set.rows[0].effective_to, Some(day(1999, 4, 28)));
    }

    #[test]
    fn sz_same_day_conflict_fails_closed() {
        let rows = vec![
            change("000001", "1998-04-28", "深发展A", "ST深发展"),
            change("000001", "1998-04-28", "ST深发展", "深发展A"),
        ];
        assert_eq!(assemble_sz(&rows, Vec::new()), Err(Error::SameDayConflict));
    }

    #[test]
    fn no_events_is_not_a_normal_row() {
        let set = assemble_sz(&[], Vec::new()).expect("empty input is valid");
        assert!(set.rows.is_empty());
        assert_eq!(set.not_observed_windows(), 0);
    }

    #[test]
    fn sh_events_make_an_interval_and_a_gap_cuts_it() {
        let events = vec![
            RiskWarningStatusEvent {
                code: "600001".into(),
                day: day(2020, 1, 2),
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

    #[test]
    fn sh_same_day_implement_and_revoke_conflict() {
        let events = vec![
            RiskWarningStatusEvent {
                code: "600001".into(),
                day: day(2020, 1, 2),
                action: RiskWarningAction::Implement,
                status: Some(RiskStatus::OtherRiskWarning),
                announced_at: None,
                known_at: None,
                raw_status_code: "implement".into(),
                source_kind: "exchange_announcement".into(),
                source_record_id: "1".into(),
                source_document_sha256: "b".repeat(64),
            },
            RiskWarningStatusEvent {
                code: "600001".into(),
                day: day(2020, 1, 2),
                action: RiskWarningAction::Revoke,
                status: None,
                announced_at: None,
                known_at: None,
                raw_status_code: "revoke".into(),
                source_kind: "exchange_announcement".into(),
                source_record_id: "2".into(),
                source_document_sha256: "b".repeat(64),
            },
        ];
        assert_eq!(
            assemble_sh(&events, &[], Vec::new()),
            Err(Error::SameDayActionConflict)
        );
    }
}
