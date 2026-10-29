//! Source store: content-addressed original bytes plus an append-only
//! manifest (design: `docs/tasks/issue-446-m3-delisted-industry.md` §9.4.1).
//!
//! The store is the authority for facts that a fresh-init deployment cannot
//! regenerate (historical `tdxhy.cfg`/`incon.dat` extractions, frozen exchange
//! responses and one-off archive imports). SQLite index tables are derived
//! from it, never the other way round: a blob plus its manifest line must be
//! durable before any index row may be written.
//!
//! Layout under the configured store root:
//!
//! ```text
//! <store>/blobs/<sha256>   content addressed, append-only, never overwritten
//! <store>/manifest.jsonl   append-only registration lines
//! <store>/tmp/             staging for the fsync-then-link sequence
//! ```
//!
//! Write order (§9.4.1, forced): write `tmp/` → `fsync(file)` → publish into
//! `blobs/` → `fsync(dir)` → append the manifest line → `fsync(manifest)`.
//! Publishing uses a hard link, so an existing blob is never replaced; the
//! temporary name is unlinked afterwards. A failure before the manifest
//! append leaves at most an orphan blob (allowed intermediate state); a
//! manifest line never exists without a readable, digest-matching blob.
//!
//! Scope rebuilds live in [`replay`]; the archive importer in [`import`].

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

pub mod import;
pub mod replay;

mod lock;

/// Subdirectory holding the content-addressed blobs.
pub const BLOBS_DIR: &str = "blobs";
/// Append-only manifest file name.
pub const MANIFEST_FILE: &str = "manifest.jsonl";
/// Staging subdirectory inside the store.
pub const TMP_DIR: &str = "tmp";
/// Advisory `flock` file: exclusive for writers, shared for manifest readers.
pub const LOCK_FILE: &str = ".lock";

/// Logical source id for the `base.zip` member extraction.
pub const TDXHY_SOURCE_ID: &str = "tdx.base.zip!tdxhy.cfg";
/// Logical source id for the nested `gbbq.zip!zhb.zip!incon.dat` extraction.
pub const INCON_SOURCE_ID: &str = "tdx.gbbq.zip!zhb.zip!incon.dat";
/// Parser version recorded for `tdxhy.cfg` bytes.
pub const TDXHY_PARSER_VERSION: &str = "tdxhy-v1";
/// Parser version recorded for `incon.dat` bytes.
pub const INCON_PARSER_VERSION: &str = "incon-v1";

static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

/// Idempotency key of one registration: content, logical source, origin.
pub type RegistrationKey = (String, String, String);

/// What the stored bytes are (§9.4.1 manifest table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    GenerationMember,
    HttpFetch,
    ArchiveImport,
    RepoEvidence,
}

impl StoreKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GenerationMember => "generation_member",
            Self::HttpFetch => "http_fetch",
            Self::ArchiveImport => "archive_import",
            Self::RepoEvidence => "repo_evidence",
        }
    }
}

/// One `manifest.jsonl` line: the registration facts for one blob.
///
/// `sha256`/`size_bytes` are computed by [`SourceStore::register`], so a caller
/// cannot register a name that disagrees with the bytes it hands over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestRecord {
    /// Content sha256 (hex); also the blob file name.
    pub sha256: String,
    /// Logical source, e.g. [`TDXHY_SOURCE_ID`] or an exchange source id.
    pub source_id: String,
    /// Provenance class.
    pub store_kind: StoreKind,
    /// Evidence origin: generation manifest sha, request fingerprint, archive
    /// batch id plus member path, or repository path.
    pub origin: String,
    /// When the bytes were first obtained (UTC, RFC 3339 seconds).
    pub acquired_at: String,
    /// Publisher `last_modified` when the source exposed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_last_modified: Option<String>,
    /// Industry evidence grade (`A`/`B`/`C`) when the source is industry data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_grade: Option<String>,
    /// Exchange source basis status when the source is an exchange response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis_status: Option<String>,
    /// Parser version the bytes must be replayed with; unknown → fail-closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parser_version: Option<String>,
    /// Size of the blob in bytes.
    pub size_bytes: u64,
    /// sha256 of the container the member was extracted from, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_sha256: Option<String>,
    /// One-off import batch id, when the bytes came from an archive import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_batch: Option<String>,
    /// HTTP `ETag` of a frozen exchange response (M3 PR-2). `publisher_last_modified`
    /// already carries `Last-Modified`; the ETag has no other home, and without
    /// it `replay --scope exchange` could not rebuild
    /// `exchange_source_snapshots.http_etag` (§9.4.2). Absent on records written
    /// before the field existed (additive, so an old manifest still reads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_etag: Option<String>,
}

