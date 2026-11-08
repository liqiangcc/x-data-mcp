//! Declarative per-source refresh rules and the read-through reuse evidence
//! (design `docs/tasks/issue-446-exchange-refresh.md` §5.1, §5.2, §5.3, §8 R1).
//!
//! The refresh rules of all seven HTTP sources live in exactly one place: the
//! [`ReusePolicyTable`] declared below. A rule says what kind of source it is
//! (windowed or whole-table), how much of it the daily tail must re-read, the
//! full-refresh period `P` and the hard age bound `H = 2P` (§5.1). Consumers do
//! not carry these constants, and adding a source means adding one row plus its
//! parser/writer (design §9 scenario 2).
//!
//! **The table ships disabled.** Every row starts as [`Enabled::No`], so the
//! entry keeps fetching page by page exactly as before. R2 (the
//! `refresh_exchange_sources` task) and R3 (daily items read the source store
//! only) pass an enabled table once the refresh run exists; the machinery and
//! its evidence are what R1 delivers.
//!
//! ## What "reuse" means here
//!
//! A page is reusable when the index already holds a snapshot for the same
//! canonical request fingerprint (§5.2: "指纹稳定性本身就是复用边界"). Reuse
//! reads the frozen bytes back through [`crate::source_store::SourceStore`]'s
//! `read_verified` (a missing or tampered blob is `source_store_corrupt`, never
//! a silent fallback) and parses them again: the index row supplies the byte
//! identity, the fetch date and the recorded counts, never the parsed result.
//!
//! Three time fields stay apart (§5.3). Reuse never advances either of the
//! first two:
//!
//! - `as_of`/`fetched_at`: the day this byte identity was first observed;
//! - `known_at`: the day a fact first entered the system (consumers, R5);
//! - [`ReuseEvidence::verified_through`]: the last day the selected bytes were
//!   confirmed unchanged. Without a refresh run (R2) it falls back
//!   conservatively to the oldest selected `as_of`, never to "today".

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use super::SourceId;
use super::index::SnapshotRow;

/// Full-refresh period `P` of every source (§5.1): closed windows may be up to
/// one week stale, and one `full` run per week covers them.
pub const REFRESH_PERIOD_DAYS: u32 = 7;

/// Hard age bound `H = 2P` (§5.1): after two weeks without a successful
/// refresh the source items fail closed with `source_refresh_overdue` instead
/// of silently serving older bytes.
pub const MAX_AGE_DAYS: u32 = 14;

/// `L`, the tail span (§5.1): the open window plus one closed **natural
/// quarter** — not a day count, because these windows are cut by quarter.
pub const TAIL_CLOSED_QUARTERS: u32 = 1;

/// How a source changes, and therefore how it is re-used (§2.4, §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseClass {
    /// One request per window (natural quarter); closed windows change only
    /// when the exchange revises history, so they are reused by window.
    Window,
    /// A whole table paged by the source; one page cannot stand for the table,
    /// so reuse is by whole-table snapshot.
    Table,
}

impl ReuseClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Table => "table",
        }
    }

    /// `as_of` of a *dataset* built from these pages (§5.3): a whole table is
    /// as old as its oldest page, a windowed source as new as its newest one.
    fn dataset_as_of<'a>(self, pages: impl Iterator<Item = &'a str>) -> Option<&'a str> {
        match self {
            Self::Window => pages.max(),
            Self::Table => pages.min(),
        }
    }
}

/// What the daily `tail` re-reads for one source (§5.1's "日更 tail" column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TailScope {
    /// Open window plus [`TAIL_CLOSED_QUARTERS`] closed natural quarter(s); the
    /// other closed windows come from the source store.
    OpenWindowPlusPreviousQuarter,
    /// The whole table (cheap sources: 2–12 pages).
    WholeTable,
    /// Page 1 only as a change probe; a whole-table refresh is triggered when
    /// the probe moves (expensive sources: `szse_listing`, `szse_short_name`).
    FirstPageProbe,
}

impl TailScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenWindowPlusPreviousQuarter => "open_window_plus_previous_quarter",
            Self::WholeTable => "whole_table",
            Self::FirstPageProbe => "first_page_probe",
        }
    }
}

