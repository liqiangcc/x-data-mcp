//! Historical research-industry snapshots and intervals (design
//! `docs/tasks/issue-446-m3-delisted-industry.md` §5.1–§5.6, §9.4.2).
//!
//! Two layers, never one:
//!
//! - **snapshots** are the immutable raw fact: one `tdxhy.cfg` content
//!   (`member_sha256`), how it was obtained, and its evidence grade. The
//!   original bytes never live here — they are content-addressed in the source
//!   store (§9.4.1) and re-parsed on demand;
//! - **assignments** are the queryable intervals, computed from adjacent
//!   snapshots and *not* stored as full snapshot rows (§5.1).
//!
//! Everything in this module is pure: bytes in, rows out. The database writer
//! is `crate::sqlite_market::industry` (crate-private), the source-store
//! replay is [`crate::source_store::replay`], and the daily item is
//! `derived:industry_snapshot`. Keeping the computation here is what lets the
//! daily path and the fresh-init replay produce *bit-identical* tables from the
//! same inputs — the acceptance that makes this fact survive a deployment.
//!
//! Interval rules (§5.1/§5.2), all of them "never earlier than we can prove":
//!
//! - the timeline is the **observation** sequence, not the set of contents: each
//!   manifest registration is one observation, so a file that goes A → B → A was
//!   changed twice and yields three intervals. Only *adjacent* observations of
//!   the same content fold into one step, dated by the earliest publisher time in
//!   that step (§5.3);
//! - a code present in adjacent steps with the same `(t_code, x_code)` does not
//!   open a new row, it extends the interval;
//! - a changed mapping closes the old interval at the new step's start and opens
//!   a new one;
//! - a code that disappeared closes at *that step's start* (we can only prove
//!   "not in it from this observation on");
//! - two steps on one day are ordered by `(ingested_at, member_sha256)`; the
//!   later one starts that same day and the earlier one's same-day segment is
//!   zero-length and therefore not written — the collision is counted as a
//!   diagnostic instead of being silently resolved;
//! - grade C evidence (no publisher time, no manifest) is stored as a snapshot
//!   but never produces an interval.
//!
//! Storage and history answer different questions (§5.3): `industry_snapshots`
//! keeps one row per content with the evidence merged over every registration of
//! it, while the interval table distinguishes the same content seen at different
//! times.
//!
//! `known_at` is when *this system* knew the content: the acquisition time for
//! a forward generation, the import time for a backfilled archive. It is never
//! derived from a file mtime and never back-dated to the archive's own
//! acquisition time, because that would claim historical knowledge we did not
//! have (§5.2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{NaiveDate, SecondsFormat, Utc};
use serde::Serialize;

use crate::catalog_tdx::{incon, scope, tdxhy};
use crate::source_store::SourceStore;

/// The only industry scheme this dataset carries (§5.5).
pub const SCHEME: &str = "tdx_research_industry";
/// Configuration override of the source-store root. Absent, the store is
/// `<database parent>/source-store`, the layout `deploy_phase_g.sh` uses.
pub const SOURCE_STORE_ENV: &str = "X_DATA_MCP_SOURCE_STORE";
/// `incon.dat` section holding the research-industry dictionary (§5.6).
pub const DICTIONARY_SECTION: &str = "TDXRSHY";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("industry_member_invalid: {0}")]
    Tdxhy(#[from] tdxhy::TdxhyError),
    #[error("industry_dictionary_invalid: {0}")]
    Incon(#[from] incon::InconError),
    #[error("industry_snapshot_invalid: {0}")]
    Invalid(String),
    #[error("{0}")]
    Store(#[from] crate::source_store::StoreError),
}

impl Error {
    /// Stable reason code for the item/CLI surface.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Tdxhy(_) => "industry_member_invalid",
            Self::Incon(_) => "industry_dictionary_invalid",
            Self::Invalid(_) => "industry_snapshot_invalid",
            Self::Store(error) => error.code(),
        }
    }
}

/// Where a snapshot's bytes came from (§5.1 `source_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Generation,
    Archive,
    RepoEvidence,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generation => "generation",
            Self::Archive => "archive",
            Self::RepoEvidence => "repo_evidence",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "generation" => Some(Self::Generation),
            "archive" => Some(Self::Archive),
            "repo_evidence" => Some(Self::RepoEvidence),
            _ => None,
        }
    }
}

/// How an interval's start date was obtained (§5.2 basis column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    PublisherLastModified,
    AcquisitionTime,
    EvidenceCommitDate,
}

impl Basis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PublisherLastModified => "publisher_last_modified",
            Self::AcquisitionTime => "acquisition_time",
            Self::EvidenceCommitDate => "evidence_commit_date",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "publisher_last_modified" => Some(Self::PublisherLastModified),
            "acquisition_time" => Some(Self::AcquisitionTime),
            "evidence_commit_date" => Some(Self::EvidenceCommitDate),
            _ => None,
        }
    }
}

/// Name resolution outcome of one assignment (§5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameStatus {
    Resolved,
    UnknownDictionary,
    NotInDictionary,
}

impl NameStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::UnknownDictionary => "unknown_dictionary",
            Self::NotInDictionary => "not_in_dictionary",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "resolved" => Some(Self::Resolved),
            "unknown_dictionary" => Some(Self::UnknownDictionary),
            "not_in_dictionary" => Some(Self::NotInDictionary),
            _ => None,
        }
    }
}

/// One in-scope `tdxhy.cfg` row, projected to the read-model identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndustryMember {
    /// `sh` or `sz` (the market digit maps 1/0, as everywhere else).
    pub exchange: String,
    pub code: String,
    /// Level-2 research-industry code (the `X` code's level-2 ancestor).
    pub industry_code: String,
    pub t_code: String,
    pub level: i64,
}

