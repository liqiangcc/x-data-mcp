//! Replay skeleton over the source store (design §9.4.2).
//!
//! PR-1a registers the three scopes and their byte-verification path; schema
//! 25 (PR-1) creates the empty index tables these scopes fill. Each scope
//! reports what it *selected*, a deterministic digest of the manifest, and —
//! when a database is supplied — the summary of the index rows it rebuilt.
//!
//! - `exchange` (M3 PR-2): every frozen `http_fetch` response is re-parsed with
//!   the envelope parser version the manifest recorded, and
//!   `exchange_source_snapshots` is replaced from the store without any HTTP
//!   request. `--verify-only` recomputes the same summary and compares it with
//!   the stored rows (`match` / `drift` + the first 20 differing snapshot ids).
//! - `industry` (M3 PR-6): `tdxhy.cfg` snapshots and their `incon.dat`
//!   dictionary are re-parsed with the parser version the manifest recorded,
//!   and the intervals are recomputed (§5.1/§5.2, deterministic); the table set
//!   is replaced in one transaction and `--verify-only` compares the recomputed
//!   summary with the stored rows (`match` / `drift`).
//! - `generation_facts` (M3 PR-8): the industry rows are rebuilt from the
//!   source-store manifest, the market-derived rows (capital/adjustment
//!   events, daily series) from the derived tables' generation bindings; the
//!   table is replaced in one transaction and `--verify-only` compares the
//!   recomputed summary with the stored rows (`match` / `drift`).
//! - `risk_warnings` (M3 PR-10): the frozen SZSE short-name table and SSE
//!   bulletin pages are re-parsed and `risk_warning_intervals` is replaced.
//! - `security_names` (M3 PR-12): the frozen SZSE `SSGSGMXX`/`tab2` table is
//!   re-parsed and `security_name_history` (plus its
//!   `derived_dataset_versions` registration) is replaced through the same
//!   writer the live item uses. The coverage windows are derived from the
//!   bytes — the SH/BJ `NOT_AVAILABLE` windows end on the table's last change
//!   day, never on `Utc::now()` — so a fresh init replays the evidence the
//!   first write recorded instead of re-stamping it. A frozen table with no
//!   A-share change row is `empty_name_set`: nothing is written, and "no data"
//!   is never reported as "the empty table was rebuilt".
//! - `suspensions` (XDM-138): the frozen A-3 suspension/resumption pages are
//!   re-parsed into the admitted facts, then the whole dataset is assembled by
//!   the **same** function the daily `derived:suspensions` item calls
//!   (`security_master::suspensions::assemble_suspensions`) from three
//!   database inputs: the `security_master` intervals, the derived trading
//!   calendar and the raw bar days. Those inputs are market data, not source
//!   store bytes, so this scope is only *conditionally* replayable: without a
//!   database it counts the facts the frozen pages carry and writes nothing;
//!   with a database that has no master/calendar it fails closed with
//!   `suspension_replay_not_ready` instead of writing an empty table. Rebuild
//!   the master and calendar first (or simply run the daily item), then run
//!   this scope. It is deliberately not part of `Scope::ALL`: a fresh-init
//!   `--scope all` runs before the services exist and therefore cannot have
//!   those inputs.
//!
//! Determinism: two replays of one store with one parser version produce the
//! same summary digest — the summary is sha256 over the canonical row lines
//! (`security_names` adds its coverage windows to the row digest), so "clear
//! the DB and replay" is checkable instead of asserted. `suspensions` is
//! deterministic over **both** inputs: the same store plus the same database
//! state gives the same digest, while a changed master/calendar legitimately
//! changes it. The replay records that state's digest (`inputs_digest`) in the
//! registration evidence, so `--verify-only` labels a database advance
//! `inputs_changed` (not `rows_changed`) and never calls it store corruption.
//!
//! Without a database the scopes only compute and report their summary, so a
//! read never touches a database and never creates the store.
//!
//! A blob that does not read back to its digest fails the whole scope with
//! `source_store_corrupt`; an unknown `parser_version` is fail-closed (§9.4.1).
//! No partial rebuild is produced: the index replacement is one transaction.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::NaiveDate;
use serde::Serialize;

use super::{ManifestRecord, SourceStore, StoreError, known_parser_version};
use crate::hash::sha256;
use crate::security_master::Exchange;
use crate::security_master::names::{self, NameInterval};
use crate::security_master::risk_warnings::{self, CoverageWindow};
use crate::sqlite_market::backend::MarketBackend;

/// Marks a scope report whose rebuild body is not implemented yet.
pub const SKELETON_REBUILT: &str = "skeleton_no_op";
/// The exchange table set was recomputed from the store and written.
pub const REBUILT_EXCHANGE: &str = "rebuilt";
/// The exchange table set was recomputed but no `--db` was given.
pub const COMPUTED_EXCHANGE_NO_DB: &str = "computed_no_db";
/// The industry table set was recomputed and written.
pub const REBUILT_INDUSTRY: &str = "rebuilt";
/// The industry table set was recomputed but no `--db` was given.
pub const COMPUTED_INDUSTRY_NO_DB: &str = "computed_no_db";
/// The generation-facts index was recomputed and written.
pub const REBUILT_GENERATION_FACTS: &str = "rebuilt";
/// The generation-facts index was recomputed but no `--db` was given (only
/// the source-store industry rows can be computed without a database).
pub const COMPUTED_GENERATION_FACTS_NO_DB: &str = "computed_no_db";
/// The risk-warning table set was recomputed and written.
pub const REBUILT_RISK_WARNINGS: &str = "rebuilt";
/// The risk-warning table set was recomputed but no `--db` was given.
pub const COMPUTED_RISK_WARNINGS_NO_DB: &str = "computed_no_db";
/// The name-history table set was recomputed and written.
pub const REBUILT_SECURITY_NAMES: &str = "rebuilt";
/// The name-history table set was recomputed but no `--db` was given.
pub const COMPUTED_SECURITY_NAMES_NO_DB: &str = "computed_no_db";
/// The `suspensions` scope had no writable dataset to produce (missing input
/// in the database), so nothing was rebuilt and nothing was written.
pub const REBUILT_NOT_READY: &str = "not_rebuilt";
/// The suspension dataset was recomputed from the frozen A-3 pages and written.
pub const REBUILT_SUSPENSIONS: &str = "rebuilt";
/// The frozen A-3 pages were parsed but no `--db` was given, so no dataset
/// could be assembled (the three other inputs live in the market database).
pub const COMPUTED_SUSPENSIONS_NO_DB: &str = "computed_no_db";
/// Stable code of a `--db` replay whose database has no `security_master` or
/// no trading calendar. It fails closed without writing an empty dataset.
pub const SUSPENSION_REPLAY_NOT_READY: &str = "suspension_replay_not_ready";
/// The scope had no matching manifest records: nothing was computed or written.
/// It is never reported as `rebuilt` — "no data" is not "an empty table".
pub const NOTHING_SELECTED: &str = "nothing_selected";

/// Source id of the frozen SZSE `SSGSGMXX`/`tab2` short-name table (PR-2).
const SZSE_SHORT_NAME_SOURCE_ID: &str = "szse_short_name";
/// Source id of the frozen SSE suspension/resumption event pages (A-3).
const SSE_SUSPENSION_SOURCE_ID: &str = "sse_suspension";

pub const NOTE: &str = "`exchange` (M3 PR-2), `industry` (M3 PR-6), `generation_facts` \
(M3 PR-8), `risk_warnings` (M3 PR-10) and `security_names` (M3 PR-12) rebuild their \
tables from the store; `suspensions` (XDM-138) additionally needs the database's \
security_master, trading calendar and bar days, so it is run explicitly after those \
are rebuilt (`--scope all` leaves it out).";

/// One replay domain (§9.4.2 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Industry,
    Exchange,
    GenerationFacts,
    RiskWarnings,
    SecurityNames,
    Suspensions,
}

