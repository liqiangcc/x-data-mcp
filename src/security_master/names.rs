//! Name-history domain: interval assembly for the one admitted dated name
//! source (issue-391 §4.4; issue-446 PR-12, D-61).
//!
//! Layer position (separation point 1): the domain owns the rules that turn
//! dated short-name changes into intervals. It never fetches, freezes or
//! writes. The source adapter (`official_security::security_names`) parses
//! frozen bytes into the [`SzNameChange`] rows that PR-10 shares, this module
//! opens and closes intervals, and the storage writer persists the validated
//! set.
//!
//! ## What the source proves, and what it does not
//!
//! - A change row proves the *new* name from its change day on. The previous
//!   name's start is unknown, so it never becomes an interval: a row has no
//!   `effective_from` to give, and inventing one would fabricate a start.
//! - A code's last change has no end: `certainty='unbounded'`.
//! - `not_observed` coverage windows cut an open interval at the window start.
//!   After a gap the name is unknown, not "unchanged".
//! - SH and BJ have no dated name history (D-61: `NOT_AVAILABLE`). This module
//!   never turns a current name into an interval; their coverage is recorded
//!   with `source_kind='unknown'` and zero rows, which is the `unbounded`/
//!   `unknown` vocabulary of §4.4 used for absence instead of a guessed start.
//!
//! ## XD/XR normalisation
//!
//! XD/XR/DR are ex-dividend/ex-rights markers the exchange quotes on the
//! marker day; they are not a name change (the DDL note names this writer
//! rule). A row whose before/after names differ only by such a marker is a
//! marker row: it links the chain but opens no interval. The stored name never
//! carries the marker.
//!
//! The current name is a different fact (the TDX snapshot M1 already serves);
//! nothing here is used by any derivation — 涨跌停 and the universe never read
//! a name.
//!
//! ## ETF names (D-76 Q1, `etf-name-search` §5.1)
//!
//! D-61 stays as written for A-shares: a current name is never turned into an
//! interval. D-76 relaxes that **for ETFs only**, and only in one shape: the
//! observed chain of [`assemble_etf_names`]. Its rows say "this name was on the
//! exchange list at the first snapshot that showed it" and carry
//! `source_kind='exchange_snapshot'`:
//!
//! - `effective_from` is the first observation day of the name
//!   (`basis=fetch_time`), not the exchange's rename day. No row exists before
//!   the first observation and nothing is back-filled.
//! - `effective_to` is the next name's `effective_from`; the last name is
//!   `unbounded`, or closed on the day the code left the list.
//! - [`validate_set`] never admits an `exchange_snapshot` row (so an A-share
//!   "current name as interval" is refused, as before). [`validate_set_for`]
//!   admits one only for an entity the caller declares to be an ETF, so the
//!   relaxation cannot be reached from an A-share path.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;

use super::Exchange;
use super::assembly::PendingSource;
use super::coverage::CoverageWindow;
use super::risk_warnings::SzNameChange;

/// Dataset name the daily item reports (`derived_dataset_versions.dataset_kind`)
/// and the read model consumes.
pub const DATASET_KIND: &str = "security_names";
/// One whole-market dataset, like the main table.
pub const DATASET_KEY: &str = "a_share";
/// Content-digest format registered with the dataset version.
pub const DIGEST_FORMAT: &str = "sha256-security-name-intervals-v1";
/// Algorithm version of the interval assembly.
pub const ALGORITHM_VERSION: &str = "security-names-v1";
/// Ex-dividend/ex-rights markers the SZSE quotes on the marker day.
pub const EX_MARKERS: [&str; 3] = ["XD", "XR", "DR"];

/// One `security_name_history` interval (§4.4). The item writes the whole set
/// or none of it: a partial copy could pair a name with another day's chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameInterval {
    pub exchange: Exchange,
    pub code: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub name: String,
    pub certainty: NameCertainty,
    /// The announcement day the source proves; `None` stays unknown rather
    /// than being filled with `effective_from` (separation point 3).
    pub known_at: Option<NaiveDate>,
    pub source_kind: NameSourceKind,
    pub source_record_id: Option<String>,
}

/// Whether the row's end is proven. `Unbounded` is not "still current".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameCertainty {
    Interval,
    Unbounded,
}

impl NameCertainty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interval => "interval",
            Self::Unbounded => "unbounded",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "interval" => Some(Self::Interval),
            "unbounded" => Some(Self::Unbounded),
            _ => None,
        }
    }
}

