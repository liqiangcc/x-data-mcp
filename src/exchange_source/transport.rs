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
//! Outbound spacing lives here too (XDM-143, Refs #446): adjacent request
//! tries of one [`Client`] start at least
//! [`DEFAULT_MIN_REQUEST_INTERVAL`] apart, overridable through
//! [`MIN_REQUEST_INTERVAL_ENV`]. All five consumers (A-1/A-2, A-3, historical
//! ST, security names) fetch through this transport, so none of them has to
//! remember the limit.
//!
//! Tests never invoke the helper: they inject a [`Transport`] fixture and a
//! [`Pacer`] (fake clock) so no test waits on the wall clock.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// User-Agent every exchange request sends (§6.3).
pub const USER_AGENT: &str = "x-data-mcp-exchange-source/1";
/// Environment variable naming the helper script (same shape as
/// `X_DATA_MCP_TDX_FETCH`).
pub const EXCHANGE_FETCH_ENV: &str = "X_DATA_MCP_EXCHANGE_FETCH";
/// Packaged helper location relative to the binary directory.
const PACKAGED_HELPER: &str = "../scripts/exchange_fetch.py";
/// Upper bound on retry attempts, so a caller cannot configure an unbounded loop.
pub const MAX_ATTEMPTS: u32 = 5;
/// Minimum spacing between two adjacent outbound request starts, enforced per
/// [`Client`] unless overridden. Owner decision 2026-10-31 (XDM-143, option
/// (a)): the exchanges require at least one request per second, so compliance
/// wins over the wall-clock cost of e.g. A-3's 1,296-request backfill.
pub const DEFAULT_MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(1000);
/// Environment override for [`DEFAULT_MIN_REQUEST_INTERVAL`], in whole
/// milliseconds. `0` disables the spacing and exists for tests/offline use
/// only; a deployment must not set it. An unparsable value is an error (the
/// [`Client`] refuses to be built) rather than a silent change of the limit.
pub const MIN_REQUEST_INTERVAL_ENV: &str = "X_DATA_MCP_MIN_REQUEST_INTERVAL_MS";

/// Resolve [`MIN_REQUEST_INTERVAL_ENV`] against the default.
///
/// Unset means [`DEFAULT_MIN_REQUEST_INTERVAL`] (1,000 ms). `Some("0")` means
/// [`Duration::ZERO`]; any other value must be a whole number of milliseconds.
/// Invalid input returns the operator-facing reason; callers fail closed (the
/// data service refuses to start, and [`Client::helper`] fails the item)
/// instead of guessing a different rate limit.
pub fn min_request_interval(value: Option<&str>) -> Result<Duration, String> {
    let Some(raw) = value else {
        return Ok(DEFAULT_MIN_REQUEST_INTERVAL);
    };
    let millis: u64 = raw.trim().parse().map_err(|_| {
        format!(
            "{MIN_REQUEST_INTERVAL_ENV}={raw:?} is not a whole number of milliseconds; set 0 \
             (tests/offline only) or a positive value, or unset it for the 1,000 ms default"
        )
    })?;
    Ok(Duration::from_millis(millis))
}

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

/// Clock and wait surface of a [`Client`].
///
/// Production uses [`SystemPacer`] (the real clock and `std::thread::sleep`).
/// Tests inject a fake clock so CI never waits and can assert the exact sleep
/// durations; the pacer is a per-client value, never a global.
pub trait Pacer: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

/// Wall-clock pacer: [`Instant::now`] plus [`std::thread::sleep`].
pub struct SystemPacer;

impl Pacer for SystemPacer {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Test pacer: real clock, no waiting. Used by [`Client::without_sleep`].
struct NoSleepPacer;

impl Pacer for NoSleepPacer {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, _: Duration) {}
}