/// Snapshot metadata: the content identity plus its evidence (§5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRecord {
    pub member_sha256: String,
    pub store_sha256: String,
    pub base_zip_sha256: Option<String>,
    pub generation_manifest_sha256: Option<String>,
    pub source_kind: SourceKind,
    pub publisher_last_modified: Option<String>,
    pub observed_at: Option<String>,
    /// When this system first obtained the content (never a file mtime).
    pub ingested_at: String,
    /// When the bytes were imported into this system, for backfilled archives
    /// (§5.2: `known_at` = backfill ingestion, not the historical fetch time).
    pub imported_at: Option<String>,
    /// `A`/`B`/`C` (§5.3).
    pub evidence_grade: String,
    pub parser_version: String,
    pub row_count: i64,
    pub scope_member_count: i64,
    pub dictionary_sha256: Option<String>,
    pub notes: Option<String>,
}

impl SnapshotRecord {
    /// The knowledge time of the intervals this snapshot produces: forward
    /// generations use the acquisition time, backfilled archives the import
    /// time (§5.2). Grade C produces no interval, so its value is unused.
    pub fn known_at(&self) -> String {
        match self.source_kind {
            SourceKind::Archive => self
                .imported_at
                .clone()
                .unwrap_or_else(|| self.ingested_at.clone()),
            SourceKind::Generation | SourceKind::RepoEvidence => self.ingested_at.clone(),
        }
    }

    /// The interval start and its basis, or `None` for grade C (stored, never
    /// interval-defining).
    pub fn boundary(&self) -> Result<Option<(i64, Basis)>, Error> {
        if self.evidence_grade == "C" {
            return Ok(None);
        }
        if let Some(last_modified) = self
            .publisher_last_modified
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            let day = parse_publisher_day(last_modified)?;
            return Ok(Some((epoch_day(day), Basis::PublisherLastModified)));
        }
        let observed = self
            .observed_at
            .as_deref()
            .unwrap_or(self.ingested_at.as_str());
        let day = parse_day_prefix(observed)?;
        Ok(Some((epoch_day(day), Basis::AcquisitionTime)))
    }

    /// §5.3 step 1: several registrations of one content describe **one**
    /// snapshot. The merged record keeps the earliest non-empty publisher time,
    /// the earliest observation and ingestion times and the best evidence
    /// grade; the content-identifying fields come from the earliest
    /// registration.
    ///
    /// `known_at` is deliberately *not* merged here: it is a property of an
    /// observation, and the run merge adds the observations' knowledge times
    /// explicitly (one record cannot see the others').
    pub fn merged_with(&self, other: &Self) -> SnapshotRecord {
        let self_first = (
            self.ingested_at.as_str(),
            self.observed_at.as_deref().unwrap_or(""),
        ) <= (
            other.ingested_at.as_str(),
            other.observed_at.as_deref().unwrap_or(""),
        );
        let (base, extra) = if self_first {
            (self, other)
        } else {
            (other, self)
        };
        SnapshotRecord {
            member_sha256: base.member_sha256.clone(),
            store_sha256: base.store_sha256.clone(),
            base_zip_sha256: base.base_zip_sha256.clone(),
            generation_manifest_sha256: base.generation_manifest_sha256.clone(),
            source_kind: base.source_kind,
            publisher_last_modified: earliest_publisher_time(
                base.publisher_last_modified.as_deref(),
                extra.publisher_last_modified.as_deref(),
            ),
            observed_at: earliest_timestamp(
                base.observed_at.as_deref(),
                extra.observed_at.as_deref(),
            ),
            ingested_at: earliest_timestamp(
                Some(base.ingested_at.as_str()),
                Some(extra.ingested_at.as_str()),
            )
            .unwrap_or_else(|| base.ingested_at.clone()),
            imported_at: earliest_timestamp(
                base.imported_at.as_deref(),
                extra.imported_at.as_deref(),
            ),
            evidence_grade: best_grade(&base.evidence_grade, &extra.evidence_grade),
            parser_version: base.parser_version.clone(),
            row_count: base.row_count,
            scope_member_count: base.scope_member_count,
            dictionary_sha256: base
                .dictionary_sha256
                .clone()
                .or_else(|| extra.dictionary_sha256.clone()),
            notes: base.notes.clone().or_else(|| extra.notes.clone()),
        }
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

/// The earlier of two publisher times, compared by the day they name: the
/// archive keeps both HTTP-dates and RFC 3339 spellings of the same moment.
fn earliest_publisher_time(left: Option<&str>, right: Option<&str>) -> Option<String> {
    let (left, right) = match (non_empty(left), non_empty(right)) {
        (Some(left), Some(right)) => (left, right),
        (Some(only), None) | (None, Some(only)) => return Some(only.to_string()),
        (None, None) => return None,
    };
    let left_first = match (
        parse_publisher_day(left).ok(),
        parse_publisher_day(right).ok(),
    ) {
        (Some(left_day), Some(right_day)) => (left_day, left) <= (right_day, right),
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => left <= right,
    };
    let earliest = if left_first { left } else { right };
    Some(earliest.to_string())
}

/// The earlier of two RFC 3339 timestamps (lexicographic order is chronological
/// for that shape), keeping whichever side exists.
fn earliest_timestamp(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (non_empty(left), non_empty(right)) {
        (Some(left), Some(right)) => {
            let earliest = if left <= right { left } else { right };
            Some(earliest.to_string())
        }
        (Some(only), None) | (None, Some(only)) => Some(only.to_string()),
        (None, None) => None,
    }
}

/// `A` beats `B` beats `C`: the best evidence a run carries.
fn best_grade(left: &str, right: &str) -> String {
    if left <= right {
        left.to_string()
    } else {
        right.to_string()
    }
}

/// Everything one snapshot contributes to a rebuild: its metadata, its parsed
/// members and the dictionary of the same source (§5.6).
#[derive(Debug, Clone)]
pub struct SnapshotFacts {
    pub record: SnapshotRecord,
    pub members: Vec<IndustryMember>,
    pub dictionary: BTreeMap<String, String>,
    /// The source carried a dictionary at all. A snapshot without one names
    /// every assignment `unknown_dictionary` instead of pretending the code is
    /// absent from a dictionary we never read.
    pub dictionary_present: bool,
}

/// One interval row (§5.1) — the exact shape `industry_assignments` stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssignmentRow {
    pub exchange: String,
    pub code: String,
    pub level: i64,
    pub industry_code: String,
    pub name: Option<String>,
    pub name_status: String,
    /// Epoch day, the same unit as `series_bars.trade_day`.
    pub effective_from: i64,
    pub effective_to: Option<i64>,
    pub known_at: String,
    pub basis: String,
    pub evidence_grade: String,
    pub t_code: Option<String>,
    pub snapshot_member_sha256: String,
}