impl ManifestRecord {
    /// Idempotency key: the same bytes from the same logical source and the
    /// same origin is one registration; a new origin adds a line (§9.4.4).
    pub fn key(&self) -> (&str, &str, &str) {
        (&self.sha256, &self.source_id, &self.origin)
    }
}

/// Caller-supplied registration facts; the content fields are filled in by
/// [`SourceStore::register`].
#[derive(Debug, Clone)]
pub struct Registration {
    pub source_id: String,
    pub store_kind: StoreKind,
    pub origin: String,
    pub acquired_at: String,
    pub publisher_last_modified: Option<String>,
    pub evidence_grade: Option<String>,
    pub basis_status: Option<String>,
    pub parser_version: Option<String>,
    pub container_sha256: Option<String>,
    pub import_batch: Option<String>,
    /// HTTP `ETag` of a frozen exchange response (see `ManifestRecord::http_etag`).
    pub http_etag: Option<String>,
}

impl Registration {
    pub fn new(
        source_id: impl Into<String>,
        store_kind: StoreKind,
        origin: impl Into<String>,
        acquired_at: impl Into<String>,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            store_kind,
            origin: origin.into(),
            acquired_at: acquired_at.into(),
            publisher_last_modified: None,
            evidence_grade: None,
            basis_status: None,
            parser_version: None,
            container_sha256: None,
            import_batch: None,
            http_etag: None,
        }
    }
}

/// Result of one registration attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterOutcome {
    pub sha256: String,
    /// The blob file was created by this call.
    pub written_blob: bool,
    /// A manifest line was appended by this call.
    pub appended_line: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("source_store_io: {0}")]
    Io(#[from] io::Error),
    #[error("source_store_invalid_sha256: {0:?}")]
    InvalidSha256(String),
    #[error("source_store_missing_blob: {sha256}")]
    MissingBlob { sha256: String },
    #[error("source_store_corrupt: {sha256} expected {expected} read back {actual}")]
    Corrupt {
        sha256: String,
        expected: String,
        actual: String,
    },
    #[error("source_store_manifest_invalid: {0}")]
    Manifest(String),
    #[error(
        "manifest_torn_tail: {path} line {line} has no trailing newline; \
         repair is an explicit owner action (no automatic truncation)"
    )]
    ManifestTornTail { path: String, line: usize },
    #[error("source_store_invalid_path: {0}")]
    InvalidPath(String),
}

impl StoreError {
    /// Stable reason code for the CLI/JSON surface.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "source_store_io",
            Self::InvalidSha256(_) => "source_store_invalid_sha256",
            Self::MissingBlob { .. } => "source_store_missing_blob",
            Self::Corrupt { .. } => "source_store_corrupt",
            Self::Manifest(_) => "source_store_manifest_invalid",
            Self::ManifestTornTail { .. } => "manifest_torn_tail",
            Self::InvalidPath(_) => "source_store_invalid_path",
        }
    }
}

/// Fault-injection points of the forced write order. Production code never
/// passes one; the unit tests use them to prove there is no manifest line
/// without its blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultPoint {
    FileFsync,
    Publish,
    DirFsync,
    ManifestAppend,
    ManifestFsync,
}

fn injected(point: FaultPoint) -> StoreError {
    StoreError::Io(io::Error::other(format!(
        "injected source-store fault at {point:?}"
    )))
}

/// A store rooted at a deployment-state directory. The root is supplied by
/// configuration/CLI; no path is baked into the code.
#[derive(Debug, Clone)]
pub struct SourceStore {
    root: PathBuf,
}

