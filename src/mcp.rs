use crate::{
    market_data::{Adjustment, MarketDataPipeline, Period},
    task::{
        CancellationResult, FrozenScope, ScopeExecutionResult, TaskCancellationService, TaskError,
        TaskRetryService, TaskStatus, TaskStore,
    },
    worker::{Worker, WorkerError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

pub const TOOL_NAMES: [&str; 8] = [
    "start_collection",
    "get_task",
    "run_worker_once",
    "cancel_task",
    "retry_task",
    "get_market_data",
    "list_market_data_versions",
    "get_request",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartCollectionArgs {
    pub request_id: String,
    pub symbols: Vec<String>,
    pub periods: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetTaskArgs {
    pub task_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelTaskArgs {
    pub task_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryScopeArgs {
    pub symbol: String,
    pub period: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryTaskArgs {
    pub original_task_id: String,
    pub new_request_id: String,
    pub selected_scopes: Vec<RetryScopeArgs>,
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
    pub retry_of: Option<String>,
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
pub struct CancelTaskResult {
    pub task_id: String,
    pub status: String,
    pub scope_results: Vec<ScopeResultView>,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetryTaskResult {
    pub task_id: String,
    pub status: String,
    pub frozen_range: Vec<ScopeView>,
    pub retry_of: Option<String>,
    pub reused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<ScopeView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome_certainty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_actions: Option<Vec<String>>,
}

impl ApiError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            scopes: None,
            outcome_certainty: None,
            next_actions: None,
        }
    }

    fn with_scopes(mut self, scopes: Vec<ScopeView>) -> Self {
        self.scopes = Some(scopes);
        self
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetRequestArgs {
    pub request_id: String,
}

pub struct McpApi<S> {
    query: crate::query::QueryService,
    acceptance: crate::task::TaskAcceptanceService,
    cancellation: TaskCancellationService,
    retry: TaskRetryService,
    store: TaskStore,
    worker: Worker<S>,
}

impl<S: MarketDataPipeline> McpApi<S> {
    pub fn new(source: S, store: TaskStore) -> Self {
        Self {
            query: crate::query::QueryService::new(crate::storage::JsonFileStore::new(
                store.root(),
            )),
            acceptance: crate::task::TaskAcceptanceService::new(store.clone()),
            cancellation: TaskCancellationService::new(store.clone()),
            retry: TaskRetryService::new(store.clone()),
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
            .map(|record| visible_status(record.status))
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
                    .map(|record| visible_status(record.status))
                    .unwrap_or(TaskStatus::Pending),
            ),
            frozen_range: binding.frozen_range.iter().map(scope_view).collect(),
            scope_results: execution
                .map(|record| record.scope_results.iter().map(scope_result_view).collect())
                .unwrap_or_default(),
            retry_of: binding.retry_of,
        })
    }

    pub fn get_request(&self, args: GetRequestArgs) -> Result<Value, ApiError> {
        if args.request_id.trim().is_empty() {
            return Err(ApiError::new("invalid_arguments", "empty request_id"));
        }
        let binding = self
            .store
            .get(&args.request_id)
            .map_err(|e| ApiError::new("storage_error", e.to_string()))?;
        let Some(binding) = binding else {
            return Ok(json!({"request_id":args.request_id,"bound":false,"task":null}));
        };
        self.store
            .verify_reusable(&binding)
            .map_err(|e| ApiError::new("request_needs_verification", e.to_string()))?;
        let task = self.get_task(GetTaskArgs {
            task_id: binding.task_id,
        })?;
        Ok(json!({"request_id":args.request_id,"bound":true,"task":task}))
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

    pub fn cancel_task(&self, args: CancelTaskArgs) -> Result<CancelTaskResult, ApiError> {
        let result = self
            .cancellation
            .cancel_task(&args.task_id)
            .map_err(task_error)?;
        Ok(cancel_result(result))
    }

    pub fn retry_task(&self, args: RetryTaskArgs) -> Result<RetryTaskResult, ApiError> {
        let selected_scopes = args
            .selected_scopes
            .iter()
            .map(|scope| {
                Ok(FrozenScope {
                    symbol: scope.symbol.clone(),
                    period: parse_period(&scope.period)?,
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        let accepted = self
            .retry
            .retry_task(
                &args.original_task_id,
                &args.new_request_id,
                &selected_scopes,
                Adjustment::Forward,
            )
            .map_err(task_error)?;
        Ok(RetryTaskResult {
            task_id: accepted.task_id,
            status: status_name(TaskStatus::Pending),
            frozen_range: accepted
                .binding
                .frozen_range
                .iter()
                .map(scope_view)
                .collect(),
            retry_of: accepted.binding.retry_of,
            reused: accepted.reused,
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
        TaskStatus::CancelRequested => "cancel_requested",
        TaskStatus::NeedsVerification => "needs_verification",
        TaskStatus::Succeeded => "succeeded",
        TaskStatus::PartiallyFailed => "partially_failed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
    .into()
}

fn visible_status(status: TaskStatus) -> TaskStatus {
    if status == TaskStatus::Running {
        TaskStatus::NeedsVerification
    } else {
        status
    }
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

fn cancel_result(result: CancellationResult) -> CancelTaskResult {
    CancelTaskResult {
        task_id: result.task_id,
        status: status_name(result.status),
        scope_results: result.scope_results.iter().map(scope_result_view).collect(),
        changed: result.changed,
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
        TaskError::RangeConflict { scopes } => ApiError::new(
            "range_conflict",
            "requested range is busy; no task was accepted",
        )
        .with_scopes(scopes.iter().map(scope_view).collect()),
        TaskError::TaskNotFound { .. } => ApiError::new("task_not_found", error.to_string()),
        TaskError::RetryRequestIdConflict => {
            ApiError::new("retry_request_id_conflict", error.to_string())
        }
        TaskError::RetrySelectionEmpty => ApiError::new("empty_selected_scopes", error.to_string()),
        TaskError::RetrySelectionInvalid { scopes } => ApiError::new(
            "invalid_retry_selection",
            "retry selected scopes are invalid, outside the original range, or already completed",
        )
        .with_scopes(scopes.iter().map(scope_view).collect()),
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
    let mut tools = json!([
        {
            "name": "start_collection",
            "description": "Accept local official-file processing: day is QFQ daily; year derives from one published QFQ daily version. Production source is NOT_VERIFIED.",
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
        },
        {
            "name": "cancel_task",
            "description": "Request cancellation of a pending or running task.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["task_id"],
                "properties": {"task_id": {"type": "string"}}
            }
        },
        {
            "name": "retry_task",
            "description": "Retry explicitly selected unfinished scopes as a new linked task.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["original_task_id", "new_request_id", "selected_scopes"],
                "properties": {
                    "original_task_id": {"type": "string"},
                    "new_request_id": {"type": "string"},
                    "selected_scopes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["symbol", "period"],
                            "properties": {
                                "symbol": {"type": "string"},
                                "period": {"type": "string", "enum": ["day", "year"]}
                            }
                        }
                    }
                }
            }
        }
    ]);
    for (name, dates) in [
        ("get_market_data", true),
        ("list_market_data_versions", false),
    ] {
        let mut properties = json!({
            "exchange":{"type":"string","enum":["sh","sz"]},
            "code":{"type":"string","pattern":"^[0-9]{6}$"},
            "period":{"type":"string","enum":["day","year"]},
            "page_size":{"type":"integer","minimum":1,"maximum":1000,"default":100},
            "cursor":{"type":"string"}
        });
        let mut required = vec!["exchange", "code", "period"];
        if dates {
            properties["start_date"] = json!({"type":"string","format":"date"});
            properties["end_date"] = json!({"type":"string","format":"date"});
            properties["version_id"] = json!({"type":"string"});
            required.extend(["start_date", "end_date"]);
        }
        tools.as_array_mut().unwrap().push(json!({"name":name,
            "description":"Read integrity-checked published storage only. No collection, repair or source fallback. Cursors fix the version and query within this process.",
            "inputSchema":{"type":"object","additionalProperties":false,"required":required,"properties":properties}}));
    }
    tools.as_array_mut().unwrap().push(json!({"name":"get_request",
        "description":"Read the actual request_id binding and task state without accepting work.",
        "inputSchema":{"type":"object","additionalProperties":false,"required":["request_id"],"properties":{"request_id":{"type":"string"}}}}));
    tools
}

pub fn run_stdio<H: ToolHandler>(api: H) -> io::Result<()> {
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

pub fn handle_json<H: ToolHandler>(api: &H, input: &str) -> Option<String> {
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
        "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"tools": api.tools()}}),
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

fn handle_tool_call<H: ToolHandler>(api: &H, id: Value, params: Value) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "invalid_tool_call", "missing tool name");
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let result = api.call(name, args);
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

fn query_error(error: crate::query::QueryError) -> ApiError {
    let code = match &error {
        crate::query::QueryError::Invalid(_) => "invalid_arguments",
        crate::query::QueryError::Cursor => "invalid_cursor",
        crate::query::QueryError::Unavailable(_) => "published_data_unavailable",
    };
    ApiError::new(code, error.to_string())
}

pub trait ToolHandler {
    fn tools(&self) -> Value;
    fn call(&self, name: &str, args: Value) -> Result<Value, ApiError>;
}
impl<S: MarketDataPipeline> ToolHandler for McpApi<S> {
    fn tools(&self) -> Value {
        public_tools()
    }
    fn call(&self, name: &str, args: Value) -> Result<Value, ApiError> {
        match name {
            "start_collection" => serde_json::from_value::<StartCollectionArgs>(args)
                .map_err(|error| ApiError::new("invalid_arguments", error.to_string()))
                .and_then(|args| {
                    self.start_collection(args)
                        .map(|value| serde_json::to_value(value).unwrap())
                }),
            "get_task" => serde_json::from_value::<GetTaskArgs>(args)
                .map_err(|error| ApiError::new("invalid_arguments", error.to_string()))
                .and_then(|args| {
                    self.get_task(args)
                        .map(|value| serde_json::to_value(value).unwrap())
                }),
            "run_worker_once" => serde_json::from_value::<EmptyArgs>(args)
                .map_err(|e| ApiError::new("invalid_arguments", e.to_string()))
                .and_then(|_| {
                    self.run_worker_once()
                        .map(|value| serde_json::to_value(value).unwrap())
                }),
            "get_market_data" => serde_json::from_value::<crate::query::MarketQuery>(args)
                .map_err(|e| ApiError::new("invalid_arguments", e.to_string()))
                .and_then(|args| {
                    self.query
                        .market(args, chrono::Utc::now())
                        .map_err(query_error)
                }),
            "list_market_data_versions" => {
                serde_json::from_value::<crate::query::VersionQuery>(args)
                    .map_err(|e| ApiError::new("invalid_arguments", e.to_string()))
                    .and_then(|args| {
                        self.query
                            .versions(args, chrono::Utc::now())
                            .map_err(query_error)
                    })
            }
            "get_request" => serde_json::from_value::<GetRequestArgs>(args)
                .map_err(|e| ApiError::new("invalid_arguments", e.to_string()))
                .and_then(|args| self.get_request(args)),
            "cancel_task" => serde_json::from_value::<CancelTaskArgs>(args)
                .map_err(|error| ApiError::new("invalid_arguments", error.to_string()))
                .and_then(|args| {
                    self.cancel_task(args)
                        .map(|value| serde_json::to_value(value).unwrap())
                }),
            "retry_task" => serde_json::from_value::<RetryTaskArgs>(args)
                .map_err(|error| ApiError::new("invalid_arguments", error.to_string()))
                .and_then(|args| {
                    self.retry_task(args)
                        .map(|value| serde_json::to_value(value).unwrap())
                }),
            _ => Err(ApiError::new(
                "unknown_tool",
                format!("unsupported tool: {name}"),
            )),
        }
    }
}
