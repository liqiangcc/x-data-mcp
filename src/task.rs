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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedTask {
    pub task_id: String,
    pub binding: TaskBinding,
    pub reused: bool,
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
        if let Some(existing) = self.store.get(request_id)? {
            if same_semantics(&existing, &binding) {
                return Ok(AcceptedTask {
                    task_id: existing.task_id.clone(),
                    binding: existing,
                    reused: true,
                });
            }
            return Err(TaskError::RequestConflict {
                request_id: request_id.to_owned(),
            });
        }
        let mut created = binding;
        created.task_id = next_task_id();
        self.store.append(created.clone())?;
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
    })
}

fn same_semantics(left: &TaskBinding, right: &TaskBinding) -> bool {
    left.request_id == right.request_id
        && left.symbols == right.symbols
        && left.periods == right.periods
        && left.adjustment == right.adjustment
        && left.frozen_range == right.frozen_range
        && left.contract_version == right.contract_version
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