/// Whether a registered rule is in force.
///
/// Every declared row starts [`Enabled::No`]: R1 changes no existing behaviour
/// until R2/R3 turn the rows on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enabled {
    No,
    Yes,
}

impl Enabled {
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Yes)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::No => "no",
            Self::Yes => "yes",
        }
    }
}

/// One `source_id → rule` row (§5.1). It is evidence: the row that decided a
/// reuse is recorded in [`ReuseEvidence::policy`], so an audit does not have to
/// guess which constants were in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReusePolicy {
    pub source_id: SourceId,
    pub class: ReuseClass,
    pub tail: TailScope,
    /// `P`, days.
    pub refresh_period_days: u32,
    /// `H = 2P`, days.
    pub max_age_days: u32,
    pub enabled: Enabled,
}

impl ReusePolicy {
    const fn declared(
        source_id: SourceId,
        class: ReuseClass,
        tail: TailScope,
        enabled: Enabled,
    ) -> Self {
        Self {
            source_id,
            class,
            tail,
            refresh_period_days: REFRESH_PERIOD_DAYS,
            max_age_days: MAX_AGE_DAYS,
            enabled,
        }
    }
}

/// The one registry of refresh rules (§5.1).
///
/// Every HTTP source is registered, in [`SourceId::ALL`] order, with `P=7`,
/// `H=14` and `Enabled::No`. A new source adds a row here; a caller that must
/// not reuse anything keeps the default table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReusePolicyTable {
    policies: [ReusePolicy; 11],
}

impl ReusePolicyTable {
    /// The declared table as it ships: every rule registered, every rule off.
    pub const fn declared() -> Self {
        Self {
            policies: [
                ReusePolicy::declared(
                    SourceId::SseListing,
                    ReuseClass::Table,
                    TailScope::WholeTable,
                    Enabled::No,
                ),
                ReusePolicy::declared(
                    SourceId::SseDelisting,
                    ReuseClass::Table,
                    TailScope::WholeTable,
                    Enabled::No,
                ),
                ReusePolicy::declared(
                    SourceId::SseBulletin,
                    ReuseClass::Window,
                    TailScope::OpenWindowPlusPreviousQuarter,
                    Enabled::No,
                ),
                ReusePolicy::declared(
                    SourceId::SseSuspension,
                    ReuseClass::Window,
                    TailScope::OpenWindowPlusPreviousQuarter,
                    Enabled::No,
                ),
                ReusePolicy::declared(
                    SourceId::SseEtfList,
                    ReuseClass::Table,
                    TailScope::WholeTable,
                    Enabled::No,
                ),
                ReusePolicy::declared(
                    SourceId::SseFundDelistAnn,
                    ReuseClass::Window,
                    TailScope::OpenWindowPlusPreviousQuarter,
                    Enabled::No,
                ),
                ReusePolicy::declared(
                    SourceId::SzseListing,
                    ReuseClass::Table,
                    TailScope::FirstPageProbe,
                    Enabled::No,
                ),
                ReusePolicy::declared(
                    SourceId::SzseDelisting,
                    ReuseClass::Table,
                    TailScope::WholeTable,
                    Enabled::No,
                ),
                ReusePolicy::declared(
                    SourceId::SzseShortName,
                    ReuseClass::Table,
                    TailScope::FirstPageProbe,
                    Enabled::No,
                ),
                ReusePolicy::declared(
                    SourceId::SzseEtfList,
                    ReuseClass::Table,
                    TailScope::WholeTable,
                    Enabled::No,
                ),
                ReusePolicy::declared(
                    SourceId::SzseEtfNoticeAnn,
                    ReuseClass::Window,
                    TailScope::OpenWindowPlusPreviousQuarter,
                    Enabled::No,
                ),
            ],
        }
    }

    /// The rule registered for `source`; every [`SourceId`] has exactly one.
    pub fn for_source(&self, source: SourceId) -> ReusePolicy {
        self.policies
            .iter()
            .copied()
            .find(|policy| policy.source_id == source)
            .expect("the declared table registers every SourceId")
    }

    /// True when the source's rule is in force.
    pub fn enabled(&self, source: SourceId) -> bool {
        self.for_source(source).enabled.is_enabled()
    }

    /// Turn one registered rule on (R2/R3 and the R1 acceptance tests).
    pub fn with_enabled(mut self, source: SourceId, enabled: Enabled) -> Self {
        for policy in &mut self.policies {
            if policy.source_id == source {
                policy.enabled = enabled;
            }
        }
        self
    }

