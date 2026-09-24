// NM-082-style golden-vector oracle for NM-106 — drives the REAL Rust
// task_verification decision modules behind a logging stub repository.
// Build (baseline 16df795):
//   cargo build --locked && rustc --edition 2024 -L target/debug/deps \
//     --extern x_data_mcp=<libx_data_mcp.rlib> --extern serde_json=... \
//     --extern chrono=... --extern schemars=... -o /tmp/tv_oracle \
//     scripts/node_migration_p4/task_verification_oracle.rs
// Usage: tv_oracle scenarios.jsonl > golden.jsonl
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::rc::Rc;
use x_data_mcp::task_control::{
    Acceptance, Change, Command, CommandKey, Contract, Error, FrozenWork,
    RequestOutcome, RequestRecord, RetryGuard, TaskRepository, TaskSnapshot,
};
use x_data_mcp::task_control::service::TaskControl;
use x_data_mcp::task_verification::StopVerification;
use x_data_mcp::task_verification::decision::{
    self, Observation, Receipt, StopInspector, StopRepository,
};
use x_data_mcp::task_verification::existing::{self, ExistingStopRepository};
use x_data_mcp::task_verification::{determinate, never_started};
use x_data_mcp::task_verification::wire::VerifyStopped;

// ---------------------------------------------------------------- errors
fn err(e: &Error) -> Value {
    use Error::*;
    let (code, msg) = match e {
        InvalidCommand(m) => ("invalid_input", format!("INVALID_COMMAND: {m}")),
        RequestConflict => ("request_conflict", "REQUEST_CONFLICT".into()),
        RangeConflict(s) => ("range_conflict", format!("RANGE_CONFLICT: {s:?}")),
        NeedsVerification(m) => {
            ("needs_verification", format!("ACCEPTANCE_NEEDS_VERIFICATION: {m}"))
        }
        StorageUnavailable(m) => ("unavailable", format!("STORAGE_UNAVAILABLE: {m}")),
        OutcomeUnknown(m) => (
            "outcome_unknown",
            format!("OUTCOME_UNKNOWN: {m}; query the same request_id before retrying"),
        ),
        RevisionConflict => ("revision_conflict", "STATE_REVISION_CONFLICT".into()),
        UnsupportedContract => ("unsupported_contract", "UNSUPPORTED_CONTRACT".into()),
        TaskNotFound => ("task_not_found", "TASK_NOT_FOUND".into()),
        InvalidRetrySelection =>
            ("invalid_retry_selection", "INVALID_RETRY_SELECTION".into()),
        RetryRequestIdConflict => (
            "retry_request_id_conflict",
            "retry request_id must differ from the original task request_id".into(),
        ),
        EmptyRetrySelection => (
            "empty_retry_selection",
            "retry selected_scopes must not be empty".into(),
        ),
        RetrySelectionInvalid { .. } => (
            "retry_selection_invalid",
            "retry selected scopes are invalid, outside the original range, or already completed"
                .into(),
        ),
        EmptyRequestId => ("empty_request_id", "request_id must not be empty".into()),
        _ => ("other", e.to_string()),
    };
    json!({"code": code, "message": msg})
}

// ---------------------------------------------------------------- stub repo
#[derive(Default)]
struct Stub {
    calls: RefCell<Vec<Value>>,
    task: RefCell<Option<TaskSnapshot>>,
    task2: RefCell<Option<TaskSnapshot>>, // second read_task answer (bounded recheck)
    reads: RefCell<usize>,
    request: RefCell<Option<RequestRecord>>,
    accept_result: RefCell<Option<Value>>, // {"receipt":{...}} | {"err":{kind,message}}
    receipt: RefCell<Option<Receipt>>,
    receipt_cfg: RefCell<Option<Value>>,
    never_facts: RefCell<Option<Value>>,   // {"snapshot":snap,"has_durable_effects":b}
    det_facts: RefCell<Option<Value>>,     // {"snapshot":snap,"started_items":[],"committed_items":[],"has_durable_effects":b}
    bind_result: RefCell<Option<Value>>,
}
#[derive(Clone)]
struct Repo(Rc<Stub>);
type R = Repo;
impl std::ops::Deref for Repo { type Target = Stub; fn deref(&self) -> &Stub { &self.0 } }

