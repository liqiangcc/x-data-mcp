//! Bounded fetch surface: the service shells out to the stdlib-only
//! `scripts/tdx_fetch.py` helper so the Rust side never holds a socket.
//! `file://` sources work the same way, which is how pinned snapshots and
//! tests flow through the identical byte-verified pipeline.
use serde::Deserialize;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

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
    #[error("fetch helper not found: {0}")]
    NotFound(String),
}

/// The fetch surface the service depends on; production impl shells to the
/// pinned helper, tests inject fixtures.
pub trait Fetcher: Send + Sync {
    fn head(&self, url: &str) -> Result<Head, Error>;
    fn fetch(&self, url: &str, dest: &Path) -> Result<FetchResult, Error>;
}

/// Environment variable naming the helper script. Production units set it;
/// when set it wins outright and its value is used as-is.
pub const TDX_FETCH_ENV: &str = "X_DATA_MCP_TDX_FETCH";
/// Release-package layout (`scripts/build_release.sh`): binaries land in
/// `<package>/bin/`, the helper in `<package>/scripts/tdx_fetch.py`.
const PACKAGED_HELPER: &str = "../scripts/tdx_fetch.py";

/// Runs the pinned `tdx_fetch.py` helper: `X_DATA_MCP_TDX_FETCH` when set,
/// otherwise the packaged copy next to the executable.
pub struct Helper {
    script: PathBuf,
}
impl Helper {
    pub fn new() -> Result<Self, Error> {
        let exe = std::env::current_exe().ok();
        let script = Self::resolve(std::env::var_os(TDX_FETCH_ENV), exe.as_deref())?;
        Ok(Self { script })
    }
    /// Resolve the helper path without touching the environment, so the
    /// rules are directly testable. There is deliberately no compile-time
    /// fallback: a packaged binary must never reach back into the build
    /// machine's source tree, so a miss fails closed here instead of
    /// surfacing as an unexplained download failure later (#375).
    fn resolve(explicit: Option<OsString>, exe: Option<&Path>) -> Result<PathBuf, Error> {
        if let Some(path) = explicit {
            return Ok(PathBuf::from(path));
        }
        let mut tried = Vec::new();
        if let Some(dir) = exe.and_then(Path::parent) {
            let path = dir.join(PACKAGED_HELPER);
            if path.is_file() {
                return Ok(path);
            }
            tried.push(path.display().to_string());
        }
        let tried = if tried.is_empty() {
            "no path (executable path unavailable)".to_string()
        } else {
            tried.join(", ")
        };
        Err(Error::NotFound(format!(
            "{TDX_FETCH_ENV} is not set and the packaged helper is missing; tried {tried} \
             (release layout: <exe dir>/{PACKAGED_HELPER}). Set {TDX_FETCH_ENV} to the \
             helper script path."
        )))
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
        serde_json::from_slice(&output.stdout).map_err(|e| Error::Output(e.to_string()))
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
        let f: FetchJson =
            serde_json::from_value(self.run(&["fetch", url, &dest.to_string_lossy()])?)
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

/// Local mirror fetcher: maps a declared role URL to a file under one
/// directory by its canonical artifact name. This is how pinned snapshots
/// and isolated acceptance flow through the identical staged, manifest-
/// pinned, byte-verified pipeline — the recorded `source_url` stays the
/// declared TDX URL the manifest contract requires.
pub struct Mirror {
    dir: std::path::PathBuf,
}
impl Mirror {
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
    fn file(&self, url: &str) -> Result<std::path::PathBuf, Error> {
        let name = url.rsplit('/').next().unwrap_or_default();
        if !["base.zip", "gbbq.zip", "hsjday.zip"].contains(&name) {
            return Err(Error::Failed(format!("undeclared url {url}")));
        }
        Ok(self.dir.join(name))
    }
}
impl Fetcher for Mirror {
    fn head(&self, url: &str) -> Result<Head, Error> {
        let path = self.file(url)?;
        let meta = std::fs::metadata(&path).map_err(|e| Error::Failed(e.to_string()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(Head {
            status: 200,
            etag: Some(format!("\"{:x}-{:x}\"", meta.len(), mtime)),
            last_modified: None,
            content_length: Some(meta.len()),
        })
    }
    fn fetch(&self, url: &str, dest: &Path) -> Result<FetchResult, Error> {
        let path = self.file(url)?;
        let bytes = std::fs::read(&path).map_err(|e| Error::Failed(e.to_string()))?;
        std::fs::write(dest, &bytes).map_err(|e| Error::Failed(e.to_string()))?;
        let head = self.head(url)?;
        Ok(FetchResult {
            head,
            bytes: bytes.len() as u64,
            sha256: crate::hash::sha256(&bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throw-away release-package layout: `<package>/bin/<exe>` plus an
    /// empty `<package>/scripts/` directory.
    fn package_layout() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        std::fs::create_dir(dir.path().join("scripts")).unwrap();
        let exe = dir.path().join("bin").join("acquire_service");
        std::fs::write(&exe, b"").unwrap();
        (dir, exe)
    }

    /// #375: an explicit `X_DATA_MCP_TDX_FETCH` wins outright and is not
    /// second-guessed — production units rely on it, and a missing file
    /// stays python's error to report, exactly as before the change.
    #[test]
    fn explicit_helper_path_wins_even_when_the_file_is_absent() {
        let (dir, exe) = package_layout();
        std::fs::write(dir.path().join("scripts/tdx_fetch.py"), b"").unwrap();
        let explicit = dir.path().join("elsewhere").join("helper.py");
        let resolved =
            Helper::resolve(Some(explicit.clone().into_os_string()), Some(&exe)).unwrap();
        assert_eq!(resolved, explicit);
    }

    /// #375: without the variable, the release-package layout next to the
    /// executable is the only candidate.
    #[test]
    fn packaged_helper_is_found_next_to_the_executable() {
        let (dir, exe) = package_layout();
        let packaged = dir.path().join("scripts").join("tdx_fetch.py");
        std::fs::write(&packaged, b"").unwrap();
        let resolved = Helper::resolve(None, Some(&exe)).unwrap();
        assert_eq!(resolved, exe.parent().unwrap().join(PACKAGED_HELPER));
        assert_eq!(
            resolved.canonicalize().unwrap(),
            packaged.canonicalize().unwrap()
        );
    }

    /// #375: a miss is fail-closed and names the variable and the path it
    /// tried. No compile-time path may appear anywhere in the message.
    #[test]
    fn missing_packaged_helper_fails_closed_with_the_tried_path() {
        let (_dir, exe) = package_layout();
        let err = Helper::resolve(None, Some(&exe)).unwrap_err();
        assert!(matches!(&err, Error::NotFound(_)), "{err:?}");
        let message = err.to_string();
        assert!(message.contains(TDX_FETCH_ENV), "{message}");
        let tried = exe.parent().unwrap().join(PACKAGED_HELPER);
        assert!(message.contains(&tried.display().to_string()), "{message}");
        assert!(!message.contains("CARGO_MANIFEST_DIR"), "{message}");
    }

    /// The helper owns the head/fetch wire shape: numeric `status`,
    /// `content_length` and `bytes` — HTTP header strings are normalized
    /// inside `tdx_fetch.py`, not here. A stringly-typed regression fails
    /// decode instead of silently passing (#262 G live-deploy catch).
    #[test]
    fn helper_wire_contract_decodes_numeric_lengths() {
        let head: HeadJson = serde_json::from_str(
            r#"{"status":200,"etag":"\"6aafb05c-f84ba5\"","last_modified":"Sun, 20 Sep 2026 10:07:24 GMT","content_length":16272293}"#,
        )
        .unwrap();
        assert_eq!(head.content_length, Some(16272293));
        let fetch: FetchJson = serde_json::from_str(
            r#"{"status":200,"etag":null,"last_modified":null,"content_length":5,"bytes":5,"sha256":"ab"}"#,
        )
        .unwrap();
        assert_eq!(fetch.bytes, 5);
        assert!(
            serde_json::from_str::<HeadJson>(
                r#"{"status":200,"etag":null,"last_modified":null,"content_length":"16272293"}"#,
            )
            .is_err()
        );
    }
}