/// Which announcement family proved the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameSourceKind {
    SzseNameHistory,
    Gp29Marker,
    ExchangeSnapshot,
    Unknown,
}

impl NameSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SzseNameHistory => "szse_name_history",
            Self::Gp29Marker => "gp29_marker",
            Self::ExchangeSnapshot => "exchange_snapshot",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "szse_name_history" => Some(Self::SzseNameHistory),
            "gp29_marker" => Some(Self::Gp29Marker),
            "exchange_snapshot" => Some(Self::ExchangeSnapshot),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// The complete admitted answer for one dataset version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameSet {
    pub rows: Vec<NameInterval>,
    pub coverage: Vec<CoverageWindow>,
    pub pending_sources: Vec<PendingSource>,
}

impl NameSet {
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

/// The name-history adapter's answer for one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameSource {
    Admitted(NameSet),
    Pending(PendingSource),
}

/// The adapter boundary (§16 PR-8): one function, no trait.
///
/// `Some(set)` is an admitted snapshot; `None` is not "no name changes": it is
/// the `pending_source` state, and the item then writes zero name rows.
pub fn name_history_source(set: Option<NameSet>) -> NameSource {
    match set {
        Some(set) => NameSource::Admitted(set),
        None => NameSource::Pending(PendingSource {
            dataset: DATASET_KIND,
            source: "exchange_name_history",
            reason: "no admitted name-history snapshot is frozen for this generation \
                     (#308 S-6); the item writes zero security_name_history rows and the \
                     current name stays the TDX snapshot M1 already serves",
        }),
    }
}

/// Why a parsed change set cannot become a stored interval set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("a name interval ends before it starts")]
    Range,
    #[error("two name intervals overlap for one code")]
    Overlap,
    #[error("two name rows share the identity (exchange, code, effective_from)")]
    Duplicate,
    #[error("an unbounded name interval is followed by another row for the same code")]
    UnboundedNotLast,
    #[error("a name row carries an empty name")]
    Name,
    #[error("a name change chain has two changes on the same day")]
    SameDayConflict,
    #[error("a name change chain does not connect before_name to the prior after_name")]
    BrokenChain,
    #[error("a source event names no 6-digit A-share code")]
    Code,
    #[error("a name row names a market without an admitted dated name source")]
    Market,
    #[error("an ETF name chain is not strictly increasing in observation day")]
    Chain,
}

/// Assemble SZ intervals from a dated short-name change chain.
///
/// Only a change's `after_name` opens an interval; the first row's
/// `before_name` links the chain but is never turned into a row because its
/// start day is unknown. A `not_observed` window cuts every open interval at
/// its start, so a gap leaves the name unknown instead of extending the last
/// proven name across it.
pub fn assemble_sz(
    changes: &[SzNameChange],
    mut coverage: Vec<CoverageWindow>,
) -> Result<NameSet, Error> {
    let mut by_code: BTreeMap<&str, Vec<&SzNameChange>> = BTreeMap::new();
    for change in changes {
        check_code(&change.code)?;
        by_code
            .entry(change.code.as_str())
            .or_default()
            .push(change);
    }
    // Only a window over the SZ source can cut a SZ interval: an unobserved
    // SH/BJ window says nothing about when the SZ table was read.
    let gap_days: Vec<NaiveDate> = coverage
        .iter()
        .filter(|window| !window.observed && window.exchange == Exchange::Sz)
        .map(|window| window.from)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut rows = Vec::new();
    for (code, mut changes) in by_code {
        changes.sort_by_key(|change| change.day);
        for pair in changes.windows(2) {
            if pair[0].day == pair[1].day {
                return Err(Error::SameDayConflict);
            }
        }
        let mut current = normalize_name(&changes[0].before_name)?;
        let mut open: Option<NameInterval> = None;
        let mut gap_index = 0usize;
        for change in changes {
            while gap_index < gap_days.len() && gap_days[gap_index] < change.day {
                close_at(&mut open, &mut rows, gap_days[gap_index]);
                gap_index += 1;
            }
            let before = normalize_name(&change.before_name)?;
            let after = normalize_name(&change.after_name)?;
            if before != current {
                return Err(Error::BrokenChain);
            }
            // A marker-only row (XD/XR/DR difference) proves no rename.
            if after != before {
                close_at(&mut open, &mut rows, change.day);
                open = Some(interval_from_change(code, change, after.clone()));
            }
            current = after;
        }
        while gap_index < gap_days.len() {
            close_at(&mut open, &mut rows, gap_days[gap_index]);
            gap_index += 1;
        }
        if let Some(interval) = open {
            rows.push(interval);
        }
    }
    coverage.sort_by_key(|window| (window.from, window.to));
    Ok(NameSet {
        rows,
        coverage,
        pending_sources: Vec::new(),
    })
}

