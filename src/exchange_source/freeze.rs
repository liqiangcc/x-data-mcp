//! Freezing exchange responses into the source store (design §6.3, §9.4.1).
//!
//! Order is forced and not configurable:
//!
//! 1. [`freeze_bytes`] publishes the bytes, fsyncs the blob, appends the
//!    manifest line and fsyncs it. The line records the request fingerprint as
//!    `origin`, the fetch time, the HTTP validators, the parser version and the
//!    basis status.
//! 2. [`record_snapshots`] writes the `exchange_source_snapshots` rows, and only
//!    after validation produced the row/page counts.
//!
//! Freezing happens before any parse, so a response that later fails
//! validation still leaves its bytes in the store for diagnosis and replay.
//! The reverse state ("index row present, blob unreadable") is impossible here
//! and rejected on every read path.

use chrono::{SecondsFormat, Utc};

use super::BasisStatus;
use super::index::{STORE_KIND_HTTP, SnapshotRow, observation_date};
use super::request::{ExchangeRequest, SourceId};
use super::transport::HttpResponse;
use super::validate::PageEnvelope;
use crate::market_storage::Error as StorageError;
use crate::source_store::{Registration, SourceStore, StoreError, StoreKind};
use crate::sqlite_market::backend::MarketBackend;

/// Bytes already durably registered in the store, waiting for a validated
/// row/page count before the index row may be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenBytes {
    pub snapshot_id: String,
    pub source_id: SourceId,
    pub url: String,
    pub request_fingerprint: String,
    pub fetched_at: String,
    pub as_of: String,
    pub http_etag: Option<String>,
    pub http_last_modified: Option<String>,
    pub size_bytes: u64,
    pub parser_version: String,
    pub basis_status: BasisStatus,
    /// A manifest line was appended by this call (false on a duplicate).
    pub appended_manifest_line: bool,
}

/// A frozen response with its index row written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenSnapshot {
    pub row: SnapshotRow,
    pub appended_manifest_line: bool,
    pub inserted_index_row: bool,
}

impl FrozenSnapshot {
    pub fn snapshot_id(&self) -> &str {
        &self.row.snapshot_id
    }

    /// True when this call neither added a manifest line nor an index row
    /// ("同指纹同 sha 不新增", §6.3).
    pub fn reused(&self) -> bool {
        !self.appended_manifest_line && !self.inserted_index_row
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FreezeError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Database(#[from] StorageError),
    #[error("invalid_fetch_time: {value} has no calendar date")]
    InvalidFetchTime { value: String },
}

impl FreezeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Store(error) => error.code(),
            Self::Database(_) => "source_index_write_failed",
            Self::InvalidFetchTime { .. } => "invalid_fetch_time",
        }
    }
}

/// Current UTC second, RFC 3339 (the manifest's `acquired_at`).
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Step 1: register the accepted response bytes. Nothing is parsed yet.
///
/// `fetched_at` is passed in instead of read from the clock so a caller can
/// group a multi-page fetch under one acquisition time and tests stay
/// deterministic; the entry passes [`now`].
pub fn freeze_bytes(
    store: &SourceStore,
    request: &ExchangeRequest,
    response: &HttpResponse,
    parser_version: &str,
    basis_status: BasisStatus,
    fetched_at: &str,
) -> Result<FrozenBytes, FreezeError> {
    let Some(as_of) = observation_date(fetched_at) else {
        return Err(FreezeError::InvalidFetchTime {
            value: fetched_at.to_string(),
        });
    };
    let fingerprint = request.fingerprint();
    let mut registration = Registration::new(
        request.source_id.as_str(),
        StoreKind::HttpFetch,
        fingerprint.clone(),
        fetched_at,
    );
    registration.publisher_last_modified = response.last_modified.clone();
    registration.basis_status = Some(basis_status.as_str().to_string());
    registration.parser_version = Some(parser_version.to_string());
    registration.http_etag = response.etag.clone();
    let outcome = store.register(&response.body, &registration)?;
    Ok(FrozenBytes {
        snapshot_id: outcome.sha256,
        source_id: request.source_id,
        url: request.display_url(),
        request_fingerprint: fingerprint,
        fetched_at: fetched_at.to_string(),
        as_of,
        http_etag: response.etag.clone(),
        http_last_modified: response.last_modified.clone(),
        size_bytes: response.body.len() as u64,
        parser_version: parser_version.to_string(),
        basis_status,
        appended_manifest_line: outcome.appended_line,
    })
}

/// Step 2: write the index rows of a validated window in one transaction
/// (§9.4.1 "每域一个事务"), then read the rows back through the same summary the
/// replay produces.
pub fn record_snapshots(
    backend: &MarketBackend,
    frozen: &[(FrozenBytes, PageEnvelope)],
) -> Result<Vec<FrozenSnapshot>, FreezeError> {
    let rows: Vec<SnapshotRow> = frozen
        .iter()
        .map(|(bytes, envelope)| SnapshotRow {
            snapshot_id: bytes.snapshot_id.clone(),
            store_sha256: bytes.snapshot_id.clone(),
            store_kind: STORE_KIND_HTTP.to_string(),
            source_id: bytes.source_id.as_str().to_string(),
            url: bytes.url.clone(),
            request_fingerprint: bytes.request_fingerprint.clone(),
            fetched_at: bytes.fetched_at.clone(),
            http_etag: bytes.http_etag.clone(),
            http_last_modified: bytes.http_last_modified.clone(),
            as_of: bytes.as_of.clone(),
            size_bytes: bytes.size_bytes,
            row_count: envelope.rows,
            page_count: Some(envelope.page_count),
            parser_version: bytes.parser_version.clone(),
            basis_status: bytes.basis_status.as_str().to_string(),
            notes: None,
        })
        .collect();
    // Read the key set *before* the insert: the difference is what this call
    // added, and the measurement must not be taken from its own write.
    let existing: std::collections::BTreeSet<String> = backend
        .read_exchange_snapshots()?
        .into_iter()
        .map(|row| row.snapshot_id)
        .collect();
    backend.write_exchange_snapshots(&rows)?;
    let mut snapshots = Vec::new();
    for ((bytes, _), row) in frozen.iter().zip(rows.iter()) {
        snapshots.push(FrozenSnapshot {
            row: row.clone(),
            appended_manifest_line: bytes.appended_manifest_line,
            // A snapshot id already present before this call is one row, not
            // two: the table is keyed by the response byte identity (§9.2).
            inserted_index_row: !existing.contains(&row.snapshot_id),
        });
    }
    Ok(snapshots)
}

/// Convenience for a single accepted response: freeze the bytes and, once the
/// caller has its envelope, index it in one call.
#[allow(clippy::too_many_arguments)]
pub fn freeze_page(
    store: &SourceStore,
    backend: &MarketBackend,
    request: &ExchangeRequest,
    response: &HttpResponse,
    envelope: &PageEnvelope,
    parser_version: &str,
    basis_status: BasisStatus,
    fetched_at: &str,
) -> Result<FrozenSnapshot, FreezeError> {
    let frozen = freeze_bytes(
        store,
        request,
        response,
        parser_version,
        basis_status,
        fetched_at,
    )?;
    let mut snapshots = record_snapshots(backend, &[(frozen, envelope.clone())])?;
    Ok(snapshots.remove(0))
}