impl AssignmentRow {
    /// `(exchange, code, scheme, effective_from)` — the table's primary key.
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.exchange, self.code, SCHEME, self.effective_from
        )
    }
}

/// The deterministic outcome of replaying a set of snapshots.
#[derive(Debug, Clone, Default)]
pub struct Rebuild {
    /// Snapshots in `(ingested_at, member_sha256)` order (append order).
    pub snapshots: Vec<SnapshotRecord>,
    /// Intervals, ordered by `(exchange, code, effective_from)`.
    pub assignments: Vec<AssignmentRow>,
    /// Timeline runs — adjacent identical observations folded into one step —
    /// that defined an interval (the "boundaries"). A content that comes back
    /// after a different one counts again: A → B → A is three.
    pub boundaries: usize,
    /// Same-day collisions where a later snapshot superseded an earlier
    /// same-day mapping (zero-length segments are not written).
    pub superseded: usize,
}

/// Parse one `tdxhy.cfg` member: the frozen scope rule picks the `sh`/`sz`
/// A-share rows, and the `X` code is reduced to its level-2 ancestor (§5.5).
///
/// A repeated code with a conflicting mapping inside one file is rejected: the
/// interval table has one row per `(entity, effective_from)` and guessing which
/// duplicate wins would invent a fact. Identical repeats are folded.
pub fn parse_members(bytes: &[u8]) -> Result<(i64, Vec<IndustryMember>), Error> {
    let rows = tdxhy::parse(bytes)?;
    let row_count = i64::try_from(rows.len())
        .map_err(|_| Error::Invalid("tdxhy.cfg row count overflow".into()))?;
    let outcome = scope::apply(&rows);
    let mut members: BTreeMap<(String, String), IndustryMember> = BTreeMap::new();
    for member in outcome.members {
        let exchange = match member.market {
            1 => "sh".to_string(),
            0 => "sz".to_string(),
            other => {
                return Err(Error::Invalid(format!(
                    "tdxhy.cfg line {} has out-of-scope market digit {other}",
                    member.line
                )));
            }
        };
        let Some(industry_code) = incon::research_level2(&member.x_code) else {
            // A level-1 X code has no level-2 ancestor; it is not an
            // assignment in this scheme and must not be guessed one.
            continue;
        };
        // `research_level2` returns the level-2 code itself or its level-2
        // ancestor, so anything else is a projection bug, not a default.
        let level = match incon::code_level(&industry_code) {
            Some(2) => 2i64,
            _ => {
                return Err(Error::Invalid(format!(
                    "tdxhy.cfg line {} yields {industry_code:?}, not a level-2 code",
                    member.line
                )));
            }
        };
        let projected = IndustryMember {
            exchange,
            code: member.code.clone(),
            industry_code,
            t_code: member.t_code.clone(),
            level,
        };
        match members.get(&(projected.exchange.clone(), projected.code.clone())) {
            Some(existing) if existing != &projected => {
                return Err(Error::Invalid(format!(
                    "tdxhy.cfg line {} repeats {}.{} with a conflicting mapping",
                    member.line, projected.exchange, projected.code
                )));
            }
            _ => {
                members.insert(
                    (projected.exchange.clone(), projected.code.clone()),
                    projected,
                );
            }
        }
    }
    Ok((row_count, members.into_values().collect()))
}

/// Parse `incon.dat` and keep the research-industry dictionary (§5.6).
/// `None` means the file carries no `#TDXRSHY` section; an empty map from a
/// present section is reported as present-but-empty.
pub fn parse_dictionary(bytes: &[u8]) -> Result<Option<BTreeMap<String, String>>, Error> {
    let sections = incon::parse(bytes)?;
    let Some(entries) = incon::section_map(&sections, DICTIONARY_SECTION) else {
        return Ok(None);
    };
    let mut dictionary = BTreeMap::new();
    for (code, name) in entries {
        dictionary.insert(code.to_string(), name.to_string());
    }
    Ok(Some(dictionary))
}

/// One timeline step handed to [`Segments`]: the content, the merged evidence
/// of the run it belongs to, and the dictionary that names it. `rebuild` and
/// `apply_on` share this shape on purpose — an appended observation and a
/// replayed one cannot diverge.
struct Step<'a> {
    member_sha256: &'a str,
    members: &'a [IndustryMember],
    dictionary: &'a BTreeMap<String, String>,
    dictionary_present: bool,
    /// Interval start of this step (§5.2).
    day: i64,
    basis: Basis,
    /// When this system knew the content (§5.2) — never a file mtime.
    known_at: &'a str,
    evidence_grade: &'a str,
}

