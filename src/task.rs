use crate::market_data::{Adjustment, Period};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub const CONTRACT_VERSION: &str = "task-acceptance-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenScope {
    pub symbol: String,
    pub period: Period,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBinding {
    pub request_id: String,
    pub task_id: String,
    pub symbols: Vec<String>,
    pub periods: Vec<Period>,
    pub adjustment: Adjustment,
    pub frozen_range: Vec<FrozenScope>,
    pub contract_version: String,
    #[serde(default)]
    pub retry_of: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Running,
    CancelRequested,
    NeedsVerification,
    Succeeded,
    PartiallyFailed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeExecutionResult {
    pub symbol: String,
    pub period: Period,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskExecutionRecord {
    pub task_id: String,
    pub status: TaskStatus,
    pub scope_results: Vec<ScopeExecutionResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccupancyRecord {
    pub task_id: String,
    pub scopes: Vec<FrozenScope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedTask {
    pub task_id: String,
    pub binding: TaskBinding,
    pub reused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationResult {
    pub task_id: String,
    pub status: TaskStatus,
    pub scope_results: Vec<ScopeExecutionResult>,
    pub changed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("request_id must not be empty")]
    EmptyRequestId,
    #[error("symbols must not be empty")]
    EmptySymbols,
    #[error("periods must not be empty")]
    EmptyPeriods,
    #[error("unsupported adjustment: only forward/qfq is supported")]
    UnsupportedAdjustment,
    #[error("request_id {request_id} is already bound to different parameters")]
    RequestConflict { request_id: String },
    #[error("requested frozen range is occupied")]
    RangeConflict { scopes: Vec<FrozenScope> },
    #[error("task {task_id} was not found")]
    TaskNotFound { task_id: String },
    #[error("retry request_id must differ from the original task request_id")]
    RetryRequestIdConflict,
    #[error("retry selected_scopes must not be empty")]
    RetrySelectionEmpty,
    #[error("retry selection contains invalid or already completed scopes")]
    RetrySelectionInvalid { scopes: Vec<FrozenScope> },
    #[error("task store I/O failed: {0}")]
    Storage(#[from] io::Error),
}

#[derive(Debug, Clone)]
pub struct TaskStore {
    root: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TaskState {
    bindings: Vec<TaskBinding>,
    executions: Vec<TaskExecutionRecord>,
    occupancy: Vec<OccupancyRecord>,
}

impl TaskStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn read_state(&self) -> io::Result<TaskState> {
        match fs::read(self.root.join("task-state.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(TaskState {
                bindings: self.read_legacy("task-bindings.json")?,
                executions: self.read_legacy("task-executions.json")?,
                occupancy: self.read_legacy("task-occupancy.json")?,
            }),
            Err(error) => Err(error),
        }
    }

    fn read_legacy<T: serde::de::DeserializeOwned>(&self, name: &str) -> io::Result<Vec<T>> {
        match fs::read(self.root.join(name)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    fn write_state(&self, state: &TaskState) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let payload = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        crate::storage::atomic_replace(&self.root.join("task-state.json"), |file| {
            file.write_all(&payload)
        })
    }

    pub fn get(&self, request_id: &str) -> io::Result<Option<TaskBinding>> {
        Ok(self
            .read_state()?
            .bindings
            .into_iter()
            .find(|item| item.request_id == request_id))
    }
    pub fn list(&self) -> io::Result<Vec<TaskBinding>> {
        Ok(self.read_state()?.bindings)
    }
    pub fn get_task(&self, task_id: &str) -> io::Result<Option<TaskBinding>> {
        Ok(self
            .read_state()?
            .bindings
            .into_iter()
            .find(|item| item.task_id == task_id))
    }
    pub fn get_execution(&self, task_id: &str) -> io::Result<Option<TaskExecutionRecord>> {
        Ok(self
            .read_state()?
            .executions
            .into_iter()
            .find(|item| item.task_id == task_id))
    }
    pub fn save_execution(&self, record: TaskExecutionRecord) -> io::Result<()> {
        let mut state = self.read_state()?;
        if let Some(existing) = state
            .executions
            .iter_mut()
            .find(|item| item.task_id == record.task_id)
        {
            *existing = record;
        } else {
            state.executions.push(record);
        }
        self.write_state(&state)
    }
    pub fn list_occupancy(&self) -> io::Result<Vec<OccupancyRecord>> {
        Ok(self.read_state()?.occupancy)
    }
    pub fn active_conflicts(&self, requested: &[FrozenScope]) -> io::Result<Vec<FrozenScope>> {
        let state = self.read_state()?;
        let active = |id: &str| {
            state
                .executions
                .iter()
                .find(|e| e.task_id == id)
                .map(|e| e.status.occupies_range())
                .unwrap_or(true)
        };
        // An incomplete legacy binding still reserves its scope conservatively.
        // Absence of its occupancy file is not evidence that work has stopped.
        Ok(requested
            .iter()
            .filter(|scope| {
                state
                    .occupancy
                    .iter()
                    .any(|o| active(&o.task_id) && o.scopes.contains(scope))
                    || state
                        .bindings
                        .iter()
                        .any(|b| active(&b.task_id) && b.frozen_range.contains(scope))
            })
            .cloned()
            .collect())
    }
    pub fn save_occupancy(&self, task_id: &str, scopes: &[FrozenScope]) -> io::Result<()> {
        let mut state = self.read_state()?;
        if let Some(existing) = state
            .occupancy
            .iter_mut()
            .find(|item| item.task_id == task_id)
        {
            existing.scopes = scopes.to_vec();
        } else {
            state.occupancy.push(OccupancyRecord {
                task_id: task_id.into(),
                scopes: scopes.to_vec(),
            });
        }
        self.write_state(&state)
    }
    pub fn release_occupancy(&self, task_id: &str) -> io::Result<()> {
        let mut state = self.read_state()?;
        state.occupancy.retain(|item| item.task_id != task_id);
        self.write_state(&state)
    }
    fn commit_acceptance(&self, binding: TaskBinding) -> io::Result<()> {
        let mut state = self.read_state()?;
        state.executions.push(TaskExecutionRecord {
            task_id: binding.task_id.clone(),
            status: TaskStatus::Pending,
            scope_results: Vec::new(),
        });
        state.occupancy.push(OccupancyRecord {
            task_id: binding.task_id.clone(),
            scopes: binding.frozen_range.clone(),
        });
        state.bindings.push(binding);
        self.write_state(&state)
    }
    pub(crate) fn verify_reusable(&self, binding: &TaskBinding) -> io::Result<()> {
        let state = self.read_state()?;
        let execution = state
            .executions
            .iter()
            .find(|e| e.task_id == binding.task_id)
            .ok_or_else(|| io::Error::other("ACCEPTANCE_NEEDS_VERIFICATION: execution missing"))?;
        if execution.status.occupies_range()
            && !state
                .occupancy
                .iter()
                .any(|o| o.task_id == binding.task_id && o.scopes == binding.frozen_range)
        {
            return Err(io::Error::other(
                "ACCEPTANCE_NEEDS_VERIFICATION: occupancy missing or inconsistent",
            ));
        }
        Ok(())
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[derive(Debug, Clone)]
pub struct TaskAcceptanceService {
    store: TaskStore,
}

impl TaskAcceptanceService {
    pub fn new(store: TaskStore) -> Self {
        Self { store }
    }

    pub fn accept(
        &self,
        request_id: &str,
        symbols: &[&str],
        periods: &[Period],
        adjustment: Adjustment,
    ) -> Result<AcceptedTask, TaskError> {
        let binding = normalize(request_id, symbols, periods, adjustment)?;
        self.accept_binding(binding)
    }

    pub fn accept_frozen(
        &self,
        request_id: &str,
        scopes: &[FrozenScope],
        adjustment: Adjustment,
        retry_of: Option<&str>,
    ) -> Result<AcceptedTask, TaskError> {
        if request_id.is_empty() {
            return Err(TaskError::EmptyRequestId);
        }
        if scopes.is_empty() {
            return Err(TaskError::EmptyPeriods);
        }
        if adjustment != Adjustment::Forward {
            return Err(TaskError::UnsupportedAdjustment);
        }
        let frozen_range = canonical_scopes(scopes);
        let mut symbols = Vec::new();
        let mut periods = Vec::new();
        for scope in &frozen_range {
            if !symbols.contains(&scope.symbol) {
                symbols.push(scope.symbol.clone());
            }
            if !periods.contains(&scope.period) {
                periods.push(scope.period);
            }
        }
        self.accept_binding(TaskBinding {
            request_id: request_id.to_owned(),
            task_id: String::new(),
            symbols,
            periods,
            adjustment,
            frozen_range,
            contract_version: CONTRACT_VERSION.to_owned(),
            retry_of: retry_of.map(str::to_owned),
        })
    }

    fn accept_binding(&self, binding: TaskBinding) -> Result<AcceptedTask, TaskError> {
        if let Some(existing) = self.store.get(&binding.request_id)? {
            if same_semantics(&existing, &binding) {
                self.store.verify_reusable(&existing)?;
                return Ok(AcceptedTask {
                    task_id: existing.task_id.clone(),
                    binding: existing,
                    reused: true,
                });
            }
            return Err(TaskError::RequestConflict {
                request_id: binding.request_id.clone(),
            });
        }
        let mut created = binding;
        created.task_id = next_task_id();
        let conflicts = self.store.active_conflicts(&created.frozen_range)?;
        if !conflicts.is_empty() {
            return Err(TaskError::RangeConflict { scopes: conflicts });
        }
        self.store.commit_acceptance(created.clone())?;
        Ok(AcceptedTask {
            task_id: created.task_id.clone(),
            binding: created,
            reused: false,
        })
    }

    pub fn store(&self) -> &TaskStore {
        &self.store
    }
}

#[derive(Debug, Clone)]
pub struct TaskRetryService {
    store: TaskStore,
    acceptance: TaskAcceptanceService,
}

impl TaskRetryService {
    pub fn new(store: TaskStore) -> Self {
        Self {
            acceptance: TaskAcceptanceService::new(store.clone()),
            store,
        }
    }

    pub fn retry_task(
        &self,
        original_task_id: &str,
        new_request_id: &str,
        selected_scopes: &[FrozenScope],
        adjustment: Adjustment,
    ) -> Result<AcceptedTask, TaskError> {
        let original =
            self.store
                .get_task(original_task_id)?
                .ok_or_else(|| TaskError::TaskNotFound {
                    task_id: original_task_id.to_owned(),
                })?;
        if original.request_id == new_request_id {
            return Err(TaskError::RetryRequestIdConflict);
        }
        if selected_scopes.is_empty() {
            return Err(TaskError::RetrySelectionEmpty);
        }
        let selected = canonical_scopes(selected_scopes);
        let execution = self.store.get_execution(original_task_id)?;
        let invalid = selected
            .iter()
            .filter(|scope| {
                !original.frozen_range.contains(scope)
                    || execution.as_ref().is_some_and(|record| {
                        record.scope_results.iter().any(|result| {
                            result.symbol == scope.symbol
                                && result.period == scope.period
                                && result.success
                        })
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        if !invalid.is_empty() {
            return Err(TaskError::RetrySelectionInvalid { scopes: invalid });
        }
        self.acceptance.accept_frozen(
            new_request_id,
            &selected,
            adjustment,
            Some(original_task_id),
        )
    }
}

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::PartiallyFailed | Self::Failed | Self::Cancelled
        )
    }

    pub fn occupies_range(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::Running | Self::CancelRequested | Self::NeedsVerification
        )
    }
}

#[derive(Debug, Clone)]
pub struct TaskCancellationService {
    store: TaskStore,
}

impl TaskCancellationService {
    pub fn new(store: TaskStore) -> Self {
        Self { store }
    }

    pub fn cancel_task(&self, task_id: &str) -> Result<CancellationResult, TaskError> {
        if self.store.get_task(task_id)?.is_none() {
            return Err(TaskError::TaskNotFound {
                task_id: task_id.to_owned(),
            });
        }
        let record = self
            .store
            .get_execution(task_id)?
            .unwrap_or(TaskExecutionRecord {
                task_id: task_id.to_owned(),
                status: TaskStatus::Pending,
                scope_results: Vec::new(),
            });
        let (result, changed) = match record.status {
            TaskStatus::Pending => {
                let cancelled = TaskExecutionRecord {
                    task_id: task_id.to_owned(),
                    status: TaskStatus::Cancelled,
                    scope_results: record.scope_results,
                };
                self.store.save_execution(cancelled.clone())?;
                self.store.release_occupancy(task_id)?;
                (cancelled, true)
            }
            TaskStatus::Running => {
                let requested = TaskExecutionRecord {
                    task_id: task_id.to_owned(),
                    status: TaskStatus::CancelRequested,
                    scope_results: record.scope_results,
                };
                self.store.save_execution(requested.clone())?;
                (requested, true)
            }
            TaskStatus::CancelRequested
            | TaskStatus::NeedsVerification
            | TaskStatus::Succeeded
            | TaskStatus::PartiallyFailed
            | TaskStatus::Failed
            | TaskStatus::Cancelled => (record, false),
        };
        Ok(CancellationResult {
            task_id: result.task_id,
            status: result.status,
            scope_results: result.scope_results,
            changed,
        })
    }
}

fn normalize(
    request_id: &str,
    symbols: &[&str],
    periods: &[Period],
    adjustment: Adjustment,
) -> Result<TaskBinding, TaskError> {
    if request_id.is_empty() {
        return Err(TaskError::EmptyRequestId);
    }
    if symbols.is_empty() {
        return Err(TaskError::EmptySymbols);
    }
    if periods.is_empty() {
        return Err(TaskError::EmptyPeriods);
    }
    if adjustment != Adjustment::Forward {
        return Err(TaskError::UnsupportedAdjustment);
    }
    let mut normalized_symbols: Vec<String> = symbols.iter().map(|s| (*s).to_owned()).collect();
    normalized_symbols.sort();
    normalized_symbols.dedup();
    let mut normalized_periods = periods.to_vec();
    normalized_periods.sort_by_key(|period| match period {
        Period::Day => 0,
        Period::Year => 1,
    });
    normalized_periods.dedup();
    let frozen_range = normalized_symbols
        .iter()
        .flat_map(|symbol| {
            normalized_periods.iter().map(move |period| FrozenScope {
                symbol: symbol.clone(),
                period: *period,
            })
        })
        .collect();
    Ok(TaskBinding {
        request_id: request_id.to_owned(),
        task_id: String::new(),
        symbols: normalized_symbols,
        periods: normalized_periods,
        adjustment,
        frozen_range,
        contract_version: CONTRACT_VERSION.to_owned(),
        retry_of: None,
    })
}

fn canonical_scopes(scopes: &[FrozenScope]) -> Vec<FrozenScope> {
    let mut canonical = scopes.to_vec();
    canonical.sort_by(|left, right| {
        left.symbol
            .cmp(&right.symbol)
            .then_with(|| period_order(left.period).cmp(&period_order(right.period)))
    });
    canonical.dedup();
    canonical
}

fn period_order(period: Period) -> u8 {
    match period {
        Period::Day => 0,
        Period::Year => 1,
    }
}

fn same_semantics(left: &TaskBinding, right: &TaskBinding) -> bool {
    left.request_id == right.request_id
        && left.symbols == right.symbols
        && left.periods == right.periods
        && left.adjustment == right.adjustment
        && left.frozen_range == right.frozen_range
        && left.contract_version == right.contract_version
        && left.retry_of == right.retry_of
}

static TASK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn next_task_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TASK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("task-{nanos:x}-{sequence:x}")
}
