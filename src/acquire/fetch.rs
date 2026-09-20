//! Bounded fetch surface: the service shells out to the stdlib-only
//! `scripts/tdx_fetch.py` helper so the Rust side never holds a socket.
//! `file://` sources work the same way, which is how pinned snapshots and
//! tests flow through the identical byte-verified pipeline.
use serde::Deserialize;
use std::{path::Path, process::Command};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_length: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResult {
    pub head: Head,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("fetch failed: {0}")]
    Failed(String),
    #[error("fetch helper output invalid: {0}")]
    Output(String),
}

/// The fetch surface the service depends on; production impl shells to the
/// pinned helper, tests inject fixtures.
pub trait Fetcher: Send + Sync {
    fn head(&self, url: &str) -> Result<Head, Error>;
    fn fetch(&self, url: &str, dest: &Path) -> Result<FetchResult, Error>;
}

/// Runs `scripts/tdx_fetch.py` (or `X_DATA_MCP_TDX_FETCH` override).
pub struct Helper {
    script: std::path::PathBuf,
}
impl Helper {
    pub fn new() -> Self {
        let script = std::env::var_os("X_DATA_MCP_TDX_FETCH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts/tdx_fetch.py")
            });
        Self { script }
    }
    fn run(&self, args: &[&str]) -> Result<serde_json::Value, Error> {
        let output = Command::new("python3")
            .arg(&self.script)
            .args(args)
            .output()
            .map_err(|e| Error::Failed(e.to_string()))?;
        if !output.status.success() {
            return Err(Error::Failed(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|e| Error::Output(e.to_string()))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HeadJson {
    status: u16,
    etag: Option<String>,
    last_modified: Option<String>,
    content_length: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchJson {
    status: u16,
    etag: Option<String>,
    last_modified: Option<String>,
    content_length: Option<u64>,
    bytes: u64,
    sha256: String,
}
impl Fetcher for Helper {
    fn head(&self, url: &str) -> Result<Head, Error> {
        let h: HeadJson = serde_json::from_value(self.run(&["head", url])?)
            .map_err(|e| Error::Output(e.to_string()))?;
        Ok(Head {
            status: h.status,
            etag: h.etag,
            last_modified: h.last_modified,
            content_length: h.content_length,
        })
    }
    fn fetch(&self, url: &str, dest: &Path) -> Result<FetchResult, Error> {
        let f: FetchJson = serde_json::from_value(self.run(&[
            "fetch",
            url,
            &dest.to_string_lossy(),
        ])?)
        .map_err(|e| Error::Output(e.to_string()))?;
        Ok(FetchResult {
            head: Head {
                status: f.status,
                etag: f.etag,
                last_modified: f.last_modified,
                content_length: f.content_length,
            },
            bytes: f.bytes,
            sha256: f.sha256,
        })
    }
}