/// How an incoming observation relates to the newest stored run (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLink {
    /// The observation right before it carried the same content: this one
    /// extends that run, so the run's evidence may be improved in place.
    Continue,
    /// A different observation. Even a *known* content starts its own run here:
    /// a file that goes A → B → A was changed twice, and folding the two `A`
    /// observations into one would leave the history stuck at `B`.
    Fresh,
}

/// Deterministic rebuild of the snapshot list and the assignment intervals from
/// a set of observations. This is the *only* implementation of §5.1: the daily
/// item and `replay --scope industry` both end up here, which is what makes
/// "clear the DB and replay" reproduce the same table contents.
///
/// Two different dedupe rules, because storage and history are not the same
/// question (§5.3):
///
/// - **storage** keeps one row per content, with the evidence merged over every
///   registration of it (earliest publisher time, earliest observation, best
///   grade);
/// - **the timeline** keeps every registration. Observations are ordered by
///   `(interval start, ingested_at, member_sha256)` (§9.4.2) and only *adjacent*
///   observations of the same content fold into one interval, with the earliest
///   publisher time of the run as its start.
pub fn rebuild(observations: &[SnapshotFacts]) -> Result<Rebuild, Error> {
    let mut stored: BTreeMap<&str, SnapshotRecord> = BTreeMap::new();
    for facts in observations {
        let record = &facts.record;
        match stored.get_mut(record.member_sha256.as_str()) {
            Some(merged) => *merged = merged.merged_with(record),
            None => {
                stored.insert(record.member_sha256.as_str(), record.clone());
            }
        }
    }

    // Grade C is stored but never defines an interval (§5.3), so it takes no
    // part in the timeline at all.
    let mut timeline: Vec<(&SnapshotFacts, i64, Basis)> = Vec::new();
    for facts in observations {
        let Some((day, basis)) = facts.record.boundary()? else {
            continue;
        };
        timeline.push((facts, day, basis));
    }
    timeline.sort_by(|left, right| {
        left.1
            .cmp(&right.1)
            .then_with(|| left.0.record.ingested_at.cmp(&right.0.record.ingested_at))
            .then_with(|| {
                left.0
                    .record
                    .member_sha256
                    .cmp(&right.0.record.member_sha256)
            })
    });

    let mut segments = Segments::default();
    let mut steps = 0usize;
    let mut cursor = 0usize;
    while cursor < timeline.len() {
        let mut end = cursor + 1;
        while end < timeline.len()
            && timeline[end].0.record.member_sha256 == timeline[cursor].0.record.member_sha256
        {
            end += 1;
        }
        let run = &timeline[cursor..end];
        // §5.3: one interval per run, dated by the earliest publisher time in
        // it, else by the earliest acquisition time, with the best grade and the
        // earliest knowledge time of its observations.
        let mut merged = run[0].0.record.clone();
        let mut known_at = run[0].0.record.known_at();
        for (facts, _, _) in &run[1..] {
            merged = merged.merged_with(&facts.record);
            if facts.record.known_at() < known_at {
                known_at = facts.record.known_at();
            }
        }
        let Some((day, basis)) = merged.boundary()? else {
            return Err(Error::Invalid(format!(
                "the run of {} lost its interval boundary in the metadata merge",
                merged.member_sha256
            )));
        };
        steps += 1;
        let step = Step {
            member_sha256: &merged.member_sha256,
            members: &run[0].0.members,
            dictionary: &run[0].0.dictionary,
            dictionary_present: run[0].0.dictionary_present,
            day,
            basis,
            known_at: &known_at,
            evidence_grade: &merged.evidence_grade,
        };
        segments.apply(&step, RunLink::Fresh)?;
        cursor = end;
    }

    let superseded = segments.superseded;
    let assignments = segments.into_rows();
    let mut snapshots: Vec<SnapshotRecord> = stored.into_values().collect();
    snapshots.sort_by(|left, right| {
        left.ingested_at
            .cmp(&right.ingested_at)
            .then_with(|| left.member_sha256.cmp(&right.member_sha256))
    });
    Ok(Rebuild {
        snapshots,
        assignments,
        boundaries: steps,
        superseded,
    })
}

/// Apply one observation on top of the stored intervals — the daily item path.
///
/// A daily generation always observes a *later* state than what is stored, so a
/// snapshot that starts before the stored coverage is a backfill and must go
/// through `replay --scope industry`, which recomputes the whole table from the
/// source store (§5.3). Rejecting it here keeps the two paths from ever
/// disagreeing: the stored table can only be extended forward by this function.
///
/// `link` says whether the observation continues the newest run. A content that
/// was seen before but is *not* the previous observation opens a new interval
/// (A → B → A), so nothing here looks the content up in the table.
///
/// Grade C evidence is stored as a snapshot but contributes no interval, so the
/// rows come back unchanged.
pub fn apply_on(
    existing: &[AssignmentRow],
    facts: &SnapshotFacts,
    link: RunLink,
) -> Result<(Vec<AssignmentRow>, usize), Error> {
    let Some((day, basis)) = facts.record.boundary()? else {
        return Ok((existing.to_vec(), 0));
    };
    if let Some(current) = existing.iter().map(|row| row.effective_from).max()
        && current > day
    {
        return Err(Error::Invalid(format!(
            "snapshot {} starts {} before the stored coverage {}; a historical snapshot \
             is a backfill and must be replayed from the source store",
            facts.record.member_sha256, day, current
        )));
    }
    let known_at = facts.record.known_at();
    let step = Step {
        member_sha256: &facts.record.member_sha256,
        members: &facts.members,
        dictionary: &facts.dictionary,
        dictionary_present: facts.dictionary_present,
        day,
        basis,
        known_at: &known_at,
        evidence_grade: &facts.record.evidence_grade,
    };
    let mut segments = Segments::from_rows(existing)?;
    segments.apply(&step, link)?;
    let superseded = segments.superseded;
    Ok((segments.into_rows(), superseded))
}

