//! NM-049 — differential harness (Rust oracle side). Reads a JSONL
//! scenario from DIFF_SCENARIO, drives the REAL SqliteTasks /
//! task_control APIs against DIFF_DB, and emits
//! {seq,op,status,code,message,task,unchanged} JSONL to DIFF_OUT.
//!
//! Run: DIFF_SCENARIO=... DIFF_DB=... DIFF_OUT=... cargo test \
//!        diff_harness::run -- --nocapture --test-threads=1
use crate::{
    market_storage::Error as MarketError,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Change, Command, Error, FrozenWork, ItemOutcome, RequestOutcome, RetryGuard,
        TaskRepository, TaskSnapshot,
    },
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, collections::BTreeSet, fs, io::Write};

fn market_code(e: &MarketError) -> String {
    use MarketError::*;
    match e {
        Storage(_) => "Storage",
        Integrity(_) => "Integrity",
        Base(_) => "Base",
        Conflict(_) => "Conflict",
        NotFound => "NotFound",
        Invalid(_) => "Invalid",
        OutcomeUnknown(_) => "OutcomeUnknown",
        BasisNotVerified(_) => "BasisNotVerified",
        Task(t) => return err_code(t),
    }
    .into()
}

fn err_code(e: &Error) -> String {
    use Error::*;
    match e {
        InvalidCommand(_) => "InvalidCommand",
        RequestConflict => "RequestConflict",
        RangeConflict(_) => "RangeConflict",
        NeedsVerification(_) => "NeedsVerification",
        StorageUnavailable(_) => "StorageUnavailable",
        OutcomeUnknown(_) => "OutcomeUnknown",
        RevisionConflict => "RevisionConflict",
        UnsupportedContract => "UnsupportedContract",
        TaskNotFound => "TaskNotFound",
        InvalidRetrySelection => "InvalidRetrySelection",
        RetryRequestIdConflict => "RetryRequestIdConflict",
        EmptyRetrySelection => "EmptyRetrySelection",
        RetrySelectionInvalid { .. } => "RetrySelectionInvalid",
        EmptyRequestId => "EmptyRequestId",
    }
    .into()
}

type Syms = BTreeMap<String, String>;

fn sym_of(syms: &Syms, id: &str) -> String {
    for (k, v) in syms {
        if !k.starts_with("rev:") && v == id {
            return format!("${k}");
        }
    }
    id.to_string()
}

fn normalize_ids(v: &Value, syms: &Syms) -> Value {
    // replace every real task id embedded anywhere with its $symbol
    let raw = serde_json::to_string(v).unwrap();
    let mut out = raw;
    for (k, real) in syms {
        if !k.starts_with("rev:") {
            out = out.replace(real.as_str(), &format!("${k}"));
        }
    }
    serde_json::from_str(&out).unwrap_or(v.clone())
}

fn projection(s: &TaskSnapshot, syms: &Syms) -> Value {
    json!({
        "task_id": sym_of(syms, &s.task.task_id),
        "status": serde_json::to_value(&s.task.status).unwrap(),
        "revision": s.task.state_revision,
        "accepted_run_id": s.task.accepted_run_id,
        "owner_run_id": s.task.owner_run_id,
        "retry_of": s.task.retry_of.as_ref().map(|x| sym_of(syms, x)),
        "result": s.task.result.as_ref().map(|r| normalize_ids(r, syms)),
        "command_key": s.task.key,
        "items": s.items.iter().map(|i| json!({
            "item_id": i.spec.item_id,
            "stage": serde_json::to_value(&i.stage).unwrap(),
            "outcome": i.outcome,
        })).collect::<Vec<_>>(),
        "claims": s.claims.iter().map(|c| json!({
            "scope_key": c.scope_key,
            "state_revision": c.state_revision,
        })).collect::<Vec<_>>(),
    })
}

