//! `universe(as_of)` — the T-day member set (issue-391 §11, AC-M2-4).
//!
//! Layer position (separation point 4): this is the research read model. It
//! reads the parsed-fact tables through the storage adapter
//! (`sqlite_market::security`/`series`) and answers with members, never with
//! table rows. MCP (v8), the export bin and the ops deep check are adapters on
//! top of it; none of them builds SQL.
//!
//! The definition is §11.1, in this order:
//!
//! ```text
//! universe(as_of = T) = { row ∈ security_master
//!   : instrument_class = 0
//!   ∧ effective_from ≤ T ∧ (effective_to IS NULL ∨ T < effective_to)
//!   ∧ (listing_date IS NULL ∨ listing_date ≤ T)
//!   ∧ (delisting_date IS NULL ∨ T < delisting_date)
//!   ∧ 未被 code_migrations 在 ≤ T 的时点取代 }
//! ```
//!
//! Deliberate non-rules:
//!
//! - A member without a stored series stays in the set (`has_series=false`,
//!   §11.2). Dropping it would confuse "in the universe" with "we have data".
//! - A suspended member stays in the set (§11.1). Tradability is a different
//!   question with a different answer type ([`super::tradability`]).
//! - The calendar is not consulted. A non-trading `as_of` yields the
//!   interval-based set; the caller picks the last trading day (§11.1 leaves
//!   that choice to the caller, and the read model does not guess).
//!
//! `strict` is the only switch that removes otherwise-included members, and it
//! removes exactly the two ambiguity cases the design names: an interval whose
//! issuer is ambiguous (code reuse, Q3) and the duplicated copy of a
//! *candidate* migration. A confirmed migration is already applied by the
//! writer (the old code's interval ends on the migration day); the read model
//! re-checks it so a hand-written or stale row cannot be counted twice.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use rusqlite::Connection;

use super::Error;
use super::member_state::{MemberFlag, MemberState, Membership, MembershipBasis, NotMemberReason};
use super::temporal::{Temporal, TemporalReport};
use crate::security_master::assembly::{
    self as master_assembly, CodeMigrationFact, MigrationCertainty,
};
use crate::security_master::{
    Exchange, IssuerResolution, RiskStatus, RiskWarningInterval, SecurityLifecycleFact,
};
use crate::sqlite_market::{derived, security, series};

/// `derived_dataset_versions.dataset_kind` names this read model reports in its
/// `temporal` block. They are the design's dataset names (§3.2), not table
/// names, so the answer does not change when a table is renamed.
pub const DATASET_SECURITY_MASTER: &str = "security_master";
pub const DATASET_CODE_MIGRATIONS: &str = "code_migrations";
pub const DATASET_RISK_WARNINGS: &str = "risk_warnings";

/// The universe question: a point in time plus the strictness switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniverseRequest {
    pub temporal: Temporal,
    pub strict: bool,
}

impl UniverseRequest {
    /// `as_of` only, `strict=false` (the default of §11.3).
    pub fn new(as_of: NaiveDate) -> Self {
        Self {
            temporal: Temporal::effective_only(as_of),
            strict: false,
        }
    }

    pub fn with_known_at(mut self, known_at: NaiveDate) -> Self {
        self.temporal.known_at = Some(known_at);
        self
    }

    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }
}

/// One universe member (§11.3 `Member`). `status` is the risk-warning state,
/// `unknown` unless an admitted interval covers the day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub entity: String,
    /// The code that identifies this issuer across a confirmed code migration
    /// (§4.3): the newest code in the confirmed chain, or `entity` when the
    /// chain has none. Stable before and after the migration, so a consumer
    /// can group member intervals without counting one issuer twice.
    pub canonical_entity: String,
    /// The other codes in the confirmed chain, sorted. Candidate migrations
    /// never add an alias: they are not proven (§4.3).
    pub aliases: Vec<String>,
    pub exchange: Exchange,
    pub code: String,
    pub listing_date: Option<NaiveDate>,
    pub delisting_date: Option<NaiveDate>,
    pub status: RiskStatus,
    pub issuer_resolution: IssuerResolution,
    pub membership_basis: MembershipBasis,
    pub known_at: Option<NaiveDate>,
    pub has_series: bool,
    pub flags: Vec<MemberFlag>,
}

