use crate::market_data::{Adjustment, Period};
use serde::{Deserialize, Serialize};
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

impl TaskStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn get(&self, request_id: &str) -> Result<Option<TaskBinding>, io::Error> {
        Ok(self
            .read_all()?
            .into_iter()
            .find(|item| item.request_id == request_id))
    }

    pub fn list(&self) -> Result<Vec<TaskBinding>, io::Error> {
        self.read_all()
    }

    pub fn get_task(&self, task_id: &str) -> Result<Option<TaskBinding>, io::Error> {
        Ok(self
            .read_all()?
            .into_iter()
            .find(|item| item.task_id == task_id))
    }

    pub fn get_execution(&self, task_id: &str) -> Result<Option<TaskExecutionRecord>, io::Error> {
        let path = self.execution_path();
        if !path.exists() {
            return Ok(None);
        }
        let records: Vec<TaskExecutionRecord> =
            serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)?;
        Ok(records.into_iter().find(|item| item.task_id == task_id))
    }

    pub fn save_execution(&self, record: TaskExecutionRecord) -> Result<(), io::Error> {
        fs::create_dir_all(&self.root)?;
        let path = self.execution_path();
        let mut records = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?
        } else {
            Vec::new()
        };
        if let Some(existing) = records
            .iter_mut()
            .find(|item: &&mut TaskExecutionRecord| item.task_id == record.task_id)
        {
            *existing = record;
        } else {
            records.push(record);
        }
        fs::write(
            path,
            serde_json::to_vec_pretty(&records).map_err(io::Error::other)?,
        )
    }

    pub fn list_occupancy(&self) -> Result<Vec<OccupancyRecord>, io::Error> {
        let path = self.occupancy_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)
    }

    pub fn active_conflicts(
        &self,
        requested: &[FrozenScope],
    ) -> Result<Vec<FrozenScope>, io::Error> {
        let occupancy = self.list_occupancy()?;
        let mut conflicts = Vec::new();
        for scope in requested {
            let mut occupied = false;
            for record in &occupancy {
                let status = self
                    .get_execution(&record.task_id)?
                    .map(|execution| execution.status)
                    .unwrap_or(TaskStatus::Pending);
                if status.occupies_range() && record.scopes.contains(scope) {
                    occupied = true;
                    break;
                }
            }
            if occupied {
                conflicts.push(scope.clone());
            }
        }
        Ok(conflicts)
    }

    pub fn save_occupancy(&self, task_id: &str, scopes: &[FrozenScope]) -> Result<(), io::Error> {
        fs::create_dir_all(&self.root)?;
        let path = self.occupancy_path();
        let mut records = self.list_occupancy()?;
        if let Some(existing) = records.iter_mut().find(|item| item.task_id == task_id) {
            existing.scopes = scopes.to_vec();
        } else {
            records.push(OccupancyRecord {
                task_id: task_id.to_owned(),
                scopes: scopes.to_vec(),
            });
        }
        fs::write(
            path,
            serde_json::to_vec_pretty(&records).map_err(io::Error::other)?,
        )
    }

    pub fn release_occupancy(&self, task_id: &str) -> Result<(), io::Error> {
        let path = self.occupancy_path();
        if !path.exists() {
            return Ok(());
        }
        let mut records = self.list_occupancy()?;
        records.retain(|item| item.task_id != task_id);
        fs::write(
            path,
            serde_json::to_vec_pretty(&records).map_err(io::Error::other)?,
        )
    }

    fn read_all(&self) -> Result<Vec<TaskBinding>, io::Error> {
        let path = self.path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    fn append(&self, binding: TaskBinding) -> Result<(), io::Error> {
        fs::create_dir_all(&self.root)?;
        let mut items = self.read_all()?;
        items.push(binding);
        let payload = serde_json::to_vec_pretty(&items).map_err(io::Error::other)?;
        fs::write(self.path(), payload)
    }

    fn path(&self) -> PathBuf {
        self.root.join("task-bindings.json")
    }

    fn execution_path(&self) -> PathBuf {
        self.root.join("task-executions.json")
    }

    fn occupancy_path(&self) -> PathBuf {
        self.root.join("task-occupancy.json")
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
        self.store.append(created.clone())?;
        self.store.save_execution(TaskExecutionRecord {
            task_id: created.task_id.clone(),
            status: TaskStatus::Pending,
            scope_results: Vec::new(),
        })?;
        self.store
            .save_occupancy(&created.task_id, &created.frozen_range)?;
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
