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
            observer: Observer::new(repository, run_id.clone()),
            legacy: SqliteTaskApi::new(tasks, run_id)?,
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
    fn legacy_call(&self, name: &str, input: Value) -> Result<Value, ApiError> {
        // Legacy full-control compatibility is intentionally local-owner only. Restricted
        // application contexts retain the read discovery/query operations above.
        if self.access != Access::local() {
            return Err(ApiError::new(
                "forbidden",
                "legacy control requires local-owner context",
            ));
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
