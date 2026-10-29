//! One-off import of A/B-tier archive extractions into the source store
//! (design §9.4.4).
//!
//! Input is a JSONL archive index plus the directory holding the archive
//! blobs. Only entries whose containers, members and parser pass every check
//! are registered: container sha256, member sha256, A-tier manifest match and
//! full parse of the extracted member. Anything else lands in
//! `skipped_unverified` with a reason; nothing is silently dropped and no
//! partial member is stored.
//!
//! Imported members are the extraction, never the container: `base.zip` →
//! `tdxhy.cfg`, `gbbq.zip` → `zhb.zip!incon.dat` (§9.4.1 存货范围). The
//! container sha256 stays on the manifest line as evidence. Idempotency key is
//! `(sha256, source_id, origin)`, so a repeated run reports `already_present`
//! and writes no blob or line; the same bytes from a different origin append
//! one evidence line (§9.4.4).
//!
//! `--dry-run` performs every check and writes nothing.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{
    INCON_PARSER_VERSION, INCON_SOURCE_ID, Registration, SourceStore, StoreError, StoreKind,
    TDXHY_PARSER_VERSION, TDXHY_SOURCE_ID, safe_join,
};

/// Bounded read for a single extracted member (`tdxhy.cfg` ~150 KB).
pub const MEMBER_LIMIT: u64 = 8 * 1024 * 1024;
/// Bounded read for an intermediate container (`zhb.zip`).
pub const NESTED_LIMIT: u64 = 64 * 1024 * 1024;

fn default_tier() -> String {
    "A".to_string()
}

fn default_kind() -> String {
    "industry".to_string()
}

/// One archive-index line. Unknown extra columns are ignored so the owner's
/// index can carry additional operator fields; `source_path`/`sha256`/
/// `tdxhy_sha256` are required.
#[derive(Debug, Clone, Deserialize)]
pub struct ArchiveIndexEntry {
    pub source_path: String,
    pub sha256: String,
    pub tdxhy_sha256: String,
    #[serde(default)]
    pub manifest_sha256: Option<String>,
    #[serde(default)]
    pub manifest_match: Option<bool>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub acquisition_completed_at: Option<String>,
    #[serde(default = "default_tier")]
    pub tier: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub batch: Option<String>,
    #[serde(default)]
    pub parser_version: Option<String>,
    /// Optional `gbbq.zip` container for `zhb.zip!incon.dat`.
    #[serde(default)]
    pub gbbq_source_path: Option<String>,
    #[serde(default)]
    pub gbbq_sha256: Option<String>,
    #[serde(default)]
    pub gbbq_last_modified: Option<String>,
    #[serde(default)]
    pub incon_sha256: Option<String>,
}

