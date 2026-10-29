//! x-data-acquire protocol adapter: typed tool dispatch over
//! `AcquireService`. One descriptor table drives tools/list, dispatch and
//! capability reporting — no second schema source. Outputs are facts only
//! (state, outcome, refs, evidence, reason codes); nothing here emits next
//! actions, Skill references or orchestration hints.
use super::{service::AcquireService, wire::*};
use crate::mcp::{ApiError, ToolHandler};
use schemars::schema_for;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

pub const CAPABILITY_REVISION: &str = "x-data-acquire-v1";

struct Entry {
    name: &'static str,
    description: &'static str,
    input: fn() -> Value,
}
fn schema<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schema_for!(T)).unwrap_or_default()
}
const ENTRIES: &[Entry] = &[
    Entry {
        name: "check_official_updates",
        description: "Read-only remote probe of the declared official files. `changed` reports remote drift since the previous persisted observation; `pending_roles` compares remote heads with the active generation manifest. Unknown is a stop signal, never a prompt to act.",
        input: schema::<CheckOfficialUpdates>,
    },
    Entry {
        name: "acquire_generation",
        description: "Accept a durable download+install task for a fresh official-file generation. request_id binds the acceptance idempotently; downloading never implies verified or active.",
        input: schema::<AcquireGeneration>,
    },
    Entry {
        name: "get_acquisition_task",
        description: "Read one acquisition task's durable state, stage and terminal error.",
        input: schema::<GetAcquisitionTask>,
    },
    Entry {
        name: "get_acquisition_request",
        description: "Read the recorded binding and terminal outcome of a request_id without accepting work.",
        input: schema::<GetAcquisitionRequest>,
    },
    Entry {
        name: "verify_generation",
        description: "Run the installed generation through the structural probe suite and the file-consistency contract; records the once-written decision artifact. NOT_VERIFIED/CONFLICT are stable terminal verdicts.",
        input: schema::<VerifyGeneration>,
    },
    Entry {
        name: "activate_generation",
        description: "Conditional activation: only a consistency_verified generation may swap the atomic active pointer, and only when expected_previous still matches. After the swap, superseded generations beyond the newest two retire automatically (reported in retired_generations). Refusals are durable outcomes.",
        input: schema::<ActivateGeneration>,
    },
    Entry {
        name: "retire_generation",
        description: "Destructive retire to evidence_only: requires the generation's manifest digest as confirm_token, refuses active or referenced generations. Audit artifacts remain; raw bytes are deleted.",
        input: schema::<RetireGeneration>,
    },
    Entry {
        name: "sync_status",
        description: "Read the active pointer, every recorded generation lifecycle state and inflight acquisition tasks.",
        input: schema::<SyncStatus>,
    },
    Entry {
        name: "run_worker_once",
        description: "Advance the oldest open acquisition task by one durable stage. The service worker loop already calls this; a manual call is an operator turn, not a second worker.",
        input: schema::<WorkerOnce>,
    },
    Entry {
        name: "get_acquisition_capabilities",
        description: "Read this server's implementation, runtime and governance readiness, blocking reasons, declared scope, active generation reference and contract revisions.",
        input: schema::<SyncStatus>,
    },
];

pub struct AcquireApi {
    pub service: AcquireService,
}
impl AcquireApi {
    pub fn new(service: AcquireService) -> Self {
        Self { service }
    }

    fn capabilities(&self) -> Result<Value, ApiError> {
        let status = self.service.sync_status().map_err(mapped)?;
        let mut blocking = vec![];
        if status.active.is_none() {
            blocking.push(json!({
                "code": "no_active_generation",
                "detail": "no generation has completed verify+activate; data serving from this store is blocked",
            }));
        }
        for g in &status.generations {
            if g.state == "conflict" || g.state == "not_verified" || g.state == "corrupt" {
                blocking.push(json!({
                    "code": "unresolved_generation",
                    "detail": format!("generation {} is {}", g.generation_id, g.state),
                    "generation_id": g.generation_id,
                }));
            }
        }
        encode(json!({
            "implementation": {
                "server": "x-data-acquire",
                "version": env!("CARGO_PKG_VERSION"),
                "capability_revision": CAPABILITY_REVISION,
                "generation_contract_version": super::service::GENERATION_CONTRACT_VERSION,
                "consistency_contract_version": status.consistency_contract_version,
            },
            "declared_scope": "official TDX base/gbbq/hsjday acquisition into immutable generations; no catalog scope claims",
            "active_generation_ref": status.active.as_ref().map(|p| p.generation_id.clone()),
            "governance_readiness": "ready",
            "runtime_readiness": if blocking.is_empty() { "ready" } else { "blocked" },
            "blocking_reasons": blocking,
            "tools": ENTRIES.iter().map(|e| e.name).collect::<Vec<_>>(),
            "schema_digest": crate::hash::sha256(&serde_json::to_vec(
                &ENTRIES.iter().map(|e| json!({"name":e.name,"input":(e.input)()})).collect::<Vec<_>>(),
            ).map_err(|e| ApiError::new("internal_error", e.to_string()))?),
        }))
    }
}