impl From<MemberState> for Member {
    fn from(state: MemberState) -> Self {
        let basis = state
            .membership
            .basis()
            .expect("only included states become members");
        Self {
            canonical_entity: state.entity.clone(),
            aliases: Vec::new(),
            entity: state.entity,
            exchange: state.exchange,
            code: state.code,
            listing_date: state.listing_date,
            delisting_date: state.delisting_date,
            status: state.status,
            issuer_resolution: state.issuer_resolution,
            membership_basis: basis,
            known_at: state.known_at,
            has_series: state.has_series,
            flags: state.flags,
        }
    }
}

/// How many intervals each rule excluded. A count of zero is reported too: the
/// difference list of AC-M2-4 is only complete when every reason is named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UniverseExclusions {
    pub before_interval: usize,
    pub after_interval: usize,
    pub before_listing: usize,
    pub after_delisting: usize,
    pub before_first_bar: usize,
    pub excluded_by_known_at: usize,
    pub not_a_share: usize,
    pub migrated: usize,
    pub strict_ambiguous_issuer: usize,
    pub strict_candidate_overlap: usize,
}

impl UniverseExclusions {
    fn record(&mut self, reason: NotMemberReason) {
        match reason {
            NotMemberReason::BeforeInterval => self.before_interval += 1,
            NotMemberReason::AfterInterval => self.after_interval += 1,
            NotMemberReason::BeforeListing => self.before_listing += 1,
            NotMemberReason::AfterDelisting => self.after_delisting += 1,
            NotMemberReason::BeforeFirstBar => self.before_first_bar += 1,
            NotMemberReason::ExcludedByKnownAt => self.excluded_by_known_at += 1,
            NotMemberReason::NotAShare => self.not_a_share += 1,
            NotMemberReason::Migrated => self.migrated += 1,
            NotMemberReason::StrictAmbiguousIssuer => self.strict_ambiguous_issuer += 1,
            NotMemberReason::StrictCandidateOverlap => self.strict_candidate_overlap += 1,
        }
    }

    pub fn total(&self) -> usize {
        self.before_interval
            + self.after_interval
            + self.before_listing
            + self.after_delisting
            + self.before_first_bar
            + self.excluded_by_known_at
            + self.not_a_share
            + self.migrated
            + self.strict_ambiguous_issuer
            + self.strict_candidate_overlap
    }
}

/// The answer: the members plus what was excluded and why, and the
/// `temporal` block (§12.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Universe {
    pub as_of: NaiveDate,
    pub known_at: Option<NaiveDate>,
    pub strict: bool,
    pub members: Vec<Member>,
    pub exclusions: UniverseExclusions,
    pub temporal: TemporalReport,
    /// `security_master`'s registered version id — what makes
    /// `universe(as_of=T)` reproducible without a catalog version (§11.2).
    /// `None` when the dataset was never written.
    pub master_version: Option<String>,
}

/// Every interval the query considered, with its verdict. `universe` keeps the
/// included ones; `member_state` picks one for a single code.
pub(crate) struct Selection {
    pub states: Vec<MemberState>,
    pub temporal: TemporalReport,
    /// The migrations admitted by the information-set filter. `universe` uses
    /// them to attach the canonical identity; `member_state` does not need it.
    pub migrations: Vec<CodeMigrationFact>,
}