/// Operator choices; the defaults are the design's A/B industry import.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub tiers: Vec<String>,
    pub kind: Option<String>,
    pub dry_run: bool,
    pub batch: Option<String>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            tiers: vec!["A".to_string(), "B".to_string()],
            kind: None,
            dry_run: false,
            batch: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportedMember {
    pub member: &'static str,
    pub source_id: &'static str,
    pub sha256: String,
    pub origin: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedMember {
    pub index_line: usize,
    pub source_path: String,
    pub member: &'static str,
    pub reason: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub status: &'static str,
    pub store: String,
    pub index: String,
    pub blobs: String,
    pub batches: Vec<String>,
    pub tiers: Vec<String>,
    pub kind: String,
    pub dry_run: bool,
    pub index_lines: usize,
    pub imported: Vec<ImportedMember>,
    pub already_present: Vec<ImportedMember>,
    pub skipped_unverified: Vec<SkippedMember>,
    pub blobs_added: usize,
    pub manifest_lines_added: usize,
}

struct Skip {
    reason: &'static str,
    detail: String,
}

fn skip(reason: &'static str, detail: impl Into<String>) -> Skip {
    Skip {
        reason,
        detail: detail.into(),
    }
}

enum MemberOutcome {
    Imported {
        member: ImportedMember,
        written_blob: bool,
    },
    AlreadyPresent(ImportedMember),
    Skipped(Skip),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberKind {
    Tdxhy,
    Incon,
}

impl MemberKind {
    fn source_id(self) -> &'static str {
        match self {
            Self::Tdxhy => TDXHY_SOURCE_ID,
            Self::Incon => INCON_SOURCE_ID,
        }
    }

    fn parser_version(self) -> &'static str {
        match self {
            Self::Tdxhy => TDXHY_PARSER_VERSION,
            Self::Incon => INCON_PARSER_VERSION,
        }
    }

    /// Human/machine label used in skip reports.
    fn label(self) -> &'static str {
        match self {
            Self::Tdxhy => "base.zip!tdxhy.cfg",
            Self::Incon => "gbbq.zip!zhb.zip!incon.dat",
        }
    }

    /// Member suffix inside the origin string ("batch:container!<suffix>").
    fn origin_suffix(self) -> &'static str {
        match self {
            Self::Tdxhy => "tdxhy.cfg",
            Self::Incon => "zhb.zip!incon.dat",
        }
    }

    fn extract(self, container: &Path) -> Result<Vec<u8>, Skip> {
        match self {
            Self::Tdxhy => {
                let file = File::open(container).map_err(|error| {
                    skip(
                        "container_unreadable",
                        format!("{}: {error}", container.display()),
                    )
                })?;
                let mut zip = zip::ZipArchive::new(BufReader::new(file))
                    .map_err(|error| skip("container_unreadable", error.to_string()))?;
                read_member(&mut zip, "tdxhy.cfg", MEMBER_LIMIT)
            }
            Self::Incon => {
                let file = File::open(container).map_err(|error| {
                    skip(
                        "container_unreadable",
                        format!("{}: {error}", container.display()),
                    )
                })?;
                let mut zip = zip::ZipArchive::new(BufReader::new(file))
                    .map_err(|error| skip("container_unreadable", error.to_string()))?;
                let zhb = read_member(&mut zip, "zhb.zip", NESTED_LIMIT)?;
                let mut nested = zip::ZipArchive::new(Cursor::new(zhb))
                    .map_err(|error| skip("container_unreadable", error.to_string()))?;
                read_member(&mut nested, "incon.dat", MEMBER_LIMIT)
            }
        }
    }

    fn parse(self, bytes: &[u8]) -> Result<(), Skip> {
        match self {
            Self::Tdxhy => crate::catalog_tdx::tdxhy::parse(bytes)
                .map(|_| ())
                .map_err(|error| skip("parse_failed", error.to_string())),
            Self::Incon => crate::catalog_tdx::incon::parse(bytes)
                .map(|_| ())
                .map_err(|error| skip("parse_failed", error.to_string())),
        }
    }
}

fn read_member<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>, Skip> {
    let entry = zip
        .by_name(name)
        .map_err(|error| skip("member_missing", format!("{name}: {error}")))?;
    if entry.size() > limit {
        return Err(skip(
            "member_too_large",
            format!("{name}: {} bytes exceeds {limit}", entry.size()),
        ));
    }
    let mut bytes = Vec::new();
    entry
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| skip("member_unreadable", format!("{name}: {error}")))?;
    Ok(bytes)
}