fn close_at(open: &mut Option<NameInterval>, rows: &mut Vec<NameInterval>, day: NaiveDate) {
    let Some(mut current) = open.take() else {
        return;
    };
    if day > current.effective_from {
        current.effective_to = Some(day);
        current.certainty = NameCertainty::Interval;
        rows.push(current);
    }
}

fn interval_from_change(code: &str, change: &SzNameChange, name: String) -> NameInterval {
    NameInterval {
        exchange: Exchange::Sz,
        code: code.to_string(),
        effective_from: change.day,
        effective_to: None,
        name,
        certainty: NameCertainty::Unbounded,
        // §5.3/Q4: the knowledge time is the first snapshot that carried this
        // row (the adapter resolves it from the source store), never the
        // change day and never the write day.
        known_at: change.known_at,
        source_kind: NameSourceKind::SzseNameHistory,
        source_record_id: Some(change.source_record_id.clone()),
    }
}

/// Algorithm version of the ETF observed-name assembly.
pub const ETF_ALGORITHM_VERSION: &str = "etf-names-v1";

/// One observed name of an ETF code: the first snapshot that showed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EtfNameStep {
    /// First observation day of the name. Not the rename day.
    pub observed_from: NaiveDate,
    /// The Shanghai day the observing snapshot entered the source store.
    pub known_at: NaiveDate,
    pub name: String,
    /// Names the frozen snapshot that decided this step.
    pub source_record_id: String,
}

/// The observed names of one ETF code, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EtfNameChain {
    pub exchange: Exchange,
    pub code: String,
    pub steps: Vec<EtfNameStep>,
    /// The day after which the code is no longer on the exchange list, when the
    /// frozen snapshots prove it. `None` leaves the last name open.
    pub ended: Option<NaiveDate>,
}

/// ETF name rows plus the entities they are about.
///
/// `entities` lists every ETF the list sources know, including codes that
/// produced no row, so a writer can tell an ETF row from an A-share one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EtfNameSet {
    pub rows: Vec<NameInterval>,
    pub entities: BTreeSet<(Exchange, String)>,
}

/// Assemble ETF observed-name intervals (D-76 Q1).
///
/// Pure function of the chains: no clock, no store. A chain with no step makes
/// no row (an unnamed ETF is unknown, not empty). The first step opens the
/// first interval; there is never a row before it.
pub fn assemble_etf_names(chains: &[EtfNameChain]) -> Result<Vec<NameInterval>, Error> {
    let mut rows = Vec::new();
    for chain in chains {
        check_code(&chain.code)?;
        if chain.exchange == Exchange::Bj {
            return Err(Error::Market);
        }
        let Some(last) = chain.steps.last() else {
            continue;
        };
        if chain.ended.is_some_and(|end| end <= last.observed_from) {
            return Err(Error::Range);
        }
        for pair in chain.steps.windows(2) {
            if pair[1].observed_from <= pair[0].observed_from {
                return Err(Error::Chain);
            }
        }
        for (index, step) in chain.steps.iter().enumerate() {
            let name = step.name.trim();
            if name.is_empty() {
                return Err(Error::Name);
            }
            let effective_to = match chain.steps.get(index + 1) {
                Some(next) => Some(next.observed_from),
                None => chain.ended,
            };
            rows.push(NameInterval {
                exchange: chain.exchange,
                code: chain.code.clone(),
                effective_from: step.observed_from,
                effective_to,
                name: name.to_string(),
                certainty: if effective_to.is_some() {
                    NameCertainty::Interval
                } else {
                    NameCertainty::Unbounded
                },
                known_at: Some(step.known_at),
                source_kind: NameSourceKind::ExchangeSnapshot,
                source_record_id: Some(step.source_record_id.clone()),
            });
        }
    }
    rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
    Ok(rows)
}

/// Normalise one quoted name: drop whitespace, then a leading XD/XR/DR marker.
///
/// An empty name is a source defect, not a row: the stored `name` is NOT NULL
/// and an empty string would be indistinguishable from a missing name.
pub fn normalize_name(raw: &str) -> Result<String, Error> {
    let compact: String = raw.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.is_empty() {
        return Err(Error::Name);
    }
    for marker in EX_MARKERS {
        if let Some(rest) = compact.strip_prefix(marker)
            && !rest.is_empty()
        {
            return Ok(rest.to_string());
        }
    }
    Ok(compact)
}

