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
    /// Evidence shared verbatim by every item of this task, persisted once
    /// inside `frozen_work_json` rather than copied into each item's frozen
    /// inputs. Items carry only `{"evidence_digest": ...}` markers whose
    /// digest must equal this object's own `evidence_digest` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_evidence: Option<Value>,
}
impl FrozenWork {
    /// The digest item evidence markers must equal: the shared evidence's
    /// own `evidence_digest` field, when this task carries one.
    pub fn evidence_digest(&self) -> Option<&str> {
        self.shared_evidence
            .as_ref()?
            .get("evidence_digest")?
            .as_str()
    }
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
        let digest = self.evidence_digest();
        for item in &self.items {
            if let Some(marker) = item
                .frozen_inputs
                .get("selection_evidence")
                .and_then(|v| v.get("evidence_digest"))
                .and_then(Value::as_str)
                && digest != Some(marker)
            {
                return Err(Error::InvalidCommand(
                    "item evidence marker mismatches shared evidence".into(),
                ));
            }
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
    /// O(1) head projection of this snapshot: the row-level view
    /// `decide`/`read_task_head` maintain, without the item list.
    pub fn head(&self) -> TaskHead {
        TaskHead {
            task_id: self.task.task_id.clone(),
            key: self.task.key.clone(),
            status: self.task.status,
            state_revision: self.task.state_revision,
            accepted_run_id: self.task.accepted_run_id.clone(),
            owner_run_id: self.task.owner_run_id.clone(),
            last_progress_at: self.task.last_progress_at,
            result: self.task.result.clone(),
            counts: ItemCounts {
                total: self.items.len() as u64,
                pending: self
                    .items
                    .iter()
                    .filter(|i| i.stage == Stage::Pending)
                    .count() as u64,
                running: self
                    .items
                    .iter()
                    .filter(|i| i.stage == Stage::Running)
                    .count() as u64,
                finished: self
                    .items
                    .iter()
                    .filter(|i| i.stage == Stage::Finished)
                    .count() as u64,
                succeeded: self
                    .items
                    .iter()
                    .filter(|i| i.outcome.as_ref().is_some_and(|o| o.success))
                    .count() as u64,
            },
            evidence_digest: self.task.work.evidence_digest().map(str::to_string),
        }
    }
    pub fn visible_status(&self, run: &str) -> Status {
        visible_status(
            self.task.status,
            &self.task.accepted_run_id,
            self.task.owner_run_id.as_deref(),
            run,
        )
    }
}
fn visible_status(status: Status, accepted: &str, owner: Option<&str>, run: &str) -> Status {
    if !status.terminal() && (accepted != run || owner.is_some_and(|owner| owner != run)) {
        Status::NeedsVerification
    } else {
        status
    }
}
/// Aggregate per-stage item counters maintained on the task row; they are
/// the O(1) equivalent of scanning every item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemCounts {
    pub total: u64,
    pub pending: u64,
    pub running: u64,
    pub finished: u64,
    pub succeeded: u64,
}
/// O(1) projection of a task row for hot-loop guards and light transitions.
/// Carries no frozen work or item list; the counters answer every
/// aggregate question `decide` asks of the full item vector.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskHead {
    pub task_id: String,
    pub key: CommandKey,
    pub status: Status,
    pub state_revision: u64,
    pub accepted_run_id: String,
    pub owner_run_id: Option<String>,
    pub last_progress_at: DateTime<Utc>,
    pub result: Option<Value>,
    pub counts: ItemCounts,
    /// Digest of the task's shared evidence (schema 18+); per-item
    /// `selection_evidence` markers must equal it. `None` for tasks that
    /// carry no shared evidence.
    pub evidence_digest: Option<String>,
}
impl TaskHead {
    pub fn visible_status(&self, run: &str) -> Status {
        visible_status(
            self.status,
            &self.accepted_run_id,
            self.owner_run_id.as_deref(),
            run,
        )
    }
}
/// One task row plus the one item row a runner/commit path touches, with
/// the occupancy fact the binding checks need. O(1) to read.
#[derive(Debug, Clone)]
pub struct ItemBinding {
    pub head: TaskHead,
    pub item: TaskItem,
    /// Every scope key the item binds is occupied by this task.
    pub scopes_bound: bool,
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
    /// Streamed accept for unbounded item counts: persists each spec as
    /// the iterator yields it instead of materializing the whole frozen
    /// work tree in memory first. Semantics are identical to `accept`
    /// (same binding, gates, transaction, and on-disk rows) except the
    /// returned record is constructed rather than re-read — its `items`
    /// list is empty and callers needing full detail re-read the task.
    /// The default implementation materializes the whole list and
    /// delegates to `accept`; backends with very large task admissions
    /// override it to stream.
    fn accept_streamed(
        &self,
        id: &str,
        command: &Command,
        shared_evidence: Option<Value>,
        scopes: BTreeSet<String>,
        items: &mut dyn Iterator<Item = ItemSpec>,
        run: &str,
    ) -> Result<Acceptance, Error> {
        let mut work = FrozenWork {
            items: Vec::new(),
            scopes,
            shared_evidence,
        };
        for item in items {
            work.items.push(item);
        }
        work.validate()?;
        self.accept(id, command, &work, run, None)
    }
    fn transition(
        &self,
        id: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error>;
    /// Apply `change` to the caller-held snapshot in place. Semantically
    /// equivalent to `transition` except a stale pre-image is refreshed once
    /// instead of rejected: callers in the execution loop hold one snapshot
    /// across transitions, so an implementation may verify the persisted
    /// revision against `s` and reload only when it diverged. `decide`
    /// guards still reject an illegal change on the actual state. On error
    /// the snapshot contents are unspecified; callers re-read or propagate.
    fn transition_light(
        &self,
        s: &mut TaskSnapshot,
        run: &str,
        change: Change,
    ) -> Result<(), Error> {
        let current = self
            .read_task(&s.task.task_id)?
            .ok_or(Error::TaskNotFound)?;
        *s = self.transition(&s.task.task_id, current.task.state_revision, run, change)?;
        Ok(())
    }
    fn candidates(&self, run: &str) -> Result<Vec<TaskSnapshot>, Error>;
    fn reconcile(&self, run: &str) -> Result<usize, Error>;
}
/// Runs synchronously, returning only after all item writers have stopped.
/// Never expose a client-supplied 'stopped' flag as an implementation of this port.
pub trait Executor {
    fn execute(&self, key: &CommandKey, item: &ItemSpec) -> ItemOutcome;
}

/// Pure state decision applied in place; the adapter persists the mutated
/// snapshot under a revision-checked transaction. Callers compare
/// `state_revision` before/after to detect no-op decisions.
pub fn decide(
    s: &mut TaskSnapshot,
    run: &str,
    change: Change,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    if run.trim().is_empty() {
        return Err(Error::InvalidCommand("empty run identity".into()));
    }
    if s.task.status.terminal() {
        return if matches!(change, Change::RequestCancel | Change::Reconcile) {
            Ok(())
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
            return Ok(());
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
                    return Ok(());
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
    Ok(())
}

/// The same pure decision as `decide`, evaluated on a `TaskHead` plus the
/// single item the change touches instead of a fully materialized item
/// vector. Counters on the head answer every aggregate guard (`any running`,
/// `all pending`, finished/succeeded totals) and are updated in step so the
/// caller's head stays the exact post-decision state. `item` must be the
/// persisted row for item-scoped changes; `None` is only valid for
/// head-level changes. Equivalence with `decide` is covered by tests.
pub(crate) fn decide_head(
    head: &mut TaskHead,
    item: Option<&mut TaskItem>,
    run: &str,
    change: Change,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    if run.trim().is_empty() {
        return Err(Error::InvalidCommand("empty run identity".into()));
    }
    if head.status.terminal() {
        return if matches!(change, Change::RequestCancel | Change::Reconcile) {
            Ok(())
        } else {
            Err(Error::RevisionConflict)
        };
    }
    let cancellation_finished = matches!(change, Change::FinishCancellation);
    let recovery = matches!(change, Change::Reconcile);
    if recovery {
        if head.visible_status(run) != head.status {
            head.status = Status::NeedsVerification;
        } else {
            return Ok(());
        }
    } else {
        if head.visible_status(run) == Status::NeedsVerification {
            return Err(Error::NeedsVerification(
                "old run or unresolved execution".into(),
            ));
        }
        match change {
            Change::Claim if head.status == Status::Pending && head.owner_run_id.is_none() => {
                head.status = Status::Running;
                head.owner_run_id = Some(run.into());
            }
            Change::RequireVerification { item_id, reason } => {
                let bound = item
                    .as_deref()
                    .is_some_and(|i| i.spec.item_id == item_id && i.stage == Stage::Running);
                if reason.trim().is_empty()
                    || head.owner_run_id.as_deref() != Some(run)
                    || !matches!(head.status, Status::Running | Status::CancelRequested)
                    || !bound
                {
                    return Err(Error::RevisionConflict);
                }
                let previous = head.result.take();
                let previous_status = head.status;
                head.status = Status::NeedsVerification;
                head.result = Some(serde_json::json!({"execution_uncertainty":{
                    "item_id":item_id,"reason":reason,"previous_result":previous,"previous_status":previous_status
                }}));
            }
            Change::RequestCancel => {
                if head.status == Status::CancelRequested {
                    return Ok(());
                }
                head.status = Status::CancelRequested;
            }
            Change::StartItem(id)
                if head.status == Status::Running && head.owner_run_id.as_deref() == Some(run) =>
            {
                if head.counts.running != 0 {
                    return Err(Error::RevisionConflict);
                }
                match item {
                    Some(i) if i.spec.item_id == id && i.stage == Stage::Pending => {
                        i.stage = Stage::Running;
                        head.counts.pending -= 1;
                        head.counts.running += 1;
                    }
                    _ => return Err(Error::RevisionConflict),
                }
            }
            Change::FinishItem(id, outcome) if head.owner_run_id.as_deref() == Some(run) => {
                if outcome.success && outcome.error.is_some() {
                    return Err(Error::InvalidCommand("success with error".into()));
                }
                match item {
                    Some(i) if i.spec.item_id == id && i.stage == Stage::Running => {
                        i.stage = Stage::Finished;
                        i.outcome = Some(outcome);
                        head.counts.running -= 1;
                        head.counts.finished += 1;
                        head.counts.succeeded += u64::from(i.outcome.as_ref().unwrap().success);
                    }
                    _ => return Err(Error::RevisionConflict),
                }
            }
            Change::FinishExecution | Change::FinishCancellation => {
                if cancellation_finished && head.status != Status::CancelRequested {
                    return Err(Error::RevisionConflict);
                }
                let owned = head.owner_run_id.as_deref() == Some(run);
                let never_started =
                    head.owner_run_id.is_none() && head.counts.pending == head.counts.total;
                if !owned && !never_started || head.counts.running != 0 {
                    return Err(Error::NeedsVerification(
                        "execution has not safely ended".into(),
                    ));
                }
                let (finished, success, total) = (
                    head.counts.finished,
                    head.counts.succeeded,
                    head.counts.total,
                );
                head.status = if cancellation_finished && success < total {
                    Status::Cancelled
                } else if finished == total {
                    if success == finished {
                        Status::Succeeded
                    } else if success == 0 {
                        Status::Failed
                    } else {
                        Status::PartiallyFailed
                    }
                } else if head.status == Status::CancelRequested {
                    Status::Cancelled
                } else {
                    return Err(Error::RevisionConflict);
                };
                head.result =
                    Some(serde_json::json!({"finished_items":finished,"successful_items":success}));
                if cancellation_finished && head.status == Status::Cancelled {
                    head.result.as_mut().unwrap()["cancellation_acknowledged"] =
                        serde_json::json!(true);
                }
            }
            _ => return Err(Error::RevisionConflict),
        }
    }
    head.state_revision = head
        .state_revision
        .checked_add(1)
        .ok_or(Error::RevisionConflict)?;
    if !recovery {
        head.last_progress_at = now;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(status: Status, owner: Option<&str>, stages: &[Stage]) -> TaskSnapshot {
        let items = stages
            .iter()
            .enumerate()
            .map(|(index, stage)| TaskItem {
                spec: ItemSpec {
                    item_id: format!("i{index}"),
                    target: serde_json::json!({"symbol":format!("s{index}")}),
                    progress_unit: "u".into(),
                    frozen_inputs: serde_json::json!({}),
                    scope_keys: BTreeSet::from([format!("k{index}")]),
                },
                stage: *stage,
                outcome: if *stage == Stage::Finished {
                    Some(ItemOutcome {
                        success: true,
                        error: None,
                        evidence: None,
                    })
                } else {
                    None
                },
            })
            .collect();
        let now = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        TaskSnapshot {
            task: TaskRecord {
                task_id: "t".into(),
                key: CommandKey {
                    operation: "op".into(),
                    dataset: "d".into(),
                    contract_version: "v".into(),
                },
                status,
                state_revision: 7,
                accepted_run_id: "run".into(),
                owner_run_id: owner.map(str::to_string),
                created_at: now,
                last_progress_at: now,
                work: FrozenWork {
                    items: vec![],
                    scopes: BTreeSet::new(),
                    shared_evidence: None,
                },
                result: None,
                retry_of: None,
            },
            items,
            claims: vec![],
        }
    }
    fn head_of(s: &TaskSnapshot) -> TaskHead {
        TaskHead {
            task_id: s.task.task_id.clone(),
            key: s.task.key.clone(),
            status: s.task.status,
            state_revision: s.task.state_revision,
            accepted_run_id: s.task.accepted_run_id.clone(),
            owner_run_id: s.task.owner_run_id.clone(),
            last_progress_at: s.task.last_progress_at,
            result: s.task.result.clone(),
            evidence_digest: s.task.work.evidence_digest().map(str::to_string),
            counts: ItemCounts {
                total: s.items.len() as u64,
                pending: s.items.iter().filter(|i| i.stage == Stage::Pending).count() as u64,
                running: s.items.iter().filter(|i| i.stage == Stage::Running).count() as u64,
                finished: s
                    .items
                    .iter()
                    .filter(|i| i.stage == Stage::Finished)
                    .count() as u64,
                succeeded: s
                    .items
                    .iter()
                    .filter(|i| i.outcome.as_ref().is_some_and(|o| o.success))
                    .count() as u64,
            },
        }
    }
    /// Every guard/transition branch must produce identical outcomes whether
    /// the decision reads a materialized item vector (`decide`) or a head plus
    /// the one bound item (`decide_head`).
    #[test]
    fn decide_head_matches_decide_on_equivalent_states() {
        let now = DateTime::parse_from_rfc3339("2025-01-01T00:00:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let ok = ItemOutcome {
            success: true,
            error: None,
            evidence: None,
        };
        let fail = ItemOutcome {
            success: false,
            error: Some("x".into()),
            evidence: None,
        };
        use Stage::{Finished as F, Pending as P, Running as R};
        use Status::*;
        let states = [
            snapshot(Pending, None, &[P, P, P]),
            snapshot(Running, Some("run"), &[F, R, P]),
            snapshot(Running, Some("run"), &[F, F, F]),
            snapshot(Running, Some("run"), &[F, P, P]),
            snapshot(CancelRequested, Some("run"), &[F, P, P]),
            snapshot(Running, Some("other"), &[P, P, P]),
            snapshot(Succeeded, Some("run"), &[F, F]),
            snapshot(Interrupted, Some("run"), &[R, P]),
        ];
        let changes = [
            Change::Claim,
            Change::StartItem("i2".into()),
            Change::StartItem("i0".into()),
            Change::FinishItem("i1".into(), ok.clone()),
            Change::FinishItem("i2".into(), ok.clone()),
            Change::FinishItem("i1".into(), fail.clone()),
            Change::FinishItem("i9".into(), ok.clone()),
            Change::FinishItem(
                "i1".into(),
                ItemOutcome {
                    success: true,
                    error: Some("x".into()),
                    evidence: None,
                },
            ),
            Change::RequireVerification {
                item_id: "i1".into(),
                reason: "uncertain".into(),
            },
            Change::RequireVerification {
                item_id: "i1".into(),
                reason: " ".into(),
            },
            Change::RequestCancel,
            Change::FinishExecution,
            Change::FinishCancellation,
            Change::Reconcile,
        ];
        for base in &states {
            for change in &changes {
                let mut s = base.clone();
                let full = decide(&mut s, "run", change.clone(), now).map_err(|e| e.to_string());
                let mut head = head_of(base);
                let mut item = change_item(base, change);
                let light = decide_head(&mut head, item.as_mut(), "run", change.clone(), now)
                    .map_err(|e| e.to_string());
                assert_eq!(full, light, "{base:?} {change:?}");
                if full.is_err() {
                    continue;
                }
                let expected_head = head_of(&s);
                assert_eq!(head, expected_head, "{base:?} {change:?}");
                match &item {
                    Some(item) => {
                        let actual = s
                            .items
                            .iter()
                            .find(|i| i.spec.item_id == item.spec.item_id)
                            .unwrap();
                        assert_eq!(item, actual, "{base:?} {change:?}");
                    }
                    None => assert_eq!(s.items, base.items, "{base:?} {change:?}"),
                }
            }
        }
    }
    /// The bound item a head-level change would touch, mirroring the persisted
    /// row the SQLite adapter would supply for that change.
    fn change_item(s: &TaskSnapshot, change: &Change) -> Option<TaskItem> {
        let id = match change {
            Change::StartItem(id)
            | Change::FinishItem(id, _)
            | Change::RequireVerification { item_id: id, .. } => id,
            _ => return None,
        };
        s.items.iter().find(|i| i.spec.item_id == *id).cloned()
    }
}
