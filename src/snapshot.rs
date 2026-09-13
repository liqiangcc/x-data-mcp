//! Local byte verification is NOT official semantics, authorization or consistency approval.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    BaseIndustry,
    Day,
    Gbbq,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInput {
    pub role: FileRole,
    pub relative_path: PathBuf,
    pub source_url: String,
    pub acquisition_started_at: DateTime<Utc>,
    pub acquisition_completed_at: DateTime<Utc>,
    pub last_modified: Option<String>,
    pub etag: Option<String>,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifest {
    pub schema: String,
    pub files: Vec<FileInput>,
}
#[derive(Debug, Serialize)]
pub struct Inspection {
    pub schema: &'static str,
    pub manifest_sha256: String,
    pub files: Vec<FileInput>,
    pub byte_integrity: &'static str,
    pub declared_origin: &'static str,
    pub archive_member_binding: &'static str,
    pub file_set_consistency: &'static str,
    pub business_semantics: &'static str,
    pub access_use: &'static str,
    pub production_ready: bool,
    pub live_source: &'static str,
}
#[derive(Debug, thiserror::Error)]
pub enum InspectionError {
    #[error("invalid manifest: {0}")]
    Invalid(&'static str),
    #[error("artifact bytes do not match manifest: {0:?}")]
    Mismatch(FileRole),
    #[error("local input unavailable: {0}")]
    Io(#[from] io::Error),
    #[error("manifest JSON invalid: {0}")]
    Json(#[from] serde_json::Error),
}
/// No files are created or changed. The input directory must be trusted/private;
/// this is not a sandbox against an adversary concurrently replacing directories.
pub fn inspect(
    manifest_bytes: &[u8],
    root: &Path,
    now: DateTime<Utc>,
) -> Result<Inspection, InspectionError> {
    if manifest_bytes.len() > 1024 * 1024 {
        return Err(InspectionError::Invalid("manifest exceeds 1 MiB"));
    }
    let mut manifest: SnapshotManifest = serde_json::from_slice(manifest_bytes)?;
    if manifest.schema != "x-data-mcp.official-file-input.v1" || manifest.files.len() != 3 {
        return Err(InspectionError::Invalid("schema or required role count"));
    }
    let mut roles = BTreeSet::new();
    let mut paths = BTreeSet::new();
    // Check the complete contract before touching artifact paths.
    for input in &manifest.files {
        if !roles.insert(input.role) || !paths.insert(input.relative_path.clone()) {
            return Err(InspectionError::Invalid("duplicate role/path"));
        }
        if input.relative_path.as_os_str().is_empty()
            || input
                .relative_path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(InspectionError::Invalid(
                "artifact path must be relative without parent/current components",
            ));
        }
        if !declared_official_url(input.role, &input.source_url) {
            return Err(InspectionError::Invalid(
                "expected role-specific declared TDX http(s) URL",
            ));
        }
        if input.acquisition_started_at > input.acquisition_completed_at
            || input.acquisition_completed_at > now
        {
            return Err(InspectionError::Invalid(
                "inverted/future acquisition interval",
            ));
        }
        if input.bytes == 0
            || input.sha256.len() != 64
            || !input
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(InspectionError::Invalid(
                "empty file or noncanonical sha256",
            ));
        }
    }
    let root = root.canonicalize()?;
    for input in &manifest.files {
        let path = root.join(&input.relative_path).canonicalize()?;
        if !path.starts_with(&root) {
            return Err(InspectionError::Invalid(
                "artifact resolves outside input root",
            ));
        }
        if !path.metadata()?.is_file() {
            return Err(InspectionError::Invalid("artifact is not a regular file"));
        }
        let mut file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(InspectionError::Invalid("artifact is not a regular file"));
        }
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes = bytes
                .checked_add(n as u64)
                .ok_or(InspectionError::Invalid("file size overflow"))?;
            hash.update(&buffer[..n]);
        }
        let digest: String = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if bytes != input.bytes || digest != input.sha256 {
            return Err(InspectionError::Mismatch(input.role));
        }
    }
    manifest.files.sort_by_key(|input| input.role);
    Ok(Inspection {
        schema: "x-data-mcp.official-file-inspection.v1",
        manifest_sha256: crate::day_file::sha256(manifest_bytes),
        files: manifest.files,
        byte_integrity: "FILE_BYTES_VERIFIED",
        declared_origin: "NOT_AUTHENTICATED",
        archive_member_binding: "NOT_VERIFIED",
        file_set_consistency: "NOT_VERIFIED",
        business_semantics: "NOT_VERIFIED",
        access_use: "NOT_VERIFIED",
        production_ready: false,
        live_source: "NOT_RUN",
    })
}
fn declared_official_url(role: FileRole, url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    let expected_path = match role {
        FileRole::BaseIndustry => "products/data/data/dbf/base.zip",
        FileRole::Gbbq => "products/data/data/dbf/gbbq.zip",
        FileRole::Day => "vipdoc/hsjday.zip",
    };
    let host_allowed = match role {
        FileRole::Day => host == "data.tdx.com.cn",
        FileRole::BaseIndustry | FileRole::Gbbq => host == "tdx.com.cn" || host == "www.tdx.com.cn",
    };
    !path.is_empty()
        && !url.chars().any(char::is_whitespace)
        && !host.contains(['@', ':', '?', '#'])
        && host_allowed
        && path == expected_path
}