    /// Turn every registered rule on. Not used by the daily path: R2 enables
    /// the sources it refreshes, not all of them at once.
    pub fn with_all_enabled(mut self) -> Self {
        for policy in &mut self.policies {
            policy.enabled = Enabled::Yes;
        }
        self
    }
}

impl Default for ReusePolicyTable {
    fn default() -> Self {
        Self::declared()
    }
}

/// How one selected page came to be, recorded per page as `selected_by`
/// (§5.2). It separates "no request was sent because the bytes were already
/// frozen" from "a request was sent and answered the same bytes".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectedBy {
    /// Downloaded and frozen by this call.
    Fetch,
    /// Answered by this run's [`super::fetch::RunCache`]: already downloaded
    /// by an earlier item of the same run.
    RunCache,
    /// Read through the index: same request fingerprint, bytes already frozen
    /// by an earlier run.
    Reuse,
}

impl SelectedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::RunCache => "run_cache",
            Self::Reuse => "reuse",
        }
    }
}

/// One page's contribution to the reuse evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPage {
    pub snapshot_id: String,
    pub as_of: String,
    pub selected_by: SelectedBy,
}

/// One selected snapshot and how it was selected (§5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotSelection {
    pub snapshot_id: String,
    pub selected_by: SelectedBy,
}

/// The reuse block of an acquisition's evidence (§5.2, §5.3).
///
/// `reused` counts the pages served without a download (run cache or
/// read-through), `fetched` the pages this call downloaded; `reused + fetched`
/// is the number of selected pages. `as_of` follows §5.3's dataset rule, so it
/// never becomes "today" just because the bytes were re-used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReuseEvidence {
    /// The rule that was in force, recorded even when every page was fetched.
    pub policy: ReusePolicy,
    pub reused: usize,
    pub fetched: usize,
    /// `max(as_of)` for a windowed source, `min(as_of)` for a whole table.
    pub as_of: Option<String>,
    /// Last day every selected byte was confirmed unchanged (§5.3). Without a
    /// refresh run this is the oldest selected `as_of`: the conservative
    /// fallback the design prescribes, never the run's date.
    pub verified_through: Option<String>,
    pub selected_by: Vec<SnapshotSelection>,
}

impl ReuseEvidence {
    /// Evidence of one acquisition from its selected pages, in page order.
    pub fn from_pages(policy: ReusePolicy, pages: &[SelectedPage]) -> Self {
        let fetched = pages
            .iter()
            .filter(|page| page.selected_by == SelectedBy::Fetch)
            .count();
        let as_of = policy
            .class
            .dataset_as_of(pages.iter().map(|page| page.as_of.as_str()))
            .map(str::to_string);
        let verified_through = pages
            .iter()
            .map(|page| page.as_of.as_str())
            .min()
            .map(str::to_string);
        Self {
            policy,
            reused: pages.len() - fetched,
            fetched,
            as_of,
            verified_through,
            selected_by: pages
                .iter()
                .map(|page| SnapshotSelection {
                    snapshot_id: page.snapshot_id.clone(),
                    selected_by: page.selected_by,
                })
                .collect(),
        }
    }

    /// Evidence of a window that selected no page (it failed before the first
    /// page): the counts stay zero and no date is claimed.
    pub fn empty(policy: ReusePolicy) -> Self {
        Self::from_pages(policy, &[])
    }

    /// Merge per-window evidence of one multi-window run, keeping window/page
    /// order: sums of the counters, §5.3's date rule over the window dates, and
    /// the oldest `verified_through`.
    pub fn merged(policy: ReusePolicy, parts: &[ReuseEvidence]) -> Self {
        let reused = parts.iter().map(|part| part.reused).sum();
        let fetched = parts.iter().map(|part| part.fetched).sum();
        let as_of = match policy.class {
            ReuseClass::Window => parts.iter().filter_map(|part| part.as_of.as_deref()).max(),
            ReuseClass::Table => parts.iter().filter_map(|part| part.as_of.as_deref()).min(),
        }
        .map(str::to_string);
        let verified_through = parts
            .iter()
            .filter_map(|part| part.verified_through.as_deref())
            .min()
            .map(str::to_string);
        Self {
            policy,
            reused,
            fetched,
            as_of,
            verified_through,
            selected_by: parts
                .iter()
                .flat_map(|part| part.selected_by.iter().cloned())
                .collect(),
        }
    }
}

