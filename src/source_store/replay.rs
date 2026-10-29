//! Replay skeleton over the source store (design §9.4.2).
//!
//! PR-1a registers the three scopes and their byte-verification path before
//! schema 25 exists. Each scope therefore reports what it *selected* and a
//! deterministic digest, and rebuilds nothing yet (`rebuilt:
//! "skeleton_no_op"`): the index tables it will fill are added by PR-1 and the
//! domain writers by PR-6 (industry), PR-2/PR-8 (exchange) and PR-8
//! (generation facts). Empty store is `store_empty`, not an error, so the
//! deploy hook can record it.
//!
//! A blob that does not read back to its digest fails the whole scope with
//! `source_store_corrupt`; an unknown `parser_version` is fail-closed (§9.4.1).
//! No partial rebuild is produced, and PR-1a writes no database row at all.

use std::collections::BTreeSet;

use serde::Serialize;

use super::{ManifestRecord, SourceStore, StoreError, known_parser_version};
use crate::hash::sha256;

/// Marks a scope report whose rebuild body is not implemented yet.
pub const SKELETON_REBUILT: &str = "skeleton_no_op";

pub const NOTE: &str = "PR-1a skeleton: M3 index tables land in PR-1 and the scope writers in \
PR-2/PR-6/PR-8; this run only verifies blobs and prints a deterministic digest.";

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
}

/// Replay one scope (or all) and verify every selected blob. `verify_only` is
/// recorded in the report; with no index tables it is behaviourally identical
/// today, and PR-1 makes it skip the (now existing) writes.
pub fn replay(
    store: &SourceStore,
    scope: Option<Scope>,
    verify_only: bool,
) -> Result<ReplayReport, StoreError> {
    replay_with_mode(store, scope, verify_only, "replay")
}

/// `verify_source_store`: the read-only check of the same selection.
pub fn verify(store: &SourceStore, scope: Option<Scope>) -> Result<ReplayReport, StoreError> {
    replay_with_mode(store, scope, true, "verify")
}

fn replay_with_mode(
    store: &SourceStore,
    scope: Option<Scope>,
    verify_only: bool,
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
            if let Some(version) = &record.parser_version {
                if !known_parser_version(version) {
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
                        actual: version.clone(),
                    });
                    continue;
                }
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
        let status = if selected.is_empty() {
            "store_empty"
        } else if scope_corrupt {
            "source_store_corrupt"
        } else if scope_unknown_parser {
            "unknown_parser"
        } else {
            "ok"
        };
        scope_reports.push(ScopeReport {
            scope: scope.name(),
            status,
            entries: selected.len(),
            blobs: seen.len(),
            digest: digest(selected.iter().copied()),
            rebuilt: SKELETON_REBUILT,
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
    })
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
