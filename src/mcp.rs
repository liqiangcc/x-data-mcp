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
    pub outcome_certainty: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<Value>>,
}

impl ApiError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            scopes: None,
            outcome_certainty: None,
            details: None,
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

/// MCP clients require the root of every advertised `inputSchema`/
/// `outputSchema` to declare `"type": "object"`. Schemars emits a bare
/// `oneOf`/`anyOf` root for internally tagged enums, so the schema handed to
/// `tools/list` is normalized here. Some clients additionally complete an
/// object root that declares no `properties` with `properties: {}` and
/// `additionalProperties: false`, which then rejects every field of a
/// `oneOf`-rooted input. Such a root therefore also publishes the union of
/// its branches' properties and the keys every branch requires; the branches
/// themselves stay in place, so discrimination is unchanged. The registry
/// descriptor itself is left untouched: dispatch decodes through serde, never
/// through these schemas, so tool behavior is unchanged. A non-object schema,
/// or one whose root already carries a `type`/`properties`, keeps them.
type JsonMap = serde_json::Map<String, Value>;

pub fn ensure_object_root(schema: Value) -> Value {
    let Value::Object(mut map) = schema else {
        return schema;
    };
    map.entry("type")
        .or_insert_with(|| Value::String("object".into()));
    if !map.contains_key("properties")
        && let Some((properties, required)) = merged_branch_shape(&map)
    {
        map.insert("properties".into(), Value::Object(properties));
        map.entry("required").or_insert(Value::Array(required));
    }
    Value::Object(map)
}

/// The object schema a `oneOf`/`anyOf` branch stands for: the branch itself,
/// or the `#/$defs/<name>` definition it points at.
fn resolve_branch<'a>(branch: &'a Value, root: &'a JsonMap) -> Option<&'a JsonMap> {
    let branch = branch.as_object()?;
    match branch.get("$ref") {
        Some(Value::String(reference)) => root
            .get("$defs")?
            .get(reference.strip_prefix("#/$defs/")?)?
            .as_object(),
        _ => Some(branch),
    }
}

/// Union of the properties of every `oneOf`/`anyOf` branch plus the keys all
/// branches require. `None` unless every branch resolves to an object schema
/// with inline `properties` (anything else is left exactly as published).
fn merged_branch_shape(root: &JsonMap) -> Option<(JsonMap, Vec<Value>)> {
    let mut branches = Vec::new();
    for key in ["oneOf", "anyOf"] {
        if let Some(Value::Array(items)) = root.get(key) {
            branches.extend(items.iter());
        }
    }
    if branches.is_empty() {
        return None;
    }
    let mut variants: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    let mut common: Option<std::collections::BTreeSet<String>> = None;
    for branch in branches {
        let object = resolve_branch(branch, root)?;
        for (name, schema) in object.get("properties")?.as_object()? {
            let seen = variants.entry(name.clone()).or_default();
            if !seen.contains(schema) {
                seen.push(schema.clone());
            }
        }
        let required: std::collections::BTreeSet<String> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|keys| {
                keys.iter()
                    .filter_map(|key| key.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        common = Some(match common {
            None => required,
            Some(seen) => seen.intersection(&required).cloned().collect(),
        });
    }
    let properties = variants
        .into_iter()
        .map(|(name, mut schemas)| {
            let schema = if schemas.len() == 1 {
                schemas.remove(0)
            } else {
                serde_json::json!({ "anyOf": schemas })
            };
            (name, schema)
        })
        .collect();
    let required = common
        .unwrap_or_default()
        .into_iter()
        .map(Value::String)
        .collect();
    Some((properties, required))
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
                "serverInfo": {"name": "x-data-mcp", "version": env!("CARGO_PKG_VERSION")}
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
    let mode = api.text_mode();
    match result {
        Ok(value) => tool_result(id, value, false, name, mode),
        Err(error) => tool_result(id, serde_json::to_value(error).unwrap(), true, name, mode),
    }
}

/// How the model-visible `content[0].text` is built (v8 §5.4, AC-V8-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMode {
    /// `text` is the same compact JSON as `structuredContent` (research, and
    /// the stdio/dev surface).
    Full,
    /// Payloads above `SUMMARY_FULL_LIMIT` get a bounded summary; small
    /// payloads keep the full JSON.
    Summary,
    /// No `content` text at all; local throughput scripts only.
    Off,
}

impl TextMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full" => Some(Self::Full),
            "summary" => Some(Self::Summary),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// Entry default plus the `X_DATA_MCP_TEXT_MODE` override. Research is the
/// only entry whose clients may see text alone (ChatGPT/Claude), so it keeps
/// the full JSON; ops/maint are local/Agent surfaces and get the summary.
pub fn resolve_text_mode(entry: Option<&str>, env: Option<&str>) -> TextMode {
    if let Some(mode) = env.and_then(TextMode::parse) {
        return mode;
    }
    match entry {
        Some("ops") | Some("maint") => TextMode::Summary,
        _ => TextMode::Full,
    }
}

/// Payloads at or below this size are cheaper to send whole than to describe.
const SUMMARY_FULL_LIMIT: usize = 2048;
/// Upper bound for one summary line. The summary is a pointer, never a
/// second copy of the payload.
const SUMMARY_LIMIT: usize = 300;

fn tool_result(id: Value, value: Value, is_error: bool, name: &str, mode: TextMode) -> Value {
    let content = match mode {
        TextMode::Full => {
            json!([{"type": "text", "text": serde_json::to_string(&value).unwrap()}])
        }
        TextMode::Summary => {
            let full = serde_json::to_string(&value).unwrap();
            if !is_error && full.len() <= SUMMARY_FULL_LIMIT {
                json!([{"type": "text", "text": full}])
            } else {
                json!([{
                    "type": "text",
                    "text": serde_json::to_string(&bounded_summary(name, &value, is_error)).unwrap()
                }])
            }
        }
        TextMode::Off => json!([]),
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": content,
            "structuredContent": value,
            "isError": is_error
        }
    })
}

