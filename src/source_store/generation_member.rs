//! Retroactive registration of an installed generation's industry members
//! (`source_store register-generation-member`, XDM-150; design
//! `docs/tasks/issue-446-m3-delisted-industry.md` §8.3 / §9.4.1).
//!
//! The retirement gate accepts a generation's `tdxhy.cfg`/`incon.dat` only when
//! the manifest carries a line whose origin is exactly
//! `generation:<manifest sha>` and whose blob reads back byte for byte. A
//! generation that was acquired before the source store existed has the same
//! member bytes in the store under a different origin (`archive_import`), so it
//! can never pass — the gate checks provenance, not content equality
//! (`legacy-gen-retirement.md` §3.4). This module is the legitimate repair path:
//! it reads the members out of the generation directory, recomputes their
//! sha256 from the bytes, and registers them under the generation's origin.
//!
//! Deliberate boundaries:
//!
//! - **the member list is not re-implemented.** Extraction goes through the
//!   same adapter call the daily `derived:industry_snapshot` item uses
//!   (`TdxMarketSource::industry_facts`), and registration goes through
//!   `industry::register_generation_members_detailed`. A member this command
//!   stores is a member the daily path would have stored.
//! - **the generation identity is the manifest digest.** `--generation` must
//!   equal sha256 of `<generation-dir>/snapshot-manifest.json`; the digest of
//!   that file *is* the generation id (§`src/generation`). The directory name
//!   is not trusted for it.
//! - **only the archives it extracts are re-hashed.** `base.zip` and
//!   `gbbq.zip` (≈22 MB together) must match their manifest pins; the ~550 MB
//!   day archive is not read, because no day byte is registered here and the
//!   generation identity already covers the manifest that pins it.
//! - **append-only and idempotent.** Bytes go through
//!   [`SourceStore::register`]: the store's `flock`, its torn-tail check and
//!   its `(sha256, source_id, origin)` idempotency key apply unchanged, an
//!   existing blob is reused and verified rather than rewritten, a repeated
//!   run appends nothing and no existing line is ever modified or removed.
//! - **it cannot fake evidence.** The member digest is computed from the bytes
//!   that are registered, and the line's origin comes from the verified
//!   manifest digest; there is no parameter for either.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{SourceStore, StoreError};
use crate::generation::MANIFEST_NAME;
use crate::industry::{GenerationMembers, register_generation_members_detailed};
use crate::official_market::TdxMarketSource;
use crate::snapshot::{FileInput, FileRole, SnapshotManifest};

/// `--generation` does not name the manifest that sits in `--generation-dir`.
pub const MANIFEST_MISMATCH: &str = "generation_manifest_mismatch";
/// An archive's bytes no longer match the digest its manifest pins.
pub const CONTAINER_MISMATCH: &str = "generation_container_mismatch";
/// The generation carries no readable industry member: the gate would still
/// refuse it, so registering nothing is reported as a failure, not as `ok`.
pub const NO_INDUSTRY_MEMBER: &str = "generation_has_no_industry_member";
/// The directory/manifest/member could not be read.
pub const MEMBER_UNAVAILABLE: &str = "generation_member_unavailable";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("generation_dir_unreadable: {0}")]
    Unreadable(String),
    #[error(
        "{MANIFEST_MISMATCH}: --generation {expected} but {path} has sha256 {actual}; \
         the generation id is the digest of its manifest bytes"
    )]
    ManifestMismatch {
        expected: String,
        path: String,
        actual: String,
    },
    #[error("generation_manifest_invalid: {path}: {detail}")]
    ManifestInvalid { path: String, detail: String },
    #[error(
        "{CONTAINER_MISMATCH}: {role} {path} is {bytes} bytes / sha256 {actual}, \
         the manifest pins {expected_bytes} bytes / sha256 {expected}"
    )]
    ContainerMismatch {
        role: &'static str,
        path: String,
        expected: String,
        expected_bytes: u64,
        actual: String,
        bytes: u64,
    },
    #[error(
        "{NO_INDUSTRY_MEMBER}: manifest {generation} carries no base.zip!tdxhy.cfg, \
             so there is nothing to register and the retirement gate would still refuse it"
    )]
    NoIndustryMember { generation: String },
    #[error("{MEMBER_UNAVAILABLE}: {0}")]
    MemberUnavailable(String),
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Industry(#[from] crate::industry::Error),
}