fn mk_err(kind: &str, m: &str) -> Error {
    match kind {
        "storage" => Error::StorageUnavailable(format!("STORAGE: {m}")),
        "integrity" => Error::NeedsVerification(format!("MARKET_INTEGRITY: {m}")),
        "base" => Error::NeedsVerification(format!("BASE_UNAVAILABLE: {m}")),
        "conflict" => Error::NeedsVerification(format!("PUBLISH_PRECONDITION: {m}")),
        "not_found" => Error::TaskNotFound,
        "invalid" => Error::InvalidCommand(format!("INVALID_MARKET_COMMAND: {m}")),
        "outcome_unknown" => Error::OutcomeUnknown(m.to_string()),
        "basis" => Error::NeedsVerification(format!("GAP_BASIS_NOT_VERIFIED: {m}")),
        "tc_invalid" => Error::InvalidCommand(m.to_string()),
        "tc_conflict" => Error::RevisionConflict,
        "tc_needs" => Error::NeedsVerification(m.to_string()),
        "tc_unavailable" => Error::StorageUnavailable(m.to_string()),
        "tc_outcome" => Error::OutcomeUnknown(m.to_string()),
        "tc_request" => Error::RequestConflict,
        "tc_not_found" => Error::TaskNotFound,
        "tc_unsupported" => Error::UnsupportedContract,
        _ => Error::StorageUnavailable(format!("UNEXPECTED {kind}")),
    }
}
fn e_str(v: &Value) -> &str { v.get("message").and_then(Value::as_str).unwrap_or("") }

fn snap(cfg: &Value, key: &str) -> Option<TaskSnapshot> {
    cfg.get(key).filter(|v| !v.is_null()).map(|v| {
        serde_json::from_value::<TaskSnapshot>(v.clone())
            .unwrap_or_else(|e| panic!("snapshot decode: {e}"))
    })
}

impl TaskRepository for R {
    fn read_request(&self, id: &str) -> Result<Option<RequestRecord>, Error> {
        self.calls.borrow_mut().push(json!({"call":"read_request","id":id}));
        Ok(self.request.borrow().clone())
    }
    fn read_task(&self, id: &str) -> Result<Option<TaskSnapshot>, Error> {
        self.calls.borrow_mut().push(json!({"call":"read_task","id":id}));
        *self.reads.borrow_mut() += 1;
        // second call returns task2 when configured (bounded recheck)
        if *self.reads.borrow() > 1 {
            if let Some(t2) = self.task2.borrow().as_ref() { return Ok(Some(t2.clone())); }
        }
        Ok(self.task.borrow().clone())
    }
    fn accept(
        &self, id: &str, command: &Command, work: &FrozenWork, run: &str,
        retry: Option<&RetryGuard>,
    ) -> Result<Acceptance, Error> {
        self.calls.borrow_mut().push(json!({
            "call":"accept","id":id,"run":run,
            "command":serde_json::to_value(command).unwrap(),
            "work":serde_json::to_value(work).unwrap(),
        }));
        let cfg = self.accept_result.borrow().clone().unwrap_or(json!({"receipt":{}}));
        if let Some(e) = cfg.get("err") {
            return Err(mk_err(e["kind"].as_str().unwrap_or(""), e_str(e)));
        }
        Ok(Acceptance {
            record: RequestRecord {
                request_id: id.into(),
                command: command.clone(),
                outcome: RequestOutcome::Terminal(json!({"accepted": true})),
            },
            reused: false,
        })
    }
    fn transition(&self, id: &str, rev: u64, run: &str, change: Change)
        -> Result<TaskSnapshot, Error> {
        self.calls.borrow_mut().push(json!({
            "call":"transition","id":id,"revision":rev,"run":run,
            "change":format!("{change:?}")}));
        Err(Error::StorageUnavailable("UNEXPECTED transition".into()))
    }
    fn candidates(&self, run: &str) -> Result<Vec<TaskSnapshot>, Error> {
        self.calls.borrow_mut().push(json!({"call":"candidates","run":run}));
        Err(Error::StorageUnavailable("UNEXPECTED candidates".into()))
    }
    fn reconcile(&self, run: &str) -> Result<usize, Error> {
        self.calls.borrow_mut().push(json!({"call":"reconcile","run":run}));
        Err(Error::StorageUnavailable("UNEXPECTED reconcile".into()))
    }
}