fn state_digest(db: &str, syms: &Syms) -> String {
    // Canonical projection digest over mutable tables; timestamps stripped
    // so the digest is comparable across implementations and across
    // rejections (where no row may change).
    let c = rusqlite::Connection::open(db).unwrap();
    let mut lines = Vec::new();
    let dump = |c: &rusqlite::Connection, sql: &str, tag: &str, n: usize, lines: &mut Vec<String>| {
        let mut stmt = c.prepare(sql).unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| match r.get::<_, rusqlite::types::Value>(i).unwrap() {
                        rusqlite::types::Value::Null => String::new(),
                        rusqlite::types::Value::Integer(x) => x.to_string(),
                        rusqlite::types::Value::Real(x) => x.to_string(),
                        rusqlite::types::Value::Text(x) => x,
                        rusqlite::types::Value::Blob(x) => crate::hash::sha256(&x),
                    })
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap();
        for r in rows {
            lines.push(format!("{tag}:{}", r.unwrap()));
        }
    };
    dump(&c, "SELECT task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,frozen_work_json,result_json,retry_of,item_count,items_pending,items_running,items_finished,items_succeeded FROM tasks ORDER BY task_id", "T", 14, &mut lines);
    dump(&c, "SELECT task_id,item_id,stage,outcome_json FROM task_items ORDER BY task_id,item_id", "I", 4, &mut lines);
    dump(&c, "SELECT scope_key,task_id,state_revision FROM scope_occupancy ORDER BY 1,2", "O", 3, &mut lines);
    dump(&c, "SELECT task_id,scope_key FROM task_scopes ORDER BY 1,2", "S", 2, &mut lines);
    dump(&c, "SELECT request_id,command_digest,outcome_kind,task_id,terminal_json FROM requests ORDER BY 1", "R", 5, &mut lines);
    dump(&c, "SELECT task_id,item_id,run_id,start_revision FROM task_item_execution_starts ORDER BY 1,2", "X", 4, &mut lines);
    let joined = lines.join("\n");
    let mut out = joined;
    for (k, real) in syms {
        if !k.starts_with("rev:") {
            out = out.replace(real.as_str(), &format!("${k}"));
        }
    }
    // strip volatile bytes that differ legitimately across impls
    // (frozen_work_json identical since canonical; command digest is stable)
    crate::hash::sha256(out.as_bytes())
}

fn resolve(v: &Value, syms: &Syms) -> Value {
    match v {
        Value::String(s) => {
            if let Some(name) = s.strip_prefix("$REV:") {
                json!(syms
                    .get(&format!("rev:{name}"))
                    .and_then(|x| x.parse::<u64>().ok())
                    .unwrap_or(u64::MAX))
            } else if let Some(real) = s.strip_prefix('$').and_then(|k| syms.get(k)) {
                json!(real.clone())
            } else {
                v.clone()
            }
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| resolve(x, syms)).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, x)| (k.clone(), resolve(x, syms))).collect())
        }
        _ => v.clone(),
    }
}