/// Mutable interval assembly: one ordered segment list per `(exchange, code)`.
#[derive(Default)]
struct Segments {
    per_entity: BTreeMap<(String, String), Vec<Segment>>,
    superseded: usize,
}

struct Segment {
    level: i64,
    industry_code: String,
    t_code: Option<String>,
    name: Option<String>,
    name_status: &'static str,
    effective_from: i64,
    effective_to: Option<i64>,
    known_at: String,
    basis: &'static str,
    evidence_grade: String,
    snapshot: String,
}

impl Segments {
    /// Rebuild the segment state from stored rows, so an incremental apply and
    /// a full replay start from the same shape. Rows are grouped per entity and
    /// ordered by `effective_from`, which is the order [`Segments::apply`]
    /// expects (one open segment last).
    fn from_rows(rows: &[AssignmentRow]) -> Result<Self, Error> {
        let mut per_entity: BTreeMap<(String, String), Vec<Segment>> = BTreeMap::new();
        for row in rows {
            let name_status = NameStatus::parse(&row.name_status).ok_or_else(|| {
                Error::Invalid(format!("stored name_status {:?}", row.name_status))
            })?;
            let basis = Basis::parse(&row.basis)
                .ok_or_else(|| Error::Invalid(format!("stored basis {:?}", row.basis)))?;
            per_entity
                .entry((row.exchange.clone(), row.code.clone()))
                .or_default()
                .push(Segment {
                    level: row.level,
                    industry_code: row.industry_code.clone(),
                    t_code: row.t_code.clone(),
                    name: row.name.clone(),
                    name_status: name_status.as_str(),
                    effective_from: row.effective_from,
                    effective_to: row.effective_to,
                    known_at: row.known_at.clone(),
                    basis: basis.as_str(),
                    evidence_grade: row.evidence_grade.clone(),
                    snapshot: row.snapshot_member_sha256.clone(),
                });
        }
        for segments in per_entity.values_mut() {
            segments.sort_by_key(|segment| segment.effective_from);
        }
        Ok(Self {
            per_entity,
            superseded: 0,
        })
    }

    fn apply(&mut self, step: &Step<'_>, link: RunLink) -> Result<(), Error> {
        let day = step.day;
        let present: BTreeSet<(String, String)> = step
            .members
            .iter()
            .map(|member| (member.exchange.clone(), member.code.clone()))
            .collect();
        for member in step.members {
            let (name, name_status) = if !step.dictionary_present {
                (None, NameStatus::UnknownDictionary)
            } else {
                match step.dictionary.get(&member.industry_code) {
                    Some(name) => (Some(name.clone()), NameStatus::Resolved),
                    None => (None, NameStatus::NotInDictionary),
                }
            };
            let segment = Segment {
                level: member.level,
                industry_code: member.industry_code.clone(),
                t_code: Some(member.t_code.clone()),
                name,
                name_status: name_status.as_str(),
                effective_from: day,
                effective_to: None,
                known_at: step.known_at.to_string(),
                basis: step.basis.as_str(),
                evidence_grade: step.evidence_grade.to_string(),
                snapshot: step.member_sha256.to_string(),
            };
            let key = (member.exchange.clone(), member.code.clone());
            let segments = self.per_entity.entry(key).or_default();
            match segments.last_mut() {
                None => segments.push(segment),
                Some(last) => {
                    // Only an *open* identical segment is extended: a code that
                    // vanished and came back with the same mapping is a new
                    // interval, not a reopening of the closed one (§5.1).
                    let unchanged = last.effective_to.is_none()
                        && last.industry_code == segment.industry_code
                        && last.t_code == segment.t_code;
                    if unchanged {
                        // Extend the open interval. Inside one run the evidence
                        // may still improve (§5.3); across runs it stays as
                        // first written.
                        if link == RunLink::Continue && last.snapshot == step.member_sha256 {
                            improve_run_evidence(last, step)?;
                        }
                        continue;
                    }
                    if last.effective_to.is_some() {
                        // The code had already ended: the closed interval is
                        // left exactly as stored and a new one opens here.
                        segments.push(segment);
                    } else if last.effective_from == day {
                        // Same-day collision: the earlier same-day mapping is
                        // zero-length and is not written (§5.2).
                        *last = segment;
                        self.superseded += 1;
                    } else {
                        last.effective_to = Some(day);
                        segments.push(segment);
                    }
                }
            }
        }
        // Codes that disappeared close at this snapshot's start: we can only
        // prove "not in it from this observation on" (§5.1).
        let vanished: Vec<(String, String)> = self
            .per_entity
            .iter()
            .filter(|(key, segments)| {
                !present.contains(key)
                    && segments
                        .last()
                        .is_some_and(|last| last.effective_to.is_none())
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in vanished {
            let segments = self
                .per_entity
                .get_mut(&key)
                .ok_or_else(|| Error::Invalid("interval entity vanished mid-rebuild".into()))?;
            let last = segments
                .last_mut()
                .ok_or_else(|| Error::Invalid("interval entity has no segment".into()))?;
            if last.effective_from == day {
                segments.pop();
                self.superseded += 1;
            } else {
                last.effective_to = Some(day);
            }
            if segments.is_empty() {
                self.per_entity.remove(&key);
            }
        }
        Ok(())
    }

    fn into_rows(self) -> Vec<AssignmentRow> {
        let mut rows = Vec::new();
        for ((exchange, code), segments) in self.per_entity {
            for segment in segments {
                rows.push(AssignmentRow {
                    exchange: exchange.clone(),
                    code: code.clone(),
                    level: segment.level,
                    industry_code: segment.industry_code,
                    name: segment.name,
                    name_status: segment.name_status.to_string(),
                    effective_from: segment.effective_from,
                    effective_to: segment.effective_to,
                    known_at: segment.known_at,
                    basis: segment.basis.to_string(),
                    evidence_grade: segment.evidence_grade,
                    t_code: segment.t_code,
                    snapshot_member_sha256: segment.snapshot,
                });
            }
        }
        rows
    }
}

/// §5.3 inside one run: the run keeps the earliest publisher time and the best
/// evidence grade. Moving a published interval's start is a history rewrite, so
/// the daily path refuses it and points at `replay --scope industry`, which
/// recomputes the table from the source store.
fn improve_run_evidence(last: &mut Segment, step: &Step<'_>) -> Result<(), Error> {
    let run_has_publisher = last.basis == Basis::PublisherLastModified.as_str();
    let incoming_has_publisher = step.basis == Basis::PublisherLastModified;
    if incoming_has_publisher {
        if run_has_publisher && step.day < last.effective_from {
            return Err(Error::Invalid(format!(
                "snapshot {} is dated {} but the open interval started {}: only a \
                 source-store replay may move a published interval start",
                step.member_sha256, step.day, last.effective_from
            )));
        }
        if !run_has_publisher && step.day != last.effective_from {
            return Err(Error::Invalid(format!(
                "snapshot {} brings publisher time {} for an interval that opened on \
                 acquisition time at {}: only a source-store replay may re-date it",
                step.member_sha256, step.day, last.effective_from
            )));
        }
        last.basis = Basis::PublisherLastModified.as_str();
    }
    if step.evidence_grade < last.evidence_grade.as_str() {
        last.evidence_grade = step.evidence_grade.to_string();
    }
    Ok(())
}

/// The coverage start of the whole dataset: the earliest interval start, i.e.
/// the earliest A/B snapshot (§5.4). `None` means no interval-defining
/// snapshot exists yet and every `as_of` is before coverage.
pub fn coverage_start(assignments: &[AssignmentRow]) -> Option<i64> {
    assignments.iter().map(|row| row.effective_from).min()
}

/// Why a lookup has no assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    /// `as_of` predates the accumulation start (§5.4).
    BeforeCoverage,
    /// The code is absent from the snapshot covering `as_of`; no interpolation.
    MissingInSnapshot,
}

/// Internal read result for `(entity, as_of)` — the shape PR-7's read model
/// will project and the tests assert on (§5.5 `status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum Lookup {
    Assigned {
        level: i64,
        industry_code: String,
        name: Option<String>,
        name_status: String,
        effective_from: i64,
        effective_to: Option<i64>,
        evidence_grade: String,
        snapshot_member_sha256: String,
    },
    Unknown {
        reason: UnknownReason,
    },
}