/// `universe(as_of = T)` (§11.1, AC-M2-4).
pub fn universe(c: &Connection, request: &UniverseRequest) -> Result<Universe, Error> {
    let Selection {
        states,
        temporal,
        migrations,
    } = select(c, request, None)?;
    let mut exclusions = UniverseExclusions::default();
    let mut members = Vec::new();
    for state in states {
        match state.membership {
            Membership::In(_) => {
                let mut member = Member::from(state);
                (member.canonical_entity, member.aliases) =
                    canonical_identity(&member.entity, &migrations);
                members.push(member);
            }
            Membership::Out(reason) => exclusions.record(reason),
        }
    }
    members.sort_by(|left, right| left.entity.cmp(&right.entity));
    let master_version = derived::read_version(
        c,
        master_assembly::DATASET_KIND,
        master_assembly::DATASET_KEY,
    )
    .map_err(Error::from)?
    .map(|row| row.version_id);
    Ok(Universe {
        as_of: request.temporal.as_of,
        known_at: request.temporal.known_at,
        strict: request.strict,
        members,
        exclusions,
        temporal,
        master_version,
    })
}

/// One member interval as a whole, not a per-day verdict: what
/// `universe_intervals.parquet` carries (§12.4).
///
/// The span is the part of a `security_master` interval where the member rule
/// can hold at all — bounded by the listing and delisting dates, in the same
/// order §11.1 applies them. Day-level exclusions are deliberately **not**
/// materialised here: `known_at`, `strict` and migrations are answered per
/// day by [`universe`]/[`member_state`], and folding them into an interval
/// would claim a precision the data does not have (a `known_at` filter can
/// remove a day in the middle of a span, and a candidate migration creates an
/// overlapping copy rather than a hole).
///
/// `basis` says what proved the start: a listing date, or the first stored bar
/// when the listing date is unknown (§11.1's fallback). An interval with
/// neither cannot be a member at any day and is omitted, not exported as a
/// zero-length span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberInterval {
    pub entity: String,
    /// See [`Member::canonical_entity`].
    pub canonical_entity: String,
    /// See [`Member::aliases`].
    pub aliases: Vec<String>,
    pub member_from: NaiveDate,
    pub member_to: Option<NaiveDate>,
    pub basis: MembershipBasis,
    pub flags: Vec<MemberFlag>,
}

/// Every member interval this database defines (§12.4 `universe_intervals`).
///
/// Ordered by entity then `member_from`, so the export is deterministic. The
/// rows are A-share intervals only: a reserved `instrument_class` is not in the
/// universe today, and this file does not pretend otherwise.
pub fn member_intervals(c: &Connection) -> Result<Vec<MemberInterval>, Error> {
    let rows = security::read_security_master(c).map_err(Error::from)?;
    let migrations = security::read_code_migrations(c).map_err(Error::from)?;
    let spans = series::raw_daily_spans(c).map_err(Error::from)?;
    let mut intervals = Vec::new();
    for row in &rows {
        if row.instrument_class != 0 {
            continue;
        }
        let entity = row.entity();
        let (start, basis) = match row.listing_date {
            Some(listing) if listing > row.effective_from => {
                (listing, MembershipBasis::ListingDate)
            }
            Some(_) => (row.effective_from, MembershipBasis::ListingDate),
            None => match spans.get(&entity) {
                Some(span) if span.first_day > row.effective_from => {
                    (span.first_day, MembershipBasis::BarEvidence)
                }
                Some(_) => (row.effective_from, MembershipBasis::BarEvidence),
                None => continue,
            },
        };
        let end = match (row.effective_to, row.delisting_date) {
            (Some(to), Some(delisting)) => Some(to.min(delisting)),
            (Some(to), None) => Some(to),
            (None, Some(delisting)) => Some(delisting),
            (None, None) => None,
        };
        if end.is_some_and(|end| end <= start) {
            continue;
        }
        let mut flags = Vec::new();
        if row.issuer_resolution == IssuerResolution::Ambiguous {
            flags.push(MemberFlag::AmbiguousIssuer);
        }
        let (canonical_entity, aliases) = canonical_identity(&entity, &migrations);
        intervals.push(MemberInterval {
            entity,
            canonical_entity,
            aliases,
            member_from: start,
            member_to: end,
            basis,
            flags,
        });
    }
    intervals.sort_by(|left, right| {
        left.entity
            .cmp(&right.entity)
            .then(left.member_from.cmp(&right.member_from))
    });
    Ok(intervals)
}

