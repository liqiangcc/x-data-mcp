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
//!
//! Determinism: two replays of one store with one parser version produce the
//! same summary digest — the summary is sha256 over the canonical row lines, so
//! "clear the DB and replay" is checkable instead of asserted.
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

pub const NOTE: &str = "`exchange` (M3 PR-2), `industry` (M3 PR-6), `generation_facts` \
(M3 PR-8) and `risk_warnings` (M3 PR-10) rebuild their tables from the store.";

/// One replay domain (§9.4.2 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Industry,
    Exchange,
    GenerationFacts,
    RiskWarnings,
}

impl Scope {
    pub const ALL: [Scope; 4] = [
        Scope::Industry,
        Scope::Exchange,
        Scope::GenerationFacts,
        Scope::RiskWarnings,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "industry" => Some(Self::Industry),
            "exchange" => Some(Self::Exchange),
            "generation_facts" => Some(Self::GenerationFacts),
            "risk_warnings" => Some(Self::RiskWarnings),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Industry => "industry",
            Self::Exchange => "exchange",
            Self::GenerationFacts => "generation_facts",
            Self::RiskWarnings => "risk_warnings",
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
    /// Registered generation facts (`generation_facts`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facts: Option<usize>,
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
        let mut counts = (None, None, None);
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
            assignments: counts.0,
            boundaries: counts.1,
            superseded: counts.2,
            facts: (*scope == Scope::GenerationFacts)
                .then_some(snapshot_count)
                .flatten(),
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
    if let Some(backend) = &backend {
        if let Some((stored_coverage, _)) =
            registered_coverage(backend).map_err(|e| e.to_string())?
        {
            coverage = stored_coverage;
        }
    }
    let gaps: Vec<(NaiveDate, NaiveDate)> = coverage
        .iter()
        .filter(|window| !window.observed)
        .map(|window| (window.from, window.to))
        .collect();
    let set = risk_warnings::replay(&pages, &gaps, coverage)
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

/// The coverage windows the live writer recorded. Replay must cut intervals at
/// the same `not_observed` gaps, otherwise a gap would become a warning that
/// spans evidence the run did not observe.
fn registered_coverage(
    backend: &MarketBackend,
) -> Result<Option<(Vec<CoverageWindow>, usize)>, String> {
    let registration = backend
        .read(|c| {
            crate::sqlite_market::derived::read_version(
                c,
                risk_warnings::DATASET_KIND,
                risk_warnings::DATASET_KEY,
            )
        })
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
        return Err("risk-warning registration evidence is not derived evidence".into());
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
            .ok_or_else(|| "risk-warning coverage has an unregistered exchange".to_string())?;
        let from = parse_window_day(row, "from")?;
        let to = parse_window_day(row, "to")?;
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

fn parse_window_day(row: &serde_json::Value, field: &str) -> Result<NaiveDate, String> {
    let text = row
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("risk-warning coverage has no {field}"))?;
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| format!("risk-warning coverage {field}={text:?} is not a day"))
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