/// Transport plus retry policy and the outbound request spacing.
///
/// Every attempt (first try and each retry) waits until at least
/// [`Client::min_interval`] has elapsed since the previous attempt *started*.
/// The state is per client: two clients never pace each other, and a fresh
/// client sends its first request immediately. Sleeps are injectable, so CI
/// never waits.
pub struct Client {
    transport: Box<dyn Transport>,
    policy: RetryPolicy,
    pacer: Box<dyn Pacer>,
    min_interval: Duration,
    last_request_start: Mutex<Option<Instant>>,
}

impl Client {
    /// Test/offline form: the caller supplies the transport. The request
    /// interval starts at the production default so a forgotten override does
    /// not silently remove the limit.
    pub fn with_transport(transport: impl Transport + 'static) -> Self {
        Self {
            transport: Box::new(transport),
            policy: RetryPolicy::default(),
            pacer: Box::new(SystemPacer),
            min_interval: DEFAULT_MIN_REQUEST_INTERVAL,
            last_request_start: Mutex::new(None),
        }
    }

    /// The production transport: the packaged helper script, paced by
    /// [`MIN_REQUEST_INTERVAL_ENV`] (default 1,000 ms). An invalid override is
    /// `source_transport_unavailable` (fail-closed), never a different limit.
    pub fn helper() -> Result<Self, TransportError> {
        let interval =
            min_request_interval(std::env::var(MIN_REQUEST_INTERVAL_ENV).ok().as_deref())
                .map_err(TransportError::Unavailable)?;
        Ok(Self::with_transport(Helper::new()?).with_min_interval(interval))
    }

    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Override the minimum spacing between adjacent request starts.
    /// [`Duration::ZERO`] disables pacing (tests/offline only).
    pub fn with_min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }

    /// Inject the clock/sleep surface (tests only; production uses
    /// [`SystemPacer`]).
    pub fn with_pacer(mut self, pacer: impl Pacer + 'static) -> Self {
        self.pacer = Box::new(pacer);
        self
    }

    /// Skip the backoff sleep and the request-interval wait in tests; the
    /// attempt bound, the request order and the pacing bookkeeping are
    /// unchanged.
    pub fn without_sleep(mut self) -> Self {
        self.pacer = Box::new(NoSleepPacer);
        self
    }

    pub fn policy(&self) -> RetryPolicy {
        self.policy
    }

    /// Effective minimum spacing between adjacent request starts.
    pub fn min_interval(&self) -> Duration {
        self.min_interval
    }

    /// One GET with bounded retries and the minimum request spacing. A non-200
    /// status is *returned*, not retried into an error: the caller classifies it
    /// as `source_unavailable` (§6.3 failure classes).
    pub fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut attempt = 1;
        loop {
            self.wait_for_start_slot();
            let outcome = self.transport.get(request);
            let retry = match &outcome {
                Ok(response) => retryable_status(response.status),
                Err(error) => error.retryable(),
            };
            if !retry || attempt >= self.policy.attempts {
                return outcome;
            }
            // Backoff sleeps first; the next loop turn then measures the real
            // time since the previous attempt start. The two waits therefore
            // stack as `max(backoff, interval - elapsed)` instead of sleeping
            // the backoff and the full interval one after the other.
            self.pacer.sleep(self.policy.base_delay * attempt);
            attempt += 1;
        }
    }

    /// Wait until `min_interval` has passed since the previous attempt started,
    /// then record this attempt's start.
    ///
    /// The reference point is deliberately the **start** of the previous
    /// attempt (recorded just before `transport.get`), not its completion:
    /// serial callers then see adjacent starts at least `min_interval` apart
    /// whatever the response latency. The lock is per client and held across
    /// the wait, so concurrent callers of one client are spaced too; separate
    /// clients (and tests) are independent.
    fn wait_for_start_slot(&self) {
        if self.min_interval.is_zero() {
            return;
        }
        let mut last = self
            .last_request_start
            .lock()
            .expect("the request-interval lock is never held across a panic");
        if let Some(previous) = *last {
            let elapsed = self.pacer.now().saturating_duration_since(previous);
            if elapsed < self.min_interval {
                self.pacer.sleep(self.min_interval - elapsed);
            }
        }
        *last = Some(self.pacer.now());
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
