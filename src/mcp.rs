use crate::{
    market_data::{Adjustment, MarketDataSource, Period},
    task::{FrozenScope, ScopeExecutionResult, TaskError, TaskStatus, TaskStore},
    worker::{Worker, WorkerError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

pub const TOOL_NAMES: [&str; 3] = ["start_collection", "get_task", "run_worker_once"];

#[derive(Debug, Clone, Deserialize)]
pub struct StartCollectionArgs {
    pub request_id: String,
    pub symbols: Vec<String>,
    pub periods: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetTaskArgs {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeView {
    pub symbol: String,
    pub period: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeResultView {
    pub symbol: String,
    pub period: String,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskView {
    pub task_id: String,
    pub status: String,
    pub frozen_range: Vec<ScopeView>,
    pub scope_results: Vec<ScopeResultView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartCollectionResult {
    pub task_id: String,
    pub status: String,
    pub frozen_range: Vec<ScopeView>,
    pub reused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkerRunResult {
    pub idle: bool,
    pub task_id: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

pub struct McpApi<S> {
    acceptance: crate::task::TaskAcceptanceService,
    store: TaskStore,
    worker: Worker<S>,
}

impl<S: MarketDataSource> McpApi<S> {
    pub fn new(source: S, store: TaskStore) -> Self {
        Self {
            acceptance: crate::task::TaskAcceptanceService::new(store.clone()),
            worker: Worker::new(source, store.clone()),
            store,
        }
    }

    pub fn start_collection(
        &self,
        args: StartCollectionArgs,
    ) -> Result<StartCollectionResult, ApiError> {
        let periods = args
            .periods
            .iter()
            .map(|period| parse_period(period))
            .collect::<Result<Vec<_>, _>>()?;
        let symbols = args.symbols.iter().map(String::as_str).collect::<Vec<_>>();
        let accepted = self
            .acceptance
            .accept(&args.request_id, &symbols, &periods, Adjustment::Forward)
            .map_err(task_error)?;
        let status = self
            .store
            .get_execution(&accepted.task_id)
            .map_err(|error| ApiError::new("storage_error", error.to_string()))?
            .map(|record| record.status)
            .unwrap_or(TaskStatus::Pending);
        Ok(StartCollectionResult {
            task_id: accepted.task_id,
            status: status_name(status),
            frozen_range: accepted
                .binding
                .frozen_range
                .iter()
                .map(scope_view)
                .collect(),
            reused: accepted.reused,
        })
    }

    pub fn get_task(&self, args: GetTaskArgs) -> Result<TaskView, ApiError> {
        let binding = self
            .store
            .get_task(&args.task_id)
            .map_err(|error| ApiError::new("storage_error", error.to_string()))?
            .ok_or_else(|| ApiError::new("task_not_found", "task was not found"))?;
        let execution = self
            .store
            .get_execution(&args.task_id)
            .map_err(|error| ApiError::new("storage_error", error.to_string()))?;
        Ok(TaskView {
            task_id: binding.task_id,
            status: status_name(
                execution
                    .as_ref()
                    .map(|record| record.status)
                    .unwrap_or(TaskStatus::Pending),
            ),
            frozen_range: binding.frozen_range.iter().map(scope_view).collect(),
            scope_results: execution
                .map(|record| record.scope_results.iter().map(scope_result_view).collect())
                .unwrap_or_default(),
        })
    }

    pub fn run_worker_once(&self) -> Result<WorkerRunResult, ApiError> {
        let step = self.worker.run_once().map_err(worker_error)?;
        Ok(match step {
            Some(step) => WorkerRunResult {
                idle: false,
                task_id: Some(step.task_id),
                status: Some(status_name(step.status)),
            },
            None => WorkerRunResult {
                idle: true,
                task_id: None,
                status: None,
            },
        })
    }
}

fn parse_period(value: &str) -> Result<Period, ApiError> {
    match value {
        "day" => Ok(Period::Day),
        "year" => Ok(Period::Year),
        _ => Err(ApiError::new(
            "invalid_period",
            format!("unsupported period: {value}"),
        )),
    }
}

fn status_name(status: TaskStatus) -> String {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Succeeded => "succeeded",
        TaskStatus::PartiallyFailed => "partially_failed",
        TaskStatus::Failed => "failed",
    }
    .into()
}

fn scope_view(scope: &FrozenScope) -> ScopeView {
    ScopeView {
        symbol: scope.symbol.clone(),
        period: match scope.period {
            Period::Day => "day",
            Period::Year => "year",
        }
        .into(),
    }
}

fn scope_result_view(result: &ScopeExecutionResult) -> ScopeResultView {
    ScopeResultView {
        symbol: result.symbol.clone(),
        period: match result.period {
            Period::Day => "day",
            Period::Year => "year",
        }
        .into(),
        success: result.success,
        error: result.error.clone(),
    }
}

fn task_error(error: TaskError) -> ApiError {
    match error {
        TaskError::EmptyRequestId => ApiError::new("empty_request_id", error.to_string()),
        TaskError::EmptySymbols => ApiError::new("empty_symbols", error.to_string()),
        TaskError::EmptyPeriods => ApiError::new("empty_periods", error.to_string()),
        TaskError::UnsupportedAdjustment => {
            ApiError::new("unsupported_adjustment", error.to_string())
        }
        TaskError::RequestConflict { .. } => ApiError::new("request_conflict", error.to_string()),
        TaskError::Storage(_) => ApiError::new("storage_error", error.to_string()),
    }
}

fn worker_error(error: WorkerError) -> ApiError {
    match error {
        WorkerError::Storage(error) => ApiError::new("storage_error", error.to_string()),
        WorkerError::Execution(error) => ApiError::new("execution_error", error.to_string()),
    }
}

pub fn public_tools() -> Value {
    json!([
        {
            "name": "start_collection",
            "description": "Accept a collection task with explicit symbols and periods.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["request_id", "symbols", "periods"],
                "properties": {
                    "request_id": {"type": "string"},
                    "symbols": {"type": "array", "items": {"type": "string"}},
                    "periods": {"type": "array", "items": {"type": "string", "enum": ["day", "year"]}}
                }
            }
        },
        {
            "name": "get_task",
            "description": "Read a task status, frozen range, and scope results.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["task_id"],
                "properties": {"task_id": {"type": "string"}}
            }
        },
        {
            "name": "run_worker_once",
            "description": "Process at most one pending or recovering task.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }
        }
    ])
}

pub fn run_stdio<S: MarketDataSource>(api: McpApi<S>) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        if let Some(response) = handle_json(&api, &line) {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

pub fn handle_json<S: MarketDataSource>(api: &McpApi<S>, input: &str) -> Option<String> {
    let request: Value = match serde_json::from_str(input) {
        Ok(request) => request,
        Err(error) => {
            return Some(
                serde_json::to_string(&rpc_error(
                    Value::Null,
                    -32700,
                    "parse_error",
                    error.to_string(),
                ))
                .unwrap(),
            );
        }
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.starts_with("notifications/") && request.get("id").is_none() {
        return None;
    }
    let response = match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "x-data-mcp", "version": "0.1.0"}
            }
        }),
        "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"tools": public_tools()}}),
        "tools/call" => {
            handle_tool_call(api, id, request.get("params").cloned().unwrap_or_default())
        }
        _ => rpc_error(
            id,
            -32601,
            "method_not_found",
            format!("unsupported method: {method}"),
        ),
    };
    Some(serde_json::to_string(&response).unwrap())
}

fn handle_tool_call<S: MarketDataSource>(api: &McpApi<S>, id: Value, params: Value) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "invalid_tool_call", "missing tool name");
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let result = match name {
        "start_collection" => serde_json::from_value::<StartCollectionArgs>(args)
            .map_err(|error| ApiError::new("invalid_arguments", error.to_string()))
            .and_then(|args| {
                api.start_collection(args)
                    .map(|value| serde_json::to_value(value).unwrap())
            }),
        "get_task" => serde_json::from_value::<GetTaskArgs>(args)
            .map_err(|error| ApiError::new("invalid_arguments", error.to_string()))
            .and_then(|args| {
                api.get_task(args)
                    .map(|value| serde_json::to_value(value).unwrap())
            }),
        "run_worker_once" => api
            .run_worker_once()
            .map(|value| serde_json::to_value(value).unwrap()),
        _ => Err(ApiError::new(
            "unknown_tool",
            format!("unsupported tool: {name}"),
        )),
    };
    match result {
        Ok(value) => tool_result(id, value, false),
        Err(error) => tool_result(id, serde_json::to_value(error).unwrap(), true),
    }
}

fn tool_result(id: Value, value: Value, is_error: bool) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{"type": "text", "text": serde_json::to_string(&value).unwrap()}],
            "structuredContent": value,
            "isError": is_error
        }
    })
}

fn rpc_error(
    id: Value,
    rpc_code: i64,
    code: impl Into<String>,
    message: impl Into<String>,
) -> Value {
    let code = code.into();
    let message = message.into();
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": rpc_code,
            "message": message,
            "data": {"code": code, "message": message}
        }
    })
}
