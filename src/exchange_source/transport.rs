//! Bounded HTTP transport for the exchange entry.
//!
//! The process boundary mirrors `src/acquire/fetch.rs`: the Rust side never
//! holds a socket and shells out to a stdlib-only Python helper
//! (`scripts/exchange_fetch.py`) that only moves bytes and headers. Retries
//! live here (bounded attempt count, retry only on transport errors and
//! 429/5xx) so the classification is testable without a network, and the
//! helper is resolved from `X_DATA_MCP_EXCHANGE_FETCH` or the packaged layout
//! exactly like `tdx_fetch.py` (#375 rules: no compile-time fallback).
//!
//! Tests never invoke the helper: they inject a [`Transport`] fixture.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// User-Agent every exchange request sends (§6.3).
pub const USER_AGENT: &str = "x-data-mcp-exchange-source/1";
/// Environment variable naming the helper script (same shape as
/// `X_DATA_MCP_TDX_FETCH`).
pub const EXCHANGE_FETCH_ENV: &str = "X_DATA_MCP_EXCHANGE_FETCH";
/// Packaged helper location relative to the binary directory.
const PACKAGED_HELPER: &str = "../scripts/exchange_fetch.py";
/// Upper bound on retry attempts, so a caller cannot configure an unbounded loop.
pub const MAX_ATTEMPTS: u32 = 5;

/// Request headers, sorted so the wire form is deterministic.
pub type Headers = BTreeMap<String, String>;

/// One outgoing request. The URL carries no secret; headers carry the UA and
/// the Referer the exchanges require.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Headers,
}

/// What the helper returned: status, cache validators and the raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransportError {
    /// Network/helper failure; retryable.
    #[error("source_transport_failed: {0}")]
    Failed(String),
    /// The helper answered with something unusable; retrying cannot fix it.
    #[error("source_transport_invalid: {0}")]
    Output(String),
    /// The helper itself is missing; an operator problem, not a transient one.
    #[error("source_transport_unavailable: {0}")]
    Unavailable(String),
}

impl TransportError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Failed(_) => "source_transport_failed",
            Self::Output(_) => "source_transport_invalid",
            Self::Unavailable(_) => "source_transport_unavailable",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// The injectable network surface.
pub trait Transport: Send + Sync {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// Bounded retry policy (design §6.3 "bounded retries").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub base_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            base_delay: Duration::from_millis(500),
        }
    }
}

impl RetryPolicy {
    /// No retry: used by tests and by callers that want a single attempt.
    pub fn single() -> Self {
        Self {
            attempts: 1,
            base_delay: Duration::ZERO,
        }
    }

    /// Bounded policy; `attempts` is clamped to `1..=MAX_ATTEMPTS`.
    pub fn bounded(attempts: u32, base_delay: Duration) -> Self {
        Self {
            attempts: attempts.clamp(1, MAX_ATTEMPTS),
            base_delay,
        }
    }
}

fn retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

/// Transport plus retry policy. Sleeps between attempts unless a test disables
/// it, so CI never waits.
pub struct Client {
    transport: Box<dyn Transport>,
    policy: RetryPolicy,
    sleep: fn(Duration),
}

impl Client {
    pub fn with_transport(transport: impl Transport + 'static) -> Self {
        Self {
            transport: Box::new(transport),
            policy: RetryPolicy::default(),
            sleep: std::thread::sleep,
        }
    }

    /// The production transport: the packaged helper script.
    pub fn helper() -> Result<Self, TransportError> {
        Ok(Self::with_transport(Helper::new()?))
    }

    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Skip the backoff sleep (tests only; the attempt bound is unchanged).
    pub fn without_sleep(mut self) -> Self {
        self.sleep = |_| {};
        self
    }

    pub fn policy(&self) -> RetryPolicy {
        self.policy
    }