impl StopRepository for R {
    fn commit_stop_check(&self, _plan: &decision::StopPlan) -> Result<Receipt, Error> {
        self.calls.borrow_mut().push(json!({"call":"commit_stop_check"}));
        Err(Error::StorageUnavailable("UNEXPECTED commit_stop_check".into()))
    }
    fn stop_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.calls.borrow_mut().push(json!({"call":"stop_receipt","id":id}));
        let cfg = self.receipt_cfg.borrow().clone();
        if let Some(e) = cfg.as_ref().and_then(|c| c.get("err")) {
            return Err(mk_err(e["kind"].as_str().unwrap_or(""), e_str(e)));
        }
        Ok(self.receipt.borrow().clone())
    }
}

impl ExistingStopRepository for R {
    fn bind_existing_stop(&self, _plan: &existing::ExistingPlan) -> Result<Acceptance, Error> {
        self.calls.borrow_mut().push(json!({"call":"bind_existing_stop"}));
        let cfg = self.bind_result.borrow().clone().unwrap_or(json!({"ok":{}}));
        if let Some(e) = cfg.get("err") {
            return Err(mk_err(e["kind"].as_str().unwrap_or(""), e_str(e)));
        }
        Ok(Acceptance {
            record: RequestRecord {
                request_id: "bound".into(),
                command: Command { key: CommandKey {
                    operation: "verify_stopped_writes".into(),
                    dataset: "task.verification".into(),
                    contract_version: "stopped-writes-task-v1".into() },
                    intent: json!({"target_task_id":"?"}) },
                outcome: RequestOutcome::Terminal(json!({"kind":"already_verified","receipt":"stub"})),
            },
            reused: false,
        })
    }
}