/// `(entity, as_of)` lookup over interval rows: `effective_from <= as_of <
/// effective_to` (the end is exclusive, §5.1).
///
/// Before coverage the answer is `unknown{before_coverage}` — never the current
/// catalog industry and never an empty row a caller could read as "no
/// industry" (§5.4). Inside coverage a missing code is
/// `unknown{missing_in_snapshot}`; nothing is interpolated.
pub fn lookup(assignments: &[AssignmentRow], exchange: &str, code: &str, as_of: i64) -> Lookup {
    match coverage_start(assignments) {
        Some(start) if as_of >= start => {}
        _ => {
            return Lookup::Unknown {
                reason: UnknownReason::BeforeCoverage,
            };
        }
    }
    assignments
        .iter()
        .find(|row| {
            row.exchange == exchange
                && row.code == code
                && row.effective_from <= as_of
                && row.effective_to.is_none_or(|end| as_of < end)
        })
        .map_or(
            Lookup::Unknown {
                reason: UnknownReason::MissingInSnapshot,
            },
            |row| Lookup::Assigned {
                level: row.level,
                industry_code: row.industry_code.clone(),
                name: row.name.clone(),
                name_status: row.name_status.clone(),
                effective_from: row.effective_from,
                effective_to: row.effective_to,
                evidence_grade: row.evidence_grade.clone(),
                snapshot_member_sha256: row.snapshot_member_sha256.clone(),
            },
        )
}

/// Canonical summary of one rebuilt table set: fixed fields per row, sorted by
/// primary key, sha256 over the joined lines (§9.4.2 "摘要对账"). Two runs over
/// the same store and parser version agree bit for bit.
pub fn summary(snapshots: &[SnapshotRecord], assignments: &[AssignmentRow]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(snapshots.len() + assignments.len());
    lines.extend(snapshots.iter().map(snapshot_line));
    lines.extend(assignments.iter().map(assignment_line));
    lines.sort();
    crate::hash::sha256(lines.join("\n").as_bytes())
}

/// One snapshot's comparable line. `imported_at` is deliberately absent: it is
/// a source-store fact that the snapshot table does not store, so including it
/// would make a stored row differ from the row it was written from.
fn snapshot_line(row: &SnapshotRecord) -> String {
    format!(
        "s|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        row.member_sha256,
        row.store_sha256,
        row.base_zip_sha256.as_deref().unwrap_or(""),
        row.generation_manifest_sha256.as_deref().unwrap_or(""),
        row.source_kind.as_str(),
        row.publisher_last_modified.as_deref().unwrap_or(""),
        row.observed_at.as_deref().unwrap_or(""),
        row.ingested_at,
        row.evidence_grade,
        row.parser_version,
        row.row_count,
        row.scope_member_count,
        row.dictionary_sha256.as_deref().unwrap_or(""),
        row.notes.as_deref().unwrap_or(""),
    )
}

