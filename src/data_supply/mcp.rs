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
    market_source: Option<crate::official_market::TdxMarketSource>,
    generation: Option<super::generation::GenerationSource>,
    #[cfg(target_os = "linux")]
    worker_gate: Option<std::sync::Arc<crate::local_runtime::WorkerGate>>,
}
impl DataApi {
    pub fn new(
        tasks: SqliteTasks,
        run_id: String,
        access: Access,
    ) -> Result<Self, crate::task_control::Error> {
        Self::new_in(
            tasks,
            None,
            run_id,
            access,
            super::generation::GenerationSource::from_env(),
        )
    }
    /// Same wiring as `new` but takes a pre-opened market backend — the
    /// long-lived service holds the database lease, so the compact backend
    /// must be assembled once with `MarketBackend::open_shared` and shared
    /// across connections instead of re-acquiring the flock per call. The
    /// generation source is likewise shared: cloning it into every
    /// connection keeps the verified-session cache process-scoped instead
    /// of per-connection.
    pub fn new_with_backend(
        tasks: SqliteTasks,
        backend: crate::sqlite_market::backend::MarketBackend,
        run_id: String,
        access: Access,
        generation: Option<super::generation::GenerationSource>,
    ) -> Result<Self, crate::task_control::Error> {
        Self::new_in(tasks, Some(backend), run_id, access, generation)
    }
    /// Same wiring as `new` with an explicit generation source (tests and
    /// deployment assembly): source-reading contracts freeze
    /// `source_generation_id` at acceptance and worker sessions open
    /// pinned generation directories.
    pub fn new_with_generation(
        tasks: SqliteTasks,
        run_id: String,
        access: Access,
        generation: super::generation::GenerationSource,
    ) -> Result<Self, crate::task_control::Error> {
        Self::new_in(tasks, None, run_id, access, Some(generation))
    }
    fn new_in(
        tasks: SqliteTasks,
        backend: Option<crate::sqlite_market::backend::MarketBackend>,
        run_id: String,
        access: Access,
        generation: Option<super::generation::GenerationSource>,
    ) -> Result<Self, crate::task_control::Error> {
        let repository = SqliteSupply {
            market: match backend {
                Some(backend) => backend,
                None => crate::sqlite_market::backend::MarketBackend::open(&tasks)
                    .map_err(|e| crate::task_control::Error::StorageUnavailable(e.to_string()))?,
            },
        };
        // Wraps the contracts whose executors read official files so every
        // accepted item freezes source_generation_id at acceptance.
        let bind = |contract: Box<dyn crate::task_control::Contract>| match &generation {
            Some(source) => Box::new(super::generation::Bound {
                inner: contract,
                source: source.clone(),
            }) as Box<dyn crate::task_control::Contract>,
            None => contract,
        };
        Ok(Self {
            queries: Queries::new(repository.clone()),
            observer: Observer::new(repository.clone(), run_id.clone()),
            legacy: SqliteTaskApi {
                control: crate::task_control::service::TaskControl::new(
                    repository.market.tasks(),
                    run_id.clone(),
                    vec![
                        #[cfg(target_os = "linux")]
                        Box::new(crate::verification_inspection::InspectionContract),
                        Box::new(crate::task_verification::StopVerification::new(
                            repository.market.tasks(), run_id.clone(),
                        )?),
                        Box::new(crate::publication_verification::admission::PublicationVerification::new(
                            repository.market.tasks(), run_id,
                        )?),
                        Box::new(crate::task_control::legacy::LegacyCollection),
                        bind(Box::new(crate::market_update::tasks::RangeContract {
                            operation: crate::market_update::tasks::Operation::Update,
                            repository: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        })),
                        bind(Box::new(crate::market_update::tasks::RangeContract {
                            operation: crate::market_update::tasks::Operation::Backfill,
                            repository: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        })),
                        bind(Box::new(crate::market_update::tasks::RangeContract {
                            operation: crate::market_update::tasks::Operation::Republish,
                            repository: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        })),
                        bind(Box::new(super::default_range::DefaultRangeContract {
                            operation: crate::market_update::tasks::Operation::Update,
                            resolver: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        })),
                        bind(Box::new(super::default_range::DefaultRangeContract {
                            operation: crate::market_update::tasks::Operation::Backfill,
                            resolver: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        })),
                        bind(Box::new(crate::yearly_publication::tasks::YearlyContract {
                            operation: crate::market_update::tasks::Operation::Update,
                            repository: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        })),
                        bind(Box::new(crate::yearly_publication::tasks::YearlyContract {
                            operation: crate::market_update::tasks::Operation::Backfill,
                            repository: repository.market.clone(),
                            today: crate::calendar::shanghai_today,
                        })),
                        bind(Box::new(crate::gap_verification::contract::GapVerificationContract {
                            store: repository.market.clone(),
                        })),
                        // Plan-run tasks pin the generation recorded in the
                        // verified plan — stamping it here (not via `Bound`)
                        // keeps the item binding identical to what the
                        // digest check covered at accept time.
                        Box::new(super::plan::PlanRunContract {
                            dataset: "market.daily",
                        }),
                        Box::new(super::plan::PlanRunContract {
                            dataset: "market.yearly",
                        }),
                        bind(Box::new(super::collect::CollectionContract {
                            dataset: Dataset::Daily,
                            repository: repository.clone(),
                        })),
                        Box::new(super::collect::CollectionContract {
                            dataset: Dataset::Yearly,
                            repository: repository.clone(),
                        }),
                        bind(Box::new(super::collect::CollectionContract {
                            dataset: Dataset::Catalog,
                            repository: repository.clone(),
                        })),
                    ],
                )?,
            },
            access,
            market_source: crate::official_market::TdxMarketSource::from_env(),
            generation,
            #[cfg(target_os = "linux")]
            worker_gate: None,
        })
    }
    /// Override the env-installed official-file snapshot source (tests).
    /// Only the market.daily collection item uses it; absent env installs
    /// none, so the BlockedSource refusal is unchanged.
    pub fn with_market_source(mut self, source: crate::official_market::TdxMarketSource) -> Self {
        self.market_source = Some(source);
        self
    }

