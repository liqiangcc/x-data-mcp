//! `exchange_source_snapshots` as an index over the source store (design §9.2,
//! §9.4.2).
//!
//! One row per frozen response byte identity (`snapshot_id` = response sha256).
//! The store's blob + manifest line is the authority; this row is rebuilt from
//! it by [`rebuild`] without any HTTP request, so "clear the DB, replay the
//! store, compare the summary" is a real check (AC-M3-9) instead of a claim.
//!
//! Rebuild inputs come only from the manifest (`source_id`, `origin` =
//! canonical request fingerprint, `acquired_at`, `publisher_last_modified`,
//! `http_etag`, `basis_status`, `parser_version`, `size_bytes`) plus the bytes
//! themselves: `row_count`/`page_count` are re-derived with the recorded
//! envelope parser version. A response that does not parse keeps its frozen
//! bytes and is reported as a problem instead of being silently skipped or
//! given a made-up row count.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate};
use serde::Serialize;

use super::BasisStatus;
use super::request::PagingFormat;
use super::validate;
use crate::hash::sha256;
use crate::source_store::{ManifestRecord, SourceStore, StoreError, StoreKind};

/// `store_kind` value of a fetched response (§9.2 CHECK).
pub const STORE_KIND_HTTP: &str = "http_fetch";
/// `store_kind` value of an imported archive member.
pub const STORE_KIND_ARCHIVE: &str = "archive_import";

/// One `exchange_source_snapshots` row (§9.2).
///
/// `snapshot_id` and `store_sha256` are the same byte identity: the first is
/// the table key (hex text), the second the explicit "blob in the source store"
/// column (§9.2 naming note), so nothing is inferred from a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotRow {
    pub snapshot_id: String,
    pub store_sha256: String,
    pub store_kind: String,
    pub source_id: String,
    pub url: String,
    pub request_fingerprint: String,
    pub fetched_at: String,
    pub http_etag: Option<String>,
    pub http_last_modified: Option<String>,
    pub as_of: String,
    pub size_bytes: u64,
    pub row_count: usize,
    pub page_count: Option<u32>,
    pub parser_version: String,
    pub basis_status: String,
    pub notes: Option<String>,
}

impl SnapshotRow {
    /// Canonical fixed-field line of one row. JSON keeps field order fixed and
    /// escapes strings, so two replays of one store cannot disagree because of
    /// a separator inside a value.
    pub fn canonical_line(&self) -> String {
        serde_json::to_string(self).expect("a snapshot row always serializes")
    }
}

/// Deterministic summary of a table set: sha256 over the sorted canonical
/// lines (§9.4.2 "每个 scope 输出规范化摘要").
pub fn summary(rows: &[SnapshotRow]) -> String {
    let mut lines: Vec<String> = rows.iter().map(SnapshotRow::canonical_line).collect();
    lines.sort();
    lines.dedup();
    sha256(lines.join("\n").as_bytes())
}

/// Primary keys whose expected and stored rows disagree, capped at `limit`
/// (first 20 required by §9.4.2).
pub fn drift(expected: &[SnapshotRow], stored: &[SnapshotRow], limit: usize) -> Vec<String> {
    let expected: BTreeMap<&str, &SnapshotRow> = expected
        .iter()
        .map(|row| (row.snapshot_id.as_str(), row))
        .collect();
    let stored: BTreeMap<&str, &SnapshotRow> = stored
        .iter()
        .map(|row| (row.snapshot_id.as_str(), row))
        .collect();
    let mut keys: Vec<String> = expected
        .keys()
        .chain(stored.keys())
        .filter(|key| expected.get(*key) != stored.get(*key))
        .map(|key| (*key).to_string())
        .collect();
    keys.sort();
    keys.dedup();
    keys.truncate(limit);
    keys
}

/// One frozen response that cannot be turned into an index row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RebuildProblem {
    pub snapshot_id: String,
    pub source_id: String,
    pub reason: &'static str,
    pub detail: String,
}

/// What `replay --scope exchange` computes from the store alone.
#[derive(Debug, Clone, Default)]
pub struct Rebuild {
    pub rows: Vec<SnapshotRow>,
    pub problems: Vec<RebuildProblem>,
}

impl Rebuild {
    pub fn summary(&self) -> String {
        summary(&self.rows)
    }
}