/// Import one archive index into `store`. Returns a machine-readable report;
/// index syntax errors are hard errors, verification failures are per-entry
/// skips.
pub fn import(
    store: &SourceStore,
    index_path: &Path,
    blobs_dir: &Path,
    options: &ImportOptions,
) -> Result<ImportReport, StoreError> {
    let text = std::fs::read_to_string(index_path)?;
    let mut report = ImportReport {
        status: "ok",
        store: store.root().display().to_string(),
        index: index_path.display().to_string(),
        blobs: blobs_dir.display().to_string(),
        batches: Vec::new(),
        tiers: options.tiers.clone(),
        kind: options.kind.clone().unwrap_or_else(|| "any".to_string()),
        dry_run: options.dry_run,
        index_lines: 0,
        imported: Vec::new(),
        already_present: Vec::new(),
        skipped_unverified: Vec::new(),
        blobs_added: 0,
        manifest_lines_added: 0,
    };
    let mut existing = store.registered_keys()?;
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        report.index_lines += 1;
        let entry: ArchiveIndexEntry = serde_json::from_str(line).map_err(|error| {
            StoreError::Manifest(format!(
                "{}:{number}: invalid archive index entry: {error}",
                index_path.display()
            ))
        })?;
        let requested_kind = options
            .kind
            .as_deref()
            .filter(|kind| *kind != entry.kind.as_str());
        if let Some(kind) = requested_kind {
            report.skipped_unverified.push(SkippedMember {
                index_line: number,
                source_path: entry.source_path.clone(),
                member: "archive",
                reason: "kind_not_requested",
                detail: format!("entry kind {} requested {kind}", entry.kind),
            });
            continue;
        }
        if !options.tiers.iter().any(|tier| tier == &entry.tier)
            || !matches!(entry.tier.as_str(), "A" | "B")
        {
            report.skipped_unverified.push(SkippedMember {
                index_line: number,
                source_path: entry.source_path.clone(),
                member: "archive",
                reason: "tier_not_requested",
                detail: format!(
                    "entry tier {} requested {}",
                    entry.tier,
                    options.tiers.join(",")
                ),
            });
            continue;
        }
        let Some(acquired_at) = entry
            .acquisition_completed_at
            .clone()
            .filter(|value| !value.is_empty())
        else {
            report.skipped_unverified.push(SkippedMember {
                index_line: number,
                source_path: entry.source_path.clone(),
                member: "archive",
                reason: "missing_acquisition_time",
                detail: "acquisition_completed_at is required".to_string(),
            });
            continue;
        };
        let declared_parser = entry
            .parser_version
            .clone()
            .unwrap_or_else(|| TDXHY_PARSER_VERSION.to_string());
        if declared_parser != TDXHY_PARSER_VERSION {
            report.skipped_unverified.push(SkippedMember {
                index_line: number,
                source_path: entry.source_path.clone(),
                member: MemberKind::Tdxhy.label(),
                reason: "unknown_parser",
                detail: format!("declared parser_version {declared_parser}"),
            });
            continue;
        }
        // A-tier qualification is the base container's manifest binding; it
        // gates every member extracted from that batch line.
        if entry.tier == "A" && !manifest_matches(&entry) {
            report.skipped_unverified.push(SkippedMember {
                index_line: number,
                source_path: entry.source_path.clone(),
                member: MemberKind::Tdxhy.label(),
                reason: "manifest_mismatch",
                detail: format!(
                    "manifest_match={:?} manifest_sha256={:?}",
                    entry.manifest_match, entry.manifest_sha256
                ),
            });
            continue;
        }
        let batch = entry
            .batch
            .clone()
            .or_else(|| options.batch.clone())
            .unwrap_or_else(|| "archive-import".to_string());
        if !report.batches.contains(&batch) {
            report.batches.push(batch.clone());
        }
        let members = [
            (
                MemberKind::Tdxhy,
                Some(entry.source_path.as_str()),
                Some(entry.sha256.as_str()),
                Some(entry.tdxhy_sha256.as_str()),
                entry.last_modified.clone(),
            ),
            (
                MemberKind::Incon,
                entry.gbbq_source_path.as_deref(),
                entry.gbbq_sha256.as_deref(),
                entry.incon_sha256.as_deref(),
                entry.gbbq_last_modified.clone(),
            ),
        ];
        for (kind, container, container_sha, member_sha, last_modified) in members {
            let (Some(container), Some(container_sha), Some(member_sha)) =
                (container, container_sha, member_sha)
            else {
                if kind == MemberKind::Incon && entry.gbbq_source_path.is_some() {
                    report.skipped_unverified.push(SkippedMember {
                        index_line: number,
                        source_path: entry.source_path.clone(),
                        member: kind.label(),
                        reason: "index_incomplete",
                        detail: "gbbq entry needs gbbq_sha256 and incon_sha256".to_string(),
                    });
                }
                continue;
            };
            let facts = ImportFacts {
                store,
                blobs_dir,
                batch: batch.as_str(),
                acquired_at: acquired_at.as_str(),
                publisher_last_modified: last_modified,
                tier: entry.tier.as_str(),
                dry_run: options.dry_run,
            };
            match import_member(
                &facts,
                &mut existing,
                kind,
                container,
                container_sha,
                member_sha,
            ) {
                Ok(MemberOutcome::Imported {
                    member,
                    written_blob,
                }) => {
                    if written_blob {
                        report.blobs_added += 1;
                    }
                    report.manifest_lines_added += 1;
                    report.imported.push(member);
                }
                Ok(MemberOutcome::AlreadyPresent(member)) => {
                    report.already_present.push(member);
                }
                Ok(MemberOutcome::Skipped(reason)) => {
                    report.skipped_unverified.push(SkippedMember {
                        index_line: number,
                        source_path: container.to_string(),
                        member: kind.label(),
                        reason: reason.reason,
                        detail: reason.detail,
                    });
                }
                Err(error) => return Err(error),
            }
        }
    }
    Ok(report)
}