impl SourceStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join(BLOBS_DIR)
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join(TMP_DIR)
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    /// True when the store has bytes registered (an empty or missing manifest
    /// is the empty store, not an error).
    pub fn has_entries(&self) -> Result<bool, StoreError> {
        Ok(!self.records()?.is_empty())
    }

    /// Read every manifest line in append order under a shared lock. A
    /// missing manifest file is an empty store; a malformed line is a hard
    /// error with its line number. A final line without `\n` is a torn write
    /// (`manifest_torn_tail`), never an empty or truncated store: repairing it
    /// is an explicit owner action (§9.4.5).
    pub fn records(&self) -> Result<Vec<ManifestRecord>, StoreError> {
        let _lock = self.shared_lock()?;
        self.records_unlocked()
    }

    fn records_unlocked(&self) -> Result<Vec<ManifestRecord>, StoreError> {
        let path = self.manifest_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            let line = bytes.iter().filter(|byte| **byte == b'\n').count() + 1;
            return Err(StoreError::ManifestTornTail {
                path: path.display().to_string(),
                line,
            });
        }
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            StoreError::Manifest(format!("{}: not UTF-8: {error}", path.display()))
        })?;
        let mut records = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record: ManifestRecord = serde_json::from_str(line).map_err(|error| {
                StoreError::Manifest(format!("{}:{}: {error}", path.display(), index + 1))
            })?;
            validate_sha256(&record.sha256)?;
            records.push(record);
        }
        Ok(records)
    }

    /// Registration keys already present, so a repeated import can report
    /// `already_present` without writing anything.
    pub fn registered_keys(&self) -> Result<BTreeSet<RegistrationKey>, StoreError> {
        Ok(keys(&self.records()?))
    }

    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.blobs_dir().join(sha256)
    }

    pub fn blob_exists(&self, sha256: &str) -> bool {
        self.blob_path(sha256).is_file()
    }

    /// Register bytes: publish the blob (never overwriting) and append one
    /// manifest line. A duplicate `(sha256, source_id, origin)` is a no-op.
    pub fn register(
        &self,
        bytes: &[u8],
        facts: &Registration,
    ) -> Result<RegisterOutcome, StoreError> {
        self.register_inner(bytes, facts, None)
    }

    pub(crate) fn register_inner(
        &self,
        bytes: &[u8],
        facts: &Registration,
        fault: Option<FaultPoint>,
    ) -> Result<RegisterOutcome, StoreError> {
        // One exclusive lock covers dedupe, blob publish, manifest append and
        // both fsyncs, so concurrent writers cannot interleave a line or
        // register the same key twice.
        let _lock = self.exclusive_lock()?;
        let sha256 = crate::hash::sha256(bytes);
        let key = (
            sha256.clone(),
            facts.source_id.clone(),
            facts.origin.clone(),
        );
        if keys(&self.records_unlocked()?).contains(&key) {
            return Ok(RegisterOutcome {
                sha256,
                written_blob: false,
                appended_line: false,
            });
        }
        let written_blob = self.publish_blob(&sha256, bytes, fault)?;
        let record = ManifestRecord {
            sha256: sha256.clone(),
            source_id: facts.source_id.clone(),
            store_kind: facts.store_kind,
            origin: facts.origin.clone(),
            acquired_at: facts.acquired_at.clone(),
            publisher_last_modified: facts.publisher_last_modified.clone(),
            evidence_grade: facts.evidence_grade.clone(),
            basis_status: facts.basis_status.clone(),
            parser_version: facts.parser_version.clone(),
            size_bytes: bytes.len() as u64,
            container_sha256: facts.container_sha256.clone(),
            import_batch: facts.import_batch.clone(),
            http_etag: facts.http_etag.clone(),
        };
        self.append_manifest(&record, fault)?;
        Ok(RegisterOutcome {
            sha256,
            written_blob,
            appended_line: true,
        })
    }

    /// Read a blob back and verify its digest. A digest mismatch is
    /// `source_store_corrupt`, never a silent fallback (§9.4.5).
    pub fn read_verified(&self, sha256: &str) -> Result<Vec<u8>, StoreError> {
        validate_sha256(sha256)?;
        let path = self.blob_path(sha256);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::MissingBlob {
                    sha256: sha256.to_string(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        let actual = crate::hash::sha256(&bytes);
        if actual != sha256 {
            return Err(StoreError::Corrupt {
                sha256: sha256.to_string(),
                expected: sha256.to_string(),
                actual,
            });
        }
        Ok(bytes)
    }

    /// Shared lock for manifest reads; a missing store root is an empty store
    /// and must not be created by a read.
    fn shared_lock(&self) -> Result<Option<lock::StoreLock>, StoreError> {
        if !self.root.is_dir() {
            return Ok(None);
        }
        Ok(Some(lock::shared(&self.root)?))
    }

    fn exclusive_lock(&self) -> Result<lock::StoreLock, StoreError> {
        fs::create_dir_all(&self.root)?;
        Ok(lock::exclusive(&self.root)?)
    }

    fn publish_blob(
        &self,
        sha256: &str,
        bytes: &[u8],
        fault: Option<FaultPoint>,
    ) -> Result<bool, StoreError> {
        validate_sha256(sha256)?;
        let blobs = self.blobs_dir();
        let created_blobs = !blobs.is_dir();
        let created_tmp = !self.tmp_dir().is_dir();
        fs::create_dir_all(&blobs)?;
        let final_path = blobs.join(sha256);
        if final_path.exists() {
            // Never overwrite: an existing blob must already match its name.
            let existing = fs::read(&final_path)?;
            let actual = crate::hash::sha256(&existing);
            if actual != sha256 {
                return Err(StoreError::Corrupt {
                    sha256: sha256.to_string(),
                    expected: sha256.to_string(),
                    actual,
                });
            }
            return Ok(false);
        }
        let tmp = self.tmp_dir();
        fs::create_dir_all(&tmp)?;
        if created_blobs || created_tmp {
            // The new directory entries must be durable before any blob in
            // them is referenced by a manifest line.
            fsync_dir(&self.root)?;
        }
        let tmp_path = tmp.join(format!(
            "{sha256}.{}.{}",
            std::process::id(),
            NEXT_TMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = File::create(&tmp_path)?;
        if let Err(error) = (|| -> Result<(), StoreError> {
            file.write_all(bytes)?;
            if fault == Some(FaultPoint::FileFsync) {
                return Err(injected(FaultPoint::FileFsync));
            }
            file.sync_all()?;
            if fault == Some(FaultPoint::Publish) {
                return Err(injected(FaultPoint::Publish));
            }
            // A hard link publishes without replacing an existing name; the
            // staging name is unlinked right after.
            match fs::hard_link(&tmp_path, &final_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let existing = fs::read(&final_path)?;
                    let actual = crate::hash::sha256(&existing);
                    if actual != sha256 {
                        return Err(StoreError::Corrupt {
                            sha256: sha256.to_string(),
                            expected: sha256.to_string(),
                            actual,
                        });
                    }
                }
                Err(error) => return Err(error.into()),
            }
            fs::remove_file(&tmp_path)?;
            if fault == Some(FaultPoint::DirFsync) {
                return Err(injected(FaultPoint::DirFsync));
            }
            fsync_dir(&blobs)?;
            Ok(())
        })() {
            let _ = fs::remove_file(&tmp_path);
            return Err(error);
        }
        Ok(true)
    }

    fn append_manifest(
        &self,
        record: &ManifestRecord,
        fault: Option<FaultPoint>,
    ) -> Result<(), StoreError> {
        let mut line = serde_json::to_string(record)
            .map_err(|error| StoreError::Manifest(error.to_string()))?;
        line.push('\n');
        fs::create_dir_all(&self.root)?;
        let created = !self.manifest_path().exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.manifest_path())?;
        if fault == Some(FaultPoint::ManifestAppend) {
            return Err(injected(FaultPoint::ManifestAppend));
        }
        // One `write_all` under O_APPEND: a concurrent writer can never split
        // this line, and a torn tail is only possible on a crash.
        file.write_all(line.as_bytes())?;
        if fault == Some(FaultPoint::ManifestFsync) {
            return Err(injected(FaultPoint::ManifestFsync));
        }
        file.sync_all()?;
        if created {
            // The manifest's directory entry must be durable too, not only its
            // bytes; otherwise a crash can lose the whole file after success.
            fsync_dir(&self.root)?;
        }
        Ok(())
    }
}

/// Resolve an index-provided relative path under `base` without allowing an
/// absolute path or any `.`/`..` component to escape it.
pub(crate) fn safe_join(base: &Path, relative: &str) -> Result<PathBuf, StoreError> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(StoreError::InvalidPath(relative.to_string()));
    }
    let mut joined = base.to_path_buf();
    let mut components = 0;
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                joined.push(part);
                components += 1;
            }
            _ => return Err(StoreError::InvalidPath(relative.to_string())),
        }
    }
    if components == 0 {
        return Err(StoreError::InvalidPath(relative.to_string()));
    }
    Ok(joined)
}

/// sha256 blob names are lowercase hex; anything else is rejected before it
/// can become a path component.
pub fn validate_sha256(value: &str) -> Result<(), StoreError> {
    let bytes = value.as_bytes();
    if bytes.len() == 64
        && bytes
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Ok(());
    }
    Err(StoreError::InvalidSha256(value.to_string()))
}

/// Parser versions this binary can replay. An unknown version is fail-closed
/// (§9.4.1): the bytes stay registered but no rebuild may claim them.
pub fn known_parser_version(value: &str) -> bool {
    matches!(value, TDXHY_PARSER_VERSION | INCON_PARSER_VERSION)
        || crate::exchange_source::PagingFormat::from_parser_version(value).is_some()
}

fn keys(records: &[ManifestRecord]) -> BTreeSet<RegistrationKey> {
    records
        .iter()
        .map(|record| {
            (
                record.sha256.clone(),
                record.source_id.clone(),
                record.origin.clone(),
            )
        })
        .collect()
}

fn fsync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
