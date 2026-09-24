//! NM-049 — differential harness (Rust oracle side). Reads a JSONL
//! scenario from DIFF_SCENARIO, drives the REAL SqliteTasks /
//! task_control APIs against DIFF_DB, and emits
//! {seq,op,status,code,message,task,unchanged} JSONL to DIFF_OUT.
//!
//! Run: DIFF_SCENARIO=... DIFF_DB=... DIFF_OUT=... cargo test \
//!        diff_harness::run -- --nocapture --test-threads=1
use crate::{
    market_storage::{Error as MarketError, Publication, PublishIntent, Receipt, Scope},
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Change, Command, Error, FrozenWork, ItemOutcome, RequestOutcome, RetryGuard,
        TaskRepository, TaskSnapshot,
    },
};
use rusqlite::OptionalExtension;
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

fn storage_task(e: impl std::fmt::Display) -> Error {
    Error::StorageUnavailable(e.to_string())
}

fn resolve_str(v: &Value, syms: &Syms) -> String {
    let s = v.as_str().unwrap();
    if let Some(key) = s.strip_prefix('$') {
        syms.get(key).cloned().unwrap_or_else(|| s.to_string())
    } else {
        s.to_string()
    }
}

/// Deterministic digest over all market tables: ordered rows, values
/// normalized (int->number, text->string, blob->hex). Byte-identical
/// staging must produce identical digests on both runners.
fn market_digest(repo: &SqliteTasks, syms: &Syms) -> String {
    repo.read(|c| {
        let tables: &[(&str, &str)] = &[
            ("data_versions", "version_id"),
            ("payloads", "payload_id"),
            ("bar_segments", "segment_id"),
            ("segment_bars", "segment_id, trade_date"),
            ("manifest_nodes", "node_id"),
            ("yearly_bars", "version_id, year"),
            ("yearly_lineage", "version_id"),
            ("effective_heads", "symbol, period, adjustment"),
            ("publication_batches", "publication_id"),
            ("task_publications", "task_id, version_id"),
            ("market_update_results", "result_id"),
        ];
        let mut acc = String::new();
        for (table, pk) in tables {
            let sql = format!("SELECT * FROM {table} ORDER BY {pk}");
            let mut stmt = c.prepare(&sql).map_err(storage_task)?;
            let n = stmt.column_count();
            let rows = stmt
                .query_map([], |r| {
                    (0..n)
                        .map(|i| r.get::<_, rusqlite::types::Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map_err(storage_task)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage_task)?;
            acc.push_str(table);
            for row in rows {
                let vals: Vec<Value> = row
                    .iter()
                    .map(|v| match v {
                        rusqlite::types::Value::Null => Value::Null,
                        rusqlite::types::Value::Integer(i) => json!(i),
                        rusqlite::types::Value::Real(f) => json!(f),
                        rusqlite::types::Value::Text(t) => json!(t),
                        rusqlite::types::Value::Blob(b) => {
                            json!(b.iter().map(|x| format!("{x:02x}")).collect::<String>())
                        }
                    })
                    .collect();
                acc.push_str(&serde_json::to_string(&vals).unwrap());
                acc.push('\n');
            }
        }
        let mut acc = acc;
        for (k, real) in syms {
            if !k.starts_with("rev:") {
                acc = acc.replace(real.as_str(), &format!("${k}"));
            }
        }
        Ok(crate::hash::sha256(acc.as_bytes()))
    })
    .unwrap()
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
    let noinit = std::env::var("DIFF_NOINIT").is_ok();
    if !noinit {
        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_file(format!("{db_path}-wal"));
        let _ = fs::remove_file(format!("{db_path}-shm"));
    }
    let repo = SqliteTasks::new(&db_path);
    if noinit {
    } else if std::env::var("DIFF_INIT").as_deref() == Ok("19") {
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
            "seed" => json!({"status":"ok","seeded":true}),
            "lookup_task" => {
                let ids: Vec<String> = repo
                    .read(|c| {
                        c.prepare("SELECT task_id FROM tasks ORDER BY rowid")
                            .map_err(storage_task)?
                            .query_map([], |r| r.get::<_, String>(0))
                            .map_err(storage_task)?
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(storage_task)
                    })
                    .unwrap();
                if let Some(sym) = op.get("sym").and_then(|s| s.as_str()) {
                    for (i, id) in ids.iter().enumerate() {
                        let key = if i == 0 { sym.to_string() } else { format!("{sym}_{}", i + 1) };
                        syms.insert(key, id.clone());
                    }
                }
                json!({"status":"ok","task_id":ids.first()})
            }
            "stage" => {
                let dir = std::env::var("DIFF_FIXTURES")
                    .unwrap_or_else(|_| "scripts/node_migration_p3/records".into());
                let name = op["record"].as_str().unwrap();
                let raw: Value = serde_json::from_str(
                    &fs::read_to_string(std::path::Path::new(&dir).join(name)).unwrap(),
                ).unwrap();
                let market = SqliteMarket::new(repo.clone());
                match market.stage(&raw) {
                    Ok(()) => json!({"status":"ok","digest":market_digest(&repo, &syms)}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "read_result" => {
                let id = resolve_str(&op["result_id"], &syms);
                let market = SqliteMarket::new(repo.clone());
                match market.read(|c| crate::sqlite_market::update::read_result(c, &id)) {
                    Ok(None) => json!({"status":"ok","found":false}),
                    Ok(Some((audit, receipt))) => json!({"status":"ok","found":true,
                        "audit_sha256":crate::hash::sha256(serde_json::to_string(&audit).unwrap().as_bytes()),
                        "receipt":serde_json::to_value(&receipt).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "range_receipt" => {
                let id = resolve_str(&op["result_id"], &syms);
                let market = SqliteMarket::new(repo.clone());
                match market.read(|c| crate::sqlite_market::update::read_result(c, &id)) {
                    Ok(None) => json!({"status":"ok","found":false}),
                    Ok(Some((audit, receipt))) => json!({"status":"ok","found":true,
                        "audit_sha256":crate::hash::sha256(serde_json::to_string(&audit).unwrap().as_bytes()),
                        "receipt":serde_json::to_value(&receipt).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "item_receipt" => {
                let task = resolve_str(&op["task_id"], &syms);
                let item = op["item_id"].as_str().unwrap().to_string();
                let market = SqliteMarket::new(repo.clone());
                use crate::market_update::publication::RangeRepository;
                match RangeRepository::item_receipt(&market, &task, &item) {
                    Ok(None) => json!({"status":"ok","found":false}),
                    Ok(Some(r)) => json!({"status":"ok","found":true,
                        "receipt":serde_json::to_value(&r).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "commit_range" => {
                let p = &op["plan"];
                let dir = std::env::var("DIFF_FIXTURES")
                    .unwrap_or_else(|_| "scripts/node_migration_p3/records".into());
                let load = |name: &str| -> crate::market_storage::compat::CheckedRecord {
                    let raw: Value = serde_json::from_str(
                        &fs::read_to_string(std::path::Path::new(&dir).join(name)).unwrap(),
                    )
                    .unwrap();
                    crate::market_storage::compat::check(&raw).unwrap()
                };
                let mut cmdv = p["command"].clone();
                if let Some(er) = cmdv["expected_revision"].as_str() {
                    cmdv["expected_revision"] = resolve(&json!(er), &syms);
                }
                let mut command: Publication =
                    serde_json::from_value(cmdv).unwrap();
                if let Some(t) = command.task_id.strip_prefix('$') {
                    command.task_id = syms.get(t).cloned().unwrap_or(command.task_id);
                }
                {
                    let resolve_opt = |t: &mut Option<String>| {
                        if let Some(v) = t {
                            if let Some(sym) = v.strip_prefix('$') {
                                *v = syms.get(sym).cloned().unwrap_or_else(|| v.clone());
                            }
                        }
                    };
                    match &mut command.intent {
                        PublishIntent::Daily { expected_head } => resolve_opt(expected_head),
                        PublishIntent::CurrentHead {
                            expected_daily_head,
                            expected_yearly_head,
                        } => {
                            if let Some(sym) = expected_daily_head.strip_prefix('$') {
                                *expected_daily_head =
                                    syms.get(sym).cloned().unwrap_or_else(|| expected_daily_head.clone());
                            }
                            resolve_opt(expected_yearly_head);
                        }
                        _ => {}
                    }
                }
                let record = load(p["record"].as_str().unwrap());
                let base = load(p["base"].as_str().unwrap());
                let required_heads: Vec<(Scope, Option<String>)> = p["required_heads"]
                    .as_array()
                    .map(|a| a.iter().map(|h| {
                        let sym = h[0].as_str().unwrap().to_string();
                        let per = h[1].as_str().unwrap();
                        let exp = h.get(2).and_then(|e| e.as_str()).map(|e| {
                            e.strip_prefix('$')
                                .and_then(|k| syms.get(k))
                                .cloned()
                                .unwrap_or_else(|| e.to_string())
                        });
                        (
                            Scope {
                                symbol: sym,
                                period: if per == "day" {
                                    crate::market_data::Period::Day
                                } else {
                                    crate::market_data::Period::Year
                                },
                            },
                            exp,
                        )
                    }).collect())
                    .unwrap_or_default();
                let promote = p.get("promote").and_then(|pr| {
                    if pr.is_null() { None } else {
                        Some(Scope {
                            symbol: pr["symbol"].as_str().unwrap().to_string(),
                            period: if pr["period"].as_str().unwrap() == "day" {
                                crate::market_data::Period::Day
                            } else {
                                crate::market_data::Period::Year
                            },
                        })
                    }
                });
                let plan = crate::market_update::publication::RangePlan {
                    publication: crate::market_storage::PublishPlan {
                        command,
                        record,
                        required_heads,
                        promote,
                        base_digest: p["base_digest"].as_str().map(|x| x.to_string()),
                    },
                    base,
                    no_change: p["no_change"].as_bool().unwrap_or(false),
                    audit: {
                        let mut a = p["audit"].clone();
                        fn deep(v: &mut Value, syms: &Syms) {
                            match v {
                                Value::String(s) => {
                                    if let Some(k) = s.strip_prefix('$') {
                                        if let Some(r) = syms.get(k) {
                                            *s = r.clone();
                                        }
                                    }
                                }
                                Value::Array(xs) => xs.iter_mut().for_each(|x| deep(x, syms)),
                                Value::Object(m) => {
                                    m.values_mut().for_each(|x| deep(x, syms))
                                }
                                _ => {}
                            }
                        }
                        deep(&mut a, &syms);
                        a
                    },
                    audit_payloads: p["audit_payloads"]
                        .as_array()
                        .map(|a| a.iter().map(|x| crate::sqlite_market::envelope::Payload {
                            id: x["id"].as_str().unwrap().to_string(),
                            json: x["json"].as_str().unwrap().to_string(),
                        }).collect())
                        .unwrap_or_default(),
                };
                let market = SqliteMarket::new(repo.clone());
                use crate::market_update::publication::RangeRepository;
                match RangeRepository::commit_range(&market, &plan) {
                    Ok(r) => json!({"status":"ok",
                        "receipt":serde_json::to_value(&r).unwrap(),
                        "digest":market_digest(&repo, &syms)}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "exec_sql" => {
                let c = rusqlite::Connection::open(&db_path).unwrap();
                match c.execute_batch(op["sql"].as_str().unwrap()) {
                    Ok(()) => json!({"status":"ok"}),
                    Err(e) => json!({"status":"error","code":"Storage","message":e.to_string()}),
                }
            }
            "head" => {
                let scope = Scope {
                    symbol: op["symbol"].as_str().unwrap().into(),
                    period: serde_json::from_value(json!(match op["period"].as_str().unwrap() {
                        "day" => "Day",
                        "year" => "Year",
                        other => other,
                    })).unwrap(),
                };
                let market = SqliteMarket::new(repo.clone());
                match market.read(|c| crate::sqlite_market::head(c, &scope)) {
                    Ok(v) => json!({"status":"ok","version_id":v}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "read_record" => {
                let id = op["version_id"].as_str().unwrap().to_string();
                let fixed = op["mode"].as_str().unwrap() == "fixed";
                let market = SqliteMarket::new(repo.clone());
                let res = if fixed { market.fixed(&id) } else { market.candidate(&id) };
                match res {
                    Ok(r) => {
                        let v = &r.version;
                        json!({"status":"ok","record":{
                            "version_id":v.version_id,
                            "integrity_sha256":r.raw["integrity_sha256"],
                            "period":serde_json::to_value(&v.period).unwrap(),
                            "records":v.records.len(),
                            "first":v.records.first().map(|b|b.date.to_string()),
                            "last":v.records.last().map(|b|b.date.to_string()),
                        }})
                    }
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "range" => {
                let scope = Scope {
                    symbol: op["symbol"].as_str().unwrap().into(),
                    period: serde_json::from_value(json!(match op["period"].as_str().unwrap() {
                        "day" => "Day",
                        "year" => "Year",
                        other => other,
                    })).unwrap(),
                };
                let rng = crate::market_storage::Range {
                    start: op["start"].as_str().unwrap().parse().unwrap(),
                    end: op["end"].as_str().unwrap().parse().unwrap(),
                    after: op.get("after").and_then(|a| a.as_str()).map(|d| d.parse().unwrap()),
                    limit: op["limit"].as_u64().unwrap() as usize,
                };
                let id = op.get("version_id").and_then(|v| v.as_str()).map(|x| x.to_string());
                let market = SqliteMarket::new(repo.clone());
                match market.range(&scope, id.as_deref(), &rng) {
                    Ok(p) => json!({"status":"ok","page":{
                        "integrity_sha256":p.integrity_sha256,
                        "version_id":p.version.version_id,
                        "next_after":p.next_after.map(|d|d.to_string()),
                        "rows":p.rows.iter().map(|b|json!({
                            "date":b.date.to_string(),
                            "open":b.open.normalize().to_string(),
                            "high":b.high.normalize().to_string(),
                            "low":b.low.normalize().to_string(),
                            "close":b.close.normalize().to_string(),
                            "volume":b.volume.map(|x|x.normalize().to_string()),
                            "amount":b.amount.map(|x|x.normalize().to_string()),
                        })).collect::<Vec<_>>(),
                    }}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "versions" => {
                let scope = Scope {
                    symbol: op["symbol"].as_str().unwrap().into(),
                    period: serde_json::from_value(json!(match op["period"].as_str().unwrap() {
                        "day" => "Day",
                        "year" => "Year",
                        other => other,
                    })).unwrap(),
                };
                let market = SqliteMarket::new(repo.clone());
                match market.versions(&scope) {
                    Ok(list) => json!({"status":"ok","versions":list.iter().map(|v|json!({
                        "version_id":v.version_id,"published_at":v.published_at.to_rfc3339(),
                        "records":v.records.len()})).collect::<Vec<_>>()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
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

/// Build a market-read fixture DB on DIFF_DB: schema-18 init, one task
/// (publication FKs), then real `stage()` for each record file listed in
/// DIFF_RECORDS (comma-separated, under DIFF_FIXTURES), each marked
/// published with consistent publication_batches/task_publications rows
/// and an effective_heads entry.
#[test]
fn build_market_fixture() {
    let db_path = std::env::var("DIFF_DB").expect("DIFF_DB");
    let _ = fs::remove_file(&db_path);
    let _ = fs::remove_file(format!("{db_path}-wal"));
    let _ = fs::remove_file(format!("{db_path}-shm"));
    let repo = SqliteTasks::new(&db_path);
    repo.initialize().expect("init");
    // Task + item for publication FK references
    let acc = repo
        .accept(
            "req-pub",
            &serde_json::from_value::<Command>(json!({
                "key":{"operation":"publish","dataset":"market","contract_version":"1"},
                "intent":{}
            }))
            .unwrap(),
            &serde_json::from_value::<FrozenWork>(json!({
                "items":[{"item_id":"i1","target":{"symbol":"sh.600006","period":"day"},"progress_unit":"x","frozen_inputs":{},"scope_keys":["[\"market\",\"sh.600006\",\"day\"]"]}],
                "scopes":["[\"market\",\"sh.600006\",\"day\"]"]
            }))
            .unwrap(),
            "run-pub",
            None,
        )
        .unwrap();
    let task_id = match acc.record.outcome {
        RequestOutcome::Task(s) => s.task.task_id,
        _ => panic!("task outcome"),
    };
    let dir = std::env::var("DIFF_FIXTURES")
        .unwrap_or_else(|_| "scripts/node_migration_p3/records".into());
    let list = std::env::var("DIFF_RECORDS").expect("DIFF_RECORDS");
    let market = SqliteMarket::new(repo.clone());
    let mut result_vid: Option<String> = None;
    for entry in list.split(',') {
        let mut parts = entry.split('|');
        let name = parts.next().unwrap();
        let mut pub_override: Option<String> = None;
        let mut nohead = false;
        let mut validated = false;
        for m in parts {
            if let Some(p) = m.strip_prefix("pub=") {
                pub_override = Some(p.to_string());
            } else if m == "nohead" {
                nohead = true;
            } else if m == "validated" {
                validated = true;
            }
        }
        let raw: Value = serde_json::from_str(
            &fs::read_to_string(std::path::Path::new(&dir).join(name)).unwrap(),
        )
        .unwrap();
        market.stage(&raw).expect("stage");
        let vid = raw["version_id"].as_str().unwrap().to_string();
        let pub_id = pub_override.unwrap_or_else(|| format!("pub-{vid}"));
        if pub_id == "res-ch" {
            result_vid = Some(vid.clone());
        }
        if validated {
            repo.write(|c| {
                c.execute(
                    "UPDATE data_versions SET state='validated' WHERE version_id=?1",
                    [vid.clone()],
                )
                .map_err(storage_task)
            })
            .unwrap();
            continue;
        }
        let command = serde_json::to_string(&Publication {
            publication_id: pub_id.clone(),
            version_id: vid.clone(),
            task_id: task_id.clone(),
            item_id: "i1".into(),
            run_id: "run-pub".into(),
            expected_revision: 0,
            intent: PublishIntent::Daily { expected_head: None },
        })
        .unwrap();
        let receipt = serde_json::to_string(&Receipt {
            publication_id: pub_id.clone(),
            version_id: vid.clone(),
            task_id: task_id.clone(),
            task_revision: 0,
        })
        .unwrap();
        let p: String = raw["period"].as_str().unwrap().to_string();
        let period = if p == "Day" { "day" } else { "year" }.to_string();
        let symbol = raw["symbol"].as_str().unwrap().to_string();
        let head_sql = "INSERT OR REPLACE INTO effective_heads VALUES(?1,?2,'qfq',?3)";
        repo.write(|c| {
            c.execute(
                "UPDATE data_versions SET state='published' WHERE version_id=?1",
                [&vid],
            )
            .map_err(storage_task)?;
            c.execute(
                "INSERT INTO publication_batches VALUES(?1,?2,?3,'i1',?4,?5)",
                rusqlite::params![pub_id, vid, task_id, command, receipt],
            )
            .map_err(storage_task)?;
            c.execute(
                "INSERT INTO task_publications VALUES(?1,?2,?3)",
                rusqlite::params![task_id, vid, pub_id],
            )
            .map_err(storage_task)?;
            if !nohead {
                c.execute(head_sql, rusqlite::params![symbol, period, vid])
                    .map_err(storage_task)?;
            }
            Ok(())
        })
        .unwrap();
    }
    // DIFF_RESULTS=1: drive item i1 to finished-success, then seed two
    // market_update_results rows (no_change + changed) with consistent
    // command/receipt/digest linkage for read_result coverage.
    if std::env::var("DIFF_RESULTS").is_ok() {
        repo.transition(&task_id, 0, "run-pub", Change::Claim).unwrap();
        repo.transition(&task_id, 1, "run-pub", Change::StartItem("i1".into()))
            .unwrap();
        repo.transition(
            &task_id,
            2,
            "run-pub",
            Change::FinishItem(
                "i1".into(),
                ItemOutcome { success: true, error: None, evidence: None },
            ),
        )
        .unwrap();
        let snap = repo
            .transition(&task_id, 3, "run-pub", Change::FinishExecution)
            .unwrap();
        let rev = snap.task.state_revision;
        // second task for the changed-result row (UNIQUE(task_id,item_id))
        let acc2 = repo
            .accept(
                "req-pub2",
                &serde_json::from_value::<Command>(json!({
                    "key":{"operation":"publish","dataset":"market","contract_version":"1"},
                    "intent":{}
                }))
                .unwrap(),
                &serde_json::from_value::<FrozenWork>(json!({
                    "items":[{"item_id":"i1","target":{"symbol":"sh.600006","period":"day"},"progress_unit":"x","frozen_inputs":{},"scope_keys":["[\"market\",\"sh.600006\",\"day\"]"]}],
                    "scopes":["[\"market\",\"sh.600006\",\"day\"]"]
                }))
                .unwrap(),
                "run-pub",
                None,
            )
            .unwrap();
        let task2 = match acc2.record.outcome {
            RequestOutcome::Task(s) => s.task.task_id,
            _ => panic!("task outcome"),
        };
        repo.transition(&task2, 0, "run-pub", Change::Claim).unwrap();
        repo.transition(&task2, 1, "run-pub", Change::StartItem("i1".into()))
            .unwrap();
        repo.transition(
            &task2,
            2,
            "run-pub",
            Change::FinishItem(
                "i1".into(),
                ItemOutcome { success: true, error: None, evidence: None },
            ),
        )
        .unwrap();
        repo.transition(&task2, 3, "run-pub", Change::FinishExecution).unwrap();
        let base_sha = fs::read_to_string(
            std::path::Path::new(&dir).join("valid_daily.json"),
        )
        .unwrap();
        let base_sha: Value = serde_json::from_str(&base_sha).unwrap();
        let base_sha = base_sha["integrity_sha256"].as_str().unwrap().to_string();
        let res_vid = result_vid.expect("DIFF_RESULTS needs a |pub=res-ch record");
        let res_sha: String = repo
            .read(|c| {
                c.query_row(
                    "SELECT content_digest FROM data_versions WHERE version_id='legacy-daily'",
                    [],
                    |r| r.get(0),
                )
                .map_err(storage_task)
            })
            .unwrap();
        let seg_ref: String = repo
            .read(|c| {
                c.query_row("SELECT payload_id FROM payloads LIMIT 1", [], |r| r.get(0))
                    .map_err(storage_task)
            })
            .unwrap();
        let _ = res_sha;
        for (rid, res_v, no_change, tid) in [
            ("res-nc", "legacy-daily", true, task_id.clone()),
            ("res-ch", res_vid.as_str(), false, task2.clone()),
        ] {
            let command = json!({
                "publication_id": rid, "version_id": res_v, "task_id": tid,
                "item_id": "i1", "run_id": "run-pub", "expected_revision": 0,
                "intent": {"Daily": {"expected_head": "legacy-daily"}},
            });
            let receipt = json!({
                "publication_id": rid, "version_id": res_v,
                "task_id": tid, "task_revision": rev,
            });
            let result_sha: String = repo
                .read(|c| {
                    c.query_row(
                        "SELECT content_digest FROM data_versions WHERE version_id=?1",
                        [res_v],
                        |r| r.get(0),
                    )
                    .map_err(storage_task)
                })
                .unwrap();
            let audit = json!({
                "algorithm_version": "local-range-update-v1",
                "command": command,
                "base_integrity_sha256": base_sha,
                "result_integrity_sha256": result_sha,
                "segment": {"bars": {"$ref": format!("payload:{seg_ref}")}},
                "range": {"start": "2025-01-02", "end": "2025-12-31"},
                "evidence": {"base_version_id": "legacy-daily"},
                "combined": true,
                "no_change": no_change,
            });
            let audit_json = serde_json::to_string(&audit).unwrap();
            let receipt_json = serde_json::to_string(&receipt).unwrap();
            let digest = crate::hash::sha256(
                serde_json::json!([audit_json, receipt_json]).to_string().as_bytes(),
            );
            repo.write(|c| {
                c.execute(
                    "INSERT INTO market_update_results VALUES(?1,?2,'i1','legacy-daily',?3,?4,?5,?6,?7)",
                    rusqlite::params![
                        rid, tid, res_v, no_change, audit_json, digest, receipt_json
                    ],
                )
                .map_err(storage_task)?;
                if !no_change {
                    let pub_command = serde_json::to_string(&serde_json::json!({
                        "publication_id": rid, "version_id": res_v, "task_id": tid,
                        "item_id": "i1", "run_id": "run-pub", "expected_revision": 0,
                        "intent": {"Daily": {"expected_head": "legacy-daily"}},
                    }))
                    .unwrap();
                    c.execute(
                        "UPDATE publication_batches SET receipt_json=?1,command_json=?2,task_id=?3 WHERE publication_id='res-ch'",
                        rusqlite::params![receipt_json, pub_command, tid],
                    )
                    .map_err(storage_task)?;
                    c.execute(
                        "UPDATE task_publications SET task_id=?1 WHERE publication_id='res-ch'",
                        [tid],
                    )
                    .map_err(storage_task)?;
                }
                Ok(())
            })
            .unwrap();
        }
    }
    // DIFF_TASKS="day,day,year": extra publish-capable tasks left in
    // running state with item i1 running (for commit_range ops).
    if let Ok(specs) = std::env::var("DIFF_TASKS") {
        for (i, per) in specs.split(',').enumerate() {
            let occ = format!("[\"market\",\"sh.600006\",\"{per}\"]");
            let acc = repo
                .accept(
                    &format!("req-commit-{i}"),
                    &serde_json::from_value::<Command>(json!({
                        "key":{"operation":"publish","dataset":"market","contract_version":"1"},
                        "intent":{}
                    }))
                    .unwrap(),
                    &serde_json::from_value::<FrozenWork>(json!({
                        "items":[{"item_id":"i1","target":{"symbol":"sh.600006","period":per},"progress_unit":"x","frozen_inputs":{},"scope_keys":[occ]}],
                        "scopes":[occ]
                    }))
                    .unwrap(),
                    "run-pub",
                    None,
                )
                .unwrap();
            let tid = match acc.record.outcome {
                RequestOutcome::Task(s) => s.task.task_id,
                _ => panic!("task outcome"),
            };
            repo.transition(&tid, 0, "run-pub", Change::Claim).unwrap();
            repo.transition(&tid, 1, "run-pub", Change::StartItem("i1".into()))
                .unwrap();
        }
    }
 }