struct ImportFacts<'a> {
    store: &'a SourceStore,
    blobs_dir: &'a Path,
    batch: &'a str,
    acquired_at: &'a str,
    publisher_last_modified: Option<String>,
    tier: &'a str,
    dry_run: bool,
}

#[allow(clippy::too_many_arguments)]
fn import_member(
    facts: &ImportFacts<'_>,
    existing: &mut BTreeSet<super::RegistrationKey>,
    kind: MemberKind,
    container_relative: &str,
    container_sha: &str,
    member_sha: &str,
) -> Result<MemberOutcome, StoreError> {
    let container = match safe_join(facts.blobs_dir, container_relative) {
        Ok(path) => path,
        Err(error) => {
            return Ok(MemberOutcome::Skipped(skip(
                "invalid_path",
                error.to_string(),
            )));
        }
    };
    let actual_container_sha = match crate::hash::digest_file(&container) {
        Ok((_, sha)) => sha,
        Err(error) => {
            return Ok(MemberOutcome::Skipped(skip(
                "container_unreadable",
                format!("{}: {error}", container.display()),
            )));
        }
    };
    if !same_sha(container_sha, &actual_container_sha) {
        return Ok(MemberOutcome::Skipped(skip(
            "sha_mismatch",
            format!(
                "container {container_relative} expected {container_sha} read back {actual_container_sha}"
            ),
        )));
    }
    let bytes = match kind.extract(&container) {
        Ok(bytes) => bytes,
        Err(reason) => return Ok(MemberOutcome::Skipped(reason)),
    };
    let actual_member_sha = crate::hash::sha256(&bytes);
    if !same_sha(member_sha, &actual_member_sha) {
        return Ok(MemberOutcome::Skipped(skip(
            "sha_mismatch",
            format!(
                "member {} expected {member_sha} read back {actual_member_sha}",
                kind.label()
            ),
        )));
    }
    if let Err(reason) = kind.parse(&bytes) {
        return Ok(MemberOutcome::Skipped(reason));
    }
    let origin = format!(
        "{}:{container_relative}!{}",
        facts.batch,
        kind.origin_suffix()
    );
    let member = ImportedMember {
        member: kind.label(),
        source_id: kind.source_id(),
        sha256: actual_member_sha.clone(),
        origin: origin.clone(),
    };
    let key = (
        actual_member_sha.clone(),
        kind.source_id().to_string(),
        origin.clone(),
    );
    if existing.contains(&key) {
        return Ok(MemberOutcome::AlreadyPresent(member));
    }
    let registration = Registration {
        source_id: kind.source_id().to_string(),
        store_kind: StoreKind::ArchiveImport,
        origin,
        acquired_at: facts.acquired_at.to_string(),
        publisher_last_modified: facts.publisher_last_modified.clone(),
        evidence_grade: Some(facts.tier.to_string()),
        basis_status: None,
        parser_version: Some(kind.parser_version().to_string()),
        container_sha256: Some(actual_container_sha),
        import_batch: Some(facts.batch.to_string()),
        // §5.2: a backfilled interval's `known_at` is when the import brought
        // the bytes into this system, not the archive's own acquisition time
        // (claiming the latter would claim knowledge we did not have).
        imported_at: Some(crate::industry::now()),
    };
    if facts.dry_run {
        let written_blob = !facts.store.blob_exists(&actual_member_sha);
        return Ok(MemberOutcome::Imported {
            member,
            written_blob,
        });
    }
    let outcome = facts.store.register(&bytes, &registration)?;
    existing.insert(key);
    if outcome.appended_line {
        Ok(MemberOutcome::Imported {
            member,
            written_blob: outcome.written_blob,
        })
    } else {
        Ok(MemberOutcome::AlreadyPresent(member))
    }
}

fn manifest_matches(entry: &ArchiveIndexEntry) -> bool {
    entry.manifest_match == Some(true)
        && entry
            .manifest_sha256
            .as_deref()
            .is_some_and(|value| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn same_sha(expected: &str, actual: &str) -> bool {
    expected.trim().eq_ignore_ascii_case(actual)
}