/// One interval's comparable line, keyed under [`AssignmentRow::key`].
fn assignment_line(row: &AssignmentRow) -> String {
    format!(
        "a|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        row.exchange,
        row.code,
        row.level,
        row.industry_code,
        row.t_code.as_deref().unwrap_or(""),
        row.name.as_deref().unwrap_or(""),
        row.name_status,
        row.effective_from,
        row.effective_to
            .map(|day| day.to_string())
            .unwrap_or_default(),
        row.known_at,
        row.basis,
        row.evidence_grade,
        row.snapshot_member_sha256,
    )
}

/// Primary keys that differ between two table sets, capped at `limit`. Used by
/// `--verify-only` to name the drift instead of only reporting it.
pub fn drift(
    expected_snapshots: &[SnapshotRecord],
    expected_assignments: &[AssignmentRow],
    actual_snapshots: &[SnapshotRecord],
    actual_assignments: &[AssignmentRow],
    limit: usize,
) -> Vec<String> {
    let mut drift = Vec::new();
    let expected_snapshot_lines = snapshot_lines(expected_snapshots);
    let actual_snapshot_lines = snapshot_lines(actual_snapshots);
    for key in expected_snapshot_lines.keys() {
        match actual_snapshot_lines.get(key) {
            Some(actual) if actual == expected_snapshot_lines.get(key).unwrap() => {}
            _ => drift.push(format!("snapshot:{key}")),
        }
    }
    for key in actual_snapshot_lines.keys() {
        if !expected_snapshot_lines.contains_key(key) {
            drift.push(format!("snapshot:{key}"));
        }
    }
    let expected_assignment_lines = assignment_lines(expected_assignments);
    let actual_assignment_lines = assignment_lines(actual_assignments);
    for key in expected_assignment_lines.keys() {
        match actual_assignment_lines.get(key) {
            Some(actual) if actual == expected_assignment_lines.get(key).unwrap() => {}
            _ => drift.push(format!("assignment:{key}")),
        }
    }
    for key in actual_assignment_lines.keys() {
        if !expected_assignment_lines.contains_key(key) {
            drift.push(format!("assignment:{key}"));
        }
    }
    drift.sort();
    drift.dedup();
    drift.truncate(limit);
    drift
}

fn snapshot_lines(rows: &[SnapshotRecord]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for row in rows {
        map.insert(row.member_sha256.clone(), snapshot_line(row));
    }
    map
}

fn assignment_lines(rows: &[AssignmentRow]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for row in rows {
        map.insert(row.key(), assignment_line(row));
    }
    map
}

/// The source store a database's deployment owns. The default is the layout
/// `scripts/deploy_phase_g.sh` installs (`<state>/source-store`, sibling of the
/// database); `X_DATA_MCP_SOURCE_STORE` overrides it for a relocated store.
pub fn source_store_for_database(database: &Path) -> SourceStore {
    if let Some(root) = std::env::var_os(SOURCE_STORE_ENV) {
        return SourceStore::new(PathBuf::from(root));
    }
    let state = database.parent().unwrap_or_else(|| Path::new("."));
    SourceStore::new(state.join("source-store"))
}

/// Epoch day of a calendar date — the unit `series_bars.trade_day` uses.
pub fn epoch_day(date: NaiveDate) -> i64 {
    date.signed_duration_since(NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch"))
        .num_days()
}

/// RFC 3339 seconds in UTC, the one timestamp shape the manifest and the
/// interval rows store.
pub fn timestamp(moment: chrono::DateTime<Utc>) -> String {
    moment.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A publisher `last_modified`: the raw HTTP-date the acquire step recorded §5.2.
pub fn parse_publisher_day(value: &str) -> Result<NaiveDate, Error> {
    let trimmed = value.trim();
    if let Ok(moment) = chrono::DateTime::parse_from_rfc2822(trimmed) {
        return Ok(moment.with_timezone(&Utc).date_naive());
    }
    if let Ok(moment) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Ok(moment.with_timezone(&Utc).date_naive());
    }
    for format in ["%Y-%m-%dT%H:%M:%SZ", "%Y-%m-%d"] {
        if let Ok(date) = NaiveDate::parse_from_str(trimmed, format) {
            return Ok(date);
        }
    }
    Err(Error::Invalid(format!(
        "publisher_last_modified {value:?} is not an HTTP-date or RFC 3339 timestamp"
    )))
}

/// The UTC date part of a stored timestamp (`2026-10-24T02:00:00Z` → its day).
pub fn parse_day_prefix(value: &str) -> Result<NaiveDate, Error> {
    let prefix = value
        .get(..10)
        .ok_or_else(|| Error::Invalid(format!("timestamp {value:?} is shorter than a date")))?;
    NaiveDate::parse_from_str(prefix, "%Y-%m-%d")
        .map_err(|_| Error::Invalid(format!("timestamp {value:?} has no leading date")))
}

/// UTC now, seconds precision — the `imported_at` of a backfill.
pub fn now() -> String {
    timestamp(Utc::now())
}

/// The industry members one generation carries, in the adapter's neutral form
/// (§10.1 `base.zip!tdxhy.cfg` + `gbbq.zip!zhb.zip!incon.dat`). The domain
/// never names an archive: the caller ([`crate::official_market`]) extracted the
/// bytes, this module decides what they mean.
pub struct GenerationMembers<'a> {
    pub manifest_sha256: &'a str,
    pub tdxhy_member: &'a str,
    pub tdxhy_bytes: &'a [u8],
    pub tdxhy_publisher_last_modified: Option<&'a str>,
    /// Acquisition-complete time of the archive the member came from.
    pub tdxhy_observed_at: &'a str,
    pub incon_member: &'a str,
    pub incon_bytes: &'a [u8],
    pub incon_publisher_last_modified: Option<&'a str>,
    pub incon_observed_at: &'a str,
    /// sha256 of the `base.zip` container the member was extracted from.
    pub base_zip_sha256: &'a str,
}