impl Scope {
    /// The scopes `--scope all` replays. `suspensions` is deliberately not in
    /// this list: it derives from the database's `security_master`, calendar
    /// and bar days, which a fresh-init `--scope all` (run before the services
    /// start) does not have. Including it would make every fresh-init deployment
    /// fail with `suspension_replay_not_ready` before those inputs can exist.
    pub const ALL: [Scope; 5] = [
        Scope::Industry,
        Scope::Exchange,
        Scope::GenerationFacts,
        Scope::RiskWarnings,
        Scope::SecurityNames,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "industry" => Some(Self::Industry),
            "exchange" => Some(Self::Exchange),
            "generation_facts" => Some(Self::GenerationFacts),
            "risk_warnings" => Some(Self::RiskWarnings),
            "security_names" => Some(Self::SecurityNames),
            "suspensions" => Some(Self::Suspensions),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Industry => "industry",
            Self::Exchange => "exchange",
            Self::GenerationFacts => "generation_facts",
            Self::RiskWarnings => "risk_warnings",
            Self::SecurityNames => "security_names",
            Self::Suspensions => "suspensions",
        }
    }

    /// Which manifest records this scope owns.
    ///
    /// PR-1a keeps the selectors deliberately coarse: the exact partitioning
    /// is refined by the writer PRs, and a record may legitimately appear in
    /// more than one scope (e.g. a generation member that also carries
    /// industry bytes).
    pub fn selects(self, record: &ManifestRecord) -> bool {
        match self {
            Self::Industry => {
                record.source_id == super::TDXHY_SOURCE_ID
                    || record.source_id == super::INCON_SOURCE_ID
                    || (record.store_kind == super::StoreKind::RepoEvidence
                        && record.source_id.starts_with("tdx."))
            }
            Self::Exchange => {
                record.store_kind == super::StoreKind::HttpFetch
                    || record.source_id.starts_with("sse_")
                    || record.source_id.starts_with("szse_")
            }
            Self::GenerationFacts => record.store_kind == super::StoreKind::GenerationMember,
            Self::RiskWarnings => {
                record.store_kind == super::StoreKind::HttpFetch
                    && matches!(
                        record.source_id.as_str(),
                        "sse_bulletin" | "szse_short_name"
                    )
            }
            Self::SecurityNames => {
                record.store_kind == super::StoreKind::HttpFetch
                    && record.source_id == SZSE_SHORT_NAME_SOURCE_ID
            }
            Self::Suspensions => {
                record.store_kind == super::StoreKind::HttpFetch
                    && record.source_id == SSE_SUSPENSION_SOURCE_ID
            }
        }
    }
}

/// What one `replay`/`verify` call should do.
#[derive(Debug, Clone, Copy)]
pub struct ReplayRequest<'a> {
    /// `None` means every scope.
    pub scope: Option<Scope>,
    /// Recompute and compare against the database instead of writing it.
    pub verify_only: bool,
    /// The market database to rebuild; absent means "compute only".
    pub database: Option<&'a Path>,
}

