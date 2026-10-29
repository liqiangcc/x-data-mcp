//! Replay over the source store (design §9.4.2).
//!
//! The store is the authority; the M3 tables are indexes over it. Each scope
//! therefore does two things: verify every selected blob by digest, and — for
//! the scopes whose writer exists — rebuild its index rows from those bytes.
//!
//! Scope status today:
//!
//! - `industry`: **implemented** (M3 PR-6). `tdxhy.cfg` snapshots and their
//!   `incon.dat` dictionary are re-parsed with the parser version the manifest
//!   registered, the intervals are recomputed deterministically (§5.1/§5.2),
//!   and the whole table set is replaced in one transaction. `--verify-only`
//!   recomputes the same summary and compares it with the stored rows.
//! - `exchange` / `generation_facts`: still the PR-1a skeleton (`rebuilt:
//!   "skeleton_no_op"`); their writers are PR-2/PR-8.
//!
//! Determinism: two replays of one store with one parser version produce the
//! same summary digest. The summary is sha256 over the canonical row lines, so
//! "clear the DB and replay" is checkable instead of asserted. Without `--db`
//! the industry scope only computes (and reports) that summary; it touches no
//! database, which keeps the read-only forms of the CLI read-only.
//!
//! A blob that does not read back to its digest fails the whole scope with
//! `source_store_corrupt` and no partial rebuild is produced; an unknown
//! `parser_version` is fail-closed (§9.4.1).

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use super::{ManifestRecord, SourceStore, StoreError, known_parser_version};
use crate::hash::sha256;
use crate::industry;

/// Marks a scope report whose rebuild body is not implemented yet.
pub const SKELETON_REBUILT: &str = "skeleton_no_op";
/// The industry table set was recomputed and written to the database.
pub const REBUILT_INDUSTRY: &str = "rebuilt";
/// The industry table set was recomputed but no `--db` was given.
pub const COMPUTED_NO_DB: &str = "computed_no_db";

pub const NOTE: &str = "PR-1a skeleton scopes (exchange/generation_facts) still report \
`skeleton_no_op`; `industry` rebuilds snapshots, dictionary pairing and intervals from the \
store (M3 PR-6).";

/// One replay domain (§9.4.2 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Industry,
    Exchange,
    GenerationFacts,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Industry, Scope::Exchange, Scope::GenerationFacts];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "industry" => Some(Self::Industry),
            "exchange" => Some(Self::Exchange),
            "generation_facts" => Some(Self::GenerationFacts),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Industry => "industry",
            Self::Exchange => "exchange",
            Self::GenerationFacts => "generation_facts",
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
    /// Canonical summary of the table set this scope rebuilt (`industry` only):
    /// sha256 over the sorted row lines (§9.4.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// `match` / `drift` / `not_compared` against the database.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<&'static str>,
    /// Rows the database should change, capped at 20 primary keys.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drift: Vec<String>,
    /// Snapshot/interval counts of the rebuilt table set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignments: Option<usize>,
    /// Distinct snapshot start dates (§5.3 "6 boundaries").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundaries: Option<usize>,
    /// Same-day collisions that dropped a zero-length segment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded: Option<usize>,
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

/// Replay one scope (or all), verify every selected blob, and rebuild the
/// scopes whose writer exists.
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
    let mut industry_status: Option<&'static str> = None;
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
                    expected: format!(
                        "{}|{}",
                        super::TDXHY_PARSER_VERSION,
                        super::INCON_PARSER_VERSION
                    ),
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
        let mut rebuilt = SKELETON_REBUILT;
        let mut summary = None;
        let mut database = None;
        let mut drift = Vec::new();
        let mut counts = (None, None, None, None);
        if *scope == Scope::Industry && status == "ok" {
            match rebuild_industry(store, request) {
                Ok(outcome) => {
                    rebuilt = outcome.rebuilt;
                    summary = Some(outcome.summary);
                    database = outcome.database;
                    drift = outcome.drift;
                    counts = (
                        Some(outcome.snapshots),
                        Some(outcome.assignments),
                        Some(outcome.boundaries),
                        Some(outcome.superseded),
                    );
                    industry_status = Some(outcome.status);
                    if outcome.status != "ok" {
                        status = outcome.status;
                    }
                }
                Err(error) => {
                    status = "industry_rebuild_failed";
                    industry_status = Some(status);
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
        scope_reports.push(ScopeReport {
            scope: scope.name(),
            status,
            entries: selected.len(),
            blobs: seen.len(),
            digest: digest(selected.iter().copied()),
            rebuilt,
            summary,
            database,
            drift,
            snapshots: counts.0,
            assignments: counts.1,
            boundaries: counts.2,
            superseded: counts.3,
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
    } else if let Some(status) = industry_status.filter(|status| *status != "ok") {
        status
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
    let rebuild = industry::rebuild(&facts).map_err(|error| error.to_string())?;
    let summary = industry::summary(&rebuild.snapshots, &rebuild.assignments);
    let mut outcome = IndustryOutcome {
        status: "ok",
        rebuilt: COMPUTED_NO_DB,
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
        let stored_summary = industry::summary(&stored_snapshots, &stored_assignments);
        if stored_summary == outcome.summary {
            outcome.database = Some("match");
            return Ok(outcome);
        }
        outcome.status = "drift";
        outcome.database = Some("drift");
        outcome.drift = industry::drift(
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
    let stored_summary = industry::summary(&stored_snapshots, &stored_assignments);
    if stored_summary == outcome.summary {
        outcome.database = Some("match");
        return Ok(outcome);
    }
    outcome.status = "drift";
    outcome.database = Some("drift");
    outcome.drift = industry::drift(
        &rebuild.snapshots,
        &rebuild.assignments,
        &stored_snapshots,
        &stored_assignments,
        20,
    );
    Ok(outcome)
}

/// Open the compact market database for the replay. The replay runs before the
/// services start (the deploy script replays between fresh-init and start), so
/// it takes the service lease rather than racing a live writer for it.
fn open_database(database: &Path) -> Result<crate::sqlite_market::backend::MarketBackend, String> {
    let tasks = crate::sqlite_tasks::SqliteTasks::new(database);
    crate::sqlite_market::backend::MarketBackend::open(&tasks)
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
