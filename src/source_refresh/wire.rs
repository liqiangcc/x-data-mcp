//! Wire shapes of `refresh_exchange_sources` (issue-446 §5.5/§5.6, R2).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const DESCRIPTION: &str = "Refresh exchange sources from HTTP. `tail` is synchronous \
    (28 requests); `full` returns `running` at once and continues in the background. One run at \
    a time; `status_of` reads a run record; `dry_run` lists the plan.";

/// `tail` refreshes the open + previous natural quarter of window sources, the
/// cheap whole tables, and probes the two large tables; `full` observes every
/// fingerprint of every selected source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RefreshMode {
    Tail,
    Full,
}

impl RefreshMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tail => "tail",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefreshRequest {
    /// Required unless `status_of` is set. `tail` is synchronous and bounded;
    /// `full` returns `running` at once and continues in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RefreshMode>,
    /// Empty/absent means every HTTP exchange source. Names are the stable
    /// source ids (`sse_listing`, `szse_short_name`, ...).
    #[serde(default)]
    pub sources: Vec<String>,
    /// List the plan and touch nothing: no HTTP, no store write, no run row.
    #[serde(default)]
    pub dry_run: bool,
    /// Read one durable run record instead of starting a run; not combined
    /// with `mode`, `sources` or `dry_run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_of: Option<String>,
}

impl RefreshRequest {
    /// The mode of a request that starts a run.
    pub fn require_mode(&self) -> Result<RefreshMode, String> {
        self.mode
            .ok_or_else(|| "mode is required unless status_of is set".to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RefreshStatus {
    Planned,
    /// The durable row is open; a `full` continues in the background.
    Running,
    Succeeded,
    Failed,
}

/// One request unit the plan will perform (a whole-table plan or one window of
/// a windowed plan). `label` is stable enough for an operator to match the
/// adapter's own evidence labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlannedUnit {
    pub source_id: String,
    pub label: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_end: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceReport {
    pub source_id: String,
    /// `window` or `table`.
    pub class: String,
    /// Planned request units; for a table, one per whole-table plan.
    pub planned_units: usize,
    /// Planned windows (0 for a table class).
    pub planned_windows: usize,
    /// True only when this run completed every planned request of the source.
    /// A tail of a windowed source is never complete; a probe without a change
    /// is never complete; a failed table is never complete. Observed empty
    /// windows are counted in `unobserved_windows` (coverage gaps), not as
    /// failures, and do not by themselves make a full run incomplete.
    pub complete: bool,
    pub requests: u64,
    pub snapshots: usize,
    /// Pages whose bytes were already in the store (no new manifest line).
    pub reused: usize,
    /// Selected pages whose sha differs from the latest snapshot of the same
    /// request before this run.
    pub changed: usize,
    pub unobserved_windows: usize,
    pub failed_windows: usize,
    /// The date this source's complete planned request set was last confirmed
    /// by a successful run, when this run itself completed it. `None` for a
    /// tail that only looked at two windows or one probe page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_through: Option<String>,
}

/// One request whose selected bytes differ from the previous observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Revision {
    pub source_id: String,
    pub request_fingerprint: String,
    pub previous_snapshot_id: String,
    pub snapshot_id: String,
    pub previous_as_of: String,
    pub as_of: String,
    pub row_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_end: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RefreshFailure {
    pub source_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_end: Option<String>,
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RefreshReport {
    /// `None` only for a dry run: a dry run writes no run row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub mode: RefreshMode,
    pub status: RefreshStatus,
    pub dry_run: bool,
    pub started_at: String,
    /// Absent while `status` is `running`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub scope: Vec<String>,
    pub requests: u64,
    pub reused: usize,
    pub changed: usize,
    pub failed_windows: usize,
    pub unobserved_windows: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_through: Option<String>,
    pub sources: Vec<SourceReport>,
    pub planned: Vec<PlannedUnit>,
    pub revisions: Vec<Revision>,
    pub failures: Vec<RefreshFailure>,
}

impl RefreshReport {
    pub fn is_success(&self) -> bool {
        matches!(self.status, RefreshStatus::Succeeded)
    }
}
