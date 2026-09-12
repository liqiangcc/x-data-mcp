//! Protocol mapping only; read decisions live in query/observe applications.
use super::{Access, observe::Observer, query::Queries, registry::*, wire::*};
use crate::{
    market_storage::Error,
    mcp::{ApiError, ToolHandler},
    sqlite_market::SqliteMarket,
    sqlite_supply::SqliteSupply,
    sqlite_task_mcp::SqliteTaskApi,
    sqlite_tasks::SqliteTasks,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
pub struct DataApi {
    pub queries: Queries<SqliteSupply>,
    pub observer: Observer<SqliteSupply>,
    pub legacy: SqliteTaskApi,
    pub access: Access,
}
impl DataApi {
    pub fn new(
        tasks: SqliteTasks,
        run_id: String,
        access: Access,
    ) -> Result<Self, crate::task_control::Error> {
        let repository = SqliteSupply {
            market: SqliteMarket::new(tasks.clone()),
        };
        Ok(Self {
            queries: Queries::new(repository.clone()),
            observer: Observer::new(repository.clone(), run_id.clone()),
            legacy: SqliteTaskApi {
                control: crate::task_control::service::TaskControl::new(
                    tasks,
                    run_id,
                    vec![
                        Box::new(crate::task_control::legacy::LegacyCollection),
                        Box::new(super::collect::CollectionContract {
                            dataset: Dataset::Daily,
                            repository: repository.clone(),
                        }),
                        Box::new(super::collect::CollectionContract {
                            dataset: Dataset::Yearly,
                            repository: repository.clone(),
                        }),
                        Box::new(super::collect::CollectionContract {
                            dataset: Dataset::Catalog,
                            repository: repository.clone(),
                        }),
                    ],
                )?,
            },
            access,
        })
    }
    fn capabilities(&self, args: CapabilitiesQuery) -> Result<Value, ApiError> {
        if let Some(d) = args.dataset {
            self.access.require(d, None).map_err(mapped)?;
        }
        if args
            .operation
            .as_ref()
            .is_some_and(|op| !entries().iter().any(|e| e.name == op))
        {
            return Err(ApiError::new(
                "unsupported_operation",
                "operation not registered",
            ));
        }
        let mut items = vec![];
        for e in entries() {
            if args.operation.as_ref().is_some_and(|op| op != e.name) {
                continue;
            }
            let datasets = if matches!(
                e.handler,
                Handler::Query | Handler::Versions | Handler::Availability
            ) {
                self.access
                    .datasets
                    .iter()
                    .filter(|d| args.dataset.is_none_or(|a| a == **d))
                    .copied()
                    .map(Some)
                    .collect::<Vec<_>>()
            } else {
                vec![None]
            };
            for dataset in datasets {
                let detailed = args.operation.is_some();
                items.push(Capability {
                    tool_name: e.name.into(),
                    dataset,
                    contract_version: ContractVersion::V1,
                    implemented: true,
                    readiness: "ready".into(),
                    blocking_reasons: vec![],
                    input_schema: detailed.then(|| e.input.clone()),
                    output_schema: detailed.then(|| e.output.clone()),
                    example: detailed.then(|| e.example.clone()),
                    schema_digest: crate::hash::sha256(
                        serde_json::to_vec(&json!([e.input, e.output]))
                            .unwrap()
                            .as_slice(),
                    ),
                });
            }
        }
        encode(Capabilities {
            capability_revision: "generic-data-v1".into(),
            items,
            max_batch_entities: 100,
            max_page_size: 1000,
            max_snapshot_rows: 100000,
            max_snapshot_bytes: super::query::MAX_BYTES,
            snapshots_per_category: 32,
            production_ready: false,
        })
    }
    fn acceptance(&self, id: &str, a: crate::task_control::Acceptance) -> Result<Value, ApiError> {
        let crate::task_control::RequestOutcome::Task(f) = a.record.outcome else {
            return Err(ApiError::new(
                "unsupported_request_outcome",
                "task binding required",
            ));
        };
        encode(WriteAcceptance {
            request_id: id.into(),
            task_id: f.task.task_id.clone(),
            dataset: f.task.key.dataset.clone(),
            contract_version: f.task.key.contract_version.clone(),
            reused: a.reused,
            status: serde_json::from_value(
                serde_json::to_value(f.visible_status(&self.legacy.control.run_id)).unwrap(),
            )
            .unwrap(),
            item_ids: f.items.iter().map(|i| i.spec.item_id.clone()).collect(),
            readiness: "blocked".into(),
            blocking_reasons: crate::official_files::PRODUCTION_BLOCKERS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        })
    }
    fn legacy_call(&self, name: &str, input: Value) -> Result<Value, ApiError> {
        // Legacy full-control compatibility is intentionally local-owner only. Restricted
        // application contexts retain the read discovery/query operations above.
        if self.access != Access::local() {
            return Err(ApiError::new(
                "forbidden",
                "legacy control requires local-owner context",
            ));
        }
        if name == "start_collection" && input.get("dataset").is_some() {
            use crate::task_control::Contract;
            let command: Collection = decode(input)?;
            if command.request_id().trim().is_empty() {
                return Err(ApiError::new("empty_request_id", "request_id required"));
            }
            let contract = super::collect::CollectionContract {
                dataset: command.dataset(),
                repository: self.queries.repository.clone(),
            };
            let accepted = self
                .legacy
                .control
                .accept(command.request_id(), contract.key(), command.payload())
                .map_err(task_error)?;
            return self.acceptance(command.request_id(), accepted);
        }
        if name == "retry_task" && input.get("contract_version").is_some() {
            let a: RetryItems = decode(input)?;
            let accepted = self
                .legacy
                .control
                .retry(
                    &a.original_task_id,
                    &a.new_request_id,
                    a.item_ids.into_iter().collect(),
                )
                .map_err(task_error)?;
            return self.acceptance(&a.new_request_id, accepted);
        }
        self.legacy.call(name, input)
    }
}
fn decode<T: DeserializeOwned>(v: Value) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|e| ApiError::new("invalid_arguments", e.to_string()))
}
fn encode<T: Serialize>(v: T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::new("internal_error", e.to_string()))
}
fn mapped(e: Error) -> ApiError {
    let code = match &e {
        Error::Invalid(s) if s == "forbidden" => "forbidden",
        Error::Invalid(_) => "invalid_arguments",
        Error::Integrity(_) | Error::Base(_) => "integrity_error",
        Error::NotFound => "version_not_found",
        Error::Conflict(_) => "precondition_conflict",
        Error::OutcomeUnknown(_) => "outcome_unknown",
        Error::Task(crate::task_control::Error::TaskNotFound) => "task_not_found",
        _ => "storage_error",
    };
    ApiError::new(code, e.to_string())
}
impl ToolHandler for DataApi {
    fn tools(&self) -> Value {
        let mut tools = if self.access == Access::local() {
            self.legacy.tools().as_array().unwrap().clone()
        } else {
            vec![]
        };
        for t in &mut tools {
            let ty = match t["name"].as_str() {
                Some("start_collection") => {
                    Some(serde_json::to_value(schemars::schema_for!(Collection)).unwrap())
                }
                Some("retry_task") => {
                    Some(serde_json::to_value(schemars::schema_for!(RetryItems)).unwrap())
                }
                _ => None,
            };
            if let Some(mut ty) = ty {
                let old = t["inputSchema"].clone();
                let defs = ty.as_object_mut().unwrap().remove("$defs");
                t["inputSchema"] = json!({"oneOf":[old,ty]});
                if let Some(defs) = defs {
                    t["inputSchema"]["$defs"] = defs;
                }
            }
        }
        tools.extend(entries().into_iter().map(|e|json!({"name":e.name,"description":"Bounded SQLite v1 data capability. No implicit collection. Discover details via get_data_capabilities.","inputSchema":e.input,"outputSchema":e.output})));
        Value::Array(tools)
    }
    fn call(&self, name: &str, input: Value) -> Result<Value, ApiError> {
        let Some(entry) = entries().into_iter().find(|e| e.name == name) else {
            return self.legacy_call(name, input);
        };
        match entry.handler {
            Handler::Query => encode(
                self.queries
                    .query(decode::<Query>(input)?, &self.access)
                    .map_err(mapped)?,
            ),
            Handler::Versions => encode(
                self.queries
                    .versions(decode::<VersionsQuery>(input)?, &self.access)
                    .map_err(mapped)?,
            ),
            Handler::Availability => encode(
                self.observer
                    .availability(decode::<Query>(input)?, &self.access)
                    .map_err(mapped)?,
            ),
            Handler::Tasks => encode(
                self.observer
                    .tasks(decode::<TasksQuery>(input)?, &self.access)
                    .map_err(mapped)?,
            ),
            Handler::Progress => encode(
                self.observer
                    .progress(decode::<ProgressQuery>(input)?, &self.access)
                    .map_err(mapped)?,
            ),
            Handler::Capabilities => self.capabilities(decode(input)?),
        }
    }
}

fn task_error(e: crate::task_control::Error) -> ApiError {
    use crate::task_control::Error as E;
    let code = match &e {
        E::RequestConflict => "request_conflict",
        E::RangeConflict(_) => "range_conflict",
        E::RetryRequestIdConflict => "retry_request_id_conflict",
        E::EmptyRequestId => "empty_request_id",
        E::EmptyRetrySelection | E::RetrySelectionInvalid { .. } | E::InvalidRetrySelection => {
            "invalid_retry_selection"
        }
        E::InvalidCommand(_) => "invalid_arguments",
        E::OutcomeUnknown(_) => "outcome_unknown",
        _ => "storage_error",
    };
    ApiError::new(code, e.to_string())
}