impl Error {
    /// Stable reason code for the CLI JSON surface (`status: failed`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unreadable(_) => "generation_dir_unreadable",
            Self::ManifestMismatch { .. } => MANIFEST_MISMATCH,
            Self::ManifestInvalid { .. } => "generation_manifest_invalid",
            Self::ContainerMismatch { .. } => CONTAINER_MISMATCH,
            Self::NoIndustryMember { .. } => NO_INDUSTRY_MEMBER,
            Self::MemberUnavailable(_) => MEMBER_UNAVAILABLE,
            Self::Store(error) => error.code(),
            Self::Industry(error) => error.code(),
        }
    }
}

/// One member's outcome: what was registered and whether anything changed.
#[derive(Debug, Clone, Serialize)]
pub struct MemberReport {
    /// Gate role the member belongs to (`base.zip` / `gbbq.zip`).
    pub role: &'static str,
    pub source_id: &'static str,
    /// Member name inside its container(s), as the adapter names it.
    pub member: String,
    /// sha256 recomputed from the member bytes and read back from the store.
    pub sha256: String,
    pub size_bytes: u64,
    /// A manifest line was appended by this run; `false` = already registered
    /// under this generation (the idempotent case).
    pub added: bool,
    /// The blob was already in the store and was reused; no byte was written.
    pub blob_reused: bool,
}

impl MemberReport {
    fn from_registration(
        role: &'static str,
        registration: &crate::industry::GenerationMemberRegistration,
        size_bytes: u64,
    ) -> Self {
        Self {
            role,
            source_id: registration.source_id,
            member: registration.member.clone(),
            sha256: registration.sha256.clone(),
            size_bytes,
            added: registration.appended_line,
            blob_reused: !registration.written_blob,
        }
    }
}

/// One command invocation's JSON report; `status: ok` means both members are
/// registered under the generation's origin (either by this run or before it).
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub status: &'static str,
    pub generation: String,
    pub generation_dir: String,
    pub store: String,
    pub members: Vec<MemberReport>,
    /// The manifest line's origin (`generation:<manifest sha>`), i.e. exactly
    /// what the retirement gate looks for.
    pub origin: String,
}

/// Register one installed generation's `tdxhy.cfg` + `incon.dat` in the source
/// store under `generation:<generation>`.
///
/// The generation must still have its `base.zip`/`gbbq.zip` bytes on disk
/// (retiring deletes all three archives, so this must run *before* a retire).
pub fn register_generation_member(
    store: &SourceStore,
    generation: &str,
    generation_dir: &Path,
) -> Result<Report, Error> {
    let (root, manifest) = load_manifest(generation_dir, generation)?;
    // Verify only what is extracted below. Both digests become the manifest
    // lines' container evidence, so an unverified container would put a claim
    // into the store that the bytes do not support.
    for input in &manifest.files {
        if matches!(input.role, FileRole::BaseIndustry | FileRole::Gbbq) {
            verify_container(&root, input)?;
        }
    }
    let source = TdxMarketSource::from_generation(root.clone(), generation.to_string(), &manifest)
        .map_err(|error| Error::MemberUnavailable(error.to_string()))?;
    let facts = source
        .materialize_session()
        .industry_facts()
        .map_err(|error| Error::MemberUnavailable(error.to_string()))?
        .ok_or_else(|| Error::NoIndustryMember {
            generation: generation.to_string(),
        })?;
    let registered = register_generation_members_detailed(
        store,
        &GenerationMembers {
            manifest_sha256: &facts.manifest_sha256,
            tdxhy_member: &facts.tdxhy_member,
            tdxhy_bytes: &facts.tdxhy_bytes,
            tdxhy_publisher_last_modified: facts.tdxhy_publisher_last_modified.as_deref(),
            tdxhy_observed_at: &facts.tdxhy_observed_at,
            incon_member: &facts.incon_member,
            incon_bytes: &facts.incon_bytes,
            incon_publisher_last_modified: facts.incon_publisher_last_modified.as_deref(),
            incon_observed_at: &facts.incon_observed_at,
            base_zip_sha256: &facts.base_zip_sha256,
            gbbq_zip_sha256: &facts.gbbq_zip_sha256,
        },
    )?;
    Ok(Report {
        status: "ok",
        generation: generation.to_string(),
        generation_dir: root.display().to_string(),
        store: store.root().display().to_string(),
        members: vec![
            MemberReport::from_registration(
                crate::generation_gate::ROLE_BASE,
                &registered.tdxhy,
                facts.tdxhy_bytes.len() as u64,
            ),
            MemberReport::from_registration(
                crate::generation_gate::ROLE_GBBQ,
                &registered.incon,
                facts.incon_bytes.len() as u64,
            ),
        ],
        origin: format!("generation:{generation}"),
    })
}