impl<'a> ReplayRequest<'a> {
    pub fn new(scope: Option<Scope>, verify_only: bool, database: Option<&'a Path>) -> Self {
        Self {
            scope,
            verify_only,
            database,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopeReport {
    pub scope: &'static str,
    pub status: &'static str,
    pub entries: usize,
    pub blobs: usize,
    pub digest: String,
    pub rebuilt: &'static str,
    /// Canonical summary of the rebuilt table set (§9.4.2); `exchange` today.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// `match` / `drift` / `not_compared` against the database.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<&'static str>,
    /// Primary keys the database should change, capped at 20.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drift: Vec<String>,
    /// Why `--verify-only` did not match (`suspensions`): `inputs_changed`
    /// when the stored registration names a different master/calendar/bar-day
    /// state than the current database, otherwise `rows_changed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift_cause: Option<&'static str>,
    /// Digest of the database inputs a conditional scope derived from
    /// (`suspensions`). The same store with a different master/calendar has a
    /// different digest by design; it is not a store corruption.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs_digest: Option<String>,
    /// Whether the current `inputs_digest` equals the one recorded when the
    /// stored dataset was written (`suspensions`). `None` when the stored
    /// registration carries no such evidence (a dataset written before this
    /// scope existed). `false` with the rows still matching simply means the
    /// database advanced without changing the suspension rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs_match: Option<bool>,
    /// Rows of the rebuilt table set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<usize>,
    /// Interval rows (`industry`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignments: Option<usize>,
    /// Distinct snapshot start dates (§5.3 "6 boundaries").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundaries: Option<usize>,
    /// Same-day collisions that dropped a zero-length segment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded: Option<usize>,
    /// Registered generation facts (`generation_facts`) or confirmed facts
    /// parsed from the frozen pages (`suspensions`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facts: Option<usize>,
    /// Rows of the assembled suspension dataset (`suspensions` with `--db`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_rows: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap_rows: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_days: Option<i64>,
    /// Confirmed intervals the calendar cannot place (XDM-134).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred_beyond_calendar: Option<usize>,
    /// Confirmed intervals inside the calendar that cover no trading day.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_no_trading_day: Option<usize>,
    /// The replayable as-of bound (`security_names`): the last change day the
    /// frozen table proves. The SH/BJ `NOT_AVAILABLE` coverage windows end
    /// here. It is derived from the bytes, never from the clock, so the same
    /// store reports the same value on any day.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlobProblem {
    pub sha256: String,
    pub source_id: String,
    pub reason: &'static str,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayReport {
    pub status: &'static str,
    pub mode: &'static str,
    pub store: String,
    pub scope: String,
    pub verify_only: bool,
    pub entries: usize,
    pub digest: String,
    pub scopes: Vec<ScopeReport>,
    pub problems: Vec<BlobProblem>,
    pub note: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
}

/// Replay one scope (or all), verify every selected blob and rebuild the index
/// rows of the scopes whose writer exists (`exchange`, `industry`).
///
/// The replay runs before the services start (the deploy script replays between
/// fresh init and start), so it takes the database lease instead of racing a
/// live writer for it.
pub fn replay(
    store: &SourceStore,
    request: &ReplayRequest<'_>,
) -> Result<ReplayReport, StoreError> {
    run(store, request, "replay")
}

/// `verify_source_store`: the read-only check of the same selection. It forces
/// `verify_only`, so this entry point can never write the database even when
/// the caller's request says otherwise.
pub fn verify(
    store: &SourceStore,
    request: &ReplayRequest<'_>,
) -> Result<ReplayReport, StoreError> {
    run(
        store,
        &ReplayRequest {
            verify_only: true,
            ..*request
        },
        "verify",
    )
}

fn run(
    store: &SourceStore,
    request: &ReplayRequest<'_>,
    mode: &'static str,
) -> Result<ReplayReport, StoreError> {
    let records = store.records()?;
    let scopes = match request.scope {
        Some(scope) => vec![scope],
        None => Scope::ALL.to_vec(),
    };
    let mut scope_reports = Vec::new();
    let mut problems = Vec::new();
    let mut corrupt = false;
    let mut unknown_parser = false;
    for scope in &scopes {
        let selected: Vec<&ManifestRecord> = records.iter().filter(|r| scope.selects(r)).collect();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut scope_corrupt = false;
        let mut scope_unknown_parser = false;
        for record in &selected {
            let unsupported = record
                .parser_version
                .as_deref()
                .filter(|version| !known_parser_version(version));
            if let Some(version) = unsupported {
                unknown_parser = true;
                scope_unknown_parser = true;
                problems.push(BlobProblem {
                    sha256: record.sha256.clone(),
                    source_id: record.source_id.clone(),
                    reason: "unknown_parser",
                    expected: "a parser_version this binary can replay \
                               (source_store::known_parser_version)"
                        .to_string(),
                    actual: version.to_string(),
                });
                continue;
            }
            if !seen.insert(record.sha256.as_str()) {
                continue;
            }
            if let Err(error) = store.read_verified(&record.sha256) {
                corrupt = true;
                scope_corrupt = true;
                problems.push(problem(&error, record));
            }
        }
        let mut status = if selected.is_empty() {
            "store_empty"
        } else if scope_corrupt {
            "source_store_corrupt"
        } else if scope_unknown_parser {
            "unknown_parser"
        } else {
            "ok"
        };
        let mut rebuilt =
            if matches!(*scope, Scope::SecurityNames | Scope::Suspensions) && selected.is_empty() {
                // "No matching record" is not "rebuilt": the fresh-init table is
                // left untouched and the status stays `store_empty`.
                NOTHING_SELECTED
            } else {
                SKELETON_REBUILT
            };
        let mut summary = None;
        let mut database_verdict = None;
        let mut drift_keys = Vec::new();
        let mut drift_cause = None;
        let mut inputs_digest = None;
        let mut inputs_match = None;
        let mut snapshot_count = None;
        let mut fact_count = None;
        let mut as_of = None;
        let mut counts = (None, None, None);
        let mut suspension_counts = (None, None, None, None, None);
        if *scope == Scope::Industry && status == "ok" {
            match rebuild_industry(store, request) {
                Ok(outcome) => {
                    rebuilt = outcome.rebuilt;
                    summary = Some(outcome.summary);
                    database_verdict = outcome.database;
                    drift_keys = outcome.drift;
                    snapshot_count = Some(outcome.snapshots);
                    counts = (
                        Some(outcome.assignments),
                        Some(outcome.boundaries),
                        Some(outcome.superseded),
                    );
                    if outcome.status != "ok" {
                        status = outcome.status;
                    }
                }
                Err(error) => {
                    status = "industry_rebuild_failed";
                    problems.push(BlobProblem {
                        sha256: String::new(),
                        source_id: "tdx.base.zip!tdxhy.cfg".to_string(),
                        reason: "industry_rebuild_failed",
                        expected: "a parseable snapshot set".to_string(),
                        actual: error,
                    });
                }
            }
        }
        if *scope == Scope::Exchange && status == "ok" {
            match rebuild_exchange(store, &selected, request.verify_only, request.database) {
                Ok(outcome) => {
                    rebuilt = outcome.rebuilt;
                    summary = Some(outcome.summary);
                    database_verdict = outcome.database;
                    drift_keys = outcome.drift;
                    snapshot_count = Some(outcome.snapshots);
                    for issue in outcome.problems {
                        problems.push(BlobProblem {
                            sha256: issue.snapshot_id,
                            source_id: issue.source_id,
                            reason: issue.reason,
                            expected: outcome.expected.clone(),
                            actual: issue.detail,
                        });
                    }
                    if outcome.status != "ok" {
                        status = outcome.status;
                    }
                }
                Err(error) => {
                    status = "exchange_rebuild_failed";
                    problems.push(BlobProblem {
                        sha256: String::new(),
                        source_id: String::new(),
                        reason: "exchange_rebuild_failed",
                        expected: "a readable source store".to_string(),
                        actual: error,
                    });
                }
            }
        }
        if *scope == Scope::RiskWarnings && status == "ok" {
            match rebuild_risk_warnings(store, &selected, request.verify_only, request.database) {
                Ok(outcome) => {
                    rebuilt = outcome.rebuilt;
                    summary = Some(outcome.summary);
                    database_verdict = outcome.database;
                    drift_keys = outcome.drift;
                    snapshot_count = Some(outcome.snapshots);
                    if outcome.status != "ok" {
                        status = outcome.status;
                    }
                }
                Err(error) => {
                    status = "risk_warnings_rebuild_failed";
                    problems.push(BlobProblem {
                        sha256: String::new(),
                        source_id: "risk_warnings".to_string(),
                        reason: "risk_warnings_rebuild_failed",
                        expected: "a readable source store and market database".to_string(),
                        actual: error,
                    });
                }
            }
        }
        if *scope == Scope::GenerationFacts && status == "ok" {
            match rebuild_generation_facts(store, request) {
                Ok(outcome) => {
                    rebuilt = outcome.rebuilt;
                    summary = Some(outcome.summary);
                    database_verdict = outcome.database;
                    drift_keys = outcome.drift;
                    snapshot_count = Some(outcome.facts);
                    fact_count = Some(outcome.facts);
                    if outcome.status != "ok" {
                        status = outcome.status;
                    }
                }
                Err(error) => {
                    status = "generation_facts_rebuild_failed";
                    problems.push(BlobProblem {
                        sha256: String::new(),
                        source_id: "generation_facts".to_string(),
                        reason: "generation_facts_rebuild_failed",
                        expected: "a readable source store and market database".to_string(),
                        actual: error,
                    });
                }
            }
        }
        if *scope == Scope::SecurityNames && status == "ok" {
            match rebuild_security_names(store, &selected, request.verify_only, request.database) {
                Ok(outcome) => {
                    rebuilt = outcome.rebuilt;
                    summary = Some(outcome.summary);
                    database_verdict = outcome.database;
                    drift_keys = outcome.drift;
                    snapshot_count = Some(outcome.rows);
                    as_of = outcome.as_of;
                    if outcome.status != "ok" {
                        status = outcome.status;
                    }
                }
                Err(error) => {
                    // A frozen table that cannot become a name set is
                    // fail-closed and nothing is written; the reason code says
                    // which (`empty_name_set`, `source_invalid`, ...).
                    status = "security_names_rebuild_failed";
                    problems.push(BlobProblem {
                        sha256: String::new(),
                        source_id: SZSE_SHORT_NAME_SOURCE_ID.to_string(),
                        reason: "security_names_rebuild_failed",
                        expected: "a parseable SZSE short-name change table".to_string(),
                        actual: error,
                    });
                }
            }
        }
        if *scope == Scope::Suspensions && status == "ok" {
            match rebuild_suspensions(store, &selected, request.verify_only, request.database) {
                Ok(outcome) => {
                    rebuilt = outcome.rebuilt;
                    summary = outcome.summary;
                    database_verdict = outcome.database;
                    drift_keys = outcome.drift;
                    drift_cause = outcome.drift_cause;
                    inputs_digest = outcome.inputs_digest;
                    inputs_match = outcome.inputs_match;
                    snapshot_count = outcome.rows;
                    fact_count = Some(outcome.facts);
                    suspension_counts = (
                        Some(outcome.confirmed_rows),
                        Some(outcome.gap_rows),
                        Some(outcome.missing_days),
                        Some(outcome.deferred_beyond_calendar),
                        Some(outcome.skipped_no_trading_day),
                    );
                    for issue in outcome.problems {
                        problems.push(issue);
                    }
                    if outcome.status != "ok" {
                        status = outcome.status;
                    }
                }
                Err(error) => {
                    status = "suspensions_rebuild_failed";
                    problems.push(BlobProblem {
                        sha256: String::new(),
                        source_id: SSE_SUSPENSION_SOURCE_ID.to_string(),
                        reason: "suspensions_rebuild_failed",
                        expected: "a parseable A-3 page set and a readable market database"
                            .to_string(),
                        actual: error,
                    });
                }
            }
        }
        scope_reports.push(ScopeReport {
            scope: scope.name(),
            status,
            entries: selected.len(),
            blobs: seen.len(),
            digest: digest(selected.iter().copied()),
            rebuilt,
            summary,
            database: database_verdict,
            drift: drift_keys,
            drift_cause,
            inputs_digest,
            inputs_match,
            snapshots: snapshot_count,
            assignments: counts.0,
            boundaries: counts.1,
            superseded: counts.2,
            facts: fact_count,
            confirmed_rows: suspension_counts.0,
            gap_rows: suspension_counts.1,
            missing_days: suspension_counts.2,
            deferred_beyond_calendar: suspension_counts.3,
            skipped_no_trading_day: suspension_counts.4,
            as_of,
        });
    }
    let selected_all: Vec<&ManifestRecord> = records
        .iter()
        .filter(|record| scopes.iter().any(|scope| scope.selects(record)))
        .collect();
    let status = if records.is_empty() {
        "store_empty"
    } else if corrupt {
        "source_store_corrupt"
    } else if unknown_parser {
        "unknown_parser"
    } else if let Some(failed) = scope_reports
        .iter()
        .find(|report| report.status != "ok" && report.status != "store_empty")
    {
        // A scope that failed its rebuild (or found drift) is the run status;
        // reporting `ok` here would hide it behind the manifest digest.
        failed.status
    } else if scope_reports
        .iter()
        .any(|report| report.database == Some("drift"))
    {
        "drift"
    } else {
        "ok"
    };
    Ok(ReplayReport {
        status,
        mode,
        store: store.root().display().to_string(),
        scope: request
            .scope
            .map_or_else(|| "all".to_string(), |scope| scope.name().to_string()),
        verify_only: request.verify_only,
        entries: selected_all.len(),
        digest: digest(selected_all.into_iter()),
        scopes: scope_reports,
        problems,
        note: NOTE,
        database: request
            .database
            .map(|database| database.display().to_string()),
    })
}

/// The exchange scope's outcome, or the reason no table set was produced.
struct ExchangeOutcome {
    status: &'static str,
    rebuilt: &'static str,
    summary: String,
    database: Option<&'static str>,
    drift: Vec<String>,
    snapshots: usize,
    problems: Vec<crate::exchange_source::RebuildProblem>,
    expected: String,
}

fn rebuild_exchange(
    store: &SourceStore,
    records: &[&ManifestRecord],
    verify_only: bool,
    database: Option<&Path>,
) -> Result<ExchangeOutcome, String> {
    let rebuild =
        crate::exchange_source::rebuild(store, records).map_err(|error| error.to_string())?;
    let summary = rebuild.summary();
    let expected = format!(
        "a registered envelope parser ({}, {})",
        crate::exchange_source::request::SSE_PARSER_VERSION,
        crate::exchange_source::request::SZSE_PARSER_VERSION
    );
    let mut outcome = ExchangeOutcome {
        status: if rebuild.problems.is_empty() {
            "ok"
        } else {
            // Frozen bytes without a usable envelope stay registered and are
            // reported; they never become a row with a made-up count.
            "partially_failed"
        },
        rebuilt: COMPUTED_EXCHANGE_NO_DB,
        summary,
        database: None,
        drift: Vec::new(),
        snapshots: rebuild.rows.len(),
        problems: rebuild.problems,
        expected,
    };
    let Some(database) = database else {
        return Ok(outcome);
    };
    let tasks = crate::sqlite_tasks::SqliteTasks::new(database);
    let backend = MarketBackend::open(&tasks)
        .map_err(|error| format!("database_unavailable: {}: {error}", database.display()))?;
    if verify_only {
        let stored = backend
            .read_exchange_snapshots()
            .map_err(|error| error.to_string())?;
        let stored_summary = crate::exchange_source::summary(&stored);
        if stored_summary == outcome.summary {
            outcome.database = Some("match");
            return Ok(outcome);
        }
        outcome.status = "drift";
        outcome.database = Some("drift");
        outcome.drift = crate::exchange_source::index::drift(&rebuild.rows, &stored, 20);
        return Ok(outcome);
    }
    backend
        .replace_exchange_snapshots(&rebuild.rows)
        .map_err(|error| error.to_string())?;
    outcome.rebuilt = REBUILT_EXCHANGE;
    // A write is only reported as rebuilt once it reads back as the computed
    // rows: the same summary comparison `--verify-only` performs.
    let stored = backend
        .read_exchange_snapshots()
        .map_err(|error| error.to_string())?;
    let stored_summary = crate::exchange_source::summary(&stored);
    if stored_summary == outcome.summary {
        outcome.database = Some("match");
        return Ok(outcome);
    }
    outcome.status = "drift";
    outcome.database = Some("drift");
    outcome.drift = crate::exchange_source::index::drift(&rebuild.rows, &stored, 20);
    Ok(outcome)
}

/// Rebuild `risk_warning_intervals` from the frozen SZSE short-name table and
/// SSE bulletin pages. No HTTP request is made; the live coverage windows (and
/// therefore the gaps that cut open SH intervals) are read back from the
/// registration evidence when a database is supplied.
fn rebuild_risk_warnings(
    store: &SourceStore,
    records: &[&ManifestRecord],
    verify_only: bool,
    database: Option<&Path>,
) -> Result<ExchangeOutcome, String> {
    let mut selected: Vec<&ManifestRecord> = records
        .iter()
        .copied()
        .filter(|record| {
            matches!(
                record.source_id.as_str(),
                "sse_bulletin" | "szse_short_name"
            )
        })
        .collect();
    selected.sort_by(|left, right| {
        (
            left.source_id.as_str(),
            left.acquired_at.as_str(),
            left.sha256.as_str(),
        )
            .cmp(&(
                right.source_id.as_str(),
                right.acquired_at.as_str(),
                right.sha256.as_str(),
            ))
    });
    // The SZSE table is a whole snapshot, not an append log. Replay only the
    // latest frozen acquisition; older snapshots would duplicate the same
    // change chain and look like same-day conflicts.
    let latest_sz = selected
        .iter()
        .filter(|record| record.source_id == "szse_short_name")
        .map(|record| record.acquired_at.as_str())
        .max();
    let mut seen = BTreeSet::new();
    let mut pages = Vec::new();
    for record in selected {
        if record.source_id == "szse_short_name" && Some(record.acquired_at.as_str()) != latest_sz {
            continue;
        }
        if !seen.insert(record.sha256.as_str()) {
            continue;
        }
        let bytes = store
            .read_verified(&record.sha256)
            .map_err(|error| error.to_string())?;
        pages.push((record.source_id.as_str(), record.sha256.clone(), bytes));
    }

    let backend =
        match database {
            Some(path) => {
                let tasks = crate::sqlite_tasks::SqliteTasks::new(path);
                Some(MarketBackend::open(&tasks).map_err(|error| {
                    format!("database_unavailable: {}: {error}", path.display())
                })?)
            }
            None => None,
        };

    let mut coverage = Vec::new();
    if let Some(backend) = &backend
        && let Some((stored_coverage, _)) = registered_coverage(
            backend,
            risk_warnings::DATASET_KIND,
            risk_warnings::DATASET_KEY,
        )
        .map_err(|e| e.to_string())?
    {
        coverage = stored_coverage;
    }
    let gaps: Vec<(NaiveDate, NaiveDate)> = coverage
        .iter()
        .filter(|window| !window.observed)
        .map(|window| (window.from, window.to))
        .collect();
    let set = crate::official_security::risk_warnings::replay(&pages, &gaps, coverage)
        .map_err(|error| format!("{}: {error}", error.code()))?;
    let summary = crate::compact_market::digest_hex(&risk_warnings::dataset_digest(&set.rows));
    let mut outcome = ExchangeOutcome {
        status: "ok",
        rebuilt: COMPUTED_RISK_WARNINGS_NO_DB,
        summary: summary.clone(),
        database: None,
        drift: Vec::new(),
        snapshots: set.rows.len(),
        problems: Vec::new(),
        expected: "a parseable SZSE short-name and SSE bulletin page set".to_string(),
    };
    let Some(backend) = backend else {
        return Ok(outcome);
    };
    if verify_only {
        let stored = backend
            .read(crate::sqlite_market::risk_warnings::read_risk_warnings)
            .map_err(|error| error.to_string())?;
        let stored_summary =
            crate::compact_market::digest_hex(&risk_warnings::dataset_digest(&stored));
        if stored_summary == summary {
            outcome.database = Some("match");
            return Ok(outcome);
        }
        outcome.status = "drift";
        outcome.database = Some("drift");
        outcome.drift = row_drift(&set.rows, &stored);
        return Ok(outcome);
    }

    let manifest_text = records
        .iter()
        .map(|record| {
            format!(
                "{}|{}|{}|{}",
                record.source_id, record.sha256, record.acquired_at, record.origin
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let manifest_sha = sha256(manifest_text.as_bytes());
    let member_sha = sha256(b"source_store_replay:member");
    let event_sha = sha256(b"source_store_replay:event");
    let generation_evidence = serde_json::json!({
        "replay": true,
        "scope": "risk_warnings",
        "records": pages.len(),
        "coverage_windows": set.coverage.len(),
    });
    let write = crate::sqlite_market::risk_warnings::RiskWarningWrite {
        set: &set,
        task_id: "source_store_replay",
        item_id: "derived:risk_warnings",
        generation_id: "source_store_replay",
        generation_manifest_sha256: &manifest_sha,
        member_sha256: &member_sha,
        security_event_sha256: &event_sha,
        generation_evidence: &generation_evidence,
    };
    backend
        .commit_risk_warnings(&write)
        .map_err(|error| error.to_string())?;
    let stored = backend
        .read(crate::sqlite_market::risk_warnings::read_risk_warnings)
        .map_err(|error| error.to_string())?;
    let stored_summary = crate::compact_market::digest_hex(&risk_warnings::dataset_digest(&stored));
    if stored_summary == summary {
        outcome.rebuilt = REBUILT_RISK_WARNINGS;
        outcome.database = Some("match");
        return Ok(outcome);
    }
    outcome.status = "drift";
    outcome.database = Some("drift");
    outcome.drift = row_drift(&set.rows, &stored);
    Ok(outcome)
}

/// The coverage windows a derived dataset's live writer recorded, read from its
/// `derived_dataset_versions` registration evidence.
///
/// `risk_warnings` must cut intervals at the same `not_observed` gaps, otherwise
/// a gap would become a warning that spans evidence the run did not observe;
/// `security_names` compares them with the windows it recomputes from the store
/// (its own path needs no stored cover, but `--verify-only` must catch a
/// database whose evidence was stamped with a different window set).
fn registered_coverage(
    backend: &MarketBackend,
    dataset_kind: &str,
    dataset_key: &str,
) -> Result<Option<(Vec<CoverageWindow>, usize)>, String> {
    let registration = backend
        .read(|c| crate::sqlite_market::derived::read_version(c, dataset_kind, dataset_key))
        .map_err(|error| error.to_string())?;
    let Some(registration) = registration else {
        return Ok(None);
    };
    let (kind, text) = backend
        .read(|c| {
            crate::sqlite_market::evidence_store::read_evidence(c, &registration.evidence_digest)
        })
        .map_err(|error| error.to_string())?;
    if kind != crate::compact_market::EvidenceKind::Derived {
        return Err(format!(
            "{dataset_kind} registration evidence is not derived evidence"
        ));
    }
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("evidence_json: {error}"))?;
    let Some(rows) = value.get("coverage").and_then(serde_json::Value::as_array) else {
        return Ok(None);
    };
    let mut windows = Vec::new();
    for row in rows {
        let exchange = row
            .get("exchange")
            .and_then(serde_json::Value::as_str)
            .and_then(Exchange::parse)
            .ok_or_else(|| format!("{dataset_kind} coverage has an unregistered exchange"))?;
        let from = parse_window_day(row, "from", dataset_kind)?;
        let to = parse_window_day(row, "to", dataset_kind)?;
        windows.push(CoverageWindow {
            source_kind: row
                .get("source_kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            exchange,
            from,
            to,
            observed: row
                .get("observed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            rows: row
                .get("rows")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(0),
            reason: row
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    let not_observed = windows.iter().filter(|window| !window.observed).count();
    Ok(Some((windows, not_observed)))
}

fn parse_window_day(
    row: &serde_json::Value,
    field: &str,
    dataset_kind: &str,
) -> Result<NaiveDate, String> {
    let text = row
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{dataset_kind} coverage has no {field}"))?;
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| format!("{dataset_kind} coverage {field}={text:?} is not a day"))
}

fn row_drift(
    left: &[crate::security_master::RiskWarningInterval],
    right: &[crate::security_master::RiskWarningInterval],
) -> Vec<String> {
    let keys = |rows: &[crate::security_master::RiskWarningInterval]| {
        rows.iter()
            .map(|row| {
                format!(
                    "{}.{}@{}..{}:{}",
                    row.exchange.as_str(),
                    row.code,
                    row.effective_from,
                    row.effective_to
                        .map_or_else(|| "open".to_string(), |day| day.to_string()),
                    row.status.as_str()
                )
            })
            .collect::<BTreeSet<_>>()
    };
    let left = keys(left);
    let right = keys(right);
    left.symmetric_difference(&right)
        .take(20)
        .cloned()
        .collect()
}

/// The `security_names` scope's outcome, or the reason no table set was
/// produced.
struct SecurityNamesOutcome {
    status: &'static str,
    rebuilt: &'static str,
    summary: String,
    database: Option<&'static str>,
    drift: Vec<String>,
    rows: usize,
    as_of: Option<String>,
}

/// Rebuild `security_name_history` from the frozen SZSE short-name table
/// (M3 PR-12, D-61).
///
/// Only `szse_short_name` `http_fetch` records are selected, and only the
/// latest acquisition group: the table is a whole-market snapshot, so replaying
/// an older page set would duplicate the same change chain. One multi-page
/// fetch stamps every page with the same `acquired_at` (PR-2
/// `freeze_bytes(fetched_at)`), so a group is one complete page set. The
/// manifest has no acquisition-completeness flag: pages frozen by a fetch that
/// failed on a later page stay behind with the same `acquired_at`, so replay
/// cannot tell such a partial group apart (the live item never wrote it). The
/// set is built by
/// the adapter's replay entry, which derives the coverage windows from the
/// bytes, and written through the live item's own writer (whole-table replace +
/// `derived_dataset_versions` registration) in one transaction. `--verify-only`
/// compares the recomputed rows **and** coverage windows with the stored ones.
fn rebuild_security_names(
    store: &SourceStore,
    records: &[&ManifestRecord],
    verify_only: bool,
    database: Option<&Path>,
) -> Result<SecurityNamesOutcome, String> {
    let mut selected: Vec<&ManifestRecord> = records
        .iter()
        .copied()
        .filter(|record| record.source_id == SZSE_SHORT_NAME_SOURCE_ID)
        .collect();
    selected.sort_by(|left, right| {
        (
            left.acquired_at.as_str(),
            left.origin.as_str(),
            left.sha256.as_str(),
        )
            .cmp(&(
                right.acquired_at.as_str(),
                right.origin.as_str(),
                right.sha256.as_str(),
            ))
    });
    let latest = selected
        .iter()
        .map(|record| record.acquired_at.as_str())
        .max();
    let mut seen = BTreeSet::new();
    let mut pages = Vec::new();
    for record in selected {
        if Some(record.acquired_at.as_str()) != latest {
            continue;
        }
        if !seen.insert(record.sha256.as_str()) {
            continue;
        }
        let bytes = store
            .read_verified(&record.sha256)
            .map_err(|error| error.to_string())?;
        pages.push((record.source_id.as_str(), record.sha256.clone(), bytes));
    }

    let set = crate::official_security::security_names::replay(&pages)
        .map_err(|error| error.to_string())?;
    let summary = names_summary(&set.rows, &set.coverage);
    let as_of = set
        .coverage
        .iter()
        .map(|window| window.to)
        .max()
        .map(|day| day.to_string());
    let mut outcome = SecurityNamesOutcome {
        status: "ok",
        rebuilt: COMPUTED_SECURITY_NAMES_NO_DB,
        summary,
        database: None,
        drift: Vec::new(),
        rows: set.rows.len(),
        as_of,
    };
    let Some(database) = database else {
        return Ok(outcome);
    };
    let backend = open_database(database)?;
    if verify_only {
        let (stored_rows, stored_coverage) = stored_security_names(&backend)?;
        let stored_rows = stored_intervals(&stored_rows)?;
        let stored_coverage = stored_coverage.unwrap_or_default();
        let stored_summary = names_summary(&stored_rows, &stored_coverage);
        if stored_summary == outcome.summary {
            outcome.database = Some("match");
            return Ok(outcome);
        }
        outcome.status = "drift";
        outcome.database = Some("drift");
        outcome.drift = names_drift(&set.rows, &set.coverage, &stored_rows, &stored_coverage);
        return Ok(outcome);
    }

    let manifest_text = records
        .iter()
        .map(|record| {
            format!(
                "{}|{}|{}|{}",
                record.source_id, record.sha256, record.acquired_at, record.origin
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let manifest_sha = sha256(manifest_text.as_bytes());
    let member_sha = sha256(b"source_store_replay:member");
    let event_sha = sha256(b"source_store_replay:event");
    let generation_evidence = serde_json::json!({
        "replay": true,
        "scope": "security_names",
        "records": pages.len(),
        "rows": set.rows.len(),
        "coverage_windows": set.coverage.len(),
        // The replayable bound of the SH/BJ `NOT_AVAILABLE` windows (the last
        // change day the frozen table proves), not the acquisition clock.
        "as_of": outcome.as_of.clone(),
    });
    let write = crate::sqlite_market::security_names::SecurityNameWrite {
        set: &set,
        task_id: "source_store_replay",
        item_id: "derived:security_names",
        generation_id: "source_store_replay",
        generation_manifest_sha256: &manifest_sha,
        member_sha256: &member_sha,
        security_event_sha256: &event_sha,
        generation_evidence: &generation_evidence,
    };
    backend
        .commit_security_names(&write)
        .map_err(|error| error.to_string())?;
    // A write only counts once it reads back as the computed rows and windows.
    let (stored_rows, stored_coverage) = stored_security_names(&backend)?;
    let stored_rows = stored_intervals(&stored_rows)?;
    let stored_coverage = stored_coverage
        .ok_or_else(|| "the name-history registration has no coverage evidence".to_string())?;
    let stored_summary = names_summary(&stored_rows, &stored_coverage);
    if stored_summary == outcome.summary {
        outcome.rebuilt = REBUILT_SECURITY_NAMES;
        outcome.database = Some("match");
        return Ok(outcome);
    }
    outcome.status = "drift";
    outcome.database = Some("drift");
    outcome.drift = names_drift(&set.rows, &set.coverage, &stored_rows, &stored_coverage);
    Ok(outcome)
}

/// Stored `security_name_history` rows and the coverage windows of its
/// registration (`None` when it was never written).
fn stored_security_names(
    backend: &MarketBackend,
) -> Result<
    (
        Vec<crate::sqlite_market::security::SecurityNameRow>,
        Option<Vec<CoverageWindow>>,
    ),
    String,
> {
    let rows = backend
        .read(crate::sqlite_market::security::read_security_names)
        .map_err(|error| error.to_string())?;
    let coverage = registered_coverage(backend, names::DATASET_KIND, names::DATASET_KEY)
        .map_err(|error| error.to_string())?
        .map(|(windows, _)| windows);
    Ok((rows, coverage))
}

/// Decode the read model's rows back into the domain type so one canonical
/// summary covers both sides of the comparison.
fn stored_intervals(
    rows: &[crate::sqlite_market::security::SecurityNameRow],
) -> Result<Vec<NameInterval>, String> {
    rows.iter()
        .map(|row| {
            let exchange = Exchange::parse(&row.exchange).ok_or_else(|| {
                format!(
                    "security_name_history exchange {:?} is unregistered",
                    row.exchange
                )
            })?;
            Ok(NameInterval {
                exchange,
                code: row.code.clone(),
                effective_from: row.effective_from,
                effective_to: row.effective_to,
                name: row.name.clone(),
                certainty: row.certainty,
                known_at: row.known_at,
                source_kind: row.source_kind,
                source_record_id: row.source_record_id.clone(),
            })
        })
        .collect()
}

/// Canonical summary of one name-history answer: the length-prefixed row digest
/// plus the coverage windows. Coverage is part of the recorded evidence, so a
/// set with the same rows but re-stamped windows must not compare as `match`.
fn names_summary(rows: &[NameInterval], coverage: &[CoverageWindow]) -> String {
    let mut lines = vec![format!(
        "rows|{}",
        crate::compact_market::digest_hex(&names::dataset_digest(rows))
    )];
    lines.extend(coverage.iter().map(coverage_line));
    lines.sort();
    lines.dedup();
    sha256(lines.join("\n").as_bytes())
}

fn coverage_line(window: &CoverageWindow) -> String {
    format!(
        "coverage|{}|{}|{}|{}|{}|{}|{}",
        window.source_kind,
        window.exchange.as_str(),
        window.from,
        window.to,
        window.observed,
        window.rows,
        window.reason
    )
}

fn name_row_key(row: &NameInterval) -> String {
    format!(
        "{}.{}@{}..{}|{}|{}",
        row.exchange.as_str(),
        row.code,
        row.effective_from,
        row.effective_to
            .map_or_else(|| "open".to_string(), |day| day.to_string()),
        row.name,
        row.certainty.as_str()
    )
}

fn names_drift(
    recomputed_rows: &[NameInterval],
    recomputed_coverage: &[CoverageWindow],
    stored_rows: &[NameInterval],
    stored_coverage: &[CoverageWindow],
) -> Vec<String> {
    let keys = |rows: &[NameInterval], coverage: &[CoverageWindow]| {
        let mut keys: BTreeSet<String> = rows.iter().map(name_row_key).collect();
        keys.extend(coverage.iter().map(coverage_line));
        keys
    };
    keys(recomputed_rows, recomputed_coverage)
        .symmetric_difference(&keys(stored_rows, stored_coverage))
        .take(20)
        .cloned()
        .collect()
}

/// The `suspensions` scope's outcome, or the reason no dataset was produced.
struct SuspensionsOutcome {
    status: &'static str,
    rebuilt: &'static str,
    summary: Option<String>,
    database: Option<&'static str>,
    drift: Vec<String>,
    drift_cause: Option<&'static str>,
    inputs_digest: Option<String>,
    inputs_match: Option<bool>,
    /// Rows of the rebuilt dataset; `None` without a database (no assembly).
    rows: Option<usize>,
    /// Confirmed facts parsed from the frozen pages (placed when `--db`).
    facts: usize,
    confirmed_rows: usize,
    gap_rows: usize,
    missing_days: i64,
    deferred_beyond_calendar: usize,
    skipped_no_trading_day: usize,
    problems: Vec<BlobProblem>,
}

impl SuspensionsOutcome {
    fn computed(facts: usize) -> Self {
        Self {
            status: "ok",
            rebuilt: COMPUTED_SUSPENSIONS_NO_DB,
            summary: None,
            database: None,
            drift: Vec::new(),
            drift_cause: None,
            inputs_digest: None,
            inputs_match: None,
            rows: None,
            facts,
            confirmed_rows: 0,
            gap_rows: 0,
            missing_days: 0,
            deferred_beyond_calendar: 0,
            skipped_no_trading_day: 0,
            problems: Vec::new(),
        }
    }

    fn not_ready(detail: String) -> Self {
        Self {
            status: SUSPENSION_REPLAY_NOT_READY,
            rebuilt: REBUILT_NOT_READY,
            problems: vec![BlobProblem {
                sha256: String::new(),
                source_id: SSE_SUSPENSION_SOURCE_ID.to_string(),
                reason: SUSPENSION_REPLAY_NOT_READY,
                expected: "a database with a stored security_master and a non-empty \
                           trading calendar"
                    .to_string(),
                actual: detail,
            }],
            ..Self::computed(0)
        }
    }

    fn from_set(set: &crate::security_master::suspensions::SuspensionSet) -> Self {
        let mut outcome = Self::computed(0);
        outcome.rows = Some(set.rows.len());
        outcome.summary = Some(suspension_summary(&set.rows));
        outcome.confirmed_rows = set.confirmed_rows;
        outcome.gap_rows = set.gap_rows;
        outcome.missing_days = set.missing_days();
        outcome.deferred_beyond_calendar = set.deferred_beyond_calendar;
        outcome.skipped_no_trading_day = set.skipped_no_trading_day;
        outcome
    }
}

pub(crate) fn suspension_summary(rows: &[crate::security_master::SuspensionInterval]) -> String {
    // The writer sorts before it digests (`sort_rows` in
    // `sqlite_market::suspensions`); the summary must use the same order or a
    // set that would be written identically could compare as drift.
    let mut rows = rows.to_vec();
    rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.start_day).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.start_day,
        ))
    });
    crate::compact_market::digest_hex(&crate::security_master::suspensions::dataset_digest(&rows))
}

fn suspension_row_key(row: &crate::security_master::SuspensionInterval) -> String {
    format!(
        "{}.{}@{}..{}|{}|{}|{}|{}|{}|{}",
        row.exchange.as_str(),
        row.code,
        row.start_day,
        row.end_day,
        row.certainty.as_str(),
        row.reason.as_str(),
        row.missing_days,
        row.known_at.map(|day| day.to_string()).unwrap_or_default(),
        row.source_kind,
        row.source_record_id.as_deref().unwrap_or_default()
    )
}

fn suspension_drift(
    recomputed: &[crate::security_master::SuspensionInterval],
    stored: &[crate::security_master::SuspensionInterval],
) -> Vec<String> {
    let keys = |rows: &[crate::security_master::SuspensionInterval]| {
        rows.iter().map(suspension_row_key).collect::<BTreeSet<_>>()
    };
    keys(recomputed)
        .symmetric_difference(&keys(stored))
        .take(20)
        .cloned()
        .collect()
}

/// The derived dataset's registration evidence, `None` when it was never
/// written. The suspension replay reads its own `replay_inputs_digest` from
/// here to distinguish an input advance from changed rows.
fn derived_evidence(
    backend: &MarketBackend,
    dataset_kind: &str,
    dataset_key: &str,
) -> Result<Option<serde_json::Value>, String> {
    let registration = backend
        .read(|c| crate::sqlite_market::derived::read_version(c, dataset_kind, dataset_key))
        .map_err(|error| error.to_string())?;
    let Some(registration) = registration else {
        return Ok(None);
    };
    let (kind, text) = backend
        .read(|c| {
            crate::sqlite_market::evidence_store::read_evidence(c, &registration.evidence_digest)
        })
        .map_err(|error| error.to_string())?;
    if kind != crate::compact_market::EvidenceKind::Derived {
        return Err(format!(
            "{dataset_kind} registration evidence is not derived evidence"
        ));
    }
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("evidence_json: {error}"))?;
    Ok(Some(value))
}

fn stored_suspension_inputs_digest(backend: &MarketBackend) -> Result<Option<String>, String> {
    let evidence = derived_evidence(
        backend,
        crate::security_master::suspensions::DATASET_KIND,
        crate::security_master::suspensions::DATASET_KEY,
    )?;
    Ok(evidence
        .as_ref()
        .and_then(|value| value.get("replay_inputs_digest"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

/// Reconstruct the generation binding row the `security_master` was written
/// under, so `commit_suspensions` can prove (again) that the rows are bound to
/// the same generation as the master they derive from. A fresh-init replay has
/// no other way to name that binding: the master registration carries its
/// digest, and the binding row carries the generation evidence verbatim.
struct ReplayBinding {
    generation_id: String,
    manifest_sha: String,
    member_sha: String,
    event_sha: String,
    evidence: serde_json::Value,
}

fn replay_generation_binding(
    backend: &MarketBackend,
    master: &crate::sqlite_market::derived::DerivedVersionRow,
) -> Result<ReplayBinding, String> {
    let binding = backend
        .read(|c| {
            crate::sqlite_market::evidence_store::read_source_binding(
                c,
                &master.source_binding_digest,
            )
        })
        .map_err(|error| format!("security_master source binding: {error}"))?;
    let (kind, text) = backend
        .read(|c| crate::sqlite_market::evidence_store::read_evidence(c, &binding.evidence_digest))
        .map_err(|error| format!("security_master binding evidence: {error}"))?;
    if kind != crate::compact_market::EvidenceKind::Generation {
        return Err("security_master source binding evidence is not generation".to_string());
    }
    let evidence: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("binding evidence_json: {error}"))?;
    Ok(ReplayBinding {
        generation_id: binding.generation_id,
        manifest_sha: crate::compact_market::digest_hex(&binding.generation_manifest_sha256),
        member_sha: crate::compact_market::digest_hex(&binding.day_member_sha256),
        event_sha: crate::compact_market::digest_hex(&binding.security_event_sha256),
        evidence,
    })
}

/// Rebuild `suspension_intervals` from the frozen A-3 pages plus the database's
/// own inputs (XDM-138).
///
/// The three non-store inputs are read from the supplied database in one
/// snapshot (`sqlite_market::suspensions::read_inputs_if_ready`), the pages are re-parsed with
/// the same A-3 parser the daily item uses, and the dataset is assembled by
/// the daily item's own `assemble_suspensions` and written by its own
/// `commit_suspensions` (bound to the same generation as the master). There is
/// no second derivation and no second writer.
///
/// Without a database only the frozen pages are parsed and counted: without a
/// `security_master` an interval cannot be placed in a member window, so the
/// scope reports `computed_no_db` and writes nothing. With a database that has
/// no master/calendar it reports `suspension_replay_not_ready` and leaves the
/// table untouched: an empty table must never stand in for "not assembled".
fn rebuild_suspensions(
    store: &SourceStore,
    records: &[&ManifestRecord],
    verify_only: bool,
    database: Option<&Path>,
) -> Result<SuspensionsOutcome, String> {
    let index =
        crate::exchange_source::rebuild(store, records).map_err(|error| error.to_string())?;
    if !index.problems.is_empty() {
        let mut outcome = SuspensionsOutcome::computed(0);
        outcome.status = "suspensions_rebuild_failed";
        outcome.problems = index
            .problems
            .into_iter()
            .map(|issue| BlobProblem {
                sha256: issue.snapshot_id,
                source_id: issue.source_id,
                reason: issue.reason,
                expected: "a registered A-3 page envelope".to_string(),
                actual: issue.detail,
            })
            .collect();
        return Ok(outcome);
    }
    let Some(database) = database else {
        let facts = crate::official_security::a3::reparse_interval_count(store, &index.rows)
            .map_err(|error| format!("{}: {error}", error.code()))?;
        return Ok(SuspensionsOutcome::computed(facts));
    };
    let backend = open_database(database)?;
    let readiness = backend
        .read(crate::sqlite_market::suspensions::read_inputs_if_ready)
        .map_err(|error| format!("database_inputs: {error}"))?;
    let inputs = match readiness {
        crate::sqlite_market::suspensions::SuspensionInputsReadiness::Ready(inputs) => *inputs,
        crate::sqlite_market::suspensions::SuspensionInputsReadiness::MissingMaster => {
            return Ok(SuspensionsOutcome::not_ready(
                "no stored security_master; rebuild it (or run the daily item) before \
                 replaying suspensions"
                    .to_string(),
            ));
        }
        crate::sqlite_market::suspensions::SuspensionInputsReadiness::EmptyCalendar => {
            return Ok(SuspensionsOutcome::not_ready(
                "the derived trading calendar is empty; rebuild it (or run the daily \
                 item) before replaying suspensions"
                    .to_string(),
            ));
        }
    };
    // `reparse` only uses the day for the admitted snapshot's `as_of`
    // evidence; facts come from the pages and their fingerprints. Use the
    // newest observed acquisition day so the replay never reads the clock.
    let as_of = index
        .rows
        .iter()
        .filter_map(|row| NaiveDate::parse_from_str(&row.as_of, "%Y-%m-%d").ok())
        .max()
        .ok_or_else(|| "the frozen A-3 pages carry no observation date".to_string())?;
    let admitted =
        crate::official_security::a3::reparse(store, &index.rows, &inputs.intervals, as_of)
            .map_err(|error| format!("{}: {error}", error.code()))?;
    let facts = admitted.snapshot.facts.len();
    let seam = crate::security_master::suspensions::suspension_source(Some(admitted.snapshot));
    let set = crate::security_master::suspensions::assemble_suspensions(
        &seam,
        &inputs.intervals,
        &inputs.calendar,
        &inputs.bar_days,
    )
    .map_err(|error| error.to_string())?;
    let mut outcome = SuspensionsOutcome::from_set(&set);
    outcome.facts = facts;
    let inputs_digest = crate::security_master::suspensions::inputs_digest(
        &inputs.master.version_id,
        &inputs.calendar,
        &inputs.bar_days,
    );
    outcome.inputs_digest = Some(inputs_digest.clone());

    let stored = backend
        .read(crate::sqlite_market::suspensions::read_suspensions)
        .map_err(|error| format!("stored suspensions: {error}"))?;
    if verify_only {
        let stored_inputs = stored_suspension_inputs_digest(&backend)?;
        outcome.inputs_match = stored_inputs
            .as_deref()
            .map(|stored| stored == inputs_digest.as_str());
        let stored_summary = suspension_summary(&stored);
        if outcome.summary.as_deref() == Some(stored_summary.as_str()) {
            outcome.database = Some("match");
            return Ok(outcome);
        }
        outcome.status = "drift";
        outcome.database = Some("drift");
        outcome.drift = suspension_drift(&set.rows, &stored);
        outcome.drift_cause = Some(match stored_inputs {
            Some(stored) if stored != inputs_digest => "inputs_changed",
            _ => "rows_changed",
        });
        return Ok(outcome);
    }

    let binding = replay_generation_binding(&backend, &inputs.master)?;
    backend
        .commit_suspensions(&crate::sqlite_market::suspensions::SuspensionWrite {
            set: &set,
            master: &inputs.master,
            task_id: "source_store_replay",
            item_id: "derived:suspensions",
            generation_id: &binding.generation_id,
            generation_manifest_sha256: &binding.manifest_sha,
            member_sha256: &binding.member_sha,
            security_event_sha256: &binding.event_sha,
            generation_evidence: &binding.evidence,
            replay_inputs_digest: Some(inputs_digest.as_str()),
        })
        .map_err(|error| error.to_string())?;
    let stored = backend
        .read(crate::sqlite_market::suspensions::read_suspensions)
        .map_err(|error| format!("stored suspensions: {error}"))?;
    let stored_inputs = stored_suspension_inputs_digest(&backend)?;
    outcome.inputs_match = Some(
        stored_inputs
            .as_deref()
            .is_some_and(|stored| stored == inputs_digest.as_str()),
    );
    let stored_summary = suspension_summary(&stored);
    if outcome.summary.as_deref() == Some(stored_summary.as_str()) {
        outcome.rebuilt = REBUILT_SUSPENSIONS;
        outcome.database = Some("match");
        return Ok(outcome);
    }
    outcome.status = "drift";
    outcome.database = Some("drift");
    outcome.drift = suspension_drift(&set.rows, &stored);
    outcome.drift_cause = Some(match stored_inputs {
        Some(stored) if stored != inputs_digest => "inputs_changed",
        _ => "rows_changed",
    });
    Ok(outcome)
}

/// The generation-facts scope's outcome, or the reason no table set was
/// produced.
struct GenerationFactsOutcome {
    status: &'static str,
    rebuilt: &'static str,
    summary: String,
    database: Option<&'static str>,
    drift: Vec<String>,
    facts: usize,
}

/// Rebuild `generation_facts` from its two authorities (§9.4.2): the
/// source-store manifest (industry snapshot + dictionary) and the derived
/// tables' generation bindings (capital/adjustment events, daily series).
fn rebuild_generation_facts(
    store: &SourceStore,
    request: &ReplayRequest<'_>,
) -> Result<GenerationFactsOutcome, String> {
    let mut computed = crate::generation_gate::industry_fact_rows_from_store(store)
        .map_err(|error| error.to_string())?;
    let Some(database) = request.database else {
        return Ok(GenerationFactsOutcome {
            status: "ok",
            rebuilt: COMPUTED_GENERATION_FACTS_NO_DB,
            summary: crate::generation_gate::facts_summary(&computed),
            database: None,
            drift: Vec::new(),
            facts: computed.len(),
        });
    };
    let backend = open_database(database)?;
    computed.extend(
        backend
            .all_derived_generation_facts()
            .map_err(|error| error.to_string())?,
    );
    let summary = crate::generation_gate::facts_summary(&computed);
    let mut outcome = GenerationFactsOutcome {
        status: "ok",
        rebuilt: COMPUTED_GENERATION_FACTS_NO_DB,
        summary,
        database: None,
        drift: Vec::new(),
        facts: computed.len(),
    };
    if request.verify_only {
        let stored = backend
            .read_generation_facts()
            .map_err(|error| error.to_string())?;
        let stored_summary = crate::generation_gate::facts_summary(&stored);
        if stored_summary == outcome.summary {
            outcome.database = Some("match");
            return Ok(outcome);
        }
        outcome.status = "drift";
        outcome.database = Some("drift");
        outcome.drift = crate::generation_gate::drift_keys(&computed, &stored, 20);
        return Ok(outcome);
    }
    backend
        .replace_generation_facts(&computed)
        .map_err(|error| error.to_string())?;
    outcome.rebuilt = REBUILT_GENERATION_FACTS;
    // A write only counts once it reads back as the computed rows.
    let stored = backend
        .read_generation_facts()
        .map_err(|error| error.to_string())?;
    let stored_summary = crate::generation_gate::facts_summary(&stored);
    if stored_summary == outcome.summary {
        outcome.database = Some("match");
        return Ok(outcome);
    }
    outcome.status = "drift";
    outcome.database = Some("drift");
    outcome.drift = crate::generation_gate::drift_keys(&computed, &stored, 20);
    Ok(outcome)
}

/// The industry scope's outcome, or the reason no table was produced.
struct IndustryOutcome {
    status: &'static str,
    rebuilt: &'static str,
    summary: String,
    database: Option<&'static str>,
    drift: Vec<String>,
    snapshots: usize,
    assignments: usize,
    boundaries: usize,
    superseded: usize,
}

fn rebuild_industry(
    store: &SourceStore,
    request: &ReplayRequest<'_>,
) -> Result<IndustryOutcome, String> {
    let facts = crate::sqlite_market::industry::facts_from_store(store)
        .map_err(|error| error.to_string())?;
    let rebuild = crate::industry::rebuild(&facts).map_err(|error| error.to_string())?;
    let summary = crate::industry::summary(&rebuild.snapshots, &rebuild.assignments);
    let mut outcome = IndustryOutcome {
        status: "ok",
        rebuilt: COMPUTED_INDUSTRY_NO_DB,
        summary,
        database: None,
        drift: Vec::new(),
        snapshots: rebuild.snapshots.len(),
        assignments: rebuild.assignments.len(),
        boundaries: rebuild.boundaries,
        superseded: rebuild.superseded,
    };
    let Some(database) = request.database else {
        return Ok(outcome);
    };
    let backend = open_database(database)?;
    if request.verify_only {
        let (stored_snapshots, stored_assignments) =
            backend.read_industry().map_err(|error| error.to_string())?;
        let stored_summary = crate::industry::summary(&stored_snapshots, &stored_assignments);
        if stored_summary == outcome.summary {
            outcome.database = Some("match");
            return Ok(outcome);
        }
        outcome.status = "drift";
        outcome.database = Some("drift");
        outcome.drift = crate::industry::drift(
            &rebuild.snapshots,
            &rebuild.assignments,
            &stored_snapshots,
            &stored_assignments,
            20,
        );
        return Ok(outcome);
    }
    backend
        .replace_industry(&rebuild)
        .map_err(|error| error.to_string())?;
    outcome.rebuilt = REBUILT_INDUSTRY;
    // A write is only reported as rebuilt once it reads back as the computed
    // rows: the same summary comparison the verify mode performs.
    let (stored_snapshots, stored_assignments) =
        backend.read_industry().map_err(|error| error.to_string())?;
    let stored_summary = crate::industry::summary(&stored_snapshots, &stored_assignments);
    if stored_summary == outcome.summary {
        outcome.database = Some("match");
        return Ok(outcome);
    }
    outcome.status = "drift";
    outcome.database = Some("drift");
    outcome.drift = crate::industry::drift(
        &rebuild.snapshots,
        &rebuild.assignments,
        &stored_snapshots,
        &stored_assignments,
        20,
    );
    Ok(outcome)
}

/// Open the compact market database for the replay. The replay runs before the
/// services start, so it takes the service lease rather than racing a live
/// writer for it.
fn open_database(database: &Path) -> Result<MarketBackend, String> {
    let tasks = crate::sqlite_tasks::SqliteTasks::new(database);
    MarketBackend::open(&tasks)
        .map_err(|error| format!("database_unavailable: {}: {error}", database.display()))
}

fn problem(error: &StoreError, record: &ManifestRecord) -> BlobProblem {
    let (reason, expected, actual) = match error {
        StoreError::Corrupt {
            expected, actual, ..
        } => ("source_store_corrupt", expected.clone(), actual.clone()),
        StoreError::MissingBlob { sha256 } => (
            "source_store_missing_blob",
            sha256.clone(),
            "missing".to_string(),
        ),
        StoreError::InvalidSha256(value) => (
            "source_store_invalid_sha256",
            "64 lowercase hex characters".to_string(),
            value.clone(),
        ),
        other => ("source_store_io", record.sha256.clone(), other.to_string()),
    };
    BlobProblem {
        sha256: record.sha256.clone(),
        source_id: record.source_id.clone(),
        reason,
        expected,
        actual,
    }
}

/// Deterministic scope digest: fixed fields per distinct registration, sorted,
/// then sha256 over the joined lines. Two replays of one store agree bit for
/// bit; the line set is order-independent because the manifest is append-only.
pub fn digest<'a>(records: impl Iterator<Item = &'a ManifestRecord>) -> String {
    let mut lines: Vec<String> = records
        .map(|record| {
            format!(
                "{}|{}|{}|{}|{}|{}|{}|{}",
                record.sha256,
                record.source_id,
                record.store_kind.as_str(),
                record.origin,
                record.acquired_at,
                record.parser_version.as_deref().unwrap_or(""),
                record.size_bytes,
                record.publisher_last_modified.as_deref().unwrap_or(""),
            )
        })
        .collect();
    lines.sort();
    lines.dedup();
    sha256(lines.join("\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security_master::names::{NameCertainty, NameSourceKind};

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid day")
    }

    fn row() -> NameInterval {
        NameInterval {
            exchange: Exchange::Sz,
            code: "000004".into(),
            effective_from: day(2001, 3, 7),
            effective_to: None,
            name: "深安达".into(),
            certainty: NameCertainty::Unbounded,
            known_at: None,
            source_kind: NameSourceKind::SzseNameHistory,
            source_record_id: Some("2001-03-07|000004|深安达".into()),
        }
    }

    fn sh_window(to: NaiveDate) -> CoverageWindow {
        CoverageWindow {
            source_kind: "unknown".into(),
            exchange: Exchange::Sh,
            from: day(1994, 1, 3),
            to,
            observed: false,
            rows: 0,
            reason: "NOT_AVAILABLE".into(),
        }
    }

    /// A re-stamped window (`to = today` instead of the table's last change
    /// day) must be drift even when every row is identical: coverage is part of
    /// the recorded evidence, so it belongs in the compared summary.
    #[test]
    fn a_restamped_coverage_window_is_drift_without_a_row_change() {
        let rows = vec![row()];
        let first = vec![sh_window(day(2001, 3, 7))];
        let restamped = vec![sh_window(day(2026, 10, 31))];
        let stable = names_summary(&rows, &first);
        assert_eq!(stable, names_summary(&rows, &first.clone()));
        assert_ne!(stable, names_summary(&rows, &restamped));
        let drift = names_drift(&rows, &first, &rows, &restamped);
        assert_eq!(drift.len(), 2, "one window left, one entered: {drift:?}");
        assert!(
            drift.iter().all(|key| key.starts_with("coverage|")),
            "{drift:?}"
        );
    }

    /// The row digest ignores row order, so a replayed (sorted) set and a
    /// stored (SQL-ordered) set that are equal compare as `match`.
    #[test]
    fn the_summary_is_row_order_independent() {
        let mut second = row();
        second.code = "000001".into();
        second.name = "深发展A".into();
        let forward = vec![row(), second.clone()];
        let backward = vec![second, row()];
        let coverage = vec![sh_window(day(2001, 3, 7))];
        assert_eq!(
            names_summary(&forward, &coverage),
            names_summary(&backward, &coverage)
        );
    }
}