/// One code's state at `as_of` (separation point 7's atomic capability).
///
/// When a code has more than one interval (code reuse), the answer is the
/// interval that covers the day; if none covers it, the latest interval that
/// started on or before the day; otherwise the earliest one. That keeps
/// "what was this code on T" answerable without merging two subjects.
pub fn member_state(
    c: &Connection,
    entity: &str,
    request: &UniverseRequest,
) -> Result<Option<MemberState>, Error> {
    Ok(member_state_report(c, entity, request)?.0)
}

/// [`member_state`] plus the `temporal` block, for the read models that build
/// on it ([`super::tradability`]) and must report the same counts.
pub(crate) fn member_state_report(
    c: &Connection,
    entity: &str,
    request: &UniverseRequest,
) -> Result<(Option<MemberState>, TemporalReport), Error> {
    crate::compact_market::canonical_entity(entity)
        .map_err(|_| Error::Invalid(format!("non-canonical entity {entity}")))?;
    let selection = select(c, request, Some(entity))?;
    let mut states = selection.states;
    let report = selection.temporal;
    if states.is_empty() {
        return Ok((None, report));
    }
    states.sort_by_key(|state| state.effective_from);
    if let Some(index) = states.iter().position(MemberState::is_member) {
        return Ok((Some(states.swap_remove(index)), report));
    }
    let as_of = request.temporal.as_of;
    let chosen = states
        .iter()
        .rposition(|state| state.effective_from <= as_of)
        .unwrap_or(0);
    Ok((Some(states.swap_remove(chosen)), report))
}

/// The shared selection used by [`universe`] and [`member_state`].
pub(crate) fn select(
    c: &Connection,
    request: &UniverseRequest,
    only: Option<&str>,
) -> Result<Selection, Error> {
    let rows = security::read_security_master(c).map_err(Error::from)?;
    let migrations = security::read_code_migrations(c).map_err(Error::from)?;
    let spans = series::raw_daily_spans(c).map_err(Error::from)?;
    let warnings = security::read_risk_warning_intervals(c).map_err(Error::from)?;

    let mut report = TemporalReport::new(request.temporal);
    let admitted = admitted_migrations(&mut report, &migrations, only);
    let mut status_cache = StatusCache::new();
    let mut states = Vec::new();

    for row in &rows {
        let entity = row.entity();
        if only.is_some_and(|only| only != entity) {
            continue;
        }
        let has_series = spans.contains_key(&entity);
        if row.instrument_class != 0 {
            states.push(build_state(
                row,
                &entity,
                Membership::Out(NotMemberReason::NotAShare),
                RiskStatus::Unknown,
                has_series,
            ));
            continue;
        }
        if !report.observe(DATASET_SECURITY_MASTER, row.known_at) {
            states.push(build_state(
                row,
                &entity,
                Membership::Out(NotMemberReason::ExcludedByKnownAt),
                RiskStatus::Unknown,
                has_series,
            ));
            continue;
        }
        let membership = membership_of(row, request, &spans, &admitted);
        let status = if membership.is_member() {
            status_of(
                &mut report,
                &mut status_cache,
                &warnings,
                &entity,
                request.temporal,
            )?
        } else {
            RiskStatus::Unknown
        };
        let mut state = build_state(row, &entity, membership, status, has_series);
        if membership.is_member() {
            state.flags = flags_for(row, &entity, request, &admitted);
        }
        states.push(state);
    }

    Ok(Selection {
        states,
        temporal: report,
        migrations: admitted,
    })
}