/// What registering a generation's members produces: the observation itself and
/// the observation it follows.
pub struct GenerationSnapshot {
    /// The observation this generation made.
    pub facts: SnapshotFacts,
    /// The `tdxhy.cfg` registration right before this one in the source store,
    /// when there is one. The writer needs it to tell "the same content, seen
    /// again" (one run) from "an older content, seen again" (a new interval,
    /// A → B → A): the table cannot answer that, the observation log can.
    pub previous_member_sha256: Option<String>,
}

/// Register a generation's industry members in the source store and build the
/// snapshot facts the DB writer consumes (§9.4.1 write order: blob + manifest
/// first, index rows afterwards).
///
/// The bytes are parsed *before* registration, exactly like the archive
/// importer: a member no registered parser can read must not enter the store,
/// because a later replay would then fail the whole scope instead of a single
/// run.
///
/// Evidence grade is `A` when the publisher exposed a `last_modified` (the
/// generation came through an inspected manifest, so `manifest_match` and the
/// acquisition time are true by construction), `B` otherwise (§5.3).
pub fn register_generation_members(
    store: &SourceStore,
    members: &GenerationMembers<'_>,
) -> Result<GenerationSnapshot, Error> {
    // The replay pairs a snapshot with its dictionary by `origin`, so the
    // member names must be the canonical ones the pairing rule knows; a
    // differently named member is refused instead of silently stored as
    // industry data.
    if !members.tdxhy_member.ends_with("tdxhy.cfg") || !members.incon_member.ends_with("incon.dat")
    {
        return Err(Error::Invalid(format!(
            "industry members {} + {} are not tdxhy.cfg + incon.dat",
            members.tdxhy_member, members.incon_member
        )));
    }
    let (row_count, parsed_members) = parse_members(members.tdxhy_bytes)?;
    let dictionary = parse_dictionary(members.incon_bytes)?;
    // `generation:<manifest sha>` for both members is what pairs the snapshot
    // with its dictionary on replay and recovers the generation sha.
    let origin = format!("generation:{}", members.manifest_sha256);
    let grade = if members
        .tdxhy_publisher_last_modified
        .is_some_and(|value| !value.trim().is_empty())
    {
        "A"
    } else {
        "B"
    };
    let mut tdxhy = crate::source_store::Registration::new(
        crate::source_store::TDXHY_SOURCE_ID,
        crate::source_store::StoreKind::GenerationMember,
        origin.clone(),
        members.tdxhy_observed_at,
    );
    tdxhy.parser_version = Some(crate::source_store::TDXHY_PARSER_VERSION.to_string());
    tdxhy.evidence_grade = Some(grade.to_string());
    tdxhy.publisher_last_modified = members.tdxhy_publisher_last_modified.map(str::to_string);
    tdxhy.container_sha256 = Some(members.base_zip_sha256.to_string());
    let registered = store.register(members.tdxhy_bytes, &tdxhy)?;
    let member_sha256 = crate::hash::sha256(members.tdxhy_bytes);
    if registered.sha256 != member_sha256 {
        return Err(Error::Invalid(format!(
            "{member_sha256} read back as {} from the source store",
            registered.sha256
        )));
    }

    let mut incon = crate::source_store::Registration::new(
        crate::source_store::INCON_SOURCE_ID,
        crate::source_store::StoreKind::GenerationMember,
        origin.clone(),
        members.incon_observed_at,
    );
    incon.parser_version = Some(crate::source_store::INCON_PARSER_VERSION.to_string());
    incon.evidence_grade = Some(grade.to_string());
    incon.publisher_last_modified = members.incon_publisher_last_modified.map(str::to_string);
    incon.container_sha256 = Some(members.base_zip_sha256.to_string());
    let dictionary_sha256 = store.register(members.incon_bytes, &incon)?.sha256;
    // The observation log, not the table, says what came before (§5.3): the
    // registration above is the last line of the manifest, so the previous
    // `tdxhy.cfg` observation is the last one that is not ours.
    let previous_member_sha256 = store
        .records()?
        .iter()
        .filter(|record| record.source_id == crate::source_store::TDXHY_SOURCE_ID)
        .filter(|record| !(record.sha256 == member_sha256 && record.origin == origin))
        .next_back()
        .map(|record| record.sha256.clone());

    let record = SnapshotRecord {
        member_sha256,
        store_sha256: registered.sha256,
        base_zip_sha256: Some(members.base_zip_sha256.to_string()),
        generation_manifest_sha256: Some(members.manifest_sha256.to_string()),
        source_kind: SourceKind::Generation,
        publisher_last_modified: members.tdxhy_publisher_last_modified.map(str::to_string),
        observed_at: Some(members.tdxhy_observed_at.to_string()),
        ingested_at: members.tdxhy_observed_at.to_string(),
        imported_at: None,
        evidence_grade: grade.to_string(),
        parser_version: crate::source_store::TDXHY_PARSER_VERSION.to_string(),
        row_count,
        scope_member_count: i64::try_from(parsed_members.len())
            .map_err(|_| Error::Invalid("scope member count overflow".into()))?,
        dictionary_sha256: Some(dictionary_sha256),
        // The member names are not a snapshot fact: they are visible in the
        // source-store registration, and `notes` is part of the replay summary
        // (a value only the daily path could set would drift on rebuild).
        notes: None,
    };
    Ok(GenerationSnapshot {
        facts: SnapshotFacts {
            record,
            members: parsed_members,
            dictionary_present: dictionary.is_some(),
            dictionary: dictionary.unwrap_or_default(),
        },
        previous_member_sha256,
    })
}

#[cfg(test)]
mod tests;