/// Rebuild the index rows from manifest records (already selected for the
/// `exchange` scope) plus their verified bytes.
///
/// A blob that does not read back to its digest fails the whole call with
/// `source_store_corrupt` (§9.4.2: no half-written scope). A response that
/// cannot be parsed is reported in `problems` and produces no row; its bytes
/// stay frozen.
pub fn rebuild(store: &SourceStore, records: &[&ManifestRecord]) -> Result<Rebuild, StoreError> {
    let mut problems = Vec::new();
    // One response byte identity is one row even when several registrations
    // (different origins) point at it; pick the earliest acquisition and, for
    // ties, the smallest origin so the choice is deterministic.
    let mut chosen: BTreeMap<String, &ManifestRecord> = BTreeMap::new();
    for record in records {
        match chosen.get(&record.sha256) {
            None => {
                chosen.insert(record.sha256.clone(), record);
            }
            Some(previous) => {
                let candidate = (record.acquired_at.as_str(), record.origin.as_str());
                let current = (previous.acquired_at.as_str(), previous.origin.as_str());
                if candidate < current {
                    chosen.insert(record.sha256.clone(), record);
                }
            }
        }
    }
    let mut rows = Vec::new();
    for (snapshot_id, record) in &chosen {
        let Some(store_kind) = store_kind(record.store_kind) else {
            problems.push(problem(
                snapshot_id,
                record,
                "unregistered_store_kind",
                record.store_kind.as_str(),
            ));
            continue;
        };
        let parser_version = record.parser_version.as_deref().unwrap_or_default();
        let Some(format) = PagingFormat::from_parser_version(parser_version) else {
            problems.push(problem(
                snapshot_id,
                record,
                "unknown_parser",
                parser_version,
            ));
            continue;
        };
        let Some(basis_status) = record.basis_status.as_deref().and_then(BasisStatus::parse) else {
            problems.push(problem(
                snapshot_id,
                record,
                "missing_basis_status",
                record.basis_status.as_deref().unwrap_or(""),
            ));
            continue;
        };
        let Some(as_of) = observation_date(&record.acquired_at) else {
            problems.push(problem(
                snapshot_id,
                record,
                "invalid_acquired_at",
                &record.acquired_at,
            ));
            continue;
        };
        let bytes = store.read_verified(snapshot_id)?;
        let envelope = match validate::parse_page(format, &bytes) {
            Ok(envelope) => envelope,
            Err(error) => {
                problems.push(problem(
                    snapshot_id,
                    record,
                    error.code(),
                    &error.to_string(),
                ));
                continue;
            }
        };
        rows.push(SnapshotRow {
            snapshot_id: snapshot_id.clone(),
            store_sha256: snapshot_id.clone(),
            store_kind: store_kind.to_string(),
            source_id: record.source_id.clone(),
            url: request_url(&record.origin),
            request_fingerprint: record.origin.clone(),
            fetched_at: record.acquired_at.clone(),
            http_etag: record.http_etag.clone(),
            http_last_modified: record.publisher_last_modified.clone(),
            as_of,
            size_bytes: record.size_bytes,
            row_count: envelope.rows,
            page_count: Some(envelope.page_count),
            parser_version: parser_version.to_string(),
            basis_status: basis_status.as_str().to_string(),
            notes: None,
        });
    }
    Ok(Rebuild { rows, problems })
}

fn problem(
    snapshot_id: &str,
    record: &ManifestRecord,
    reason: &'static str,
    detail: &str,
) -> RebuildProblem {
    RebuildProblem {
        snapshot_id: snapshot_id.to_string(),
        source_id: record.source_id.clone(),
        reason,
        detail: detail.to_string(),
    }
}

fn store_kind(kind: StoreKind) -> Option<&'static str> {
    match kind {
        StoreKind::HttpFetch => Some(STORE_KIND_HTTP),
        StoreKind::ArchiveImport => Some(STORE_KIND_ARCHIVE),
        StoreKind::GenerationMember | StoreKind::RepoEvidence => None,
    }
}

/// The `url` column of a rebuilt row: the exact page URL, read back from the
/// canonical request fingerprint the manifest registered as `origin`.
/// `display_url` is deterministic from the fingerprint (sorted parameters), so
/// freeze and replay write the same string. An origin that is not a version-1
/// fingerprint (e.g. a legacy importer label) keeps the label, which is still
/// deterministic and auditable.
fn request_url(origin: &str) -> String {
    super::request::ExchangeRequest::from_fingerprint(origin)
        .map(|request| request.display_url())
        .unwrap_or_else(|_| origin.to_string())
}

/// Observation date (`as_of`) of a fetched response: the UTC date of the first
/// acquisition. Freeze and replay both call this, so the column is reproducible
/// from the manifest alone.
pub(crate) fn observation_date(fetched_at: &str) -> Option<String> {
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(fetched_at) {
        return Some(timestamp.date_naive().format("%Y-%m-%d").to_string());
    }
    let prefix = fetched_at.get(..10)?;
    NaiveDate::parse_from_str(prefix, "%Y-%m-%d")
        .ok()
        .map(|date| date.format("%Y-%m-%d").to_string())
}