#[test]
fn run() {
    let scenario_path = std::env::var("DIFF_SCENARIO").expect("DIFF_SCENARIO");
    let db_path = std::env::var("DIFF_DB").expect("DIFF_DB");
    let out_path = std::env::var("DIFF_OUT").expect("DIFF_OUT");
    let _ = fs::remove_file(&db_path);
    let _ = fs::remove_file(format!("{db_path}-wal"));
    let _ = fs::remove_file(format!("{db_path}-shm"));
    let repo = SqliteTasks::new(&db_path);
    if std::env::var("DIFF_INIT").as_deref() == Ok("19") {
        repo.initialize19().expect("initialize schema-19");
    } else {
        repo.initialize().expect("initialize schema-18");
    }
    let scenario = fs::read_to_string(&scenario_path).expect("scenario");
    let mut out = fs::File::create(&out_path).expect("out");
    let mut syms: Syms = Default::default();
    for (idx, line) in scenario.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let raw: Value = serde_json::from_str(line).expect("op json");
        let op = resolve(&raw, &syms);
        let name = op["op"].as_str().unwrap().to_string();
        let before = state_digest(&db_path, &syms);
        let mut rec = match name.as_str() {
            "admit" | "admit_retry" => {
                let command: Command = serde_json::from_value(op["command"].clone()).unwrap();
                let work: FrozenWork = serde_json::from_value(op["work"].clone()).unwrap();
                let retry = op.get("retry").map(|r| RetryGuard {
                    original_task_id: r["task"].as_str().unwrap().into(),
                    expected_revision: r["expected_revision"].as_u64().unwrap(),
                    selected_items: r["selected_items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_str().unwrap().to_string())
                        .collect::<BTreeSet<_>>(),
                });
                match repo.accept(
                    op["request_id"].as_str().unwrap(),
                    &command,
                    &work,
                    op["run"].as_str().unwrap(),
                    retry.as_ref(),
                ) {
                    Ok(a) => {
                        let tid = match &a.record.outcome {
                            RequestOutcome::Task(s) => Some(s.task.task_id.clone()),
                            _ => None,
                        };
                        if let (Some(sym), Some(tid)) =
                            (op.get("as").and_then(|x| x.as_str()), tid.as_ref())
                        {
                            syms.insert(sym.to_string(), tid.clone());
                            syms.insert(format!("rev:{sym}"), "0".into());
                        }
                        json!({"status":"ok","task":{"task_id":tid.map(|t|sym_of(&syms,&t)),"reused":a.reused}})
                    }
                    Err(e) => json!({"status":"error","code":err_code(&e),"message":e.to_string()}),
                }
            }
            "claim" | "startItem" | "finishItem" | "requestCancel" | "finishExecution"
            | "finishCancellation" | "requireVerification" => {
                let change = match name.as_str() {
                    "claim" => Change::Claim,
                    "startItem" => Change::StartItem(op["item"].as_str().unwrap().into()),
                    "finishItem" => Change::FinishItem(
                        op["item"].as_str().unwrap().into(),
                        serde_json::from_value::<ItemOutcome>(op["outcome"].clone()).unwrap(),
                    ),
                    "requestCancel" => Change::RequestCancel,
                    "finishExecution" => Change::FinishExecution,
                    "finishCancellation" => Change::FinishCancellation,
                    _ => Change::RequireVerification {
                        item_id: op["item"].as_str().unwrap().into(),
                        reason: op["reason"].as_str().unwrap().into(),
                    },
                };
                let tid = op["task"].as_str().unwrap().to_string();
                match repo.transition(
                    &tid,
                    op["revision"].as_u64().unwrap(),
                    op["run"].as_str().unwrap(),
                    change,
                ) {
                    Ok(s) => {
                        if let Some(k) =
                            syms.iter().find(|(_, v)| **v == tid).map(|(k, _)| k.clone())
                        {
                            syms.insert(format!("rev:{k}"), s.task.state_revision.to_string());
                        }
                        json!({"status":"ok","task":projection(&s,&syms)})
                    }
                    Err(e) => json!({"status":"error","code":err_code(&e),"message":e.to_string()}),
                }
            }
            "candidates" => match repo.candidates(op["run"].as_str().unwrap()) {
                Ok(list) => json!({"status":"ok","candidates":list.iter().map(|s| sym_of(&syms,&s.task.task_id)).collect::<Vec<_>>()}),
                Err(e) => json!({"status":"error","code":err_code(&e),"message":e.to_string()}),
            },
            "reconcile" => match repo.reconcile(op["run"].as_str().unwrap()) {
                Ok(n) => json!({"status":"ok","reconciled":n}),
                Err(e) => json!({"status":"error","code":err_code(&e),"message":e.to_string()}),
            },
            "check_record" => {
                let dir = std::env::var("DIFF_FIXTURES")
                    .unwrap_or_else(|_| "scripts/node_migration_p3/records".into());
                let path = std::path::Path::new(&dir).join(op["file"].as_str().unwrap());
                let raw: Value = serde_json::from_str(
                    &fs::read_to_string(&path).expect("record file"),
                )
                .unwrap();
                match crate::market_storage::compat::check(&raw) {
                    Ok(c) => json!({"status":"ok","version_id":c.version().version_id}),
                    Err(e) => {
                        let msg = e.to_string();
                        json!({"status":"error","code":market_code(&e),"message":msg})
                    }
                }
            }
            "checkpoint" => {
                let tid = op["task"].as_str().unwrap().to_string();
                match repo.read_task(&tid) {
                    Ok(Some(s)) => {
                        if let Some(k) =
                            syms.iter().find(|(_, v)| **v == tid).map(|(k, _)| k.clone())
                        {
                            syms.insert(format!("rev:{k}"), s.task.state_revision.to_string());
                        }
                        json!({"status":"ok","task":projection(&s,&syms)})
                    }
                    Ok(None) => json!({"status":"ok","task":null}),
                    Err(e) => json!({"status":"error","code":err_code(&e),"message":e.to_string()}),
                }
            }
            other => panic!("unknown op {other}"),
        };
        if rec["status"] == "error" {
            rec["unchanged"] = json!(state_digest(&db_path, &syms) == before);
        }
        rec["seq"] = json!(idx);
        rec["op"] = json!(name);
        writeln!(out, "{}", serde_json::to_string(&rec).unwrap()).unwrap();
    }
}
