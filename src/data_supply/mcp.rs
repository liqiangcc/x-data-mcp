//! Protocol mapping only; read decisions live in query/observe applications.
use super::{
    Access,
    observe::{ObservationRepository, Observer},
    query::Queries,
    registry::*,
    wire::*,
};
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
                    tasks.clone(),
                    run_id.clone(),
                    vec![
                        #[cfg(target_os = "linux")]
                        Box::new(crate::verification_inspection::InspectionContract),
                        Box::new(crate::task_verification::StopVerification::new(
                            tasks.clone(), run_id.clone(),
                        )?),
                        Box::new(crate::publication_verification::admission::PublicationVerification::new(
                            tasks, run_id,
                        )?),
                        Box::new(crate::task_control::legacy::LegacyCollection),
                        Box::new(crate::market_update::tasks::RangeContract {
                            operation: crate::market_update::tasks::Operation::Update,
                            repository: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        }),
                        Box::new(crate::market_update::tasks::RangeContract {
                            operation: crate::market_update::tasks::Operation::Backfill,
                            repository: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        }),
                        Box::new(crate::yearly_publication::tasks::YearlyContract {
                            operation: crate::market_update::tasks::Operation::Update,
                            repository: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        }),
                        Box::new(crate::yearly_publication::tasks::YearlyContract {
                            operation: crate::market_update::tasks::Operation::Backfill,
                            repository: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        }),
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
            if matches!(e.handler, Handler::Compute)
                && (!self.access.allows(Dataset::Yearly, None)
                    || args.dataset.is_some_and(|d| d != Dataset::Yearly))
            {
                continue;
            }
            if self.access != Access::local()
                && matches!(
                    e.handler,
                    Handler::Collection
                        | Handler::Range(_)
                        | Handler::Retry
                        | Handler::Task
                        | Handler::Request
                        | Handler::Cancel
                        | Handler::Worker
                        | Handler::VerifyStopped
                        | Handler::VerifyPublication
                        | Handler::InspectVerification
                )
            {
                continue;
            }
            if args.operation.as_ref().is_some_and(|op| op != e.name) {
                continue;
            }
            let datasets = if matches!(e.handler, Handler::Range(_)) {
                [Dataset::Daily, Dataset::Yearly]
                    .into_iter()
                    .filter(|d| {
                        self.access.allows(*d, None) && args.dataset.is_none_or(|a| a == *d)
                    })
                    .map(Some)
                    .collect::<Vec<_>>()
            } else if matches!(
                e.handler,
                Handler::Query | Handler::Versions | Handler::Availability | Handler::Collection
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
                    description: if matches!(e.handler, Handler::InspectVerification) { INSPECTION_DESCRIPTION } else if matches!(e.handler, Handler::VerifyPublication) { crate::publication_verification::wire::DESCRIPTION } else if matches!(e.handler, Handler::VerifyStopped) { crate::task_verification::wire::DESCRIPTION } else if matches!(e.handler, Handler::Worker) { WORKER_DESCRIPTION } else if matches!(e.handler,Handler::Range(_)) { RANGE_DESCRIPTION } else { "Implemented interface; readiness does not imply published data or production source readiness." }.into(),
                    dataset,
                    contract_version: ContractVersion::V1,
                    implemented: true,
                    readiness: if matches!(e.handler, Handler::Collection | Handler::Range(_) | Handler::VerifyStopped) {
                        "blocked"
                    } else {
                        "ready"
                    }
                    .into(),
                    blocking_reasons: if matches!(e.handler, Handler::VerifyStopped) { vec![crate::task_verification::wire::BLOCKER.into()] } else if matches!(e.handler, Handler::Collection | Handler::Range(_)) {
                        crate::official_files::PRODUCTION_BLOCKERS
                            .iter()
                            .map(|s| s.to_string())
                            .collect()
                    } else {
                        vec![]
                    },
                    input_schema: detailed.then(|| e.input.clone()),
                    output_schema: detailed.then(|| e.output.clone()),
                    example: detailed.then(|| capability_example(&e, dataset)),
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
            readiness: if f.task.key == crate::publication_verification::admission::command_key() {
                "ready"
            } else {
                "blocked"
            }
            .into(),
            blocking_reasons: if f.task.key
                == crate::publication_verification::admission::command_key()
            {
                vec![]
            } else if f.task.key.dataset == crate::task_verification::DATASET {
                vec![crate::task_verification::wire::BLOCKER.into()]
            } else {
                crate::official_files::PRODUCTION_BLOCKERS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            },
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
        if input.get("contract_version").is_some() {
            match name {
                "get_task" => {
                    let a: TaskV1 = decode(input)?;
                    let f = self
                        .observer
                        .repository
                        .task(&a.task_id, &self.access)
                        .map_err(mapped)?;
                    return encode(super::observe::summary(&f, &self.observer.run_id));
                }
                "get_request" => {
                    let a: RequestV1 = decode(input)?;
                    let record = self
                        .legacy
                        .control
                        .read_request(&a.request_id)
                        .map_err(task_error)?;
                    let mut terminal_result = None;
                    let task = match record {
                        None => None,
                        Some(r) => match r.outcome {
                            crate::task_control::RequestOutcome::Task(s) => {
                                Some(super::observe::summary(
                                    &super::observe::TaskFacts {
                                        request_id: a.request_id.clone(),
                                        snapshot: *s,
                                        publications: vec![],
                                        range_results: vec![],
                                        yearly_results: vec![],
                                    },
                                    &self.observer.run_id,
                                ))
                            }
                            crate::task_control::RequestOutcome::Terminal(value) => {
                                terminal_result =
                                    Some(serde_json::from_value(value).map_err(|e| {
                                        ApiError::new(
                                            "unsupported_request_outcome",
                                            format!("terminal contract: {e}"),
                                        )
                                    })?);
                                None
                            }
                        },
                    };
                    return encode(RequestView {
                        request_id: a.request_id,
                        bound: task.is_some() || terminal_result.is_some(),
                        task,
                        terminal_result,
                    });
                }
                "cancel_task" => {
                    let a: CancelV1 = decode(input)?;
                    let s = self
                        .legacy
                        .control
                        .cancel(&a.task_id, a.expected_revision)
                        .map_err(task_error)?;
                    return encode(Cancellation {
                        task_id: s.task.task_id.clone(),
                        status: serde_json::from_value(
                            serde_json::to_value(s.visible_status(&self.observer.run_id)).unwrap(),
                        )
                        .unwrap(),
                        state_revision: s.task.state_revision,
                    });
                }
                _ => {}
            }
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
    if let Error::Task(task) = e {
        return task_error(task);
    }
    let code = match &e {
        Error::Invalid(s) if s == "forbidden" => "forbidden",
        Error::Invalid(_) => "invalid_arguments",
        Error::Integrity(_) | Error::Base(_) => "integrity_error",
        Error::NotFound => "version_not_found",
        Error::Conflict(_) => "precondition_conflict",
        Error::OutcomeUnknown(_) => "outcome_unknown",
        _ => "storage_error",
    };
    ApiError::new(code, e.to_string())
}
impl DataApi {
    fn public_tools(&self) -> Value {
        let mut tools = if self.access == Access::local() {
            self.legacy.tools().as_array().unwrap().clone()
        } else {
            vec![]
        };
        for t in &mut tools {
            if t["name"] == "run_worker_once" {
                let e = entries()
                    .into_iter()
                    .find(|e| e.name == "run_worker_once")
                    .unwrap();
                t["description"] = json!(WORKER_DESCRIPTION);
                t["inputSchema"] = e.input;
                t["outputSchema"] = e.output;
            }
            let ty = match t["name"].as_str() {
                Some("start_collection") => {
                    Some(serde_json::to_value(schemars::schema_for!(Collection)).unwrap())
                }
                Some("retry_task") => {
                    Some(serde_json::to_value(schemars::schema_for!(RetryItems)).unwrap())
                }
                Some("get_task") => {
                    Some(serde_json::to_value(schemars::schema_for!(TaskV1)).unwrap())
                }
                Some("get_request") => {
                    Some(serde_json::to_value(schemars::schema_for!(RequestV1)).unwrap())
                }
                Some("cancel_task") => {
                    Some(serde_json::to_value(schemars::schema_for!(CancelV1)).unwrap())
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
        tools.extend(entries().into_iter().filter(|e| self.access == Access::local() || !matches!(e.handler,Handler::Range(_)|Handler::VerifyStopped|Handler::VerifyPublication|Handler::InspectVerification)).filter(|e|!matches!(e.handler,Handler::Collection|Handler::Retry|Handler::Task|Handler::Request|Handler::Cancel|Handler::Worker)).map(|e|json!({"name":e.name,"description":if matches!(e.handler,Handler::InspectVerification){INSPECTION_DESCRIPTION}else if matches!(e.handler,Handler::VerifyPublication){crate::publication_verification::wire::DESCRIPTION}else if matches!(e.handler,Handler::VerifyStopped){crate::task_verification::wire::DESCRIPTION}else if matches!(e.handler,Handler::Range(_)){RANGE_DESCRIPTION}else{"Bounded SQLite v1 data capability. No implicit collection. Discover details via get_data_capabilities."},"inputSchema":e.input,"outputSchema":e.output})));
        Value::Array(tools)
    }
    fn dispatch(&self, name: &str, input: Value) -> Result<Value, ApiError> {
        if input
            .get("dataset")
            .is_some_and(|dataset| serde_json::from_value::<Dataset>(dataset.clone()).is_err())
        {
            return Err(ApiError::new(
                "unknown_dataset",
                "dataset is not registered",
            ));
        }
        if input.get("contract_version").is_some_and(|v| v != "v1") {
            return Err(ApiError::new(
                "unsupported_contract",
                "unsupported contract version",
            ));
        }
        let Some(entry) = entries().into_iter().find(|e| e.name == name) else {
            return self.legacy_call(name, input);
        };
        match entry.handler {
            Handler::InspectVerification => {
                if self.access != Access::local() {
                    return Err(ApiError::new(
                        "forbidden",
                        "verification inspection requires local owner access",
                    ));
                }
                #[cfg(target_os = "linux")]
                {
                    let command: InspectVerification = decode(input)?;
                    let accepted = crate::verification_inspection::service::inspect(
                        &self.legacy.control.repository,
                        &command.request_id,
                        &command.target_task_id,
                        &self.legacy.control.run_id,
                        &self.access.principal,
                    )
                    .map_err(task_error)?;
                    let crate::task_control::RequestOutcome::Terminal(value) =
                        accepted.record.outcome
                    else {
                        return Err(ApiError::new(
                            "needs_verification",
                            "bounded inspection has an invalid task outcome",
                        ));
                    };
                    encode(RequestView {
                        request_id: command.request_id,
                        bound: true,
                        task: None,
                        terminal_result: Some(serde_json::from_value(value).map_err(|e| {
                            ApiError::new(
                                "needs_verification",
                                format!("inspection terminal contract: {e}"),
                            )
                        })?),
                    })
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Err(ApiError::new(
                        "unsupported_contract",
                        "verification runtime protocol requires Linux",
                    ))
                }
            }
            Handler::VerifyPublication => {
                if self.access != Access::local() {
                    return Err(ApiError::new(
                        "forbidden",
                        "verification requires local owner access",
                    ));
                }
                let command: crate::publication_verification::wire::VerifyPublication =
                    decode(input)?;
                let accepted = crate::publication_verification::existing::accept(
                    &self.legacy.control,
                    &command.request_id,
                    &command.target_task_id,
                )
                .map_err(task_error)?;
                if let crate::task_control::RequestOutcome::Terminal(value) =
                    &accepted.record.outcome
                {
                    let terminal_result = serde_json::from_value(value.clone()).map_err(|e| {
                        ApiError::new("internal_error", format!("terminal contract: {e}"))
                    })?;
                    encode(RequestView {
                        request_id: command.request_id,
                        bound: true,
                        task: None,
                        terminal_result: Some(terminal_result),
                    })
                } else {
                    self.acceptance(&command.request_id, accepted)
                }
            }
            Handler::VerifyStopped => {
                if self.access != Access::local() {
                    return Err(ApiError::new(
                        "forbidden",
                        "verification requires local owner access",
                    ));
                }
                let command: crate::task_verification::wire::VerifyStopped = decode(input)?;
                let accepted = crate::task_verification::existing::accept(
                    &self.legacy.control,
                    &command.request_id,
                    &command.target_task_id,
                )
                .map_err(task_error)?;
                if let crate::task_control::RequestOutcome::Terminal(value) =
                    &accepted.record.outcome
                {
                    let terminal_result = serde_json::from_value(value.clone()).map_err(|e| {
                        ApiError::new("internal_error", format!("terminal contract: {e}"))
                    })?;
                    encode(RequestView {
                        request_id: command.request_id,
                        bound: true,
                        task: None,
                        terminal_result: Some(terminal_result),
                    })
                } else {
                    self.acceptance(&command.request_id, accepted)
                }
            }
            Handler::Range(operation) => {
                if self.access != Access::local() {
                    return Err(ApiError::new(
                        "forbidden",
                        "write controls require local owner access",
                    ));
                }
                let command: RangeWrite = decode(input)?;
                let accepted = self
                    .legacy
                    .control
                    .accept(
                        command.request_id(),
                        command.key(operation),
                        command.payload(),
                    )
                    .map_err(task_error)?;
                self.acceptance(command.request_id(), accepted)
            }
            Handler::Compute => {
                let computed = crate::metrics::service::Metrics {
                    repository: crate::sqlite_metrics::SqliteMetrics {
                        market: self.queries.repository.market.clone(),
                    },
                }
                .compute(decode(input)?, &self.access)
                .map_err(mapped)?;
                if let crate::metrics::wire::Outcome::NotComputable { reason, .. } =
                    &computed.result
                {
                    let mut error = ApiError::new("not_computable", format!("{reason:?}"));
                    error.details = Some(Box::new(serde_json::to_value(computed).unwrap()));
                    Err(error)
                } else {
                    encode(computed)
                }
            }
            Handler::Worker => {
                if self.access != Access::local() {
                    return Err(ApiError::new(
                        "forbidden",
                        "worker requires local owner access",
                    ));
                }
                let _: WorkerInput = decode(input)?;
                let snapshot =
                    super::worker::run_once(&self.legacy.control, &self.queries.repository.market)
                        .map_err(mapped)?;
                encode(WorkerOutput {
                    idle: snapshot.is_none(),
                    task_id: snapshot.as_ref().map(|s| s.task.task_id.clone()),
                    status: snapshot.as_ref().map(|s| {
                        serde_json::from_value(
                            serde_json::to_value(s.visible_status(&self.observer.run_id)).unwrap(),
                        )
                        .unwrap()
                    }),
                })
            }
            Handler::Collection
            | Handler::Retry
            | Handler::Task
            | Handler::Request
            | Handler::Cancel => self.legacy_call(name, input),
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
        E::TaskNotFound => "task_not_found",
        E::UnsupportedContract => "unsupported_contract",
        E::NeedsVerification(_) => "needs_verification",
        E::RequestConflict => "request_conflict",
        E::RevisionConflict => "precondition_conflict",
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

impl ToolHandler for DataApi {
    fn tools(&self) -> Value {
        self.public_tools()
    }
    fn call(&self, name: &str, input: Value) -> Result<Value, ApiError> {
        let generic = input.get("contract_version").is_some()
            || input.get("dataset").is_some()
            || matches!(
                name,
                "query_data"
                    | "list_data_versions"
                    | "get_data_availability"
                    | "list_tasks"
                    | "get_task_progress"
                    | "get_data_capabilities"
                    | "compute_metrics"
                    | "verify_publication_result"
                    | "inspect_verification_task"
                    | "update_data"
                    | "backfill_data"
            );
        self.dispatch(name, input).map_err(|mut e| {
            if generic {
                e.outcome_certainty = Some(
                    if e.code == "outcome_unknown"
                        || (matches!(
                            name,
                            "update_data"
                                | "backfill_data"
                                | "verify_publication_result"
                                | "inspect_verification_task"
                        ) && matches!(e.code.as_str(), "storage_error" | "needs_verification"))
                    {
                        "unknown"
                    } else if matches!(
                        name,
                        "start_collection"
                            | "retry_task"
                            | "cancel_task"
                            | "update_data"
                            | "backfill_data"
                            | "verify_publication_result"
                            | "inspect_verification_task"
                    ) {
                        "not_committed"
                    } else {
                        "not_applicable"
                    }
                    .into(),
                );
                e.next_actions = Some(
                    if e.code == "outcome_unknown"
                        || (matches!(
                            name,
                            "update_data"
                                | "backfill_data"
                                | "verify_publication_result"
                                | "inspect_verification_task"
                        ) && matches!(e.code.as_str(), "storage_error" | "needs_verification"))
                    {
                        vec![
                        "inspect the same request_id/task_id; do not invent a replacement identity"
                            .into(),
                    ]
                    } else {
                        vec!["correct the request or inspect the reported storage evidence".into()]
                    },
                );
            }
            e
        })
    }
}

fn capability_example(e: &Entry, dataset: Option<Dataset>) -> Value {
    let mut v = e.example.clone();
    let Some(d) = dataset else {
        return v;
    };
    v["dataset"] = serde_json::to_value(d).unwrap();
    if d == Dataset::Catalog {
        v["selection"] = if matches!(e.handler, Handler::Collection) {
            json!({"scope":"current_sh_sz_a"})
        } else {
            json!({"kind":"catalog","scope":"current_sh_sz_a"})
        };
        if v.get("filter").is_some() {
            v["filter"] = json!({"kind":"catalog"});
        }
    } else if d == Dataset::Yearly {
        if matches!(e.handler, Handler::Range(_)) {
            v["items"] = json!([{"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":"published-daily-id","years":[2025],"publication":{"mode":"historical_fixed_base","comparison_version_id":null}}]);
        }
        if v.get("filter").is_some() {
            v["filter"] = json!({"kind":"yearly","start_year":2025,"end_year":2025});
        }
        if matches!(e.handler, Handler::Collection) {
            v["base_daily_versions"] = json!([{"entity":{"exchange":"sh","code":"600006"},"version_id":"published-daily-id"}]);
            v["years"] = json!([2025]);
        }
    }
    v
}