impl never_started::EvidenceRepository for R {
    fn never_started_facts(&self, id: &str)
        -> Result<never_started::Facts, Error> {
        self.calls.borrow_mut().push(json!({"call":"never_started_facts","id":id}));
        let f = self.never_facts.borrow().clone().unwrap_or(json!(null));
        if let Some(e) = f.get("err") {
            return Err(mk_err(e["kind"].as_str().unwrap_or(""), e_str(e)));
        }
        Ok(never_started::Facts {
            snapshot: serde_json::from_value::<TaskSnapshot>(
                f.get("snapshot").cloned().unwrap_or(json!(null)))
                .unwrap_or_else(|e| panic!("never snapshot decode: {e}")),
            has_durable_effects: f.get("has_durable_effects")
                .and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

impl determinate::EvidenceRepository for R {
    fn determinate_facts(&self, id: &str)
        -> Result<determinate::Facts, Error> {
        self.calls.borrow_mut().push(json!({"call":"determinate_facts","id":id}));
        let f = self.det_facts.borrow().clone().unwrap_or(json!(null));
        if let Some(e) = f.get("err") {
            return Err(mk_err(e["kind"].as_str().unwrap_or(""), e_str(e)));
        }
        Ok(determinate::Facts {
            snapshot: serde_json::from_value::<TaskSnapshot>(
                f.get("snapshot").cloned().unwrap_or(json!(null)))
                .unwrap_or_else(|e| panic!("det snapshot decode: {e}")),
            started_items: f.get("started_items").and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_else(BTreeSet::new),
            committed_items: f.get("committed_items").and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_else(BTreeSet::new),
            has_durable_effects: f.get("has_durable_effects")
                .and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

// Scripted inspector for prepare vectors.
struct Scripted(RefCell<Option<Value>>, Rc<RefCell<Vec<Value>>>);
impl StopInspector for Scripted {
    fn inspect(&self, _target: &TaskSnapshot) -> Result<Observation, Error> {
        self.1.borrow_mut().push(json!({"call":"inspect"}));
        let v = self.0.borrow().clone().unwrap_or(json!(null));
        if let Some(e) = v.get("err") {
            return Err(mk_err(e["kind"].as_str().unwrap_or(""), e_str(e)));
        }
        match v.get("kind").and_then(Value::as_str) {
            Some("insufficient") => Ok(Observation::Insufficient {
                reason: v["reason"].as_str().unwrap_or("").into(),
            }),
            Some("stopped") => Ok(Observation::Stopped {
                snapshot_sha256: v["snapshot_sha256"].as_str().unwrap_or("").into(),
                evidence_ref: v["evidence_ref"].as_str().unwrap_or("").into(),
            }),
            _ => panic!("bad scripted observation"),
        }
    }
}

fn obs_json(o: &Observation) -> Value {
    serde_json::to_value(o).unwrap()
}

fn run_case(seq: usize, c: &Value) -> Value {
    let mut tag = json!({"seq":seq,"op":c["op"].as_str().unwrap_or("?")});
    let stub: R = Repo(Rc::new(Stub::default()));
    let cfg = c.get("repo").cloned().unwrap_or(json!({}));
    *stub.task.borrow_mut() = snap(&cfg, "task");
    *stub.task2.borrow_mut() = snap(&cfg, "task2");
    if let Some(r) = cfg.get("request").filter(|v| !v.is_null()) {
        *stub.request.borrow_mut() = Some(serde_json::from_value::<RequestRecord>(r.clone())
            .unwrap_or_else(|e| panic!("request decode: {e}")));
    }
    *stub.receipt_cfg.borrow_mut() = cfg.get("receipt").cloned();
    if let Some(r) = cfg.get("receipt").filter(|v| !v.is_null() && v.get("err").is_none()) {
        *stub.receipt.borrow_mut() = Some(serde_json::from_value::<Receipt>(r.clone())
            .unwrap_or_else(|e| panic!("receipt decode: {e}")));
    }
    *stub.accept_result.borrow_mut() = cfg.get("accept").cloned();
    *stub.bind_result.borrow_mut() = cfg.get("bind").cloned();
    *stub.never_facts.borrow_mut() = cfg.get("never_facts").cloned();
    *stub.det_facts.borrow_mut() = cfg.get("det_facts").cloned();

    let calls = Rc::new(RefCell::new(Vec::new()));
    let out = match c["op"].as_str().unwrap() {
        "verify_decode" => {
            match serde_json::from_value::<VerifyStopped>(c["value"].clone()) {
                Ok(v) => json!({"status":"ok","value":serde_json::to_value(&v).unwrap()}),
                Err(e) => json!({"status":"err","message":e.to_string()}),
            }
        }
        "admission_new" => {
            let run = c["run"].as_str().unwrap_or("").to_string();
            match StopVerification::new(stub.clone(), run) {
                Ok(_) => json!({"status":"ok"}),
                Err(e) => json!({"status":"err","error":err(&e)}),
            }
        }
        "admission_normalize" => {
            let sv = StopVerification::new(stub.clone(), "R".into()).unwrap();
            match sv.normalize(c["payload"].clone()) {
                Ok(v) => json!({"status":"ok","normalized":v}),
                Err(e) => json!({"status":"err","error":err(&e)}),
            }
        }
        "admission_resolve" => {
            let sv = StopVerification::new(stub.clone(), "R".into()).unwrap();
            match sv.resolve(&c["intent"].clone()) {
                Ok(w) => json!({"status":"ok","work":serde_json::to_value(&w).unwrap()}),
                Err(e) => json!({"status":"err","error":err(&e)}),
            }
        }
        "inspect" => {
            let target = snap(&c.clone(), "target").unwrap();
            let which = c["inspector"].as_str().unwrap_or("audited");
            let r: Result<Observation, Error> = match which {
                "unverified" => decision::UnverifiedStopInspector.inspect(&target),
                "never_started" => never_started::NeverStarted {
                    repository: stub.clone(),
                }.inspect(&target),
                "determinate" => determinate::Determinate {
                    repository: stub.clone(),
                }.inspect(&target),
                _ => determinate::Audited { repository: stub.clone() }.inspect(&target),
            };
            match r {
                Ok(o) => json!({"status":"ok","observation":obs_json(&o)}),
                Err(e) => json!({"status":"err","error":err(&e)}),
            }
        }
        "prepare" => {
            let verification = snap(&c.clone(), "verification").unwrap();
            let target = snap(&c.clone(), "target").unwrap();
            let run = c["run"].as_str().unwrap_or("R");
            let actor = c["actor"].as_str().unwrap_or("actor");
            let now: chrono::DateTime<chrono::Utc> =
                serde_json::from_value(c["now"].clone()).unwrap();
            let insp = c.get("inspector").cloned().unwrap_or(json!("scripted"));
            let r = match insp.as_str().unwrap_or("scripted") {
                "unverified" => decision::prepare(verification, target, run, actor, now,
                    &decision::UnverifiedStopInspector),
                "audited" => decision::prepare(verification, target, run, actor, now,
                    &determinate::Audited { repository: stub.clone() }),
                "never_started" => decision::prepare(verification, target, run, actor, now,
                    &never_started::NeverStarted { repository: stub.clone() }),
                "determinate" => decision::prepare(verification, target, run, actor, now,
                    &determinate::Determinate { repository: stub.clone() }),
                _ => decision::prepare(verification, target, run, actor, now,
                    &Scripted(RefCell::new(c.get("observation").cloned()), calls.clone())),
            };
            match r {
                Ok(plan) => json!({"status":"ok",
                    "receipt":serde_json::to_value(plan.receipt()).unwrap()}),
                Err(e) => json!({"status":"err","error":err(&e)}),
            }
        }
        "validate_audit" => {
            // Replicates decision::validate_audit's observable verdict:
            // replay prepare with the Recorded inspector (built from the
            // supplied audit's evidence_ref/receipt), then compare the
            // receipt projection (the strictest auditable field set).
            let a = &c["audit"];
            let vb = serde_json::from_value::<TaskSnapshot>(
                a["verification_before"].clone()).unwrap();
            let tb = serde_json::from_value::<TaskSnapshot>(
                a["target_before"].clone()).unwrap();
            let receipt_in = serde_json::from_value::<Receipt>(a["receipt"].clone());
            let recorded: Box<dyn StopInspector> = if a["evidence_ref"].is_null() {
                Box::new(Scripted(RefCell::new(Some(json!({"kind":"insufficient",
                    "reason":a["receipt"]["reason"]}))), calls.clone()))
            } else {
                Box::new(Scripted(RefCell::new(Some(json!({"kind":"stopped",
                    "snapshot_sha256": decision::snapshot_digest(&tb).unwrap(),
                    "evidence_ref":a["evidence_ref"]}))), calls.clone()))
            };
            let run = vb.task.owner_run_id.clone().unwrap_or_default();
            let now: chrono::DateTime<chrono::Utc> =
                serde_json::from_value(a["checked_at"].clone()).unwrap();
            match decision::prepare(vb, tb, &run, a["actor"].as_str().unwrap_or(""),
                now, recorded.as_ref()) {
                Ok(plan) => match receipt_in {
                    Ok(r) if r == *plan.receipt() =>
                        json!({"status":"ok"}),
                    Ok(_) => json!({"status":"err","error":err(
                        &Error::NeedsVerification(
                            "stopped check audit projections differ".into()))}),
                    Err(e) => json!({"status":"err","message":e.to_string()}),
                },
                Err(e) => json!({"status":"err","error":err(&e)}),
            }
        }
        "existing_accept" => {
            let control = TaskControl::new(stub.clone(), "R".into(), vec![
                Box::new(StopVerification::new(stub.clone(), "R".into()).unwrap())
                    as Box<dyn Contract>,
            ]).unwrap();
            match existing::accept(&control, c["id"].as_str().unwrap_or("req-1"),
                c["target_id"].as_str().unwrap_or("T")) {
                Ok(acc) => json!({"status":"ok","acceptance":{
                    "record":serde_json::to_value(&acc.record).unwrap(),
                    "reused":acc.reused}}),
                Err(e) => json!({"status":"err","error":err(&e)}),
            }
        }
        other => panic!("unknown op {other}"),
    };
    let mut out = out;
    let mut all = calls.borrow().clone();
    all.extend(stub.calls.borrow().iter().cloned());
    out["calls"] = Value::Array(all);
    for (k, v) in tag.as_object().unwrap() { out[k] = v.clone(); }
    out
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut seq = 0usize;
    for line in BufReader::new(stdin.lock()).lines() {
        let line = line.unwrap();
        if line.trim().is_empty() { continue; }
        let c: Value = serde_json::from_str(&line).unwrap();
        let row = match std::panic::catch_unwind(|| run_case(seq, &c)) {
            Ok(v) => v,
            Err(_) => json!({"seq":seq,"status":"panic"}),
        };
        seq += 1;
        writeln!(stdout, "{}", serde_json::to_string(&row).unwrap()).unwrap();
    }
}
