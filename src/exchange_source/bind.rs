//! The binding seam consumers depend on (design §6.3 "bind", §6.3 "失败语义").
//!
//! A consumer never sees a URL, a pagination quirk or a retry: it asks for
//! [`bind`] of one `source_id` and gets the frozen snapshot ids, their parser
//! versions and their basis status plus one [`SnapshotBinding::source_binding_digest`].
//! Re-running the same consumer against the same store must produce the same
//! digest (§6.3 "同一绑定重跑必须逐位相同"), so the digest is computed over
//! sorted fixed-field lines and nothing volatile.
//!
//! `basis_status` below the writer's threshold is [`BindError::NotAdmitted`]
//! (`source_not_admitted`): the writer refuses instead of writing 0 rows that
//! would masquerade as "nothing happened" (§6.3).

use serde::Serialize;

use super::index::SnapshotRow;
use super::{BasisStatus, SourceId};
use crate::hash::sha256;
use crate::market_storage::Error as StorageError;
use crate::source_store::{SourceStore, StoreError};
use crate::sqlite_market::backend::MarketBackend;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundSnapshot {
    pub snapshot_id: String,
    pub parser_version: String,
    pub basis_status: String,
    pub as_of: String,
}

/// What one consumer is pinned to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotBinding {
    pub source_id: String,
    pub snapshots: Vec<BoundSnapshot>,
    /// sha256 over the sorted canonical snapshot lines.
    pub source_binding_digest: String,
}

impl SnapshotBinding {
    pub fn snapshot_ids(&self) -> Vec<&str> {
        self.snapshots
            .iter()
            .map(|snapshot| snapshot.snapshot_id.as_str())
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("source_snapshot_not_found: no frozen snapshot for {source_id}")]
    NotFound { source_id: String },
    #[error(
        "source_not_admitted: {source_id} snapshot {snapshot_id} is {basis}, \
         the writer requires {required}"
    )]
    NotAdmitted {
        source_id: String,
        snapshot_id: String,
        basis: String,
        required: String,
    },
    #[error(transparent)]
    Database(#[from] StorageError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl BindError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "source_snapshot_not_found",
            Self::NotAdmitted { .. } => "source_not_admitted",
            Self::Database(_) => "source_index_unavailable",
            Self::Store(error) => error.code(),
        }
    }

    pub fn class(&self) -> super::validate::FailureClass {
        match self {
            Self::NotAdmitted { .. } | Self::NotFound { .. } => {
                super::validate::FailureClass::SourceInvalid
            }
            Self::Database(_) | Self::Store(_) => super::validate::FailureClass::SourceUnavailable,
        }
    }
}

/// Bind every frozen snapshot of `source` from the database index, requiring at
/// least `required` evidence.
pub fn bind(
    backend: &MarketBackend,
    source: SourceId,
    required: BasisStatus,
) -> Result<SnapshotBinding, BindError> {
    let rows = backend.read_exchange_snapshots()?;
    bind_from_rows(&rows, source, required)
}

/// Pure form of [`bind`], for callers that already hold the rows (and for
/// tests): same ordering, same digest, same admission gate.
pub fn bind_from_rows(
    rows: &[SnapshotRow],
    source: SourceId,
    required: BasisStatus,
) -> Result<SnapshotBinding, BindError> {
    let source_id = source.as_str();
    let mut selected: Vec<&SnapshotRow> = rows
        .iter()
        .filter(|row| row.source_id == source_id)
        .collect();
    if selected.is_empty() {
        return Err(BindError::NotFound {
            source_id: source_id.to_string(),
        });
    }
    selected.sort_by(|left, right| {
        (left.as_of.as_str(), left.snapshot_id.as_str())
            .cmp(&(right.as_of.as_str(), right.snapshot_id.as_str()))
    });
    for row in &selected {
        let basis = BasisStatus::parse(&row.basis_status).unwrap_or(BasisStatus::NotAvailable);
        if !basis.admits(required) {
            return Err(BindError::NotAdmitted {
                source_id: source_id.to_string(),
                snapshot_id: row.snapshot_id.clone(),
                basis: row.basis_status.clone(),
                required: required.as_str().to_string(),
            });
        }
    }
    let snapshots: Vec<BoundSnapshot> = selected
        .iter()
        .map(|row| BoundSnapshot {
            snapshot_id: row.snapshot_id.clone(),
            parser_version: row.parser_version.clone(),
            basis_status: row.basis_status.clone(),
            as_of: row.as_of.clone(),
        })
        .collect();
    let source_binding_digest = binding_digest(&snapshots);
    Ok(SnapshotBinding {
        source_id: source_id.to_string(),
        snapshots,
        source_binding_digest,
    })
}

/// sha256 over sorted canonical snapshot lines (§6.3).
pub fn binding_digest(snapshots: &[BoundSnapshot]) -> String {
    let mut lines: Vec<String> = snapshots
        .iter()
        .map(|snapshot| serde_json::to_string(snapshot).expect("a bound snapshot serializes"))
        .collect();
    lines.sort();
    lines.dedup();
    sha256(lines.join("\n").as_bytes())
}

/// Read every bound snapshot back from the store by sha256 (AC-M3-9 evidence:
/// the binding is usable offline only if the bytes are still there).
pub fn verify_bytes(store: &SourceStore, binding: &SnapshotBinding) -> Result<usize, BindError> {
    let mut read = 0;
    for snapshot in &binding.snapshots {
        store.read_verified(&snapshot.snapshot_id)?;
        read += 1;
    }
    Ok(read)
}
