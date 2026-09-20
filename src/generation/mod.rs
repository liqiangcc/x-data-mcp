//! Immutable official-file generations: write-once directories, a
//! content-addressed identity, and an atomic active pointer.
//!
//! Layout under one private root:
//!
//! ```text
//! <root>/
//!   staging/<request_id>/        # in-flight download area (bounded, per request)
//!   generations/<generation_id>/ # sealed after install; bytes never modified
//!     base.zip  gbbq.zip  hsjday.zip  snapshot-manifest.json
//!     consistency-decision.json  # appended exactly once by verification
//!   active-generation.json       # atomic pointer swap
//!   acquire.sqlite               # durable acquisition store (see crate::acquire)
//! ```
//!
//! `generation_id` is the sha256 of the canonical snapshot manifest; the
//! manifest pins every member's sha256, so identity covers all bytes.
//! `consistency-decision.json` is created exactly once (`create_new`) and
//! carries its own recorded digest — later modification is detectable.
//! Nothing here performs network I/O.
use crate::snapshot;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const MANIFEST_NAME: &str = "snapshot-manifest.json";
pub const DECISION_NAME: &str = "consistency-decision.json";
pub const ACTIVE_NAME: &str = "active-generation.json";
const ARTIFACTS: [&str; 3] = ["base.zip", "gbbq.zip", "hsjday.zip"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivePointer {
    pub schema: String,
    pub generation_id: String,
    pub manifest_sha256: String,
    pub activated_at: DateTime<Utc>,
    pub previous_generation_id: Option<String>,
    pub activation_request_id: String,
}
impl ActivePointer {
    pub const SCHEMA: &'static str = "x-data-mcp.active-generation.v1";
}

/// A generation directory that passed byte verification and sealed install.
#[derive(Debug, Clone)]
pub struct Installed {
    pub generation_id: String,
    pub manifest_sha256: String,
    pub dir: PathBuf,
    pub manifest: snapshot::SnapshotManifest,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("manifest JSON invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("generation {0} is not installed")]
    NotInstalled(String),
    #[error("generation {0} directory exists but fails inspection: {1}")]
    Corrupt(String, String),
    #[error("staged request {0} has no canonical manifest")]
    UnverifiedStage(String),
    #[error("generation {0} already installed; refusing to overwrite immutable bytes")]
    AlreadyInstalled(String),
    #[error("consistency decision already recorded for generation {0}")]
    DecisionExists(String),
    #[error("active pointer conflict: expected {expected:?}, found {actual:?}")]
    ActivationConflict {
        expected: Option<String>,
        actual: Option<String>,
    },
    #[error("snapshot inspection failed: {0}")]
    NotVerified(String),
    #[error("staging area for request {0} is already sealed or missing")]
    Staging(String),
}

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn generations_dir(&self) -> PathBuf {
        self.root.join("generations")
    }
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }
    pub fn generation_dir(&self, generation_id: &str) -> PathBuf {
        self.generations_dir().join(generation_id)
    }
    pub fn stage_dir(&self, request_id: &str) -> PathBuf {
        self.staging_dir().join(request_id)
    }
    pub fn active_path(&self) -> PathBuf {
        self.root.join(ACTIVE_NAME)
    }
    pub fn acquire_db_path(&self) -> PathBuf {
        self.root.join("acquire.sqlite")
    }

    /// The staged request directory a downloader writes into. It is created
    /// empty; a second stage for the same request is refused so crash
    /// recovery can reason about exactly one staging area per request.
    pub fn create_stage(&self, request_id: &str) -> Result<PathBuf, Error> {
        let dir = self.stage_dir(request_id);
        if dir.exists() {
            return Err(Error::Staging(request_id.into()));
        }
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Remove a resolved staging area. Only acquire recovery calls this —
    /// after the task row records the outcome, never mid-flight.
    pub fn remove_stage(&self, request_id: &str) -> Result<(), Error> {
        let dir = self.stage_dir(request_id);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    /// Read the staged canonical manifest without installing it.
    pub fn staged_manifest(&self, request_id: &str) -> Result<(Vec<u8>, snapshot::SnapshotManifest), Error> {
        let path = self.stage_dir(request_id).join(MANIFEST_NAME);
        let bytes = fs::read(&path).map_err(|_| Error::UnverifiedStage(request_id.into()))?;
        Ok((bytes.clone(), serde_json::from_slice(&bytes)?))
    }

    /// Seal a staged request: byte-verify every artifact against the staged
    /// manifest, then atomically rename the whole directory into
    /// `generations/<id>/`. After this point the bytes are never modified.
    /// Returns `AlreadyInstalled` if the same identity was installed before.
    pub fn install_staged(&self, request_id: &str, now: DateTime<Utc>) -> Result<Installed, Error> {
        let (manifest_bytes, manifest) = self.staged_manifest(request_id)?;
        let generation_id = crate::hash::sha256(&manifest_bytes);
        let target = self.generation_dir(&generation_id);
        if target.exists() {
            return Err(Error::AlreadyInstalled(generation_id));
        }
        // Byte verification is part of install, not a separate advertised
        // state: a directory only exists once its contents match the
        // manifest that names it.
        let stage = self.stage_dir(request_id);
        let inspection = snapshot::inspect(&manifest_bytes, &stage, now)
            .map_err(|e| Error::NotVerified(e.to_string()))?;
        if inspection.production_ready {
            return Err(Error::NotVerified(
                "inspection must not approve production readiness".into(),
            ));
        }
        fs::create_dir_all(self.generations_dir())?;
        fs::rename(&stage, &target)?;
        fsync_dir(&self.generations_dir())?;
        Ok(Installed {
            generation_id,
            manifest_sha256: inspection.manifest_sha256,
            dir: target,
            manifest,
        })
    }

    /// Re-verify an installed generation's bytes against its manifest and
    /// directory identity (AC-G02): any modified artifact, manifest or
    /// tampered name fails; no read-repair ever happens here.
    pub fn verify_installed(&self, generation_id: &str, now: DateTime<Utc>) -> Result<Installed, Error> {
        let dir = self.generation_dir(generation_id);
        if !dir.is_dir() {
            return Err(Error::NotInstalled(generation_id.into()));
        }
        let manifest_bytes = fs::read(dir.join(MANIFEST_NAME))
            .map_err(|e| Error::Corrupt(generation_id.into(), e.to_string()))?;
        if crate::hash::sha256(&manifest_bytes) != generation_id {
            return Err(Error::Corrupt(
                generation_id.into(),
                "manifest digest does not match directory identity".into(),
            ));
        }
        let manifest: snapshot::SnapshotManifest = serde_json::from_slice(&manifest_bytes)?;
        let inspection = snapshot::inspect(&manifest_bytes, &dir, now)
            .map_err(|e| Error::Corrupt(generation_id.into(), e.to_string()))?;
        Ok(Installed {
            generation_id: generation_id.into(),
            manifest_sha256: inspection.manifest_sha256,
            dir,
            manifest,
        })
    }

    /// Append the verification decision exactly once. `create_new` makes a
    /// second write impossible; the decision digest is what callers persist
    /// elsewhere for tamper evidence.
    pub fn write_decision(&self, generation_id: &str, decision: &[u8]) -> Result<String, Error> {
        let dir = self.generation_dir(generation_id);
        if !dir.is_dir() {
            return Err(Error::NotInstalled(generation_id.into()));
        }
        let path = dir.join(DECISION_NAME);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::AlreadyExists {
                    Error::DecisionExists(generation_id.into())
                } else {
                    Error::Io(e)
                }
            })?;
        file.write_all(decision)?;
        file.sync_all()?;
        fsync_dir(&dir)?;
        Ok(crate::hash::sha256(decision))
    }

    /// Read the once-written decision, if present.
    pub fn decision(&self, generation_id: &str) -> Result<Option<Vec<u8>>, Error> {
        let path = self.generation_dir(generation_id).join(DECISION_NAME);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// The current active generation pointer, if any was ever activated.
    pub fn active(&self) -> Result<Option<ActivePointer>, Error> {
        match fs::read(self.active_path()) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// Atomic pointer swap under caller-held exclusion (the acquire store's
    /// single-writer transaction). `expected_previous` compares against the
    /// *generation id* of the current pointer (or None before first
    /// activation), making concurrent activations mechanically fail-closed.
    pub fn activate(
        &self,
        generation: &Installed,
        expected_previous: Option<&str>,
        request_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ActivePointer, Error> {
        let current = self.active()?;
        let actual = current.as_ref().map(|p| p.generation_id.as_str());
        if actual != expected_previous {
            return Err(Error::ActivationConflict {
                expected: expected_previous.map(str::to_string),
                actual: actual.map(str::to_string),
            });
        }
        let pointer = ActivePointer {
            schema: ActivePointer::SCHEMA.into(),
            generation_id: generation.generation_id.clone(),
            manifest_sha256: generation.manifest_sha256.clone(),
            activated_at: now,
            previous_generation_id: current.map(|p| p.generation_id),
            activation_request_id: request_id.into(),
        };
        let bytes = serde_json::to_vec_pretty(&pointer)?;
        let tmp = self.root.join(format!(".{ACTIVE_NAME}.tmp"));
        fs::create_dir_all(&self.root)?;
        fs::write(&tmp, &bytes)?;
        let f = fs::File::open(&tmp)?;
        f.sync_all()?;
        fs::rename(&tmp, self.active_path())?;
        fsync_dir(&self.root)?;
        Ok(pointer)
    }
}

fn fsync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

/// The artifact filenames every canonical manifest/dir must contain — kept
/// beside the store so callers never restate the role→file binding.
pub fn required_artifacts() -> &'static [&'static str] {
    &ARTIFACTS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{FileRole, SnapshotManifest};
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 20, 12, 0, 0).unwrap()
    }
    fn url(role: FileRole) -> &'static str {
        match role {
            FileRole::BaseIndustry => {
                "https://www.tdx.com.cn/products/data/data/dbf/base.zip"
            }
            FileRole::Gbbq => "https://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
            FileRole::Day => "https://data.tdx.com.cn/vipdoc/hsjday.zip",
        }
    }
    fn file_name(role: FileRole) -> &'static str {
        match role {
            FileRole::BaseIndustry => "base.zip",
            FileRole::Gbbq => "gbbq.zip",
            FileRole::Day => "hsjday.zip",
        }
    }
    /// Stage a canonical generation: three role artifacts plus a manifest
    /// whose pins match the written bytes. `salt` distinguishes generations.
    fn stage(store: &Store, request: &str, salt: u8) -> Vec<u8> {
        let dir = store.create_stage(request).unwrap();
        let mut files = vec![];
        for (i, role) in [FileRole::BaseIndustry, FileRole::Day, FileRole::Gbbq]
            .into_iter()
            .enumerate()
        {
            let bytes = vec![salt; 64 + i];
            fs::write(dir.join(file_name(role)), &bytes).unwrap();
            files.push(snapshot::FileInput {
                role,
                relative_path: file_name(role).into(),
                source_url: url(role).into(),
                acquisition_started_at: Utc.with_ymd_and_hms(2026, 9, 20, 10, 0, 0).unwrap(),
                acquisition_completed_at: Utc.with_ymd_and_hms(2026, 9, 20, 11, 0, 0).unwrap(),
                last_modified: Some("Sat, 19 Sep 2026 00:00:00 GMT".into()),
                etag: Some(format!("\"etag-{salt}\"")),
                bytes: bytes.len() as u64,
                sha256: crate::hash::sha256(&bytes),
            });
        }
        let manifest = serde_json::to_vec_pretty(&SnapshotManifest {
            schema: "x-data-mcp.official-file-input.v1".into(),
            files,
        })
        .unwrap();
        fs::write(dir.join(MANIFEST_NAME), &manifest).unwrap();
        manifest
    }

    #[test]
    fn install_seals_bytes_and_active_pointer_swaps_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        let manifest = stage(&store, "req-a", 1);
        let generation_id = crate::hash::sha256(&manifest);
        let installed = store.install_staged("req-a", now()).unwrap();
        assert_eq!(installed.generation_id, generation_id);
        assert!(store.generation_dir(&generation_id).is_dir());
        assert!(!store.stage_dir("req-a").exists());
        // Immutable: a second install of the same identity is refused.
        stage(&store, "req-a2", 1);
        assert!(matches!(
            store.install_staged("req-a2", now()),
            Err(Error::AlreadyInstalled(_))
        ));
        // Tampered artifact fails re-verification (AC-G02).
        let tampered = store.generation_dir(&generation_id).join("base.zip");
        fs::write(&tampered, b"tampered").unwrap();
        assert!(matches!(
            store.verify_installed(&generation_id, now()),
            Err(Error::Corrupt(..))
        ));
        fs::remove_dir_all(store.generation_dir(&generation_id)).unwrap();
        // Restore and activate: expected_previous binds the swap.
        stage(&store, "req-b", 1);
        let installed = store.install_staged("req-b", now()).unwrap();
        assert!(matches!(
            store.activate(&installed, Some("other"), "act-1", now()),
            Err(Error::ActivationConflict { .. })
        ));
        let pointer = store
            .activate(&installed, None, "act-1", now())
            .unwrap();
        assert_eq!(pointer.generation_id, generation_id);
        assert_eq!(pointer.previous_generation_id, None);
        assert_eq!(store.active().unwrap().unwrap(), pointer);
        // Second activation requires the current id as expected_previous.
        stage(&store, "req-c", 2);
        let newer = store.install_staged("req-c", now()).unwrap();
        assert!(matches!(
            store.activate(&newer, None, "act-2", now()),
            Err(Error::ActivationConflict { .. })
        ));
        let next = store
            .activate(&newer, Some(&generation_id), "act-2", now())
            .unwrap();
        assert_eq!(next.previous_generation_id, Some(generation_id.clone()));
        assert_eq!(store.active().unwrap().unwrap().generation_id, newer.generation_id);
    }

    #[test]
    fn decision_writes_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        stage(&store, "req-d", 3);
        let installed = store.install_staged("req-d", now()).unwrap();
        let digest = store
            .write_decision(&installed.generation_id, b"{\"status\":\"ok\"}")
            .unwrap();
        assert_eq!(digest.len(), 64);
        assert!(matches!(
            store.write_decision(&installed.generation_id, b"{}"),
            Err(Error::DecisionExists(_))
        ));
        assert_eq!(
            store.decision(&installed.generation_id).unwrap().unwrap(),
            b"{\"status\":\"ok\"}"
        );
    }

    #[test]
    fn unverified_or_missing_stage_cannot_install() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        assert!(matches!(
            store.install_staged("missing", now()),
            Err(Error::UnverifiedStage(_))
        ));
        // Manifest without the byte-verified artifacts is refused.
        let dir = store.create_stage("req-e").unwrap();
        fs::write(
            dir.join(MANIFEST_NAME),
            br#"{"schema":"x-data-mcp.official-file-input.v1","files":[]}"#,
        )
        .unwrap();
        assert!(matches!(
            store.install_staged("req-e", now()),
            Err(Error::NotVerified(_))
        ));
    }
}
