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
//! - `industry` / `generation_facts`: still the PR-1a skeleton (`rebuilt:
//!   "skeleton_no_op"`); their writers are PR-6 and PR-8.
//!
//! Without a database the scopes only compute and report their summary, so a
//! read never touches a database and never creates the store.
//!
//! A blob that does not read back to its digest fails the whole scope with
//! `source_store_corrupt`; an unknown `parser_version` is fail-closed (§9.4.1).
//! No partial rebuild is produced: the index replacement is one transaction.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use super::{ManifestRecord, SourceStore, StoreError, known_parser_version};
use crate::hash::sha256;
use crate::sqlite_market::backend::MarketBackend;

/// Marks a scope report whose rebuild body is not implemented yet.
pub const SKELETON_REBUILT: &str = "skeleton_no_op";
/// The exchange table set was recomputed from the store and written.
pub const REBUILT_EXCHANGE: &str = "rebuilt";
/// The exchange table set was recomputed but no `--db` was given.
pub const COMPUTED_EXCHANGE_NO_DB: &str = "computed_no_db";

pub const NOTE: &str = "`exchange` rebuilds `exchange_source_snapshots` from the store (M3 PR-2); \
`industry`/`generation_facts` still report `skeleton_no_op` (PR-6/PR-8).";

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
    /// Rows of the rebuilt table set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<usize>,
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

/// Replay one scope (or all) and verify every selected blob, without a
/// database: the store side only.
pub fn replay(
    store: &SourceStore,
    scope: Option<Scope>,
    verify_only: bool,
) -> Result<ReplayReport, StoreError> {
    replay_to(store, scope, verify_only, None)
}

/// `verify_source_store`: the read-only check of the same selection.
pub fn verify(store: &SourceStore, scope: Option<Scope>) -> Result<ReplayReport, StoreError> {
    replay_to(store, scope, true, None)
}

/// Replay with an optional database: scopes whose writer exists rebuild their
/// index rows (`exchange`, PR-2) and, with `verify_only`, only compare.
///
/// The replay runs before the services start (the deploy script replays between
/// fresh init and start), so it takes the database lease instead of racing a
/// live writer for it.
pub fn replay_to(
    store: &SourceStore,
    scope: Option<Scope>,
    verify_only: bool,
    database: Option<&Path>,
) -> Result<ReplayReport, StoreError> {
    run(store, scope, verify_only, database, "replay")
}

fn run(
    store: &SourceStore,
    scope: Option<Scope>,
    verify_only: bool,
    database: Option<&Path>,
    mode: &'static str,
) -> Result<ReplayReport, StoreError> {
    let records = store.records()?;
    let scopes = match scope {
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
        let mut database_verdict = None;
        let mut drift_keys = Vec::new();
        let mut snapshot_count = None;
        if *scope == Scope::Exchange && status == "ok" {
            match rebuild_exchange(store, &selected, verify_only, database) {
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
            snapshots: snapshot_count,
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
        scope: scope.map_or_else(|| "all".to_string(), |scope| scope.name().to_string()),
        verify_only,
        entries: selected_all.len(),
        digest: digest(selected_all.into_iter()),
        scopes: scope_reports,
        problems,
        note: NOTE,
        database: database.map(|database| database.display().to_string()),
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