    /// One GET with bounded retries. A non-200 status is *returned*, not
    /// retried into an error: the caller classifies it as
    /// `source_unavailable` (§6.3 failure classes).
    pub fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut attempt = 1;
        loop {
            let outcome = self.transport.get(request);
            let retry = match &outcome {
                Ok(response) => retryable_status(response.status),
                Err(error) => error.retryable(),
            };
            if !retry || attempt >= self.policy.attempts {
                return outcome;
            }
            (self.sleep)(self.policy.base_delay * attempt);
            attempt += 1;
        }
    }
}

/// Runs `scripts/exchange_fetch.py` as a child process.
///
/// Contract: request JSON on stdin (`{"url":...,"headers":{...}}`), body
/// written to the destination path, one JSON object on stdout
/// (`status`, `etag`, `last_modified`, `bytes`, `sha256`). The helper's sha256
/// is verified against the bytes we read back, so a truncated write fails as
/// `source_transport_invalid` instead of freezing wrong bytes.
pub struct Helper {
    script: PathBuf,
}

impl Helper {
    pub fn new() -> Result<Self, TransportError> {
        let exe = std::env::current_exe().ok();
        Self::resolve(std::env::var_os(EXCHANGE_FETCH_ENV), exe.as_deref())
    }

    /// Resolves the helper path without touching the environment.
    pub(crate) fn resolve(
        explicit: Option<OsString>,
        exe: Option<&Path>,
    ) -> Result<Self, TransportError> {
        if let Some(path) = explicit {
            return Ok(Self {
                script: PathBuf::from(path),
            });
        }
        let mut tried = Vec::new();
        if let Some(dir) = exe.and_then(Path::parent) {
            let path = dir.join(PACKAGED_HELPER);
            if path.is_file() {
                return Ok(Self { script: path });
            }
            tried.push(path.display().to_string());
        }
        let tried = if tried.is_empty() {
            "no path (executable path unavailable)".to_string()
        } else {
            tried.join(", ")
        };
        Err(TransportError::Unavailable(format!(
            "{EXCHANGE_FETCH_ENV} is not set and the packaged helper is missing; tried {tried} \
             (release layout: <exe dir>/{PACKAGED_HELPER}). Set {EXCHANGE_FETCH_ENV} to the \
             helper script path."
        )))
    }
}

impl Transport for Helper {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let scratch = tempfile::Builder::new()
            .prefix("xdm-exchange-")
            .tempfile()
            .map_err(|error| TransportError::Failed(error.to_string()))?;
        let payload = serde_json::json!({
            "url": request.url,
            "headers": request.headers,
        });
        let mut child = Command::new("python3")
            .arg(&self.script)
            .arg("get")
            .arg(scratch.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| TransportError::Unavailable(error.to_string()))?;
        {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| TransportError::Unavailable("helper stdin is closed".to_string()))?;
            stdin
                .write_all(payload.to_string().as_bytes())
                .map_err(|error| TransportError::Failed(error.to_string()))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| TransportError::Failed(error.to_string()))?;
        if !output.status.success() {
            return Err(TransportError::Failed(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let meta: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| TransportError::Output(error.to_string()))?;
        let status = meta
            .get("status")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| TransportError::Output("missing status".to_string()))?;
        let body = std::fs::read(scratch.path())
            .map_err(|error| TransportError::Failed(error.to_string()))?;
        if let Some(declared) = meta.get("sha256").and_then(serde_json::Value::as_str) {
            let actual = crate::hash::sha256(&body);
            if declared != actual {
                return Err(TransportError::Output(format!(
                    "read-back sha mismatch: helper {declared} vs bytes {actual}"
                )));
            }
        }
        Ok(HttpResponse {
            status: u16::try_from(status)
                .map_err(|_| TransportError::Output(format!("status {status} out of range")))?,
            etag: string_of(meta.get("etag")),
            last_modified: string_of(meta.get("last_modified")),
            body,
        })
    }
}

fn string_of(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}