fn decode<T: DeserializeOwned>(v: Value) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|e| ApiError::new("invalid_arguments", e.to_string()))
}
fn encode<T: Serialize>(v: T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::new("internal_error", e.to_string()))
}
fn mapped(e: super::service::Error) -> ApiError {
    let code = match &e {
        super::service::Error::Refused(code) => code,
        super::service::Error::Storage(super::store::Error::RequestConflict) => "request_conflict",
        super::service::Error::Storage(_) => "storage_error",
        super::service::Error::Generation(crate::generation::Error::NotInstalled(_)) => {
            "generation_not_installed"
        }
        super::service::Error::Generation(_) => "generation_error",
        super::service::Error::Fetch(_) => "fetch_error",
        super::service::Error::Json(_) => "internal_error",
        super::service::Error::Io(_) => "io_error",
        super::service::Error::Gate(_) => "retire_gate_unavailable",
    };
    ApiError::new(code, e.to_string())
}

impl ToolHandler for AcquireApi {
    fn tools(&self) -> Value {
        Value::Array(
            ENTRIES
                .iter()
                .map(|e| {
                    json!({
                        "name": e.name,
                        "description": e.description,
                        "inputSchema": crate::mcp::ensure_object_root((e.input)()),
                    })
                })
                .collect(),
        )
    }
    fn call(&self, name: &str, input: Value) -> Result<Value, ApiError> {
        if input.get("contract_version").is_some_and(|v| v != "v1") {
            return Err(ApiError::new(
                "unsupported_contract",
                "unsupported contract version",
            ));
        }
        match name {
            "check_official_updates" => {
                let _: CheckOfficialUpdates = decode(input)?;
                encode(self.service.check_updates().map_err(mapped)?)
            }
            "acquire_generation" => {
                let a: AcquireGeneration = decode(input)?;
                encode(
                    self.service
                        .acquire_generation(&a.request_id)
                        .map_err(mapped)?,
                )
            }
            "get_acquisition_task" => {
                let a: GetAcquisitionTask = decode(input)?;
                encode(self.service.task(&a.task_id).map_err(mapped)?)
            }
            "get_acquisition_request" => {
                let a: GetAcquisitionRequest = decode(input)?;
                encode(self.service.request(&a.request_id).map_err(mapped)?)
            }
            "verify_generation" => {
                let a: VerifyGeneration = decode(input)?;
                encode(
                    self.service
                        .verify_generation(&a.request_id, &a.generation_id)
                        .map_err(mapped)?,
                )
            }
            "activate_generation" => {
                let a: ActivateGeneration = decode(input)?;
                encode(
                    self.service
                        .activate_generation(
                            &a.request_id,
                            &a.generation_id,
                            a.expected_previous.as_deref(),
                        )
                        .map_err(mapped)?,
                )
            }
            "retire_generation" => {
                let a: RetireGeneration = decode(input)?;
                encode(
                    self.service
                        .retire_generation(&a.request_id, &a.generation_id, &a.confirm_token)
                        .map_err(mapped)?,
                )
            }
            "sync_status" => {
                let _: SyncStatus = decode(input)?;
                encode(self.service.sync_status().map_err(mapped)?)
            }
            "run_worker_once" => {
                let _: WorkerOnce = decode(input)?;
                let task = self.service.worker_once().map_err(mapped)?;
                encode(WorkerTurn {
                    idle: task.is_none(),
                    task: task.map(|t| {
                        self.service
                            .task(&t.task_id)
                            .unwrap_or_else(|_| AcquisitionTaskView {
                                task_id: t.task_id.clone(),
                                request_id: t.request_id.clone(),
                                kind: t.kind.clone(),
                                state: t.state.clone(),
                                generation_id: t.generation_id.clone(),
                                stage: t.stage.clone(),
                                bytes_done: t.bytes_done,
                                error: t.error.clone(),
                                detail: t.detail.clone(),
                                created_at: t.created_at.to_rfc3339(),
                                updated_at: t.updated_at.to_rfc3339(),
                            })
                    }),
                })
            }
            "get_acquisition_capabilities" => {
                let _: SyncStatus = decode(input)?;
                self.capabilities()
            }
            _ => Err(ApiError::new(
                "unknown_tool",
                format!("unsupported tool: {name}"),
            )),
        }
    }
}
