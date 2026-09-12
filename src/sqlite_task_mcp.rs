//! Explicit development adapter; the production default backend is unchanged.
use crate::task_control::{
    legacy::{LegacyCollection, scope_id},
    service::TaskControl,
    *,
};
use crate::{
    mcp::{
        ApiError, CancelTaskArgs, GetRequestArgs, GetTaskArgs, RetryTaskArgs, StartCollectionArgs,
        ToolHandler,
    },
    sqlite_tasks::SqliteTasks,
};
use serde::Deserialize;
use serde_json::{Value, json};
pub struct SqliteTaskApi {
    pub control: TaskControl<SqliteTasks>,
}
impl SqliteTaskApi {
    pub fn new(store: SqliteTasks, run_id: String) -> Result<Self, Error> {
        Ok(Self {
            control: TaskControl::new(store, run_id, vec![Box::new(LegacyCollection)])?,
        })
    }
    fn task_view(&self, s: &TaskSnapshot) -> Value {
        json!({"task_id":s.task.task_id,"status":s.visible_status(&self.control.run_id),"frozen_range":s.items.iter().map(|i|i.spec.target.clone()).collect::<Vec<_>>(),"scope_results":results(s),"retry_of":s.task.retry_of})
    }
    fn accepted(&self, a: Acceptance, retry: bool) -> Result<Value, Error> {
        let RequestOutcome::Task(s) = a.record.outcome else {
            return Err(Error::UnsupportedContract);
        };
        let mut v = json!({"task_id":s.task.task_id,"status":s.visible_status(&self.control.run_id),"frozen_range":s.items.iter().map(|i|i.spec.target.clone()).collect::<Vec<_>>(),"reused":a.reused});
        if retry {
            v["retry_of"] = json!(s.task.retry_of);
        }
        Ok(v)
    }
}
fn results(s: &TaskSnapshot) -> Vec<Value> {
    s.items
        .iter()
        .filter_map(|i| {
            i.outcome.as_ref().map(|o| {
                let mut v = i.spec.target.clone();
                v["success"] = json!(o.success);
                v["error"] = json!(o.error);
                v
            })
        })
        .collect()
}
fn args<T: DeserializeOwned>(v: Value) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|e| ApiError::new("invalid_arguments", e.to_string()))
}
use serde::de::DeserializeOwned;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn mapped(e: Error, request: bool) -> ApiError {
    let code = match &e {
        Error::InvalidCommand(_) => "invalid_arguments",
        Error::RequestConflict => "request_conflict",
        Error::RangeConflict(_) => "range_conflict",
        Error::TaskNotFound => "task_not_found",
        Error::InvalidRetrySelection | Error::RetrySelectionInvalid { .. } => {
            "invalid_retry_selection"
        }
        Error::RetryRequestIdConflict => "retry_request_id_conflict",
        Error::EmptyRetrySelection => "empty_selected_scopes",
        Error::EmptyRequestId => "empty_request_id",
        Error::NeedsVerification(_) if request => "request_needs_verification",
        _ => "storage_error",
    };
    let mut error = ApiError::new(code, e.to_string());
    let scopes = match e {
        Error::RangeConflict(scopes) => Some(scopes),
        Error::RetrySelectionInvalid { items } => Some(items.into_iter().collect()),
        _ => None,
    };
    if let Some(scopes) = scopes {
        let mut views: Vec<_> = scopes
            .iter()
            .filter_map(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .filter(|p| p.len() == 3 && p[0] == "market")
            .map(|p| crate::mcp::ScopeView {
                symbol: p[1].clone(),
                period: p[2].clone(),
            })
            .collect();
        views.sort_by(|a, b| (&a.symbol, &a.period).cmp(&(&b.symbol, &b.period)));
        error.scopes = Some(views);
    }

    error
}
struct BlockedSource;
impl Executor for BlockedSource {
    fn execute(&self, _key: &CommandKey, _item: &ItemSpec) -> ItemOutcome {
        ItemOutcome {
            success: false,
            error: Some(format!(
                "SOURCE_NOT_VERIFIED: {}",
                crate::official_files::PRODUCTION_BLOCKERS.join("; ")
            )),
        }
    }
}
impl ToolHandler for SqliteTaskApi {
    fn tools(&self) -> Value {
        let mut tools = crate::mcp::public_tools();
        tools.as_array_mut().unwrap().retain(|t| {
            matches!(
                t["name"].as_str(),
                Some(
                    "start_collection"
                        | "get_task"
                        | "get_request"
                        | "cancel_task"
                        | "retry_task"
                        | "run_worker_once"
                )
            )
        });
        for tool in tools.as_array_mut().unwrap() {
            tool["description"] = json!(format!(
                "Stage A SQLite development-only task control. {}",
                tool["description"].as_str().unwrap_or_default()
            ));
        }
        tools
    }
    fn call(&self, name: &str, input: Value) -> Result<Value, ApiError> {
        let result = match name {
            "start_collection" => {
                let a: StartCollectionArgs = args(input)?;
                if a.request_id.trim().is_empty() {
                    return Err(ApiError::new(
                        "empty_request_id",
                        "request_id must not be empty",
                    ));
                }
                if a.symbols.is_empty() {
                    return Err(ApiError::new("empty_symbols", "symbols must not be empty"));
                }
                if a.periods.is_empty() {
                    return Err(ApiError::new("empty_periods", "periods must not be empty"));
                }
                if a.periods
                    .iter()
                    .any(|p| !matches!(p.as_str(), "day" | "year"))
                {
                    return Err(ApiError::new("invalid_period", "unsupported period"));
                }
                self.control
                    .accept(
                        &a.request_id,
                        LegacyCollection.key(),
                        json!({"symbols":a.symbols,"periods":a.periods}),
                    )
                    .and_then(|a| self.accepted(a, false))
            }
            "get_task" => {
                let a: GetTaskArgs = args(input)?;
                self.control
                    .read_task(&a.task_id)
                    .map(|s| self.task_view(&s))
            }
            "get_request" => {
                let a: GetRequestArgs = args(input)?;
                if a.request_id.trim().is_empty() {
                    return Err(ApiError::new("invalid_arguments", "empty request_id"));
                }
                self.control.read_request(&a.request_id).map(|r|match r {
                    None=>json!({"request_id":a.request_id,"bound":false,"task":null}),
                    Some(r)=>match r.outcome{RequestOutcome::Task(s)=>json!({"request_id":a.request_id,"bound":true,"task":self.task_view(&s)}),RequestOutcome::Terminal(v)=>json!({"request_id":a.request_id,"bound":true,"task":null,"request_result":v})},
                })
            }
            "cancel_task" => {
                let a: CancelTaskArgs = args(input)?;
                self.control.read_task(&a.task_id).and_then(|before|{
                    self.control.cancel(&a.task_id,before.task.state_revision).map(|s|json!({"task_id":s.task.task_id,"status":s.visible_status(&self.control.run_id),"scope_results":results(&s),"changed":s.task.state_revision!=before.task.state_revision}))
                })
            }
            "retry_task" => {
                let a: RetryTaskArgs = args(input)?;
                // Decode periods before business validation, as the legacy Adapter does.
                if a.selected_scopes
                    .iter()
                    .any(|s| !matches!(s.period.as_str(), "day" | "year"))
                {
                    return Err(ApiError::new("invalid_period", "unsupported period"));
                }
                self.control
                    .retry(
                        &a.original_task_id,
                        &a.new_request_id,
                        a.selected_scopes
                            .iter()
                            .map(|s| scope_id(&s.symbol, &s.period))
                            .collect(),
                    )
                    .and_then(|a| self.accepted(a, true))
            }
            "run_worker_once" => {
                let _: Empty = args(input)?;
                self.control.run_once(&BlockedSource).map(|s|match s{None=>json!({"idle":true,"task_id":null,"status":null}),Some(s)=>json!({"idle":false,"task_id":s.task.task_id,"status":s.visible_status(&self.control.run_id)})})
            }
            _ => {
                return Err(ApiError::new(
                    "unknown_tool",
                    format!("unsupported development task tool: {name}"),
                ));
            }
        };
        result.map_err(|e| mapped(e, name == "get_request"))
    }
}