/// The canonical code and aliases of `entity` from confirmed migrations only
/// (§4.3). The canonical code is the end of the outgoing chain; the alias set
/// is the whole confirmed connected component, so both sides of a migration
/// answer with the same identity. Candidate migrations are deliberately
/// ignored: they change no membership and prove no rename.
fn canonical_identity(entity: &str, migrations: &[CodeMigrationFact]) -> (String, Vec<String>) {
    let mut confirmed: Vec<&CodeMigrationFact> = migrations
        .iter()
        .filter(|migration| migration.certainty == MigrationCertainty::Confirmed)
        .collect();
    confirmed.sort_by(|left, right| {
        left.from_entity()
            .cmp(&right.from_entity())
            .then(left.effective_from.cmp(&right.effective_from))
            .then(left.to_entity().cmp(&right.to_entity()))
    });
    let mut canonical = entity.to_owned();
    let mut seen = BTreeSet::new();
    while seen.insert(canonical.clone()) {
        let Some(next) = confirmed
            .iter()
            .filter(|migration| migration.from_entity() == canonical)
            .max_by_key(|migration| (migration.effective_from, migration.to_entity()))
        else {
            break;
        };
        canonical = next.to_entity();
    }
    let mut codes = BTreeSet::new();
    codes.insert(entity.to_owned());
    loop {
        let mut grew = false;
        for migration in &confirmed {
            let from = migration.from_entity();
            let to = migration.to_entity();
            if codes.contains(&from) || codes.contains(&to) {
                grew |= codes.insert(from);
                grew |= codes.insert(to);
            }
        }
        if !grew {
            break;
        }
    }
    let aliases = codes
        .into_iter()
        .filter(|code| code != &canonical)
        .collect();
    (canonical, aliases)
}

/// Per-entity 不复权 span index. The alias exists so the pre-push arity
/// heuristic (which splits a parameter list on every comma) does not read the
/// generic's comma as a second parameter — the same reason
/// `security_master::ExcludedCategories` exists.
type SpanIndex = BTreeMap<String, series::RawDailySpan>;

/// One member's cached risk status, keyed by entity.
type StatusCache = BTreeMap<String, RiskStatus>;

/// The interval/listing/delisting/migration verdict (§11.1).
fn membership_of(
    row: &SecurityLifecycleFact,
    request: &UniverseRequest,
    spans: &SpanIndex,
    migrations: &[CodeMigrationFact],
) -> Membership {
    let as_of = request.temporal.as_of;
    let entity = row.entity();
    if as_of < row.effective_from {
        return Membership::Out(NotMemberReason::BeforeInterval);
    }
    if row.effective_to.is_some_and(|to| as_of >= to) {
        return Membership::Out(NotMemberReason::AfterInterval);
    }
    // The new code of a confirmed migration does not exist as a separate
    // member before the migration day. The writer normally moves its interval
    // start there; re-check the verdict so a stale or hand-written row cannot
    // show both codes at once (§4.3).
    if migrations.iter().any(|migration| {
        migration.certainty == MigrationCertainty::Confirmed
            && migration.to_entity() == entity
            && as_of < migration.effective_from
    }) {
        return Membership::Out(NotMemberReason::Migrated);
    }
    let basis = match row.listing_date {
        Some(listing) if as_of < listing => {
            return Membership::Out(NotMemberReason::BeforeListing);
        }
        Some(_) => MembershipBasis::ListingDate,
        None => match spans.get(&entity) {
            Some(span) if as_of >= span.first_day => MembershipBasis::BarEvidence,
            _ => return Membership::Out(NotMemberReason::BeforeFirstBar),
        },
    };
    if let Some(delisting) = row.delisting_date
        && as_of >= delisting
    {
        return Membership::Out(NotMemberReason::AfterDelisting);
    }
    // A confirmed migration is already applied by the writer; re-check it so a
    // stale row cannot be counted twice (§11.1).
    if migrations.iter().any(|migration| {
        migration.certainty == MigrationCertainty::Confirmed
            && migration.from_entity() == entity
            && as_of >= migration.effective_from
    }) {
        return Membership::Out(NotMemberReason::Migrated);
    }
    if request.strict {
        if row.issuer_resolution == IssuerResolution::Ambiguous {
            return Membership::Out(NotMemberReason::StrictAmbiguousIssuer);
        }
        if overlaps_candidate_migration(row, &entity, as_of, migrations) {
            return Membership::Out(NotMemberReason::StrictCandidateOverlap);
        }
    }
    Membership::In(basis)
}