/// Bounded summary carrying the fields a poller/Skill already depends on:
/// `status`, counters and `next_cursor`, plus the request/task references.
fn bounded_summary(name: &str, value: &Value, is_error: bool) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("ok".into(), json!(!is_error));
    out.insert("tool".into(), json!(name));
    if is_error {
        out.insert("status".into(), json!("error"));
        if let Some(code) = value.get("code").and_then(Value::as_str) {
            out.insert("error".into(), json!(code));
        }
    } else {
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .get("task")
                    .and_then(|task| task.get("status"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("ok");
        out.insert("status".into(), json!(status));
    }
    let mut counts = serde_json::Map::new();
    for key in [
        "total",
        "finished",
        "succeeded",
        "failed",
        "pending",
        "running",
        "item_count",
        "row_count",
        "record_count",
    ] {
        if let Some(number) = value.get(key).and_then(Value::as_u64) {
            counts.insert(key.into(), json!(number));
        }
    }
    if let Some(map) = value.get("counts").and_then(Value::as_object) {
        for (key, number) in map {
            if let Some(number) = number.as_u64() {
                counts.insert(key.clone(), json!(number));
            }
        }
    }
    for key in ["items", "records", "unfinished"] {
        if let Some(items) = value.get(key).and_then(Value::as_array) {
            counts.insert("page_items".into(), json!(items.len()));
            break;
        }
    }
    if !counts.is_empty() {
        out.insert("counts".into(), Value::Object(counts));
    }
    if let Some(cursor) = value.get("next_cursor").and_then(Value::as_str) {
        out.insert("next_cursor".into(), json!(cursor));
    }
    let mut refs = serde_json::Map::new();
    for key in [
        "task_id",
        "request_id",
        "generation_id",
        "plan_id",
        "snapshot_id",
        "target_task_id",
    ] {
        let direct = value.get(key).and_then(Value::as_str);
        let nested = value
            .get("task")
            .and_then(|task| task.get(key))
            .and_then(Value::as_str);
        if let Some(reference) = direct.or(nested) {
            refs.insert(key.into(), json!(reference));
        }
    }
    if !refs.is_empty() {
        out.insert("refs".into(), Value::Object(refs));
    }
    out.insert("detail".into(), json!("see structuredContent"));
    // Deterministic trim: counters first (they are the least specific), then
    // refs, then the cursor; the status stays.
    let mut guard = 0;
    while summary_bytes(&out) > SUMMARY_LIMIT && guard < 64 {
        guard += 1;
        let mut trimmed = false;
        if let Some(Value::Object(counts)) = out.get_mut("counts") {
            if let Some(last) = counts.keys().next_back().cloned() {
                counts.remove(&last);
                trimmed = true;
            }
        }
        if !trimmed
            && let Some(Value::Object(refs)) = out.get_mut("refs")
            && let Some(last) = refs.keys().next_back().cloned()
        {
            refs.remove(&last);
            trimmed = true;
        }
        if !trimmed && out.remove("next_cursor").is_some() {
            trimmed = true;
        }
        if !trimmed && out.remove("detail").is_some() {
            trimmed = true;
        }
        if !trimmed {
            break;
        }
    }
    if matches!(out.get("counts"), Some(Value::Object(map)) if map.is_empty()) {
        out.remove("counts");
    }
    if matches!(out.get("refs"), Some(Value::Object(map)) if map.is_empty()) {
        out.remove("refs");
    }
    Value::Object(out)
}

type JsonObject = serde_json::Map<String, Value>;

fn summary_bytes(summary: &JsonObject) -> usize {
    serde_json::to_string(summary)
        .map(|text| text.len())
        .unwrap_or(usize::MAX)
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
    /// Model-visible text policy for this surface; the default keeps the
    /// pre-v8 behavior (full JSON in `content[0].text`).
    fn text_mode(&self) -> TextMode {
        TextMode::Full
    }
    /// Background worker entry point. It is service-internal, not a published
    /// tool: implementations may bypass their per-entry tool surface. The
    /// default keeps the old behavior for handlers that expose it directly.
    fn worker_once(&self) -> Result<Value, ApiError> {
        self.call("run_worker_once", json!({}))
    }
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

#[cfg(test)]
mod tests {
    use super::ensure_object_root;
    use serde_json::json;

    #[test]
    fn object_root_is_added_only_when_missing() {
        // No root type: a type is inserted and every other key survives.
        let any_of = json!({"anyOf": [{"type": "object"}], "title": "t"});
        let normalized = ensure_object_root(any_of.clone());
        assert_eq!(normalized["type"], "object");
        assert_eq!(normalized["anyOf"], any_of["anyOf"]);
        assert_eq!(normalized["title"], "t");
        // An existing root type is never overwritten.
        assert_eq!(
            ensure_object_root(json!({"type": "object", "properties": {}})),
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(
            ensure_object_root(json!({"type": "string"})),
            json!({"type": "string"})
        );
        // Non-object schemas pass through untouched.
        assert_eq!(ensure_object_root(json!(true)), json!(true));
        assert_eq!(ensure_object_root(json!(["a"])), json!(["a"]));
        assert_eq!(ensure_object_root(json!(null)), json!(null));
    }

    #[test]
    fn tagged_union_root_publishes_the_union_of_its_branches() {
        let schema = json!({
            "oneOf": [
                {"type": "object", "additionalProperties": false,
                 "properties": {"dataset": {"const": "a"}, "id": {"type": "string"},
                                "x": {"type": "integer"}},
                 "required": ["dataset", "id"]},
                {"type": "object", "additionalProperties": false,
                 "properties": {"dataset": {"const": "b"}, "id": {"type": "string"},
                                "x": {"type": "string"}},
                 "required": ["dataset", "id", "x"]}
            ],
            "title": "T"
        });
        let normalized = ensure_object_root(schema.clone());
        assert_eq!(normalized["type"], "object");
        // Branches are untouched, so a client still discriminates on them.
        assert_eq!(normalized["oneOf"], schema["oneOf"]);
        let properties = normalized["properties"].as_object().unwrap();
        assert_eq!(properties.len(), 3);
        // A property both branches declare identically keeps its schema; one
        // they declare differently becomes the alternatives.
        assert_eq!(properties["id"], json!({"type": "string"}));
        assert_eq!(
            properties["x"],
            json!({"anyOf": [{"type": "integer"}, {"type": "string"}]})
        );
        assert_eq!(
            properties["dataset"],
            json!({"anyOf": [{"const": "a"}, {"const": "b"}]})
        );
        // Only the keys every branch requires are required at the root.
        assert_eq!(normalized["required"], json!(["dataset", "id"]));
    }

    #[test]
    fn referenced_branches_are_resolved_and_unresolvable_ones_leave_the_root_alone() {
        let with_refs = json!({
            "$defs": {"A": {"type": "object", "properties": {"a": {"type": "string"}},
                            "required": ["a"]},
                      "B": {"type": "object", "properties": {"b": {"type": "string"}}}},
            "anyOf": [{"$ref": "#/$defs/A"}, {"$ref": "#/$defs/B"}]
        });
        let normalized = ensure_object_root(with_refs);
        assert_eq!(normalized["properties"].as_object().unwrap().len(), 2);
        assert_eq!(normalized["required"], json!([]));
        // A branch with no inline properties cannot be summarized.
        let opaque = ensure_object_root(json!({"oneOf": [{"type": "object"}]}));
        assert!(opaque.get("properties").is_none());
        // A root that already declares properties is never rewritten.
        let declared = json!({"type": "object", "oneOf": [
            {"properties": {"z": {}}}], "properties": {"y": {}}});
        assert_eq!(ensure_object_root(declared.clone()), declared);
    }
}
