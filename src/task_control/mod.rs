//! Stage A generic task decisions and application port. No SQLite or MCP types.
pub mod legacy;
pub mod service;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommandKey {
    pub operation: String,
    pub dataset: String,
    pub contract_version: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    pub key: CommandKey,
    pub intent: Value,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemSpec {
    pub item_id: String,
    pub target: Value,
    pub progress_unit: String,
    pub frozen_inputs: Value,
    pub scope_keys: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenWork {
    pub items: Vec<ItemSpec>,
    pub scopes: BTreeSet<String>,
}
impl FrozenWork {
    pub fn validate(&self) -> Result<(), Error> {
        let mut ids = BTreeSet::new();
        if self.items.is_empty() || self.scopes.is_empty() {
            return Err(Error::InvalidCommand("empty work".into()));
        }
        for item in &self.items {
            if item.item_id.trim().is_empty()
                || item.progress_unit.trim().is_empty()
                || !ids.insert(&item.item_id)
            {
                return Err(Error::InvalidCommand("invalid/duplicate item".into()));
            }
        }
        if self
            .items
            .iter()
            .flat_map(|i| i.scope_keys.iter().cloned())
            .collect::<BTreeSet<_>>()
            != self.scopes
            || self.items.iter().any(|i| i.scope_keys.is_empty())
        {
            return Err(Error::InvalidCommand(
                "item resource bindings differ from scopes".into(),
            ));
        }
        if self.scopes.iter().any(|s| s.trim().is_empty()) {
            return Err(Error::InvalidCommand("empty scope".into()));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Running,
    CancelRequested,
    NeedsVerification,
    Succeeded,
    PartiallyFailed,
    Failed,
    Cancelled,
    Interrupted,
}
impl Status {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::PartiallyFailed
                | Self::Failed
                | Self::Cancelled
                | Self::Interrupted
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Pending,
    Running,
    Finished,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemOutcome {
    pub success: bool,
    pub error: Option<String>,
    /// Optional executor-produced evidence (e.g. governed exclusion
    /// lists, diagnostics) persisted verbatim in `task_items.outcome_json`.
    /// Carries no decision power — the durable record only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Value>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskItem {
    pub spec: ItemSpec,
    pub stage: Stage,
    pub outcome: Option<ItemOutcome>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub key: CommandKey,
    pub status: Status,
    pub state_revision: u64,
    pub accepted_run_id: String,
    pub owner_run_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_progress_at: DateTime<Utc>,
    pub work: FrozenWork,
    pub result: Option<Value>,
    pub retry_of: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeClaim {
    pub scope_key: String,
    pub task_id: String,
    pub state_revision: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub task: TaskRecord,
    pub items: Vec<TaskItem>,
    pub claims: Vec<ScopeClaim>,
}
impl TaskSnapshot {
    pub fn visible_status(&self, run: &str) -> Status {
        if !self.task.status.terminal()
            && (self.task.accepted_run_id != run
                || self
                    .task
                    .owner_run_id
                    .as_deref()
                    .is_some_and(|owner| owner != run))
        {
            Status::NeedsVerification
        } else {
            self.task.status
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestRecord {
    pub request_id: String,
    pub command: Command,
    pub outcome: RequestOutcome,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestOutcome {
    Task(Box<TaskSnapshot>),
    Terminal(Value),
}
#[derive(Debug, Clone)]
pub struct Acceptance {
    pub record: RequestRecord,
    pub reused: bool,
}
#[derive(Debug, Clone)]
pub struct RetryGuard {
    pub original_task_id: String,
    pub expected_revision: u64,
    pub selected_items: BTreeSet<String>,
}
/// Internal execution observation, never an externally supplied approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionUncertainty {
    pub previous_status: Status,
    pub item_id: String,
    pub reason: String,
    pub previous_result: Option<Value>,
}
impl TaskSnapshot {
    pub fn execution_uncertainty(&self) -> Result<Option<ExecutionUncertainty>, Error> {
        let Some(value) = self
            .task
            .result
            .as_ref()
            .and_then(|r| r.get("execution_uncertainty"))
        else {
            return Ok(None);
        };
        let marker: ExecutionUncertainty = serde_json::from_value(value.clone())
            .map_err(|e| Error::NeedsVerification(format!("invalid execution uncertainty: {e}")))?;
        if !matches!(
            marker.previous_status,
            Status::Running | Status::CancelRequested
        ) || self.task.status != Status::NeedsVerification
            || self.task.owner_run_id.is_none()
            || marker.reason.trim().is_empty()
            || !self
                .items
                .iter()
                .any(|i| i.spec.item_id == marker.item_id && i.stage == Stage::Running)
        {
            return Err(Error::NeedsVerification(
                "execution uncertainty does not bind unresolved running item".into(),
            ));
        }
        Ok(Some(marker))
    }
}
#[derive(Debug, Clone)]
pub enum Change {
    Claim,
    RequestCancel,
    StartItem(String),
    FinishItem(String, ItemOutcome),
    FinishExecution,
    /// Internal executor acknowledgement after cancellation stopped all writers.
    /// Never mapped from a user-supplied stopped/approval flag.
    FinishCancellation,
    Reconcile,
    /// Internal application report, not a caller-supplied outcome or stop flag.
    RequireVerification {
        item_id: String,
        reason: String,
    },
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("INVALID_COMMAND: {0}")]
    InvalidCommand(String),
    #[error("REQUEST_CONFLICT")]
    RequestConflict,
    #[error("RANGE_CONFLICT: {0:?}")]
    RangeConflict(Vec<String>),
    #[error("ACCEPTANCE_NEEDS_VERIFICATION: {0}")]
    NeedsVerification(String),
    #[error("STORAGE_UNAVAILABLE: {0}")]
    StorageUnavailable(String),
    #[error("OUTCOME_UNKNOWN: {0}; query the same request_id before retrying")]
    OutcomeUnknown(String),
    #[error("STATE_REVISION_CONFLICT")]
    RevisionConflict,
    #[error("UNSUPPORTED_CONTRACT")]
    UnsupportedContract,
    #[error("TASK_NOT_FOUND")]
    TaskNotFound,
    #[error("INVALID_RETRY_SELECTION")]
    InvalidRetrySelection,
    #[error("retry request_id must differ from the original task request_id")]
    RetryRequestIdConflict,
    #[error("retry selected_scopes must not be empty")]
    EmptyRetrySelection,
    #[error("request_id must not be empty")]
    EmptyRequestId,
    #[error("retry selected scopes are invalid, outside the original range, or already completed")]
    RetrySelectionInvalid { items: BTreeSet<String> },
}
/// Contract codecs own defaults, ordered vs set semantics, and dynamic resolution.
pub trait Contract {
    fn key(&self) -> CommandKey;
    fn normalize(&self, payload: Value) -> Result<Value, Error>;
    fn resolve(&self, intent: &Value) -> Result<FrozenWork, Error>;
}
/// Atomicity and snapshot scope belong to this application port, not to callers.
pub trait TaskRepository {
    fn read_request(&self, id: &str) -> Result<Option<RequestRecord>, Error>;
    fn read_task(&self, id: &str) -> Result<Option<TaskSnapshot>, Error>;
    fn accept(
        &self,
        id: &str,
        command: &Command,
        work: &FrozenWork,
        run: &str,
        retry: Option<&RetryGuard>,
    ) -> Result<Acceptance, Error>;
    fn transition(
        &self,
        id: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error>;
    fn candidates(&self, run: &str) -> Result<Vec<TaskSnapshot>, Error>;
    fn reconcile(&self, run: &str) -> Result<usize, Error>;
}
/// Runs synchronously, returning only after all item writers have stopped.
/// Never expose a client-supplied 'stopped' flag as an implementation of this port.
pub trait Executor {
    fn execute(&self, key: &CommandKey, item: &ItemSpec) -> ItemOutcome;
}

/// Pure state decision; adapter applies this under a revision-checked transaction.
pub fn decide(
    mut s: TaskSnapshot,
    run: &str,
    change: Change,
    now: DateTime<Utc>,
) -> Result<TaskSnapshot, Error> {
    if run.trim().is_empty() {
        return Err(Error::InvalidCommand("empty run identity".into()));
    }
    if s.task.status.terminal() {
        return if matches!(change, Change::RequestCancel | Change::Reconcile) {
            Ok(s)
        } else {
            Err(Error::RevisionConflict)
        };
    }
    let cancellation_finished = matches!(change, Change::FinishCancellation);
    let recovery = matches!(change, Change::Reconcile);
    if recovery {
        if s.visible_status(run) != s.task.status {
            s.task.status = Status::NeedsVerification;
        } else {
            return Ok(s);
        }
    } else {
        if s.visible_status(run) == Status::NeedsVerification {
            return Err(Error::NeedsVerification(
                "old run or unresolved execution".into(),
            ));
        }
        match change {
            Change::Claim if s.task.status == Status::Pending && s.task.owner_run_id.is_none() => {
                s.task.status = Status::Running;
                s.task.owner_run_id = Some(run.into());
            }
            Change::RequireVerification { item_id, reason } => {
                if reason.trim().is_empty()
                    || s.task.owner_run_id.as_deref() != Some(run)
                    || !matches!(s.task.status, Status::Running | Status::CancelRequested)
                    || !s
                        .items
                        .iter()
                        .any(|i| i.spec.item_id == item_id && i.stage == Stage::Running)
                {
                    return Err(Error::RevisionConflict);
                }
                let previous = s.task.result.take();
                let previous_status = s.task.status;
                s.task.status = Status::NeedsVerification;
                s.task.result = Some(serde_json::json!({"execution_uncertainty":{
                    "item_id":item_id,"reason":reason,"previous_result":previous,"previous_status":previous_status
                }}));
            }
            Change::RequestCancel => {
                if s.task.status == Status::CancelRequested {
                    return Ok(s);
                }
                s.task.status = Status::CancelRequested;
            }
            Change::StartItem(id)
                if s.task.status == Status::Running
                    && s.task.owner_run_id.as_deref() == Some(run) =>
            {
                if s.items.iter().any(|i| i.stage == Stage::Running) {
                    return Err(Error::RevisionConflict);
                }
                let item = s
                    .items
                    .iter_mut()
                    .find(|i| i.spec.item_id == id && i.stage == Stage::Pending)
                    .ok_or(Error::RevisionConflict)?;
                item.stage = Stage::Running;
            }
            Change::FinishItem(id, outcome) if s.task.owner_run_id.as_deref() == Some(run) => {
                if outcome.success && outcome.error.is_some() {
                    return Err(Error::InvalidCommand("success with error".into()));
                }
                let item = s
                    .items
                    .iter_mut()
                    .find(|i| i.spec.item_id == id && i.stage == Stage::Running)
                    .ok_or(Error::RevisionConflict)?;
                item.stage = Stage::Finished;
                item.outcome = Some(outcome);
            }
            Change::FinishExecution | Change::FinishCancellation => {
                if cancellation_finished && s.task.status != Status::CancelRequested {
                    return Err(Error::RevisionConflict);
                }
                let owned = s.task.owner_run_id.as_deref() == Some(run);
                let never_started = s.task.owner_run_id.is_none()
                    && s.items.iter().all(|i| i.stage == Stage::Pending);
                if !owned && !never_started || s.items.iter().any(|i| i.stage == Stage::Running) {
                    return Err(Error::NeedsVerification(
                        "execution has not safely ended".into(),
                    ));
                }
                let finished = s
                    .items
                    .iter()
                    .filter(|i| i.stage == Stage::Finished)
                    .count();
                let success = s
                    .items
                    .iter()
                    .filter(|i| i.outcome.as_ref().is_some_and(|o| o.success))
                    .count();
                s.task.status = if cancellation_finished && success < s.items.len() {
                    Status::Cancelled
                } else if finished == s.items.len() {
                    if success == finished {
                        Status::Succeeded
                    } else if success == 0 {
                        Status::Failed
                    } else {
                        Status::PartiallyFailed
                    }
                } else if s.task.status == Status::CancelRequested {
                    Status::Cancelled
                } else {
                    return Err(Error::RevisionConflict);
                };
                s.task.result =
                    Some(serde_json::json!({"finished_items":finished,"successful_items":success}));
                if cancellation_finished && s.task.status == Status::Cancelled {
                    s.task.result.as_mut().unwrap()["cancellation_acknowledged"] =
                        serde_json::json!(true);
                }
            }
            _ => return Err(Error::RevisionConflict),
        }
    }
    s.task.state_revision = s
        .task
        .state_revision
        .checked_add(1)
        .ok_or(Error::RevisionConflict)?;
    if !recovery {
        s.task.last_progress_at = now;
    }
    if s.task.status.terminal() {
        s.claims.clear();
    } else {
        for claim in &mut s.claims {
            claim.state_revision = s.task.state_revision;
        }
    }
    Ok(s)
}