/// Whether this row is the duplicated side of a candidate migration on `as_of`.
///
/// The candidate's `effective_from` is the old code's last bar + 1 (§4.1), so
/// the new code's copied history overlaps `[new interval start, migration
/// day)`. Non-strict keeps both (the design's default); strict drops the copy
/// so one issuer is not counted twice. Dropping the *old* code instead would
/// erase the listing that actually existed, so the new side is the one that
/// goes.
fn overlaps_candidate_migration(
    row: &SecurityLifecycleFact,
    entity: &str,
    as_of: NaiveDate,
    migrations: &[CodeMigrationFact],
) -> bool {
    migrations.iter().any(|migration| {
        migration.certainty == MigrationCertainty::Candidate
            && migration.to_entity() == entity
            && as_of >= row.effective_from
            && as_of < migration.effective_from
    })
}

/// The flags an included member carries. `strict` already removed the two
/// ambiguity cases, so a strict answer has neither flag.
fn flags_for(
    row: &SecurityLifecycleFact,
    entity: &str,
    request: &UniverseRequest,
    migrations: &[CodeMigrationFact],
) -> Vec<MemberFlag> {
    let mut flags = Vec::new();
    if row.listing_date.is_none() {
        flags.push(MemberFlag::ListingUnknown);
    }
    if row.delisting_date.is_none() {
        flags.push(MemberFlag::DelistingUnknown);
    }
    if row.issuer_resolution == IssuerResolution::Ambiguous {
        flags.push(MemberFlag::AmbiguousIssuer);
    }
    if overlaps_candidate_migration(row, entity, request.temporal.as_of, migrations) {
        flags.push(MemberFlag::CandidateMigrationOverlap);
    }
    flags
}

fn build_state(
    row: &SecurityLifecycleFact,
    entity: &str,
    membership: Membership,
    status: RiskStatus,
    has_series: bool,
) -> MemberState {
    MemberState {
        entity: entity.into(),
        exchange: row.exchange,
        code: row.code.clone(),
        effective_from: row.effective_from,
        effective_to: row.effective_to,
        membership,
        listing_date: row.listing_date,
        delisting_date: row.delisting_date,
        issuer_resolution: row.issuer_resolution,
        status,
        flags: Vec::new(),
        has_series,
        known_at: row.known_at,
    }
}

/// Migrations the information-set filter admits. A confirmed migration with a
/// NULL `known_at` is excluded under a `known_at` filter, so it stops
/// deduplicating — the honest answer when the announcement day is unknown.
///
/// A single-code query keeps only the migrations that touch that code, so the
/// `temporal` counts describe the answer rather than the table.
fn admitted_migrations(
    report: &mut TemporalReport,
    migrations: &[CodeMigrationFact],
    only: Option<&str>,
) -> Vec<CodeMigrationFact> {
    migrations
        .iter()
        .filter(|migration| {
            only.is_none_or(|only| migration.from_entity() == only || migration.to_entity() == only)
        })
        .filter(|migration| report.observe(DATASET_CODE_MIGRATIONS, migration.known_at))
        .cloned()
        .collect()
}

/// The risk-warning status on `as_of` (§6.3). No covering interval is
/// `unknown`, never `normal`; two covering intervals are an integrity error,
/// because the interval chain must be non-overlapping.
fn status_of(
    report: &mut TemporalReport,
    cache: &mut StatusCache,
    warnings: &[RiskWarningInterval],
    entity: &str,
    temporal: Temporal,
) -> Result<RiskStatus, Error> {
    if let Some(status) = cache.get(entity) {
        return Ok(*status);
    }
    let mut hit: Option<RiskStatus> = None;
    for interval in warnings.iter().filter(|row| row.entity() == entity) {
        if !report.observe(DATASET_RISK_WARNINGS, interval.known_at) {
            continue;
        }
        let covers = interval.effective_from <= temporal.as_of
            && interval.effective_to.is_none_or(|to| temporal.as_of < to);
        if !covers {
            continue;
        }
        if hit.is_some() {
            return Err(Error::Integrity(format!(
                "risk_warning_intervals overlap for {entity} on {}",
                temporal.as_of
            )));
        }
        hit = Some(interval.status);
    }
    let status = hit.unwrap_or(RiskStatus::Unknown);
    cache.insert(entity.to_string(), status);
    Ok(status)
}