/// First day of the natural quarter `date` falls in.
pub fn quarter_start(date: NaiveDate) -> NaiveDate {
    let month = ((date.month() - 1) / 3) * 3 + 1;
    NaiveDate::from_ymd_opt(date.year(), month, 1).expect("a quarter start is a valid date")
}

/// How much of one window may be served from the store (#604 / D-87).
///
/// Only windowed sources can be reused per window, and only the windows that
/// are **closed** (see [`window_closed`]): the design's `L = 1` closed quarter
/// is re-read every day by the tail even though the exchange can only revise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowReuse {
    /// This window may be read through the index when its request fingerprint
    /// already holds frozen bytes.
    ClosedWindow,
    /// This window is fetched, even on a fingerprint hit. Used for the open
    /// window (whose bounds move every day, so its bytes must be re-read) and
    /// for `full`, the weekly run whose purpose is to find revisions in the
    /// closed windows (§5.1 `P = 7`).
    Fetch,
}

impl WindowReuse {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClosedWindow => "closed_window",
            Self::Fetch => "fetch",
        }
    }
}

/// Whether a window is a **closed** natural quarter relative to `today`.
///
/// D-87's rule, stated instead of inferred from the fingerpint: a window is
/// closed when its end day is before the first day of `today`'s natural
/// quarter, i.e. the quarter the window belongs to has already finished.
/// The window builders (`etf_delisting::windows`, `a3::windows`,
/// `quarter_segments`) end the open window at `today - 1`, so "ended before my
/// own quarter began" is exactly the older window `trim_tail` keeps. A window
/// that runs into the current quarter — the open window, or the last segment of
/// a quarter truncated by `today` — is open and always fetched.
///
/// This deliberately does not rely on "the open window's fingerprint changes
/// every day and therefore never hits the index": that would be a statement
/// about the calendar, not about which window the run intends to re-read, and
/// it stops holding the day the first run of a quarter uses the same bounds
/// twice.
pub fn window_closed(window_end: NaiveDate, today: NaiveDate) -> bool {
    window_end < quarter_start(today)
}

/// Which reuse a window may use, given the source's rule, the window's end day
/// and whether the run re-reads the window anyway.
///
/// `reuse_closed_windows` is the caller's statement of *intent*: it is `true`
/// only for a `tail` of a windowed source. `full` passes `false` because it
/// re-reads every quarter to find revisions, and the open window is fetched
/// even under `tail`.
pub fn window_reuse(
    policy: ReusePolicy,
    window_end: NaiveDate,
    today: NaiveDate,
    reuse_closed_windows: bool,
) -> WindowReuse {
    if !policy.enabled.is_enabled() || policy.class != ReuseClass::Window {
        return WindowReuse::Fetch;
    }
    if !reuse_closed_windows {
        return WindowReuse::Fetch;
    }
    if !window_closed(window_end, today) {
        return WindowReuse::Fetch;
    }
    WindowReuse::ClosedWindow
}

/// The latest indexed snapshot of one canonical request (§5.2).
///
/// The order is `(fetched_at, snapshot_id)`: a revised answer (same request,
/// new sha) replaces the answer it superseded, and two acquisitions in the same
/// second still select deterministically.
pub fn latest_candidate<'a>(
    rows: &'a [SnapshotRow],
    source: SourceId,
    fingerprint: &str,
) -> Option<&'a SnapshotRow> {
    rows.iter()
        .filter(|row| row.source_id == source.as_str() && row.request_fingerprint == fingerprint)
        .max_by(|left, right| {
            (left.fetched_at.as_str(), left.snapshot_id.as_str())
                .cmp(&(right.fetched_at.as_str(), right.snapshot_id.as_str()))
        })
}

/// Age of a stored `as_of` in days relative to `today`; negative for a future
/// date (clock skew must not read as a fresh observation).
pub fn age_days(as_of: &str, today: NaiveDate) -> Option<i64> {
    NaiveDate::parse_from_str(as_of, "%Y-%m-%d")
        .ok()
        .map(|date| today.signed_duration_since(date).num_days())
}