    #[cfg(target_os = "linux")]
    pub fn with_worker_gate(
        mut self,
        gate: std::sync::Arc<crate::local_runtime::WorkerGate>,
    ) -> Self {
        self.worker_gate = Some(gate);
        self
    }
    /// Source readiness fact shared by capability and acceptance checks:
    /// generation mode requires a verified active generation; legacy mode
    /// requires the inspected-env snapshot.
    fn source_ready(&self) -> bool {
        match &self.generation {
            Some(g) => g.active_verified_id().is_some(),
            None => self.market_source.is_some(),
        }
    }
    fn source_block(&self) -> BlockingReason {
        match &self.generation {
            Some(_) => BlockingReason {
                code: "verified_source_absent".into(),
                detail: "no verified active generation; executions record per-item source refusals and never advance heads (acquire+verify+activate through x-data-acquire)".into(),
                evidence_ref: None,
            },
            None => BlockingReason {
                code: "verified_source_absent".into(),
                detail: "no installed inspected official snapshot; executions record per-item source refusals and never advance heads (X_DATA_MCP_OFFICIAL_MANIFEST + X_DATA_MCP_OFFICIAL_DIR with a passing manifest)".into(),
                evidence_ref: None,
            },
        }
    }
    /// Source-dependent runtime readiness: an installed inspected official
    /// snapshot (daily writes, gap verification) or a configured catalog
    /// input location (catalog collection). Facts only; absent source is a
    /// structured block, never a hidden failure.
    fn runtime_block(&self, e: &Entry, dataset: Option<Dataset>) -> Option<BlockingReason> {
        let needs_inspected = matches!(
            (e.handler, dataset),
            (Handler::Collection, Some(Dataset::Daily))
                | (Handler::Range(_), Some(Dataset::Daily))
                | (Handler::Republish, Some(Dataset::Daily))
                | (Handler::VerifyGap, _)
        );
        let needs_catalog_input = matches!(
            (e.handler, dataset),
            (Handler::Collection, Some(Dataset::Catalog))
        );
        if needs_inspected && !self.source_ready() {
            return Some(self.source_block());
        }
        if needs_catalog_input && !self.source_ready() {
            return Some(match &self.generation {
                Some(_) => self.source_block(),
                None => BlockingReason {
                    code: "catalog_source_unconfigured".into(),
                    detail: "official input location is not configured; catalog execution keeps the source refusal".into(),
                    evidence_ref: None,
                },
            });
        }
        None
    }
    /// Acceptance-time readiness for a persisted command key: the same
    /// source facts as the capability matrix, keyed on the frozen task.
    fn task_runtime_block(&self, key: &crate::task_control::CommandKey) -> Option<BlockingReason> {
        let daily_write = key.dataset == "market.daily"
            && matches!(
                key.operation.as_str(),
                "start_collection" | "update_data" | "backfill_data" | "republish_data"
            );
        if (daily_write || key.dataset == crate::gap_verification::DATASET) && !self.source_ready()
        {
            return Some(self.source_block());
        }
        if key.dataset == "securities.catalog" && !self.source_ready() {
            return Some(match &self.generation {
                Some(_) => self.source_block(),
                None => BlockingReason {
                    code: "catalog_source_unconfigured".into(),
                    detail: "official input location is not configured; catalog execution keeps the source refusal".into(),
                    evidence_ref: None,
                },
            });
        }
        None
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
            if self.access != Access::local() && e.requires_local() {
                continue;
            }
            if args.operation.as_ref().is_some_and(|op| op != e.name) {
                continue;
            }
            for dataset in e.datasets(&self.access) {
                if dataset.is_some() && args.dataset.is_some_and(|a| Some(a) != dataset) {
                    continue;
                }
                let detailed = args.operation.is_some();
                let block = self.runtime_block(&e, dataset);
                items.push(Capability {
                    tool_name: e.name.into(),
                    description: e.description.into(),
                    dataset,
                    contract_version: ContractVersion::V1,
                    implemented: true,
                    operator_only: e.operator_only,
                    runtime_readiness: if block.is_some() {
                        Readiness::Blocked
                    } else {
                        Readiness::Ready
                    },
                    governance_readiness: Readiness::Ready,
                    blocking_reasons: block.into_iter().collect(),
                    declared_scope: crate::official_market::DECLARED_SCOPE.into(),
                    active_generation_ref: self
                        .generation
                        .as_ref()
                        .and_then(|g| g.active_verified_id()),
                    input_schema: detailed.then(|| crate::mcp::ensure_object_root(e.input.clone())),
                    output_schema: detailed
                        .then(|| crate::mcp::ensure_object_root(e.output.clone())),
                    example: detailed.then(|| capability_example(&e, dataset)),
                    schema_digest: crate::hash::sha256(
                        serde_json::to_vec(&json!([e.input, e.output]))
                            .unwrap()
                            .as_slice(),
                    ),
                });
            }
        }
        let mut production_blocking_reasons = vec![BlockingReason {
            code: "declared_scope_non_production".into(),
            detail: crate::official_market::DECLARED_SCOPE.into(),
            evidence_ref: None,
        }];
        if self.generation.is_none() {
            production_blocking_reasons.push(BlockingReason {
                code: "generation_root_unconfigured".into(),
                detail: "X_DATA_MCP_GENERATION_ROOT is not configured; source bindings use the legacy inspected-env path rather than immutable generation sessions".into(),
                evidence_ref: None,
            });
        } else if !self.source_ready() {
            production_blocking_reasons.push(self.source_block());
        }
        encode(Capabilities {
            capability_revision: "generic-data-v7".into(),
            items,
            max_batch_entities: 100,
            max_page_size: 1000,
            max_snapshot_rows: 100000,
            max_snapshot_bytes: super::query::MAX_BYTES,
            snapshots_per_category: 32,
            declared_scope: crate::official_market::DECLARED_SCOPE.into(),
            production_ready: false,
            production_blocking_reasons,
        })
    }
    fn acceptance(&self, id: &str, a: crate::task_control::Acceptance) -> Result<Value, ApiError> {
        let crate::task_control::RequestOutcome::Task(f) = a.record.outcome else {
            return Err(ApiError::new(
                "unsupported_request_outcome",
                "task binding required",
            ));
        };
        let block = self.task_runtime_block(&f.task.key);
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
            runtime_readiness: if block.is_some() {
                Readiness::Blocked
            } else {
                Readiness::Ready
            },
            blocking_reasons: block.into_iter().collect(),
        })
    }
    /// `run_market_update` acceptance for both fresh admissions and request
    /// replays: plan identity and counts come from the task's shared
    /// evidence, never re-derived after the fact.
    fn plan_acceptance(
        &self,
        request_id: &str,
        reused: bool,
        f: &crate::task_control::TaskSnapshot,
        item_count: u64,
    ) -> Result<Value, ApiError> {
        let evidence = f.task.work.shared_evidence.as_ref();
        let block = self.task_runtime_block(&f.task.key);
        encode(PlanRunAcceptance {
            request_id: request_id.into(),
            task_id: f.task.task_id.clone(),
            dataset: f.task.key.dataset.clone(),
            contract_version: f.task.key.contract_version.clone(),
            plan_id: evidence
                .and_then(|e| e["plan_id"].as_str())
                .unwrap_or_default()
                .to_string(),
            reused,
            status: serde_json::from_value(
                serde_json::to_value(f.visible_status(&self.legacy.control.run_id)).unwrap(),
            )
            .unwrap(),
            item_count,
            plan_counts: evidence
                .and_then(|e| serde_json::from_value(e["counts"].clone()).ok())
                .unwrap_or(PlanCounts {
                    eligible: item_count,
                    already_current: 0,
                    absent: 0,
                    ineligible: 0,
                }),
            runtime_readiness: if block.is_some() {
                Readiness::Blocked
            } else {
                Readiness::Ready
            },
            blocking_reasons: block.into_iter().collect(),
        })
    }
    /// Typed control calls only. The retired untyped task shapes are not
    /// advertised and are not decoded; every control input carries its
    /// contract_version/dataset in the registry schema.
    fn control_call(&self, name: &str, input: Value) -> Result<Value, ApiError> {
        if self.access != Access::local() {
            return Err(ApiError::new(
                "forbidden",
                "control requires local-owner context",
            ));
        }
        match name {
            "get_task" => {
                let a: TaskV1 = decode(input)?;
                let f = self
                    .observer
                    .repository
                    .task_head_facts(&a.task_id, &self.access)
                    .map_err(mapped)?;
                return encode(super::observe::summary_head(&f, &self.observer.run_id));
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
                            terminal_result = Some(serde_json::from_value(value).map_err(|e| {
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
            "start_collection" => {
                use crate::task_control::Contract;
                let command: Collection = decode(input)?;
                if command.request_id().trim().is_empty() {
                    return Err(ApiError::new("empty_request_id", "request_id required"));
                }
                // Resolution goes through the registered contract, which is
                // already generation-bound for source-reading datasets.
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
            "retry_task" => {
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
            _ => {}
        }
        Err(ApiError::new(
            "unknown_tool",
            format!("unsupported tool: {name}"),
        ))
    }
}
fn decode<T: DeserializeOwned>(v: Value) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|e| ApiError::new("invalid_arguments", e.to_string()))
}
fn encode<T: Serialize>(v: T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::new("internal_error", e.to_string()))
}
fn plan_error(e: super::plan::PlanError) -> ApiError {
    match e {
        super::plan::PlanError::Invalid(m) => ApiError::new("invalid_arguments", m),
        super::plan::PlanError::Storage(m) => ApiError::new("storage_error", m),
    }
}
fn plan_dataset_name(dataset: Dataset) -> &'static str {
    match dataset {
        Dataset::Daily => "market.daily",
        Dataset::Yearly => "market.yearly",
        Dataset::Catalog => "securities.catalog",
    }
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
        Error::Stale(_) => "stale_version",
        Error::Conflict(_) => "precondition_conflict",
        Error::OutcomeUnknown(_) => "outcome_unknown",
        _ => "storage_error",
    };
    ApiError::new(code, e.to_string())
}
impl DataApi {
    /// tools/list is generated from the same registry entries that drive
    /// dispatch and get_data_capabilities; no second schema source exists.
    fn public_tools(&self) -> Value {
        Value::Array(
            entries()
                .into_iter()
                .filter(|e| self.access == Access::local() || !e.requires_local())
                .map(|e| {
                    json!({"name":e.name,"description":e.description,"inputSchema":crate::mcp::ensure_object_root(e.input),"outputSchema":crate::mcp::ensure_object_root(e.output)})
                })
                .collect(),
        )
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
            return self.control_call(name, input);
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
            Handler::VerifyGap => {
                if self.access != Access::local() {
                    return Err(ApiError::new(
                        "forbidden",
                        "write controls require local owner access",
                    ));
                }
                let command: crate::gap_verification::wire::VerifyDataGap = decode(input)?;
                let accepted = self
                    .legacy
                    .control
                    .accept(
                        command.request_id(),
                        crate::gap_verification::command_key(),
                        command.payload(),
                    )
                    .map_err(task_error)?;
                self.acceptance(command.request_id(), accepted)
            }
            Handler::Range(operation) => {
                if self.access != Access::local() {
                    return Err(ApiError::new(
                        "forbidden",
                        "write controls require local owner access",
                    ));
                }
                let command: RangeWrite =
                    crate::stage_trace::timed("rpc.range.decode", || decode(input))?;
                let payload = crate::stage_trace::timed("rpc.range.payload", || {
                    command.payload().map_err(task_error)
                })?;
                let key = command.key(operation);
                let control_span = crate::stage_trace::span("rpc.range.control_accept");
                let accepted = self
                    .legacy
                    .control
                    .accept(command.request_id(), key, payload)
                    .map_err(task_error)?;
                drop(control_span);
                crate::stage_trace::timed("rpc.range.acceptance", || {
                    self.acceptance(command.request_id(), accepted)
                })
            }
            Handler::Republish => {
                if self.access != Access::local() {
                    return Err(ApiError::new(
                        "forbidden",
                        "write controls require local owner access",
                    ));
                }
                let command: RepublishWrite = decode(input)?;
                let accepted = self
                    .legacy
                    .control
                    .accept(
                        command.request_id(),
                        command.key(),
                        command.payload().map_err(task_error)?,
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
                #[cfg(target_os = "linux")]
                let _permit = self
                    .worker_gate
                    .as_ref()
                    .map(|gate| gate.acquire())
                    .transpose()
                    .map_err(|code| {
                        ApiError::new(
                            code,
                            "service execution is busy or stopping; no additional worker started",
                        )
                    })?;
                let snapshot = super::worker::run_once(
                    &self.legacy.control,
                    &self.queries.repository.market,
                    self.market_source.as_ref(),
                    self.generation.as_ref(),
                )
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
            | Handler::Cancel => self.control_call(name, input),
            Handler::PlanMarketUpdate => {
                let command: PlanMarketUpdate = decode(input)?;
                self.access
                    .require(command.dataset(), None)
                    .map_err(mapped)?;
                let today = crate::calendar::shanghai_today();
                let (end_date, source) = match command.end_date() {
                    Some(day) => (day, super::plan::TargetDaySource::Explicit),
                    None => super::plan::default_target_day(
                        &self.queries.repository.market,
                        self.generation.as_ref(),
                        today,
                    ),
                };
                let plan = super::plan::resolve_plan(
                    &self.queries.repository.market,
                    self.generation.as_ref(),
                    command.dataset(),
                    Some(end_date),
                    today,
                )
                .map_err(plan_error)?;
                let mut view = plan.view().map_err(plan_error)?;
                view.target_day_source = Some(source.as_str().to_owned());
                encode(view)
            }
            Handler::RunMarketUpdate => {
                if self.access != Access::local() {
                    return Err(ApiError::new(
                        "forbidden",
                        "run_market_update requires local owner access",
                    ));
                }
                let command: RunMarketUpdate = decode(input)?;
                if !super::plan::valid_plan_id(command.plan_id()) {
                    return Err(ApiError::new(
                        "invalid_plan",
                        "plan_id must be 64 lowercase hex",
                    ));
                }
                // Request replay outranks staleness: a request_id already
                // bound to an identical run returns its original acceptance,
                // no matter how much the state has since drifted.
                if let Some(record) = self
                    .legacy
                    .control
                    .read_request(command.request_id())
                    .map_err(task_error)?
                {
                    let stored = &record.command;
                    let matches_call = stored.key.operation == "update_data"
                        && stored.key.dataset == plan_dataset_name(command.dataset())
                        && stored.key.contract_version == super::plan::RUN_CONTRACT_VERSION
                        && stored.intent["plan_id"].as_str() == Some(command.plan_id())
                        && stored.intent["target_day"].as_str()
                            == Some(command.target_day().to_string().as_str());
                    if !matches_call {
                        return Err(ApiError::new(
                            "request_conflict",
                            "request_id bound to a different command",
                        ));
                    }
                    let crate::task_control::RequestOutcome::Task(f) = record.outcome else {
                        return Err(ApiError::new(
                            "unsupported_request_outcome",
                            "task binding required",
                        ));
                    };
                    return self.plan_acceptance(
                        command.request_id(),
                        true,
                        &f,
                        f.items.len() as u64,
                    );
                }
                let today = crate::calendar::shanghai_today();
                let resolved = super::plan::resolve_run(
                    &self.queries.repository.market,
                    self.generation.as_ref(),
                    command.dataset(),
                    command.target_day(),
                    today,
                )
                .map_err(plan_error)?;
                let recomputed = resolved.plan.plan_id().map_err(plan_error)?;
                if recomputed != command.plan_id() {
                    return Err(ApiError::new(
                        "plan_stale",
                        "resolved plan drifted from the supplied plan_id; run plan_market_update again",
                    ));
                }
                for task in &resolved.inflight {
                    if command.dataset() == Dataset::Yearly && task.dataset == "market.daily" {
                        return Err(ApiError::new(
                            "daily_update_inflight",
                            format!("non-terminal market.daily update task: {}", task.task_id),
                        ));
                    }
                    if task.dataset == plan_dataset_name(command.dataset()) {
                        return Err(ApiError::new(
                            "plan_in_progress",
                            format!(
                                "non-terminal update task on {}: {}",
                                task.dataset, task.task_id
                            ),
                        ));
                    }
                }
                let counts = resolved.plan.counts();
                if counts.eligible == 0 {
                    return Err(ApiError::new(
                        "invalid_plan",
                        "plan contains no executable items",
                    ));
                }
                let task_command = crate::task_control::Command {
                    key: crate::task_control::CommandKey {
                        operation: "update_data".into(),
                        dataset: plan_dataset_name(command.dataset()).into(),
                        contract_version: super::plan::RUN_CONTRACT_VERSION.into(),
                    },
                    intent: resolved.plan.run_intent().map_err(plan_error)?,
                };
                let super::plan::RunWorkParts {
                    plan_id,
                    generation,
                    scopes,
                    shared_evidence,
                    items,
                } = resolved.plan.run_parts().map_err(plan_error)?;
                let mut specs = items
                    .into_iter()
                    .map(|item| super::plan::item_spec(item, &plan_id, &generation));
                let acceptance = self
                    .legacy
                    .control
                    .accept_streamed(
                        command.request_id(),
                        &task_command,
                        Some(shared_evidence),
                        scopes,
                        &mut specs,
                    )
                    .map_err(task_error)?;
                let crate::task_control::RequestOutcome::Task(f) = acceptance.record.outcome else {
                    return Err(ApiError::new(
                        "unsupported_request_outcome",
                        "task binding required",
                    ));
                };
                self.plan_acceptance(command.request_id(), acceptance.reused, &f, counts.eligible)
            }
            Handler::Query => {
                let reply = self
                    .queries
                    .query(decode::<Query>(input)?, &self.access)
                    .map_err(mapped)?;
                crate::stage_trace::timed("rpc.query.encode", || encode(reply))
            }
            Handler::Versions => encode(
                self.queries
                    .versions(decode::<VersionsQuery>(input)?, &self.access)
                    .map_err(mapped)?,
            ),
            Handler::Availability => {
                let page = self
                    .observer
                    .availability(decode::<Query>(input)?, &self.access)
                    .map_err(mapped)?;
                crate::stage_trace::timed("rpc.availability.encode", || encode(page))
            }
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
            Handler::YearPresence => {
                let reply = self
                    .queries
                    .year_presence(decode::<YearPresenceQuery>(input)?, &self.access)
                    .map_err(mapped)?;
                crate::stage_trace::timed("rpc.year_presence.encode", || encode(reply))
            }
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
                    | "get_yearly_base_years"
                    | "list_tasks"
                    | "get_task_progress"
                    | "get_data_capabilities"
                    | "compute_metrics"
                    | "verify_publication_result"
                    | "inspect_verification_task"
                    | "verify_data_gap"
                    | "update_data"
                    | "backfill_data"
                    | "republish_data"
                    | "plan_market_update"
                    | "run_market_update"
            );
        let _rpc = crate::stage_trace::span_named("rpc.", name);
        self.dispatch(name, input).map_err(|mut e| {
            if generic {
                e.outcome_certainty = Some(
                    if e.code == "outcome_unknown"
                        || (matches!(
                            name,
                            "update_data"
                                | "backfill_data"
                                | "republish_data"
                                | "run_market_update"
                                | "verify_publication_result"
                                | "inspect_verification_task"
                                | "verify_data_gap"
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
                            | "republish_data"
                            | "run_market_update"
                            | "verify_publication_result"
                            | "inspect_verification_task"
                            | "verify_data_gap"
                    ) {
                        "not_committed"
                    } else {
                        "not_applicable"
                    }
                    .into(),
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

#[cfg(test)]
mod tests;