/// The search key of a name or a name query (D-76 Q8): Unicode NFKC, then every
/// whitespace character dropped, then lower-cased. Full-width letters and
/// digits become ASCII, so `沪深３００ＥＴＦ`, `沪深 300 etf` and `沪深300ETF`
/// share one key. A query matches a name when its key is a substring of the
/// name's key. No pinyin, no fuzzy matching, and no expansion of the exchange's
/// short spellings (`华夏300` is not a substring of `沪深300ETF华夏`).
///
/// Lower-casing stands in for full case folding: the two differ only for a few
/// letters (`ß`) that do not occur in fund names.
pub fn search_key(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    text.nfkc()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn check_code(code: &str) -> Result<(), Error> {
    if code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        Err(Error::Code)
    }
}

/// Validate a row set before it is stored. The DDL carries the range and
/// vocabulary CHECKs; these are the invariants it cannot state.
///
/// This is the A-share path: no entity is an ETF here, so an
/// `exchange_snapshot` row is refused ([`Error::Market`]).
pub fn validate_set(rows: &[NameInterval]) -> Result<(), Error> {
    validate_set_for(rows, &BTreeSet::new())
}

/// [`validate_set`] with the ETF entities the caller declares (D-76 Q1).
///
/// - A row of an ETF entity must be `exchange_snapshot` and may sit on SH or
///   SZ.
/// - An `exchange_snapshot` row of any other entity is refused: a current name
///   shaped as an interval is exactly what D-61 forbids for A-shares.
/// - Every other row goes through the same checks as before, unchanged: SZ only.
pub fn validate_set_for(
    rows: &[NameInterval],
    etf_entities: &BTreeSet<(Exchange, String)>,
) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    let mut sorted: Vec<&NameInterval> = rows.iter().collect();
    sorted.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
    for row in &sorted {
        check_code(&row.code)?;
        let is_etf = etf_entities.contains(&(row.exchange, row.code.clone()));
        if is_etf || row.source_kind == NameSourceKind::ExchangeSnapshot {
            if !is_etf
                || row.source_kind != NameSourceKind::ExchangeSnapshot
                || row.exchange == Exchange::Bj
                || row.known_at.is_none()
                || row.source_record_id.is_none()
            {
                return Err(Error::Market);
            }
        } else if row.exchange != Exchange::Sz {
            // D-61 admits exactly one dated name source (SZ). A SH/BJ row, or a
            // row inferred from a GP29 marker or a current-name snapshot, would
            // be a dated interval the source does not prove: refuse it here
            // rather than let a guessed start reach the table.
            return Err(Error::Market);
        }
        if row.name.trim().is_empty() {
            return Err(Error::Name);
        }
        if row
            .effective_to
            .is_some_and(|end| end <= row.effective_from)
        {
            return Err(Error::Range);
        }
        if row.certainty == NameCertainty::Interval && row.effective_to.is_none() {
            return Err(Error::Range);
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
        if left.certainty == NameCertainty::Unbounded {
            return Err(Error::UnboundedNotLast);
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

/// Content digest of the dataset: `SHA256("security-names-v1" ‖ row_count ‖
/// rows…)`. Coverage evidence describes provenance, not the dataset identity.
pub fn dataset_digest(rows: &[NameInterval]) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    let mut p = Vec::with_capacity(64 + rows.len() * 96);
    p.extend_from_slice(b"security-names-v1\0");
    p.extend_from_slice(&(rows.len() as u64).to_be_bytes());
    let mut sorted: Vec<&NameInterval> = rows.iter().collect();
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
        push_text(&mut p, &row.name);
        push_text(&mut p, row.certainty.as_str());
        push_opt_day(&mut p, row.known_at);
        push_text(&mut p, row.source_kind.as_str());
        push_opt_text(&mut p, row.source_record_id.as_deref());
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
            known_at: None,
        }
    }

    fn window(
        exchange: Exchange,
        from: NaiveDate,
        to: NaiveDate,
        observed: bool,
    ) -> CoverageWindow {
        CoverageWindow {
            source_kind: if observed {
                "szse_name_history".into()
            } else {
                NameSourceKind::Unknown.as_str().into()
            },
            exchange,
            from,
            to,
            observed,
            rows: 0,
            reason: "fixture".into(),
        }
    }

    #[test]
    fn a_change_chain_opens_and_closes_intervals() {
        let rows = vec![
            change("000001", "1998-04-28", "深发展A", "ST深发展"),
            change("000001", "1999-04-28", "ST深发展", "深发展A"),
            change("000001", "2002-06-01", "深发展A", "深发展"),
        ];
        let set = assemble_sz(&rows, Vec::new()).expect("chain assembles");
        assert_eq!(set.rows.len(), 3);
        assert_eq!(set.rows[0].name, "ST深发展");
        assert_eq!(set.rows[0].effective_from, day(1998, 4, 28));
        assert_eq!(set.rows[0].effective_to, Some(day(1999, 4, 28)));
        assert_eq!(set.rows[0].certainty, NameCertainty::Interval);
        assert_eq!(set.rows[2].name, "深发展");
        assert_eq!(set.rows[2].effective_to, None);
        assert_eq!(set.rows[2].certainty, NameCertainty::Unbounded);
        // No store answered this call: the fixture changes carry no knowledge
        // time, and the assembler does not invent one from the change day.
        assert!(set.rows.iter().all(|row| row.known_at.is_none()));
    }

    /// XDM-157: the interval carries the adapter's snapshot day, not the
    /// change day. A `known_at` filter is only honest if the two stay
    /// different fields (issue-446 §5.3).
    #[test]
    fn an_interval_carries_the_snapshot_day_it_was_first_stored() {
        let mut first = change("000001", "1998-04-28", "深发展A", "ST深发展");
        first.known_at = Some(day(2026, 10, 31));
        let set = assemble_sz(&[first], Vec::new()).expect("single change assembles");
        assert_eq!(set.rows[0].known_at, Some(day(2026, 10, 31)));
        assert_eq!(set.rows[0].effective_from, day(1998, 4, 28));
    }

    #[test]
    fn the_first_names_start_is_never_invented() {
        // The table's first row for a code only proves the *after* name from
        // its change day; the previous name has no start date to write.
        let rows = vec![change("000004", "1999-04-27", "深安达A", "ST深安达")];
        let set = assemble_sz(&rows, Vec::new()).expect("single change assembles");
        assert_eq!(set.rows.len(), 1);
        assert_eq!(set.rows[0].name, "ST深安达");
        assert_eq!(set.rows[0].effective_from, day(1999, 4, 27));
        assert_eq!(set.rows[0].certainty, NameCertainty::Unbounded);
    }

    #[test]
    fn search_key_folds_width_case_and_whitespace_only() {
        assert_eq!(search_key("沪深 300 ETF"), "沪深300etf");
        assert_eq!(search_key("　沪深３００ＥＴＦ\t"), "沪深300etf");
        assert_eq!(search_key("沪深300etf"), search_key("沪深300ETF"));
        // No pinyin, no short-form expansion, no marker stripping.
        assert_ne!(search_key("hs300"), search_key("沪深300"));
        assert!(!search_key("沪深300ETF华夏").contains(&search_key("华夏300")));
        assert_eq!(search_key("XD银行"), "xd银行");
        assert_eq!(search_key(" \u{a0}\u{3000}"), "");
    }

    #[test]
    fn xd_and_xr_markers_are_not_renames() {
        let rows = vec![
            change("000004", "2000-06-01", "深安达A", "XD深安达A"),
            change("000004", "2001-03-07", "深安达A", "深安达B"),
        ];
        let set = assemble_sz(&rows, Vec::new()).expect("marker row links the chain");
        assert_eq!(set.rows.len(), 1);
        assert_eq!(set.rows[0].name, "深安达B");
        assert_eq!(set.rows[0].effective_from, day(2001, 3, 7));
        assert!(set.rows.iter().all(|row| !row.name.contains("XD")));
        assert_eq!(normalize_name(" XR 深发展Ａ ").unwrap(), "深发展Ａ");
        assert_eq!(normalize_name("DR*ST康达").unwrap(), "*ST康达");
        // "ST" is a real name part, never stripped as a marker.
        assert_eq!(normalize_name("*ST康达").unwrap(), "*ST康达");
        assert_eq!(normalize_name("   "), Err(Error::Name));
    }

    #[test]
    fn two_changes_on_one_day_fail_closed() {
        let rows = vec![
            change("000004", "2001-03-07", "深安达A", "深安达B"),
            change("000004", "2001-03-07", "深安达B", "深安达C"),
        ];
        assert_eq!(assemble_sz(&rows, Vec::new()), Err(Error::SameDayConflict));
    }

    #[test]
    fn a_broken_chain_fails_closed() {
        let rows = vec![
            change("000004", "2001-03-07", "深安达A", "深安达B"),
            change("000004", "2002-03-07", "深安达C", "深安达D"),
        ];
        assert_eq!(assemble_sz(&rows, Vec::new()), Err(Error::BrokenChain));
    }

    #[test]
    fn no_dated_source_is_not_an_empty_history_row() {
        // SH/BJ carry no dated source (D-61): their coverage is recorded, and
        // no row is invented from a current name.
        let coverage = vec![
            window(Exchange::Sh, day(1994, 1, 3), day(2026, 10, 30), false),
            window(Exchange::Bj, day(1994, 1, 3), day(2026, 10, 30), false),
        ];
        let set = assemble_sz(&[], coverage).expect("no SZ change is valid");
        assert!(set.rows.is_empty());
        assert_eq!(set.not_observed_windows(), 2);
        assert!(!set.rows.iter().any(|row| row.exchange != Exchange::Sz));
    }

    #[test]
    fn a_not_observed_window_cuts_the_open_interval() {
        let rows = vec![
            change("000004", "2001-03-07", "深安达A", "深安达B"),
            change("000004", "2004-01-05", "深安达B", "深安达C"),
        ];
        let coverage = vec![window(
            Exchange::Sz,
            day(2002, 1, 1),
            day(2003, 12, 31),
            false,
        )];
        let set = assemble_sz(&rows, coverage).expect("gap cuts");
        assert_eq!(set.rows.len(), 2);
        assert_eq!(set.rows[0].effective_to, Some(day(2002, 1, 1)));
        assert_eq!(set.rows[1].effective_from, day(2004, 1, 5));
        assert_eq!(set.not_observed_windows(), 1);
    }

    #[test]
    fn validation_refuses_an_unbounded_row_before_a_later_one() {
        let rows = vec![
            NameInterval {
                exchange: Exchange::Sz,
                code: "000004".into(),
                effective_from: day(2001, 3, 7),
                effective_to: None,
                name: "深安达B".into(),
                certainty: NameCertainty::Unbounded,
                known_at: None,
                source_kind: NameSourceKind::SzseNameHistory,
                source_record_id: None,
            },
            NameInterval {
                exchange: Exchange::Sz,
                code: "000004".into(),
                effective_from: day(2004, 1, 5),
                effective_to: None,
                name: "深安达C".into(),
                certainty: NameCertainty::Unbounded,
                known_at: None,
                source_kind: NameSourceKind::SzseNameHistory,
                source_record_id: None,
            },
        ];
        assert_eq!(validate_set(&rows), Err(Error::UnboundedNotLast));
    }

    // ------------------------------------------------ ETF names (D-76 Q1) ---

    fn step(observed: NaiveDate, name: &str) -> EtfNameStep {
        EtfNameStep {
            observed_from: observed,
            known_at: observed,
            name: name.into(),
            source_record_id: format!("sse_etf_list:{observed}-{name}"),
        }
    }

    fn chain(
        exchange: Exchange,
        code: &str,
        steps: Vec<EtfNameStep>,
        ended: Option<NaiveDate>,
    ) -> EtfNameChain {
        EtfNameChain {
            exchange,
            code: code.into(),
            steps,
            ended,
        }
    }

    fn etf(exchange: Exchange, code: &str) -> BTreeSet<(Exchange, String)> {
        BTreeSet::from([(exchange, code.to_string())])
    }

    #[test]
    fn an_etf_chain_opens_its_first_interval_on_the_first_observation_and_never_earlier() {
        let rows = assemble_etf_names(&[chain(
            Exchange::Sh,
            "510300",
            vec![
                step(day(2026, 11, 5), "沪深300ETF华泰柏瑞"),
                step(day(2026, 11, 9), "沪深300ETF"),
            ],
            None,
        )])
        .expect("a chain assembles");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].effective_from, day(2026, 11, 5));
        assert_eq!(rows[0].effective_to, Some(day(2026, 11, 9)));
        assert_eq!(rows[0].certainty, NameCertainty::Interval);
        assert_eq!(rows[1].effective_from, day(2026, 11, 9));
        assert_eq!(rows[1].effective_to, None);
        assert_eq!(rows[1].certainty, NameCertainty::Unbounded);
        for row in &rows {
            assert_eq!(row.source_kind, NameSourceKind::ExchangeSnapshot);
            assert_eq!(row.known_at, Some(row.effective_from));
            assert!(row.source_record_id.is_some());
        }
        // No row before the first observation: nothing is back-filled.
        assert!(
            rows.iter()
                .all(|row| row.effective_from >= day(2026, 11, 5))
        );
        validate_set_for(&rows, &etf(Exchange::Sh, "510300")).expect("the set validates");
    }

    #[test]
    fn a_code_that_left_the_list_closes_its_last_name_on_the_day_it_was_missed() {
        let rows = assemble_etf_names(&[chain(
            Exchange::Sz,
            "159915",
            vec![step(day(2026, 11, 5), "创业板ETF易方达")],
            Some(day(2026, 11, 12)),
        )])
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].effective_to, Some(day(2026, 11, 12)));
        assert_eq!(rows[0].certainty, NameCertainty::Interval);
    }

    #[test]
    fn an_etf_without_a_name_step_makes_no_row_and_a_bad_chain_fails_closed() {
        assert!(
            assemble_etf_names(&[chain(Exchange::Sh, "510300", Vec::new(), None)])
                .unwrap()
                .is_empty()
        );
        let backwards = chain(
            Exchange::Sh,
            "510300",
            vec![step(day(2026, 11, 9), "甲"), step(day(2026, 11, 5), "乙")],
            None,
        );
        assert_eq!(assemble_etf_names(&[backwards]), Err(Error::Chain));
        let ended_early = chain(
            Exchange::Sh,
            "510300",
            vec![step(day(2026, 11, 9), "甲")],
            Some(day(2026, 11, 9)),
        );
        assert_eq!(assemble_etf_names(&[ended_early]), Err(Error::Range));
        let blank = chain(
            Exchange::Sh,
            "510300",
            vec![step(day(2026, 11, 9), "  ")],
            None,
        );
        assert_eq!(assemble_etf_names(&[blank]), Err(Error::Name));
        let bj = chain(
            Exchange::Bj,
            "430001",
            vec![step(day(2026, 11, 9), "甲")],
            None,
        );
        assert_eq!(assemble_etf_names(&[bj]), Err(Error::Market));
        let short_code = chain(
            Exchange::Sh,
            "5103",
            vec![step(day(2026, 11, 9), "甲")],
            None,
        );
        assert_eq!(assemble_etf_names(&[short_code]), Err(Error::Code));
    }

    /// The point of the whole relaxation being narrow: the A-share validator is
    /// the one that always ran, and "a current name as an interval" is still
    /// refused for an A-share however it is dressed.
    #[test]
    fn a_share_validation_still_refuses_a_current_name_as_an_interval() {
        let current = |exchange: Exchange, code: &str, kind: NameSourceKind| NameInterval {
            exchange,
            code: code.into(),
            effective_from: day(2026, 11, 5),
            effective_to: None,
            name: "平安银行".into(),
            certainty: NameCertainty::Unbounded,
            known_at: Some(day(2026, 11, 5)),
            source_kind: kind,
            source_record_id: Some("current".into()),
        };
        // SH A-share, any source kind: refused (no dated SH source).
        for kind in [
            NameSourceKind::ExchangeSnapshot,
            NameSourceKind::SzseNameHistory,
            NameSourceKind::Unknown,
        ] {
            assert_eq!(
                validate_set(&[current(Exchange::Sh, "600000", kind)]),
                Err(Error::Market),
                "SH {kind:?}"
            );
        }
        // SZ A-share carrying the ETF source kind: refused by the A-share path
        // and by the ETF path (the code is not a declared ETF).
        let sz_snapshot = current(Exchange::Sz, "000001", NameSourceKind::ExchangeSnapshot);
        assert_eq!(
            validate_set(std::slice::from_ref(&sz_snapshot)),
            Err(Error::Market)
        );
        assert_eq!(
            validate_set_for(
                std::slice::from_ref(&sz_snapshot),
                &etf(Exchange::Sz, "159915")
            ),
            Err(Error::Market)
        );
        // BJ stays refused everywhere.
        assert_eq!(
            validate_set_for(
                &[current(
                    Exchange::Bj,
                    "430001",
                    NameSourceKind::ExchangeSnapshot
                )],
                &etf(Exchange::Bj, "430001")
            ),
            Err(Error::Market)
        );
        // A genuine SZ A-share row validates exactly as before.
        validate_set(&[current(
            Exchange::Sz,
            "000001",
            NameSourceKind::SzseNameHistory,
        )])
        .expect("the SZ A-share path is unchanged");
    }

    #[test]
    fn the_etf_validator_admits_only_declared_etfs_with_the_snapshot_kind() {
        let row = |exchange: Exchange, code: &str, kind: NameSourceKind| NameInterval {
            exchange,
            code: code.into(),
            effective_from: day(2026, 11, 5),
            effective_to: None,
            name: "沪深300ETF".into(),
            certainty: NameCertainty::Unbounded,
            known_at: Some(day(2026, 11, 5)),
            source_kind: kind,
            source_record_id: Some("sse_etf_list:abc".into()),
        };
        let sh = row(Exchange::Sh, "510300", NameSourceKind::ExchangeSnapshot);
        // Declared ETF on SH: admitted.
        validate_set_for(std::slice::from_ref(&sh), &etf(Exchange::Sh, "510300")).unwrap();
        // The same row for a code nobody declared as ETF: refused.
        assert_eq!(
            validate_set_for(std::slice::from_ref(&sh), &BTreeSet::new()),
            Err(Error::Market)
        );
        // A declared ETF with an A-share source kind: refused.
        let wrong_kind = row(Exchange::Sh, "510300", NameSourceKind::SzseNameHistory);
        assert_eq!(
            validate_set_for(&[wrong_kind], &etf(Exchange::Sh, "510300")),
            Err(Error::Market)
        );
        // An ETF row must be dated and cite its snapshot.
        let mut undated = sh.clone();
        undated.known_at = None;
        assert_eq!(
            validate_set_for(&[undated], &etf(Exchange::Sh, "510300")),
            Err(Error::Market)
        );
        let mut uncited = sh;
        uncited.source_record_id = None;
        assert_eq!(
            validate_set_for(&[uncited], &etf(Exchange::Sh, "510300")),
            Err(Error::Market)
        );
    }

    #[test]
    fn etf_rows_keep_the_shared_interval_invariants() {
        let rows = assemble_etf_names(&[chain(
            Exchange::Sh,
            "510300",
            vec![step(day(2026, 11, 5), "甲"), step(day(2026, 11, 9), "乙")],
            None,
        )])
        .unwrap();
        let entities = etf(Exchange::Sh, "510300");
        let mut overlapping = rows.clone();
        overlapping[0].effective_to = Some(day(2026, 11, 20));
        assert_eq!(
            validate_set_for(&overlapping, &entities),
            Err(Error::Overlap)
        );
        let mut duplicated = rows.clone();
        duplicated.push(rows[0].clone());
        assert_eq!(
            validate_set_for(&duplicated, &entities),
            Err(Error::Duplicate)
        );
        let mut unbounded_first = rows;
        unbounded_first[0].effective_to = None;
        unbounded_first[0].certainty = NameCertainty::Unbounded;
        assert_eq!(
            validate_set_for(&unbounded_first, &entities),
            Err(Error::UnboundedNotLast)
        );
    }

    #[test]
    fn the_vocabulary_matches_the_table_check() {
        assert_eq!(NameCertainty::Interval.as_str(), "interval");
        assert_eq!(NameCertainty::Unbounded.as_str(), "unbounded");
        assert_eq!(
            NameSourceKind::SzseNameHistory.as_str(),
            "szse_name_history"
        );
        assert_eq!(NameSourceKind::Unknown.as_str(), "unknown");
        for value in ["interval", "unbounded"] {
            assert_eq!(NameCertainty::parse(value).unwrap().as_str(), value);
        }
        for value in [
            "szse_name_history",
            "gp29_marker",
            "exchange_snapshot",
            "unknown",
        ] {
            assert_eq!(NameSourceKind::parse(value).unwrap().as_str(), value);
        }
        assert!(NameSourceKind::parse("other").is_none());
    }

    #[test]
    fn a_missing_source_is_pending_not_an_empty_answer() {
        match name_history_source(None) {
            NameSource::Pending(pending) => {
                assert_eq!(pending.dataset, DATASET_KIND);
                assert!(!pending.reason.is_empty());
            }
            NameSource::Admitted(_) => panic!("None must not become an empty row set"),
        }
    }

    #[test]
    fn the_digest_covers_the_stored_fields_and_ignores_order() {
        let rows = vec![
            change("000004", "2001-03-07", "深安达A", "深安达B"),
            change("000001", "1998-04-28", "深发展A", "ST深发展"),
        ];
        let set = assemble_sz(&rows, Vec::new()).unwrap();
        let mut reversed = set.rows.clone();
        reversed.reverse();
        assert_eq!(dataset_digest(&set.rows), dataset_digest(&reversed));
        let mut renamed = set.rows.clone();
        let mut row = renamed[0].clone();
        row.name.push('X');
        renamed[0] = row;
        assert_ne!(dataset_digest(&set.rows), dataset_digest(&renamed));
    }
}