/// Read the generation's manifest and check that `generation` is its digest.
fn load_manifest(dir: &Path, generation: &str) -> Result<(PathBuf, SnapshotManifest), Error> {
    let path = dir.join(MANIFEST_NAME);
    let bytes = fs::read(&path)
        .map_err(|error| Error::Unreadable(format!("{}: {error}", path.display())))?;
    let actual = crate::hash::sha256(&bytes);
    if actual != generation {
        return Err(Error::ManifestMismatch {
            expected: generation.to_string(),
            path: path.display().to_string(),
            actual,
        });
    }
    let manifest: SnapshotManifest =
        serde_json::from_slice(&bytes).map_err(|error| Error::ManifestInvalid {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
    let root = dir
        .canonicalize()
        .map_err(|error| Error::Unreadable(format!("{}: {error}", dir.display())))?;
    Ok((root, manifest))
}

/// Re-hash one archive inside the generation directory and compare it with the
/// manifest pin (`bytes` and `sha256`), refusing paths that escape the root.
fn verify_container(root: &Path, input: &FileInput) -> Result<(), Error> {
    let role = match input.role {
        FileRole::BaseIndustry => crate::generation_gate::ROLE_BASE,
        FileRole::Gbbq => crate::generation_gate::ROLE_GBBQ,
        FileRole::Day => crate::generation_gate::ROLE_DAY,
    };
    let joined = root.join(&input.relative_path);
    let path = joined
        .canonicalize()
        .map_err(|error| Error::Unreadable(format!("{role} {}: {error}", joined.display())))?;
    if !path.starts_with(root) {
        return Err(Error::Unreadable(format!(
            "{role} {} resolves outside {}",
            path.display(),
            root.display()
        )));
    }
    let file = fs::File::open(&path)
        .map_err(|error| Error::Unreadable(format!("{role} {}: {error}", path.display())))?;
    let (bytes, actual) = crate::hash::digest_reader(file)
        .map_err(|error| Error::Unreadable(format!("{role} {}: {error}", path.display())))?;
    if bytes != input.bytes || actual != input.sha256 {
        return Err(Error::ContainerMismatch {
            role,
            path: path.display().to_string(),
            expected: input.sha256.clone(),
            expected_bytes: input.bytes,
            actual,
            bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_store::{INCON_SOURCE_ID, TDXHY_SOURCE_ID};

    /// The source ids are the ones the gate looks for; a command that used a
    /// different spelling would register bytes nobody checks.
    #[test]
    fn reports_name_the_gate_source_ids() {
        let registration = crate::industry::GenerationMemberRegistration {
            member: "zhb.zip!incon.dat".into(),
            source_id: INCON_SOURCE_ID,
            sha256: "a".repeat(64),
            written_blob: true,
            appended_line: true,
        };
        let report = MemberReport::from_registration(
            crate::generation_gate::ROLE_GBBQ,
            &registration,
            67_543,
        );
        assert_eq!(report.source_id, INCON_SOURCE_ID);
        assert_eq!(report.role, "gbbq.zip");
        assert!(report.added);
        assert!(!report.blob_reused);
        let tdxhy = crate::industry::GenerationMemberRegistration {
            source_id: TDXHY_SOURCE_ID,
            written_blob: false,
            appended_line: true,
            ..registration
        };
        let reused =
            MemberReport::from_registration(crate::generation_gate::ROLE_BASE, &tdxhy, 150_566);
        assert_eq!(reused.source_id, TDXHY_SOURCE_ID);
        assert!(reused.added, "a new origin line is still new evidence");
        assert!(reused.blob_reused, "the existing blob was reused");
    }
}
