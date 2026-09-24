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
        let sym_text = |v: &mut Vec<Value>| {
            for x in v.iter_mut() {
                if let Value::String(t) = x {
                    for (k, real) in syms {
                        if !k.starts_with("rev:") {
                            *t = t.replace(real.as_str(), &format!("${k}"));
                        }
                    }
                }
            }
        };
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
            acc.push('\n');
            for row in rows {
                let mut vals: Vec<Value> = row
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
                // market_update_results.result_sha256 binds the audit bytes
                // including the random task id — recompute over the
                // symbolized audit so the digest input is runner-stable.
                if *table == "market_update_results" && vals.len() == 9 {
                    if let Value::String(audit) = vals[6].clone() {
                        let mut symmed = audit;
                        for (k, real) in syms {
                            if !k.starts_with("rev:") {
                                symmed = symmed.replace(real.as_str(), &format!("${k}"));
                            }
                        }
                        vals[7] = json!(crate::hash::sha256(symmed.as_bytes()));
                    }
                }
                sym_text(&mut vals);
                acc.push_str(&serde_json::to_string(&vals).unwrap());
                acc.push('\n');
            }
        }
        for (k, real) in syms {
            if !k.starts_with("rev:") {
                acc = acc.replace(real.as_str(), &format!("${k}"));
            }
        }
        // wall-clock columns (created_at/published_at inside rows and
        // audit/receipt JSON) differ per runner — normalize RFC3339 text.
        let acc = norm_ts_text(&acc);
        // task ids are random per-runner — sort symbolized lines so the
        // digest input is order-stable (same as state_digest).
        let mut sorted: Vec<&str> = acc.lines().collect();
        sorted.sort();
        Ok(crate::hash::sha256(sorted.join("\n").as_bytes()))
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

/// Replace RFC3339 timestamps (`YYYY-MM-DDTHH:MM:SS[.frac][+00:00|Z]`)
/// with a fixed marker — wall-clock bytes differ across runners.
fn norm_ts_text(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        // try to match a timestamp at position i
        let is_dig = |j: usize| j < b.len() && b[j].is_ascii_digit();
        let fixed = |j: usize, c: u8| j < b.len() && b[j] == c;
        let mut j = i;
        let ok = is_dig(j) && is_dig(j + 1) && is_dig(j + 2) && is_dig(j + 3)
            && fixed(j + 4, b'-') && is_dig(j + 5) && is_dig(j + 6)
            && fixed(j + 7, b'-') && is_dig(j + 8) && is_dig(j + 9)
            && fixed(j + 10, b'T') && is_dig(j + 11) && is_dig(j + 12)
            && fixed(j + 13, b':') && is_dig(j + 14) && is_dig(j + 15)
            && fixed(j + 16, b':') && is_dig(j + 17) && is_dig(j + 18);
        if ok {
            j += 19;
            if j < b.len() && b[j] == b'.' {
                j += 1;
                while is_dig(j) { j += 1; }
            }
            if j < b.len() && b[j] == b'Z' {
                j += 1;
            } else if j + 5 < b.len() && b[j] == b'+' && is_dig(j + 1)
                && is_dig(j + 2) && b[j + 3] == b':' && is_dig(j + 4) && is_dig(j + 5)
            {
                j += 6;
            } else {
                out.push_str(&s[i..j]);
                i = j;
                continue;
            }
            out.push_str("<TS>");
            i = j;
            continue;
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
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
    // Compact (schema-19) DBs carry the write-side tables too. Wall-clock
    // columns (created_at, published_at) and the integrity digest that
    // binds them are normalized: the digest column is replaced by the
    // recomputed identity digest over a fixed stamp, proving the digest
    // pipeline without comparing wall-clock bytes.
    let compact = crate::sqlite_market::is_compact_schema(&c).unwrap_or(false);
    if compact {
        dump(&c, "SELECT version_key,version_id,entity,period,adjustment,state,hex(source_binding_digest),parent_version_key,manifest_mode,delta_depth,record_count,first_day,last_day,coverage_status,hex(evidence_digest),hex(projection_digest),digest_format FROM data_versions ORDER BY version_key", "CV", 17, &mut lines);
        let mut st = c.prepare("SELECT version_key,published_at IS NOT NULL FROM data_versions ORDER BY version_key").unwrap();
        let keys: Vec<(i64, bool)> = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?)))
            .unwrap().map(|r| r.unwrap()).collect();
        for (k, pubbed) in keys {
            let idhex = if pubbed {
                crate::sqlite_market::compact_versions::recompute_integrity(&c, k, "0000-01-01T00:00:00+00:00")
                    .map(|d| crate::sqlite_market::compact_repo::hex_bytes(&d))
                    .unwrap_or_else(|_| "<ERR>".into())
            } else { String::new() };
            lines.push(format!("CVI:{k}:{idhex}"));
        }
        dump(&c, "SELECT segment_key,hex(segment_digest),entity,period,first_day,last_day,row_count FROM bar_segments ORDER BY segment_key", "CS", 7, &mut lines);
        dump(&c, "SELECT segment_key,trade_day,open_cents,high_cents,low_cents,close_cents,volume,amount,r7_raw FROM segment_bars ORDER BY segment_key,trade_day", "CB", 9, &mut lines);
        dump(&c, "SELECT version_key,seq,segment_key,clip_start_day,clip_end_day,row_count FROM version_checkpoint_clips ORDER BY version_key,seq", "CC", 6, &mut lines);
        dump(&c, "SELECT version_key,seq,delta_kind,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count FROM version_delta_clips ORDER BY version_key,seq", "CD", 9, &mut lines);
        dump(&c, "SELECT version_key,base_daily_version_key,algorithm_version,first_trade_day,last_trade_day,as_of_day FROM yearly_lineage ORDER BY version_key", "CY", 6, &mut lines);
        dump(&c, "SELECT entity,period,adjustment,version_key FROM effective_heads ORDER BY 1,2,3", "CH", 4, &mut lines);
        dump(&c, "SELECT hex(evidence_digest),evidence_kind,canonical_json FROM evidence_bundles ORDER BY evidence_digest", "CE", 3, &mut lines);
        dump(&c, "SELECT hex(source_binding_digest),generation_id,hex(generation_manifest_sha256),hex(day_member_sha256),hex(security_event_sha256),hex(evidence_digest) FROM source_bindings ORDER BY source_binding_digest", "CSB", 6, &mut lines);
        dump(&c, "SELECT task_id,version_key,result_id FROM task_publications ORDER BY 1,2", "CTP", 3, &mut lines);
        dump(&c, "SELECT result_id,result_kind FROM result_registry ORDER BY result_id", "CRR", 2, &mut lines);
        // result_digest binds the runner-local task id — recompute it with
        // the task id symbolized so the digest verifies the pipeline
        // deterministically across runners.
        let vid = |c: &rusqlite::Connection, k: i64| -> String {
            c.query_row("SELECT version_id FROM data_versions WHERE version_key=?1",
                [k], |r| r.get(0)).unwrap_or_default()
        };
        let hx = |x: String| -> Option<[u8; 32]> {
            if x.len() != 64 { return None; }
            let mut d = [0u8; 32];
            for (i, b) in d.iter_mut().enumerate() {
                *b = u8::from_str_radix(&x[i * 2..i * 2 + 2], 16).unwrap();
            }
            Some(d)
        };
        {
            let mut st = c.prepare("SELECT result_key,result_id,task_id,item_id,base_version_key,result_version_key,outcome,range_start_day,range_end_day,algorithm_version,hex(source_binding_digest),continuity_mode,prefix_bytes,hex(base_projection_digest),hex(result_projection_digest),hex(delta_digest),hex(evidence_digest),task_revision,hex(result_digest) FROM market_update_results ORDER BY result_key").unwrap();
            let rows: Vec<Vec<rusqlite::types::Value>> = st.query_map([], |r| {
                Ok((0..19).map(|i| r.get::<_, rusqlite::types::Value>(i).unwrap()).collect())
            }).unwrap().map(|r| r.unwrap()).collect();
            for r in rows {
                let vs = |i: usize| -> String {
                    match &r[i] { rusqlite::types::Value::Null => String::new(),
                        rusqlite::types::Value::Integer(x) => x.to_string(),
                        rusqlite::types::Value::Text(x) => x.clone(),
                        rusqlite::types::Value::Real(x) => x.to_string(),
                        rusqlite::types::Value::Blob(x) => crate::hash::sha256(x) }
                };
                let gi = |i: usize| -> i64 {
                    match &r[i] { rusqlite::types::Value::Integer(x) => *x, _ => 0 }
                };
                let go = |i: usize| -> Option<i64> {
                    match &r[i] { rusqlite::types::Value::Integer(x) => Some(*x), _ => None }
                };
                let gd = |i: usize| -> Option<[u8; 32]> {
                    match &r[i] { rusqlite::types::Value::Text(x) => hx(x.clone()), _ => None }
                };
                let outcome = match gi(6) { 1 => crate::compact_market::UpdateOutcome::NoChange,
                    _ => crate::compact_market::UpdateOutcome::Published };
                let cont = match gi(11) { 1 => crate::compact_market::ContinuityMode::Unchanged,
                    2 => crate::compact_market::ContinuityMode::Append,
                    _ => crate::compact_market::ContinuityMode::Full };
                let rd = match (hx(vs(10)), hx(vs(13)), hx(vs(14)), hx(vs(16))) {
                    (Some(sbd), Some(bpd), Some(rpd), Some(ed)) => {
                        let identity = crate::compact_market::MarketUpdateResultIdentity {
                            result_id: vs(1), task_id: sym_of(syms, &vs(2)), item_id: vs(3),
                            base_version_id: vid(&c, gi(4)), result_version_id: vid(&c, gi(5)),
                            outcome, range_start_day: gi(7), range_end_day: gi(8),
                            algorithm_version: vs(9),
                            source_binding_digest: sbd,
                            continuity_mode: cont, prefix_bytes: go(12).map(|x| x as u64),
                            base_projection_digest: bpd,
                            result_projection_digest: rpd,
                            delta_digest: gd(15), evidence_digest: ed,
                            task_revision: gi(17) as u64,
                        };
                        crate::compact_market::market_update_result_digest(&identity)
                            .map(|d| crate::sqlite_market::compact_repo::hex_bytes(&d))
                            .unwrap_or_else(|_| "<ERR>".into())
                    }
                    _ => vs(18),
                };
                lines.push(format!("CMU:{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                    vs(0), vs(1), sym_of(syms, &vs(2)), vs(3), vs(4), vs(5), vs(6), vs(7), vs(8),
                    vs(9), vs(10), vs(11), vs(12), vs(13), vs(14), vs(15), vs(16), vs(17), rd));
            }
        }
        dump(&c, "SELECT result_key,seq,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count FROM market_update_delta_clips ORDER BY result_key,seq", "CMD", 8, &mut lines);
        {
            let mut st = c.prepare("SELECT result_key,publication_id,task_id,item_id,publication_kind,base_version_key,result_version_key,outcome,algorithm_version,hex(source_binding_digest),hex(evidence_digest),task_revision,hex(result_digest) FROM publication_results ORDER BY result_key").unwrap();
            let rows: Vec<Vec<rusqlite::types::Value>> = st.query_map([], |r| {
                Ok((0..13).map(|i| r.get::<_, rusqlite::types::Value>(i).unwrap()).collect())
            }).unwrap().map(|r| r.unwrap()).collect();
            for r in rows {
                let vs = |i: usize| -> String {
                    match &r[i] { rusqlite::types::Value::Null => String::new(),
                        rusqlite::types::Value::Integer(x) => x.to_string(),
                        rusqlite::types::Value::Text(x) => x.clone(),
                        rusqlite::types::Value::Real(x) => x.to_string(),
                        rusqlite::types::Value::Blob(x) => crate::hash::sha256(x) }
                };
                let gi = |i: usize| -> i64 {
                    match &r[i] { rusqlite::types::Value::Integer(x) => *x, _ => 0 }
                };
                let kind = match gi(4) { 1 => crate::compact_market::PublicationKind::Collection,
                    2 => crate::compact_market::PublicationKind::Yearly,
                    _ => crate::compact_market::PublicationKind::Republish };
                let basek = match &r[5] { rusqlite::types::Value::Integer(x) => Some(*x), _ => None };
                let outcome = match gi(7) { 1 => crate::compact_market::UpdateOutcome::NoChange,
                    _ => crate::compact_market::UpdateOutcome::Published };
                let rd = match (hx(vs(9)), hx(vs(10))) {
                    (Some(sbd), Some(ed)) => {
                        let identity = crate::compact_market::PublicationResultIdentity {
                            publication_id: vs(1), task_id: sym_of(syms, &vs(2)), item_id: vs(3),
                            publication_kind: kind,
                            base_version_id: basek.map(|k| vid(&c, k)),
                            result_version_id: vid(&c, gi(6)),
                            outcome, algorithm_version: vs(8),
                            source_binding_digest: sbd, evidence_digest: ed,
                            task_revision: gi(11) as u64,
                        };
                        crate::compact_market::publication_result_digest(&identity)
                            .map(|d| crate::sqlite_market::compact_repo::hex_bytes(&d))
                            .unwrap_or_else(|_| "<ERR>".into())
                    }
                    _ => vs(12),
                };
                lines.push(format!("CPR:{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                    vs(0), vs(1), sym_of(syms, &vs(2)), vs(3), vs(4), vs(5), vs(6), vs(7), vs(8),
                    vs(9), vs(10), vs(11), rd));
            }
        }
    }

    if !compact {
        dump(&c, "SELECT result_id,task_id,item_id,base_version_id,result_version_id,no_change,audit_json,result_sha256,receipt_json FROM market_update_results ORDER BY result_id", "MU", 9, &mut lines);
        dump(&c, "SELECT result_id,task_id,item_id,base_daily_version_id,result_version_id,no_change,audit_json,result_sha256,receipt_json FROM yearly_execution_results ORDER BY result_id", "YR", 9, &mut lines);
    }
    // gap + publication-check tables are schema-neutral (migration 011).
    // Wall-clock columns and the digests binding them are normalized like
    // the compact integrity column: fixed stamp + recompute.
    const TS_FIX: &str = "0000-01-01T00:00:00+00:00";
    let fixed_ts = || chrono::DateTime::parse_from_rfc3339(TS_FIX)
        .unwrap().to_utc();
    {
        let mut st = c.prepare("SELECT decision_id,symbol,period,start_date,end_date,subject_version_id,subject_version_digest,conclusion,basis_kind,basis_ref,basis_sha256,verifier_id,decided_at,decision_sha256 FROM gap_verification_decisions ORDER BY decision_id").unwrap();
        let rows: Vec<Vec<rusqlite::types::Value>> = st.query_map([], |r| {
            Ok((0..14).map(|i| r.get::<_, rusqlite::types::Value>(i).unwrap()).collect())
        }).unwrap().map(|r| r.unwrap()).collect();
        for r in &rows {
            let vs = |i: usize| -> String {
                match &r[i] { rusqlite::types::Value::Null => String::new(),
                    rusqlite::types::Value::Integer(x) => x.to_string(),
                    rusqlite::types::Value::Text(x) => x.clone(),
                    rusqlite::types::Value::Real(x) => x.to_string(),
                    rusqlite::types::Value::Blob(x) => crate::hash::sha256(x) }
            };
            let period = if vs(2) == "day" { crate::market_data::Period::Day }
                else { crate::market_data::Period::Year };
            let d = crate::market_storage::GapVerificationDecision {
                decision_id: vs(0), symbol: vs(1), period,
                start_date: chrono::NaiveDate::parse_from_str(&vs(3), "%Y-%m-%d").unwrap(),
                end_date: chrono::NaiveDate::parse_from_str(&vs(4), "%Y-%m-%d").unwrap(),
                subject_version_id: vs(5), subject_version_digest: vs(6),
                conclusion: vs(7), basis_kind: vs(8), basis_ref: vs(9),
                basis_sha256: vs(10), verifier_id: vs(11),
                decided_at: fixed_ts(), decision_sha256: vs(13),
            };
            lines.push(format!("GD:{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                vs(0), vs(1), vs(2), vs(3), vs(4), vs(5), vs(6), vs(7), vs(8),
                vs(9), vs(10), vs(11), TS_FIX, d.content_digest()));
        }
    }
    {
        // PG.evidence_digest binds the decision's sha (and its decided_at)
        // — recompute via the linked decision with the fixed stamp.
        let mut st = c.prepare("SELECT gap_id,symbol,period,start_date,end_date,evidence_kind,evidence_id,evidence_digest,observed_at FROM proven_gaps ORDER BY gap_id").unwrap();
        let rows: Vec<Vec<rusqlite::types::Value>> = st.query_map([], |r| {
            Ok((0..9).map(|i| r.get::<_, rusqlite::types::Value>(i).unwrap()).collect())
        }).unwrap().map(|r| r.unwrap()).collect();
        for r in rows {
            let vs = |i: usize| -> String {
                match &r[i] { rusqlite::types::Value::Null => String::new(),
                    rusqlite::types::Value::Integer(x) => x.to_string(),
                    rusqlite::types::Value::Text(x) => x.clone(),
                    rusqlite::types::Value::Real(x) => x.to_string(),
                    rusqlite::types::Value::Blob(x) => crate::hash::sha256(x) }
            };
            let ev = c.prepare("SELECT decision_id,symbol,period,start_date,end_date,subject_version_id,subject_version_digest,conclusion,basis_kind,basis_ref,basis_sha256,verifier_id,decided_at,decision_sha256 FROM gap_verification_decisions WHERE decision_id=?1")
                .and_then(|mut q| q.query_row([&vs(6)], |dr| {
                    Ok((0..14).map(|i| dr.get::<_, rusqlite::types::Value>(i).unwrap())
                        .collect::<Vec<_>>())
                }));
            let evd = match ev {
                Ok(dr) => {
                    let dvs = |i: usize| -> String {
                        match &dr[i] { rusqlite::types::Value::Null => String::new(),
                            rusqlite::types::Value::Integer(x) => x.to_string(),
                            rusqlite::types::Value::Text(x) => x.clone(),
                            rusqlite::types::Value::Real(x) => x.to_string(),
                            rusqlite::types::Value::Blob(x) => crate::hash::sha256(x) }
                    };
                    let period = if dvs(2) == "day" { crate::market_data::Period::Day }
                        else { crate::market_data::Period::Year };
                    crate::market_storage::GapVerificationDecision {
                        decision_id: dvs(0), symbol: dvs(1), period,
                        start_date: chrono::NaiveDate::parse_from_str(&dvs(3), "%Y-%m-%d").unwrap(),
                        end_date: chrono::NaiveDate::parse_from_str(&dvs(4), "%Y-%m-%d").unwrap(),
                        subject_version_id: dvs(5), subject_version_digest: dvs(6),
                        conclusion: dvs(7), basis_kind: dvs(8), basis_ref: dvs(9),
                        basis_sha256: dvs(10), verifier_id: dvs(11),
                        decided_at: fixed_ts(), decision_sha256: dvs(13),
                    }.content_digest()
                }
                Err(_) => vs(7),
            };
            lines.push(format!("PG:{}|{}|{}|{}|{}|{}|{}|{}|{}",
                vs(0), vs(1), vs(2), vs(3), vs(4), vs(5), vs(6), evd, TS_FIX));
        }
    }
    dump(&c, "SELECT gap_id,version_id,'0000-01-01T00:00:00+00:00' FROM gap_coverings ORDER BY gap_id", "GC", 3, &mut lines);
    dump(&c, "SELECT verification_task_id,target_task_id,audit_json,audit_sha256 FROM publication_checks ORDER BY 1", "PC", 4, &mut lines);

    let joined = lines.join("\n");
    let mut out = joined;
    for (k, real) in syms {
        if !k.starts_with("rev:") {
            out = out.replace(real.as_str(), &format!("${k}"));
        }
    }
    // task ids are random per-runner — sort the symbolized lines so the
    // digest input is order-stable.
    let mut sorted: Vec<&str> = out.lines().collect();
    sorted.sort();
    let out = sorted.join("\n");
    if let Ok(path) = std::env::var("DIFF_DIGEST_DUMP") {
        std::fs::write(path, &out).ok();
    }
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
    // DIFF_NORM_TS=1: replace every stored timestamp/integrity value with a
    // placeholder in emitted JSON — wall-clock bytes differ across runs;
    // byte-identity of the remaining columns is proven by state_digest.
    let norm_ts = std::env::var("DIFF_NORM_TS").is_ok();
    let norm_set = |db_path: &str| -> BTreeSet<String> {
        let c = rusqlite::Connection::open(db_path).unwrap();
        let mut out = BTreeSet::new();
        // Volatile columns only — the union is built over tables that
        // actually exist (result tables are compact-only).
        let has = |t: &str| -> bool {
            c.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [t], |r| r.get::<_, i64>(0)).unwrap() > 0
        };
        let col = |t: &str, k: &str| -> bool {
            c.query_row("SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2",
                [t, k], |r| r.get::<_, i64>(0)).unwrap() > 0
        };
        let mut parts: Vec<String> = Vec::new();
        if col("data_versions", "created_at") {
            parts.push("SELECT created_at AS v FROM data_versions".into());
        }
        if col("data_versions", "published_at") {
            parts.push("SELECT published_at AS v FROM data_versions".into());
        }
        if col("data_versions", "integrity_digest") {
            parts.push("SELECT hex(integrity_digest) AS v FROM data_versions".into());
        }
        if has("gap_verification_decisions") {
            parts.push("SELECT decided_at AS v FROM gap_verification_decisions".into());
            parts.push("SELECT decision_sha256 AS v FROM gap_verification_decisions".into());
        }
        if has("proven_gaps") {
            parts.push("SELECT observed_at AS v FROM proven_gaps".into());
            parts.push("SELECT evidence_digest AS v FROM proven_gaps".into());
        }
        if has("gap_coverings") {
            parts.push("SELECT covered_at AS v FROM gap_coverings".into());
        }
        if has("market_update_results") {
            // legacy column: result_sha256; compact column: result_digest
            parts.push(format!(
                "SELECT hex({}) AS v FROM market_update_results",
                if col("market_update_results", "result_digest") {
                    "result_digest"
                } else {
                    "result_sha256"
                }));
        }
        if has("publication_results") {
            parts.push("SELECT hex(result_digest) AS v FROM publication_results".into());
        }
        if has("tasks") {
            parts.push("SELECT created_at AS v FROM tasks".into());
            parts.push("SELECT last_progress_at AS v FROM tasks".into());
        }
        if parts.is_empty() {
            parts.push("SELECT CAST(NULL AS TEXT) AS v WHERE 0".into());
        }
        let sql = format!(
            "SELECT DISTINCT v FROM ({}) WHERE v IS NOT NULL AND v != ''",
            parts.join(" UNION ALL "));
        let mut st = c.prepare(&sql).unwrap();
        let rows = st.query_map([], |r| r.get::<_, String>(0)).unwrap();
        for r in rows {
            let v = r.unwrap();
            // serde DateTime emits "Z" where the stored text is "+00:00" —
            // cover both forms.
            if let Some(z) = v.strip_suffix("+00:00") {
                out.insert(format!("{z}Z"));
            }
            out.insert(v);
        }
        out
    };
    fn norm_deep(v: &Value, set: &BTreeSet<String>) -> Value {
        match v {
            Value::String(s) if set.contains(s.as_str()) => json!("<norm>"),
            Value::Array(a) => Value::Array(a.iter().map(|x| norm_deep(x, set)).collect()),
            Value::Object(m) => Value::Object(
                m.iter().map(|(k, x)| (k.clone(), norm_deep_named(k, x, set))).collect()),
            _ => v.clone(),
        }
    }
    // Digests computed over task-bound/wall-clock content can't be
    // string-matched — norm them by field name; content parity is proven by
    // the normalized state digest and the remaining emitted fields.
    fn norm_deep_named(k: &str, v: &Value, set: &BTreeSet<String>) -> Value {
        if matches!(k, "evidence_sha256" | "audit_sha256")
            && v.as_str().is_some_and(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return json!("<norm>");
        }
        norm_deep(v, set)
    }
    // Compact (schema-19) DBs bypass SqliteMarket::read's schema gate —
    // production compact reads run inside the WriteCoordinator lease; the
    // harness opens a raw connection to the same bytes instead.
    let compact_conn = rusqlite::Connection::open(&db_path)
        .ok()
        .filter(|c| crate::sqlite_market::is_compact_schema(c).unwrap_or(false));
    let compact_market = if compact_conn.is_some() {
        // DatabaseLease requires a private file (mode & 077 == 0); the
        // harness fixture arrives with umask permissions — chmod to the
        // production posture before opening the compact market.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&db_path, fs::Permissions::from_mode(0o600)).unwrap();
        Some(
            crate::sqlite_market::compact_repo::SqliteCompactMarket::open(
                std::path::Path::new(&db_path),
            )
            .expect("compact market open"),
        )
    } else {
        None
    };
    // On compact DBs the task API is reached through the coordinator-bound
    // handle (production wiring); the bare SqliteTasks gates schema-19
    // closed. All task ops + repo.read flows go through op_repo.
    let op_repo = compact_market
        .as_ref()
        .map(|m| m.tasks())
        .unwrap_or_else(|| repo.clone());
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
                match op_repo.accept(
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
                match op_repo.transition(
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
            "candidates" => match op_repo.candidates(op["run"].as_str().unwrap()) {
                Ok(list) => json!({"status":"ok","candidates":list.iter().map(|s| sym_of(&syms,&s.task.task_id)).collect::<Vec<_>>()}),
                Err(e) => json!({"status":"error","code":err_code(&e),"message":e.to_string()}),
            },
            "reconcile" => match op_repo.reconcile(op["run"].as_str().unwrap()) {
                Ok(n) => json!({"status":"ok","reconciled":n}),
                Err(e) => json!({"status":"error","code":err_code(&e),"message":e.to_string()}),
            },
            "seed" => json!({"status":"ok","seeded":true}),
            "lookup_task" => {
                let ids: Vec<String> = op_repo
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
                let market = SqliteMarket::new(op_repo.clone());
                match market.stage(&raw) {
                    Ok(()) => json!({"status":"ok","digest":market_digest(&op_repo, &syms)}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "read_result" => {
                let id = resolve_str(&op["result_id"], &syms);
                let res = match &compact_conn {
                    Some(c) => crate::sqlite_market::update::read_result(c, &id),
                    None => SqliteMarket::new(op_repo.clone())
                        .read(|c| crate::sqlite_market::update::read_result(c, &id)),
                };
                match res {
                    Ok(None) => json!({"status":"ok","found":false}),
                    Ok(Some((audit, receipt))) => json!({"status":"ok","found":true,
                        // audit embeds wall-clock integrity digests +
                        // the random task id — normalize both before hash.
                        "audit_sha256":crate::hash::sha256(
                            serde_json::to_string(&norm_deep(
                                &normalize_ids(&serde_json::to_value(&audit).unwrap(), &syms),
                                &norm_set(&db_path))).unwrap().as_bytes()),
                        "receipt":serde_json::to_value(&receipt).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "range_receipt" => {
                let id = resolve_str(&op["result_id"], &syms);
                let res = match &compact_conn {
                    Some(c) => crate::sqlite_market::update::read_result(c, &id),
                    None => SqliteMarket::new(op_repo.clone())
                        .read(|c| crate::sqlite_market::update::read_result(c, &id)),
                };
                match res {
                    Ok(None) => json!({"status":"ok","found":false}),
                    Ok(Some((audit, receipt))) => json!({"status":"ok","found":true,
                        // audit embeds wall-clock integrity digests +
                        // the random task id — normalize both before hash.
                        "audit_sha256":crate::hash::sha256(
                            serde_json::to_string(&norm_deep(
                                &normalize_ids(&serde_json::to_value(&audit).unwrap(), &syms),
                                &norm_set(&db_path))).unwrap().as_bytes()),
                        "receipt":serde_json::to_value(&receipt).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "item_receipt" => {
                let task = resolve_str(&op["task_id"], &syms);
                let item = op["item_id"].as_str().unwrap().to_string();
                let market = SqliteMarket::new(op_repo.clone());
                use crate::market_update::publication::RangeRepository;
                let res = match &compact_conn {
                    Some(c) => (|| -> Result<Option<Receipt>, MarketError> {
                        let id: Option<String> = c
                            .query_row(
                                "SELECT result_id FROM market_update_results WHERE task_id=?1 AND item_id=?2",
                                rusqlite::params![task, item],
                                |r| r.get(0),
                            )
                            .optional()
                            .map_err(|e| MarketError::Storage(e.to_string()))?;
                        match id {
                            None => Ok(None),
                            Some(id) => Ok(Some(
                                crate::sqlite_market::update::read_result(c, &id)?
                                    .map(|(_, r)| r)
                                    .ok_or_else(|| MarketError::Integrity(
                                        "range item receipt missing".into()))?,
                            )),
                        }
                    })(),
                    None => RangeRepository::item_receipt(&market, &task, &item),
                };
                match res {
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
                // "@base"/"@candidate" load the record through the
                // backend's own materialization (compact synthesizes the
                // stored raw — a fixture file can't byte-match it).
                let loaded: Result<(crate::market_storage::compat::CheckedRecord,
                                    crate::market_storage::compat::CheckedRecord),
                                   MarketError> = (|| {
                    use crate::market_storage::MarketRepository;
                    let record = match p["record"].as_str().unwrap() {
                        "@candidate" => compact_market.as_ref().unwrap()
                            .candidate(command.version_id.as_str())?,
                        "@base" => compact_market.as_ref().unwrap()
                            .fixed(p["base_version_id"].as_str().unwrap())?,
                        n => load(n),
                    };
                    let base = match p["base"].as_str().unwrap() {
                        "@base" => compact_market.as_ref().unwrap()
                            .fixed(p["base_version_id"].as_str().unwrap())?,
                        n => load(n),
                    };
                    Ok((record, base))
                })();
                let (record, base) = match loaded {
                    Ok(rb) => rb,
                    Err(e) => {
                        let mut r = json!({"status":"error","code":market_code(&e),
                            "message":e.to_string(),
                            "unchanged":state_digest(&db_path,&syms)==before});
                        r["seq"] = json!(idx);
                        r["op"] = json!(name);
                        let r = if norm_ts {
                            norm_deep(&r, &norm_set(&db_path))
                        } else { r };
                        writeln!(out, "{}", serde_json::to_string(&r).unwrap())
                            .unwrap();
                        continue;
                    }
                };
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
                use crate::market_update::publication::RangeRepository;
                let res = if let Some(m) = &compact_market {
                    RangeRepository::commit_range(m, &plan)
                } else {
                    let market = SqliteMarket::new(op_repo.clone());
                    RangeRepository::commit_range(&market, &plan)
                };
                match res {
                    Ok(r) => {
                        // legacy digest covers the schema-18 market tables;
                        // compact runs use the schema-neutral state digest.
                        let d = if compact_market.is_some() {
                            crate::hash::sha256(state_digest(&db_path, &syms).as_bytes())
                        } else {
                            market_digest(&op_repo, &syms)
                        };
                        json!({"status":"ok",
                            "receipt":serde_json::to_value(&r).unwrap(),
                            "digest":d})
                    }
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            // NM-057 — publish/result commit entry points:
            //   commit_publication -> MarketRepository::commit (commit_tx)
            //   commit_yearly      -> YearlyRepository::commit_yearly
            //     (yearly_no_change_tx / commit_tx -> commit_publication)
            "commit_publication" | "commit_yearly" => {
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
                let mut command: Publication = serde_json::from_value(cmdv).unwrap();
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
                let load_rec = |name: &str,
                                ver: &str|
                 -> Result<crate::market_storage::compat::CheckedRecord, MarketError> {
                    use crate::market_storage::MarketRepository;
                    match name {
                        "@candidate" => compact_market.as_ref().unwrap().candidate(ver),
                        "@base" | "@fixed" => compact_market.as_ref().unwrap().fixed(ver),
                        n => Ok(load(n)),
                    }
                };
                let loaded: Result<(crate::market_storage::compat::CheckedRecord,
                                    Option<crate::market_storage::compat::CheckedRecord>),
                                   MarketError> = (|| {
                    let record = load_rec(p["record"].as_str().unwrap(), &command.version_id)?;
                    let base = match p["base"].as_str() {
                        Some(m) => Some(load_rec(
                            m, p["base_version_id"].as_str().unwrap_or_default())?),
                        None => None,
                    };
                    Ok((record, base))
                })();
                let res: Result<Receipt, MarketError> = match loaded {
                    Err(e) => Err(e),
                    Ok((record, base)) => {
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
                        let publication = crate::market_storage::PublishPlan {
                            command,
                            record,
                            required_heads,
                            promote,
                            base_digest: p["base_digest"].as_str().map(|x| x.to_string()),
                        };
                        if name == "commit_publication" {
                            use crate::market_storage::MarketRepository;
                            match &compact_market {
                                Some(m) => MarketRepository::commit(m, &publication),
                                None => MarketRepository::commit(
                                    &SqliteMarket::new(op_repo.clone()), &publication),
                            }
                        } else {
                            use crate::yearly_publication::results::YearlyRepository;
                            let base = base.ok_or_else(|| MarketError::Integrity(
                                "yearly plan base missing".into()));
                            match base {
                                Err(e) => Err(e),
                                Ok(base) => {
                                    let plan = crate::yearly_publication::results::YearlyResultPlan {
                                        publication,
                                        base,
                                        prior: None,
                                        no_change: p["no_change"].as_bool().unwrap_or(false),
                                        audit: p["audit"].clone(),
                                        audit_payloads: vec![],
                                    };
                                    match &compact_market {
                                        Some(m) => YearlyRepository::commit_yearly(m, &plan),
                                        None => YearlyRepository::commit_yearly(
                                            &SqliteMarket::new(op_repo.clone()), &plan),
                                    }
                                }
                            }
                        }
                    }
                };
                match res {
                    Ok(r) => {
                        let d = if compact_market.is_some() {
                            crate::hash::sha256(state_digest(&db_path, &syms).as_bytes())
                        } else {
                            market_digest(&op_repo, &syms)
                        };
                        json!({"status":"ok",
                            "receipt":serde_json::to_value(&r).unwrap(),
                            "digest":d})
                    }
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "yearly_receipt" | "yearly_item_receipt" | "receipt" => {
                use crate::market_storage::MarketRepository;
                use crate::yearly_publication::results::YearlyRepository;
                let res: Result<Option<Receipt>, MarketError> = match name.as_str() {
                    "yearly_receipt" => {
                        let id = resolve_str(&op["id"], &syms);
                        match &compact_market {
                            Some(m) => YearlyRepository::yearly_receipt(m, &id),
                            None => YearlyRepository::yearly_receipt(
                                &SqliteMarket::new(op_repo.clone()), &id),
                        }
                    }
                    "yearly_item_receipt" => {
                        let task = resolve_str(&op["task_id"], &syms);
                        let item = op["item_id"].as_str().unwrap().to_string();
                        match &compact_market {
                            Some(m) => YearlyRepository::yearly_item_receipt(m, &task, &item),
                            None => YearlyRepository::yearly_item_receipt(
                                &SqliteMarket::new(op_repo.clone()), &task, &item),
                        }
                    }
                    _ => {
                        let id = resolve_str(&op["id"], &syms);
                        match &compact_market {
                            Some(m) => MarketRepository::receipt(m, &id),
                            None => MarketRepository::receipt(
                                &SqliteMarket::new(op_repo.clone()), &id),
                        }
                    }
                };
                match res {
                    Ok(None) => json!({"status":"ok","found":false}),
                    Ok(Some(r)) => json!({"status":"ok","found":true,
                        "receipt":serde_json::to_value(&r).unwrap()}),
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
                let res = match &compact_conn {
                    Some(c) => crate::sqlite_market::head(c, &scope),
                    None => {
                        let market = SqliteMarket::new(op_repo.clone());
                        market.read(|c| crate::sqlite_market::head(c, &scope))
                    }
                };
                match res {
                    Ok(v) => json!({"status":"ok","version_id":v}),
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }
            "read_record" => {
                let id = op["version_id"].as_str().unwrap().to_string();
                let fixed = op["mode"].as_str().unwrap() == "fixed";
                let res = match &compact_conn {
                    // read_record(id, published, allow_yearly) — same
                    // dispatch as SqliteMarket::{fixed,candidate}
                    Some(c) => crate::sqlite_market::read_record(c, &id, fixed, true),
                    None => {
                        let market = SqliteMarket::new(op_repo.clone());
                        if fixed { market.fixed(&id) } else { market.candidate(&id) }
                    }
                };
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
                let compact_range =
                    |c: &rusqlite::Connection| -> Result<crate::market_storage::ReadPage, MarketError> {
                        // SqliteMarket::range verbatim on a raw connection
                        rng.validate()?;
                        let version_missing_from_head = id.is_none();
                        let vid = match &id {
                            Some(v) => v.clone(),
                            None => crate::sqlite_market::head(c, &scope)?
                                .ok_or(MarketError::NotFound)?,
                        };
                        let record = crate::sqlite_market::read_record(c, &vid, true, true)
                            .map_err(|e| {
                                if version_missing_from_head
                                    && matches!(&e, MarketError::NotFound)
                                {
                                    MarketError::Integrity(
                                        "effective head target unavailable".into(),
                                    )
                                } else {
                                    e
                                }
                            })?;
                        if Scope::of(&record.version) != scope {
                            return Err(MarketError::Integrity(
                                "version/scope mismatch".into(),
                            ));
                        }
                        let rows = crate::sqlite_market::range_rows(
                            c, &vid, scope.period, &rng,
                        )?;
                        let more = rows.len() > rng.limit;
                        let rows: Vec<_> = rows.into_iter().take(rng.limit).collect();
                        let next_after = if more {
                            rows.last().map(|b| b.date)
                        } else {
                            None
                        };
                        Ok(crate::market_storage::ReadPage {
                            integrity_sha256: record.raw["integrity_sha256"]
                                .as_str()
                                .unwrap()
                                .to_owned(),
                            version: record.version,
                            rows,
                            next_after,
                        })
                    };
                let res = match &compact_conn {
                    Some(c) => compact_range(c),
                    None => {
                        let market = SqliteMarket::new(op_repo.clone());
                        market.range(&scope, id.as_deref(), &rng)
                    }
                };
                match res {
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
                let market = SqliteMarket::new(op_repo.clone());
                let versions_res = if let Some(c) = &compact_conn {
                    (|c: &rusqlite::Connection| -> Result<Vec<crate::publish::EffectiveVersion>, MarketError> {
                        let orphan: bool = c
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM task_publications tp LEFT JOIN data_versions v ON v.version_key=tp.version_key WHERE v.version_key IS NULL)",
                                [],
                                |r| r.get(0),
                            )
                            .map_err(|e| MarketError::Storage(e.to_string()))?;
                        if orphan {
                            return Err(MarketError::Integrity(
                                "task publication references missing version".into(),
                            ));
                        }
                        let summaries =
                            crate::sqlite_market::compact_versions::list(c, &scope)?;
                        summaries
                            .iter()
                            .map(|s| {
                                crate::sqlite_market::compact_versions::materialize(
                                    c, &s.version_id,
                                )
                                .map(|m| m.version)
                            })
                            .collect()
                    })(c)
                } else {
                    market.versions(&scope)
                };
                match versions_res {
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
                match op_repo.read_task(&tid) {
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
            "stage_ops_delta" | "validate_staged" | "commit_ops_update"
            | "commit_ops_no_change" | "update_base_facts"
            | "base_bars_in_range" | "commit_range_ops"
            | "commit_range_ops_typed" | "commit_range_no_change_typed"
            | "commit_update_batch" => {
                // Legacy backend: the trait defaults return Ok(None) —
                // surface it verbatim instead of unwrapping a missing
                // compact handle.
                let res: Result<Value, MarketError> = if let Some(m) = compact_market.as_ref() {
                let m = m;
                (|| {
                    match name.as_str() {
                        "stage_ops_delta" => {
                            let upserts: Vec<crate::market_data::Bar> = op["upserts"]
                                .as_array().unwrap().iter()
                                .map(|b| serde_json::from_value(b.clone()).unwrap())
                                .collect();
                            let removes: Vec<chrono::NaiveDate> = op["removes"]
                                .as_array().map(|a| a.iter()
                                    .map(|d| chrono::NaiveDate::parse_from_str(
                                        d.as_str().unwrap(), "%Y-%m-%d").unwrap())
                                    .collect())
                                .unwrap_or_default();
                            let key = m.stage_ops_delta(
                                op["entity"].as_str().unwrap(),
                                op["base_version_id"].as_str().unwrap(),
                                op["version_id"].as_str().unwrap(),
                                &upserts,
                                &removes,
                            )?;
                            Ok(json!({"version_key":key}))
                        }
                        "validate_staged" => {
                            m.validate_staged(op["version_id"].as_str().unwrap())?;
                            Ok(json!({}))
                        }
                        "commit_ops_update" => {
                            let r = m.commit_ops_update(
                                op["result_id"].as_str().unwrap(),
                                op["task_id"].as_str().unwrap(),
                                op["item_id"].as_str().unwrap(),
                                op["base_version_id"].as_str().unwrap(),
                                op["result_version_id"].as_str().unwrap(),
                                op["expected_revision"].as_u64().unwrap(),
                                op["run_id"].as_str().unwrap(),
                                op["occupancy_key"].as_str().unwrap(),
                                op.get("continuity_evidence"),
                            )?;
                            Ok(json!({"receipt":serde_json::to_value(&r).unwrap()}))
                        }
                        "commit_ops_no_change" => {
                            let r = m.commit_ops_no_change(
                                op["result_id"].as_str().unwrap(),
                                op["task_id"].as_str().unwrap(),
                                op["item_id"].as_str().unwrap(),
                                op["base_version_id"].as_str().unwrap(),
                                op["expected_revision"].as_u64().unwrap(),
                                op["run_id"].as_str().unwrap(),
                                op["occupancy_key"].as_str().unwrap(),
                                op["range_start_day"].as_i64().unwrap(),
                                op["range_end_day"].as_i64().unwrap(),
                                op.get("continuity_evidence"),
                            )?;
                            Ok(json!({"receipt":serde_json::to_value(&r).unwrap()}))
                        }
                        "update_base_facts" => {
                            let f = m.update_base_facts(
                                op["base_version_id"].as_str().unwrap())?;
                            Ok(json!({
                                "version_id":f.version_id,
                                "symbol":f.symbol,
                                "record_count":f.record_count,
                                "first_day":f.first_day.to_string(),
                                "last_day":f.last_day.to_string(),
                                "integrity_sha256":f.integrity_sha256,
                                "coverage":serde_json::to_value(&f.coverage).unwrap(),
                                "unit_currency":serde_json::to_value(&f.unit_currency).unwrap(),
                                "source":f.source,
                                "generation_id":f.generation_id,
                                "generation_manifest_sha256":f.generation_manifest_sha256,
                                "day_member_sha256":f.day_member_sha256,
                                "security_event_sha256":f.security_event_sha256,
                            }))
                        }
                        "commit_range_ops" | "commit_range_ops_typed" => {
                            use crate::market_update::publication::RangeRepository;
                            let command: crate::market_storage::Publication =
                                serde_json::from_value(op["command"].clone()).unwrap();
                            let scope = crate::market_storage::Scope {
                                symbol: op["scope"]["symbol"].as_str().unwrap().into(),
                                period: if op["scope"]["period"].as_str().unwrap() == "year" {
                                    crate::market_data::Period::Year
                                } else { crate::market_data::Period::Day },
                            };
                            let upserts: Vec<crate::market_data::Bar> = op["upserts"]
                                .as_array().unwrap().iter()
                                .map(|b| serde_json::from_value(b.clone()).unwrap())
                                .collect();
                            let removes: Vec<chrono::NaiveDate> = op["removes"]
                                .as_array().map(|a| a.iter()
                                    .map(|d| chrono::NaiveDate::parse_from_str(
                                        d.as_str().unwrap(), "%Y-%m-%d").unwrap())
                                    .collect())
                                .unwrap_or_default();
                            let r = if name == "commit_range_ops" {
                                let expected: Vec<crate::market_data::Bar> = op["expected"]
                                    .as_array().unwrap().iter()
                                    .map(|b| serde_json::from_value(b.clone()).unwrap())
                                    .collect();
                                m.commit_range_ops(&command, &scope,
                                    op["base_version_id"].as_str().unwrap(),
                                    &upserts, &removes, &expected)?
                            } else {
                                m.commit_range_ops_typed(&command, &scope,
                                    op["base_version_id"].as_str().unwrap(),
                                    &upserts, &removes,
                                    op.get("continuity_evidence"))?
                            };
                            Ok(json!({"receipt":r.map(|x| serde_json::to_value(&x).unwrap())}))
                        }
                        "commit_range_no_change_typed" => {
                            use crate::market_update::publication::RangeRepository;
                            let command: crate::market_storage::Publication =
                                serde_json::from_value(op["command"].clone()).unwrap();
                            let range = crate::market_update::DateRange {
                                start: chrono::NaiveDate::parse_from_str(
                                    op["range"]["start"].as_str().unwrap(), "%Y-%m-%d").unwrap(),
                                end: chrono::NaiveDate::parse_from_str(
                                    op["range"]["end"].as_str().unwrap(), "%Y-%m-%d").unwrap(),
                            };
                            let r = m.commit_range_no_change_typed(&command,
                                op["base_version_id"].as_str().unwrap(),
                                &range, op.get("continuity_evidence"))?;
                            Ok(json!({"receipt":r.map(|x| serde_json::to_value(&x).unwrap())}))
                        }
                        "commit_update_batch" => {
                            let items: Vec<crate::market_update::UpdateBatchItem> = op["items"]
                                .as_array().unwrap().iter()
                                .map(|v| {
                                    let prepared = match v["prepared"]["kind"].as_str().unwrap() {
                                        "no_change" => crate::market_update::UpdateBatchPrepared::NoChange {
                                            base_version_id: v["prepared"]["base_version_id"].as_str().unwrap().into(),
                                            range_start_day: v["prepared"]["range_start_day"].as_i64().unwrap(),
                                            range_end_day: v["prepared"]["range_end_day"].as_i64().unwrap(),
                                            continuity: v["prepared"]["continuity"].clone(),
                                        },
                                        _ => crate::market_update::UpdateBatchPrepared::Append {
                                            base_version_id: v["prepared"]["base_version_id"].as_str().unwrap().into(),
                                            result_version_id: v["prepared"]["result_version_id"].as_str().unwrap().into(),
                                            upserts: v["prepared"]["upserts"].as_array().unwrap().iter()
                                                .map(|b| serde_json::from_value(b.clone()).unwrap()).collect(),
                                            continuity: v["prepared"]["continuity"].clone(),
                                        },
                                    };
                                    crate::market_update::UpdateBatchItem {
                                        task_id: v["task_id"].as_str().unwrap().into(),
                                        item_id: v["item_id"].as_str().unwrap().into(),
                                        publication_id: v["publication_id"].as_str().unwrap().into(),
                                        run_id: v["run_id"].as_str().unwrap().into(),
                                        symbol: v["symbol"].as_str().unwrap().into(),
                                        occupancy_key: v["occupancy_key"].as_str().unwrap().into(),
                                        prepared,
                                    }
                                })
                                .collect();
                            let r = m.commit_update_batch(
                                op["task_id"].as_str().unwrap(), &items)?;
                            let outs: Vec<Value> = r.iter().map(|x| match x {
                                Ok(rc) => json!({"ok":serde_json::to_value(rc).unwrap()}),
                                Err(e) => json!({"err":format!("{}:{}",market_code(e),e)}),
                            }).collect();
                            Ok(json!({"items": outs}))
                        }
                        _ => {
                            // base_bars_in_range
                            let parse = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
                            let bars = m.base_bars_in_range(
                                op["base_version_id"].as_str().unwrap(),
                                parse(op["start"].as_str().unwrap()),
                                parse(op["end"].as_str().unwrap()),
                            )?;
                            Ok(json!({"bars":bars}))
                        }
                    }
                })()
                } else {
                    // trait defaults — Ok(None) per op's own shape
                    Ok(match name.as_str() {
                        "stage_ops_delta" => json!({"version_key":null}),
                        "update_base_facts" => json!({"facts":null}),
                        "base_bars_in_range" => json!({"bars":null}),
                        "commit_update_batch" => json!({"items":null}),
                        _ => json!({"receipt":null}),
                    })
                };
                match res {
                    Ok(v) => {
                        let mut o = json!({"status":"ok",
                            "digest":crate::hash::sha256(
                                state_digest(&db_path, &syms).as_bytes())});
                        o.as_object_mut().unwrap().extend(v.as_object().unwrap().clone());
                        o
                    }
                    Err(e) => json!({"status":"error","code":market_code(&e),"message":e.to_string()}),
                }
            }

            // ---- NM-056 — market tail ops ----
            "record_gap_decision" | "record_gap_decision_guarded" => {
                let d: crate::market_storage::GapVerificationDecision =
                    serde_json::from_value(op["decision"].clone()).unwrap();
                let guarded = name.ends_with("guarded");
                let res = if let Some(m) = &compact_market {
                    m.tx(|c| crate::sqlite_market::gaps::record_gap_decision_tx(
                        c, &d, guarded))
                } else {
                    let market = SqliteMarket::new(op_repo.clone());
                    if guarded {
                        market.record_gap_decision_guarded(&d)
                    } else {
                        market.record_gap_decision(&d)
                    }
                };
                match res {
                    Ok(b) => json!({"status":"ok","inserted":b}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "record_proven_gap" => {
                let gap: crate::market_storage::ProvenGap =
                    serde_json::from_value(op["gap"].clone()).unwrap();
                let res = if let Some(m) = &compact_market {
                    m.tx(|c| {
                        use crate::market_storage::Error as ME;
                        let st = |e: rusqlite::Error| ME::Storage(e.to_string());
                        let pt = match gap.period {
                            crate::market_data::Period::Day => "day",
                            crate::market_data::Period::Year => "year",
                        };
                        gap.validate()?;
                        match c.execute(
                            "INSERT INTO proven_gaps VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                            rusqlite::params![
                                gap.gap_id, gap.symbol, pt,
                                gap.start_date.to_string(), gap.end_date.to_string(),
                                gap.evidence_kind, gap.evidence_id,
                                gap.evidence_digest, gap.observed_at.to_rfc3339()],
                        ).map_err(st) {
                            Ok(_) => Ok(true),
                            Err(insert) => {
                                let present = c.query_row(
                                    "SELECT count(*) FROM proven_gaps WHERE gap_id=?1 AND symbol=?2 AND period=?3 AND start_date=?4 AND end_date=?5 AND evidence_kind=?6 AND evidence_id=?7 AND evidence_digest=?8 AND observed_at=?9",
                                    rusqlite::params![gap.gap_id, gap.symbol, pt,
                                        gap.start_date.to_string(), gap.end_date.to_string(),
                                        gap.evidence_kind, gap.evidence_id,
                                        gap.evidence_digest, gap.observed_at.to_rfc3339()],
                                    |r| r.get::<_, i64>(0)).map_err(st)?;
                                if present == 1 { return Ok(false); }
                                let any = c.query_row(
                                    "SELECT count(*) FROM proven_gaps WHERE gap_id=?1",
                                    rusqlite::params![gap.gap_id], |r| r.get::<_, i64>(0))
                                    .map_err(st)?;
                                if any > 0 {
                                    Err(ME::Integrity("proven gap identity conflict".into()))
                                } else { Err(insert) }
                            }
                        }
                    })
                } else {
                    let market = SqliteMarket::new(op_repo.clone());
                    market.record_proven_gap(&gap)
                };
                match res {
                    Ok(b) => json!({"status":"ok","inserted":b}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "cover_gap" => {
                let cov: crate::market_storage::GapCovering =
                    serde_json::from_value(op["covering"].clone()).unwrap();
                let res = if let Some(m) = &compact_market {
                    use crate::market_update::publication::RangeRepository;
                    m.cover_gap(&cov)
                } else {
                    let market = SqliteMarket::new(op_repo.clone());
                    market.cover_gap(&cov)
                };
                match res {
                    Ok(b) => json!({"status":"ok","inserted":b}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "gap_verification_decision" => {
                let id = op["decision_id"].as_str().unwrap();
                let res = if let Some(m) = &compact_market {
                    m.read(|c| crate::sqlite_market::gaps::read_decision(c, id))
                } else {
                    let market = SqliteMarket::new(op_repo.clone());
                    market.gap_verification_decision(id)
                };
                match res {
                    Ok(v) => json!({"status":"ok",
                        "decision":serde_json::to_value(&v).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "proven_gap" => {
                let id = op["gap_id"].as_str().unwrap();
                let res = if let Some(m) = &compact_market {
                    m.read(|c| crate::sqlite_market::gaps::read_open_gaps(
                            c, "", crate::market_data::Period::Day))
                        .map(|g| g.into_iter().find(|x| x.gap_id == id))
                } else {
                    let market = SqliteMarket::new(op_repo.clone());
                    market.proven_gap(id)
                };
                match res {
                    Ok(v) => json!({"status":"ok",
                        "gap":serde_json::to_value(&v).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "open_proven_gaps" => {
                let sym = op["symbol"].as_str().unwrap();
                let per = if op["period"].as_str().unwrap() == "year" {
                    crate::market_data::Period::Year
                } else {
                    crate::market_data::Period::Day
                };
                let res = if let Some(m) = &compact_market {
                    m.read(|c| crate::sqlite_market::gaps::read_open_gaps(c, sym, per))
                } else {
                    let market = SqliteMarket::new(op_repo.clone());
                    market.open_proven_gaps(sym, per)
                };
                match res {
                    Ok(v) => json!({"status":"ok",
                        "gaps":serde_json::to_value(&v).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "gap_coverings" => {
                let id = op["gap_id"].as_str().unwrap();
                let res = if let Some(m) = &compact_market {
                    use crate::market_update::publication::RangeRepository;
                    m.gap_coverings(id)
                } else {
                    let market = SqliteMarket::new(op_repo.clone());
                    market.gap_coverings(id)
                };
                match res {
                    Ok(v) => json!({"status":"ok",
                        "coverings":serde_json::to_value(&v).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "yearly_read_result" => {
                let id = op["result_id"].as_str().unwrap();
                let res = if let Some(c) = &compact_conn {
                    crate::sqlite_market::yearly_results::read_result(c, id)
                } else {
                    let market = SqliteMarket::new(op_repo.clone());
                    market.read(|c| crate::sqlite_market::yearly_results::read_result(c, id))
                };
                match res {
                    Ok(Some((audit, receipt))) => json!({"status":"ok",
                        "audit_sha256":crate::hash::sha256(
                            serde_json::to_string(&audit).unwrap().as_bytes()),
                        "receipt":serde_json::to_value(&receipt).unwrap()}),
                    Ok(None) => json!({"status":"ok","audit_sha256":null,"receipt":null}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "collection_receipt" => {
                let res = if let Some(m) = &compact_market {
                    use crate::official_market::DailyCollectionRepository;
                    DailyCollectionRepository::collection_receipt(
                        m, op["task"].as_str().unwrap(), op["item"].as_str().unwrap())
                } else {
                    use crate::official_market::DailyCollectionRepository;
                    let market = SqliteMarket::new(op_repo.clone());
                    DailyCollectionRepository::collection_receipt(
                        &market, op["task"].as_str().unwrap(), op["item"].as_str().unwrap())
                };
                match res {
                    Ok(v) => json!({"status":"ok",
                        "receipt":serde_json::to_value(&v).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "read_facts" => {
                let id = op["task"].as_str().unwrap();
                let res = if let Some(m) = &compact_market {
                    use crate::publication_verification::EvidenceRepository;
                    m.publication_facts(id)
                } else {
                    use crate::publication_verification::EvidenceRepository;
                    let market = SqliteMarket::new(op_repo.clone());
                    market.publication_facts(id)
                };
                match res {
                    Ok(f) => json!({"status":"ok",
                        "facts":serde_json::to_value(&f).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "check_receipt" => {
                let id = op["id"].as_str().unwrap();
                let res = if let Some(m) = &compact_market {
                    crate::publication_verification::decision::AuditStore::check_receipt(m, id)
                } else {
                    let market = SqliteMarket::new(op_repo.clone());
                    crate::publication_verification::decision::AuditStore::check_receipt(
                        &market, id)
                };
                match res {
                    Ok(v) => json!({"status":"ok",
                        "receipt":serde_json::to_value(&v).unwrap()}),
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "backend_open" => {
                // target: a path OTHER than the live db ("copy" clones it)
                let target = match op["target"].as_str().unwrap() {
                    "copy" => {
                        let dst = format!("{db_path}.backend-copy");
                        fs::copy(&db_path, &dst).unwrap();
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&dst, fs::Permissions::from_mode(0o600)).unwrap();
                        dst
                    }
                    "missing" => format!("{db_path}.backend-missing"),
                    other => other.to_string(),
                };
                match crate::sqlite_market::backend::MarketBackend::open(
                    &SqliteTasks::new(std::path::Path::new(&target))) {
                    Ok(b) => {
                        let kind = match b {
                            crate::sqlite_market::backend::MarketBackend::Legacy(_) => "legacy",
                            crate::sqlite_market::backend::MarketBackend::Compact(_) => "compact",
                        };
                        json!({"status":"ok","kind":kind})
                    }
                    Err(e) => json!({"status":"error","code":market_code(&e),
                        "message":e.to_string()}),
                }
            }
            "backend_open_shared" => {
                let target = match op["target"].as_str().unwrap() {
                    "copy" => {
                        let dst = format!("{db_path}.backend-copy");
                        fs::copy(&db_path, &dst).unwrap();
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&dst, fs::Permissions::from_mode(0o600)).unwrap();
                        dst
                    }
                    "missing" => format!("{db_path}.backend-missing"),
                    other => other.to_string(),
                };
                // open_shared only consults the lease on the compact arm;
                // acquire it on the live db so the "missing" target still
                // reaches the dispatch.
                let lease_path = if target == format!("{db_path}.backend-missing") {
                    db_path.clone()
                } else { target.clone() };
                let lease = crate::local_runtime::DatabaseLease::acquire(
                    std::path::Path::new(&lease_path));
                match lease {
                    Err(e) => json!({"status":"error","code":"Base",
                        "message":format!("BASE_UNAVAILABLE: database lease: {e}")}),
                    Ok(lease) => match crate::sqlite_market::backend::MarketBackend::open_shared(
                        &SqliteTasks::new(std::path::Path::new(&target)),
                        std::sync::Arc::new(lease)) {
                        Ok(b) => {
                            let kind = match b {
                                crate::sqlite_market::backend::MarketBackend::Legacy(_) => "legacy",
                                crate::sqlite_market::backend::MarketBackend::Compact(_) => "compact",
                            };
                            json!({"status":"ok","kind":kind})
                        }
                        Err(e) => json!({"status":"error","code":market_code(&e),
                            "message":e.to_string()}),
                    },
                }
            }
            other => panic!("unknown op {other}"),
        };
        if rec["status"] == "error" {
            rec["unchanged"] = json!(state_digest(&db_path, &syms) == before);
        }
        rec["seq"] = json!(idx);
        rec["op"] = json!(name);
        let rec = if norm_ts {
            norm_deep(&rec, &norm_set(&db_path))
        } else {
            rec
        };
        // Symbolize emitted task ids (task-<hex> differs across runners);
        // the comparator byte-compares every emitted field.
        let mut txt = serde_json::to_string(&rec).unwrap();
        for (k, real) in syms.iter() {
            if !k.starts_with("rev:") {
                txt = txt.replace(real.as_str(), &format!("${k}"));
            }
        }
        writeln!(out, "{}", txt).unwrap();
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
    let compact = std::env::var("DIFF_INIT").as_deref() == Ok("19")
        || std::env::var("DIFF_INIT").as_deref() == Ok("compact");
    if compact {
        // Schema-19 gated writes route through the compact coordinator —
        // SqliteTasks::write rejects post-18. The fixture builds directly
        // via the crate's compact write helpers on a raw connection.
        repo.initialize19().expect("init schema-19");
        build_compact_records(&db_path);
        if std::env::var("DIFF_CHECK").is_ok() {
            // compact check pair: failed-item target -> Missing evidence
            // -> not_applicable audit via the real prepare/commit path.
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&db_path, fs::Permissions::from_mode(0o600))
                .unwrap();
            let m = crate::sqlite_market::compact_repo::SqliteCompactMarket::open(
                std::path::Path::new(&db_path)).expect("compact open");
            build_check_fixture(&m.tasks(), &m, false);
        }
        return;
    }
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
    if std::env::var("DIFF_CHECK").is_ok() {
        let market = SqliteMarket::new(repo.clone());
        build_check_fixture(&repo, &market, true);
    }

 }

/// NM-054 — compact (schema-19) fixture: builds real compact rows via the
/// crate's own write helpers (insert_segment / put_evidence /
/// put_source_binding / stage_checkpoint), then publishes with the stored
/// integrity digest recomputed via recompute_integrity — identical to the
/// commit path's binding. DIFF_RECORDS entries: `name[|nohead][|validated]`
/// — yearly records derive their bars/lineage from the named daily base
/// (`yearly_base` field in the record file).
fn build_compact_records(db_path: &str) {
    use crate::compact_market::{CompactBar, EvidenceKind, SourceBinding};
    use crate::market_data::{Bar, CoverageStatus, Period};
    use crate::sqlite_market::{compact_repo, compact_v19, compact_versions};
    let dir = std::env::var("DIFF_FIXTURES")
        .unwrap_or_else(|_| "scripts/node_migration_p3/records".into());
    let list = std::env::var("DIFF_RECORDS").expect("DIFF_RECORDS");
    const PUB_AT: &str = "2026-01-01T00:00:00+00:00";
    const TASK_ID: &str = "task-pub";
    let c = rusqlite::Connection::open(db_path).unwrap();
    c.busy_timeout(std::time::Duration::from_secs(30)).unwrap();
    // Minimal task row for the task_publications FK (post-18 task writes
    // are gated, so the fixture seeds it directly).
    c.execute(
        "INSERT INTO tasks(task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of,item_count,items_pending,items_running,items_finished,items_succeeded,evidence_digest) VALUES('task-pub','{}','succeeded',0,'run-pub',NULL,?1,?1,?2,'{}',NULL,0,0,0,0,0,NULL)",
        rusqlite::params![PUB_AT, r#"{"items":[],"scopes":[]}"#],
    )
    .unwrap();
    c.execute(
        "INSERT INTO requests VALUES('req-pub','{}',?1,'task','task-pub',NULL)",
        rusqlite::params![crate::hash::sha256(b"{}")],
    )
    .unwrap();
    for entry in list.split(',') {
        let mut parts = entry.split('|');
        let name = parts.next().unwrap();
        let mut nohead = false;
        let mut validated = false;
        let mut staged = false;
        for m in parts {
            match m {
                "nohead" => nohead = true,
                "validated" => validated = true,
                "staged" => staged = true,
                _ => {}
            }
        }
        let raw: Value = serde_json::from_str(
            &fs::read_to_string(std::path::Path::new(&dir).join(name)).unwrap(),
        )
        .unwrap();
        let version_id = raw["version_id"].as_str().unwrap().to_string();
        let entity = raw["symbol"].as_str().unwrap().to_string();
        let period: Period = serde_json::from_value(raw["period"].clone()).unwrap();
        // VersionEvidence value — the raw record carries serde-shaped
        // unit_currency/adjustment_evidence and batch.coverage.
        let coverage_status: CoverageStatus =
            serde_json::from_value(raw["batch"]["coverage"]["status"].clone()).unwrap();
        let coverage_reason =
            raw["batch"]["coverage"]["reason"].as_str().unwrap().to_string();
        let version_evidence = json!({
            "source": raw["source"],
            "coverage_reason": coverage_reason,
            "unit_currency": raw["unit_currency"],
            "adjustment_evidence": raw["adjustment_evidence"],
        });
        let evidence_digest =
            compact_versions::put_evidence(&c, EvidenceKind::Version, &version_evidence)
                .unwrap();
        // Bars + lineage/source-binding per period.
        let (bars, lineage, binding_digest) = if period == Period::Day {
            let bars: Vec<CompactBar> =
                serde_json::from_value::<Vec<Bar>>(raw["records"].clone())
                    .unwrap()
                    .iter()
                    .map(|b| CompactBar::from_bar(b, period).unwrap())
                    .collect();
            // Generation evidence + source binding (deterministic fixture
            // values — the digest binds them, reads verify).
            let gen_ev = json!({
                "generation_id": format!("gen-{version_id}"),
                "note": "compact fixture",
            });
            let gen_digest =
                compact_versions::put_evidence(&c, EvidenceKind::Generation, &gen_ev)
                    .unwrap();
            let binding = SourceBinding {
                generation_id: format!("gen-{version_id}"),
                generation_manifest_sha256: [0x11; 32],
                day_member_sha256: [0x22; 32],
                security_event_sha256: [0x33; 32],
                evidence_digest: gen_digest,
            };
            let bd = compact_versions::put_source_binding(&c, &binding).unwrap();
            (bars, None, bd)
        } else {
            let base_id = raw["yearly_base"].as_str().unwrap();
            let base_key = compact_repo::version_key(&c, base_id).unwrap();
            let base = compact_versions::materialize(&c, base_id).unwrap();
            let base_row: i64 = c
                .query_row(
                    "SELECT last_day FROM data_versions WHERE version_key=?1",
                    [base_key],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap();
            let as_of = crate::compact_market::epoch_date(base_row).unwrap();
            let annual = crate::yearly::aggregate(&base.version, None, as_of).unwrap();
            let bars: Vec<CompactBar> = annual
                .records
                .iter()
                .map(|r| CompactBar::from_bar(&r.bar, period).unwrap())
                .collect();
            let first = annual.records.first().unwrap().lineage.first_trading_date;
            let last = annual.records.last().unwrap().lineage.last_trading_date;
            let lineage = compact_versions::YearlyLineageDraft {
                base_daily_version_key: base_key,
                algorithm_version: crate::yearly::ALGORITHM_VERSION.to_string(),
                first_trade_day: crate::compact_market::epoch_day(first),
                last_trade_day: crate::compact_market::epoch_day(last),
                as_of_day: base_row,
            };
            let bd: Vec<u8> = c
                .query_row(
                    "SELECT source_binding_digest FROM data_versions WHERE version_key=?1",
                    [base_key],
                    |r| r.get(0),
                )
                .unwrap();
            (bars, Some(lineage), bd.try_into().unwrap())
        };
        // One checkpoint clip per <=512-bar chunk.
        let key = {
            let mut clips = Vec::new();
            for chunk in bars.chunks(512) {
                let segment =
                    compact_v19::insert_segment(&c, &entity, period, chunk).unwrap();
                clips.push(compact_v19::StoredClip {
                    segment_key: segment.key,
                    clip: crate::compact_market::SegmentClip {
                        segment_digest: segment.digest,
                        clip_start_day: chunk.first().unwrap().trade_day,
                        clip_end_day: chunk.last().unwrap().trade_day,
                        row_count: chunk.len() as u32,
                    },
                });
            }
            let draft = compact_versions::VersionDraft {
                version_id: version_id.clone(),
                entity: entity.clone(),
                period,
                source_binding_digest: binding_digest,
                coverage_status,
                evidence_digest,
                created_at: PUB_AT.to_string(),
                yearly_lineage: lineage,
            };
            compact_versions::stage_checkpoint(&c, &draft, &clips, None).unwrap()
        };
        let period_code: i64 = if period == Period::Day { 1 } else { 2 };
        if staged {
            // state=1 (staged) — stage_checkpoint leaves it
        } else if validated {
            c.execute(
                "UPDATE data_versions SET state=2 WHERE version_key=?1",
                [key],
            )
            .unwrap();
        } else {
            let digest = compact_versions::recompute_integrity(&c, key, PUB_AT).unwrap();
            c.execute(
                "UPDATE data_versions SET state=3,published_at=?1,integrity_digest=?2 WHERE version_key=?3",
                rusqlite::params![PUB_AT, digest.as_slice(), key],
            )
            .unwrap();
            let pub_id = format!("pub-{version_id}");
            c.execute(
                "INSERT INTO result_registry(result_id,result_kind) VALUES(?1,1)",
                [pub_id.clone()],
            )
            .unwrap();
            c.execute(
                "INSERT INTO task_publications(task_id,version_key,result_id) VALUES(?1,?2,?3)",
                rusqlite::params![TASK_ID, key, pub_id],
            )
            .unwrap();
            if !nohead {
                c.execute(
                    "INSERT OR REPLACE INTO effective_heads(entity,period,adjustment,version_key) VALUES(?1,?2,1,?3)",
                    rusqlite::params![entity, period_code, key],
                )
                .unwrap();
            }
        }
    }
}

/// NM-056 — DIFF_CHECK=1 fixture body, generic over the market handle so
/// the same seed works on schema-18 (SqliteMarket) and schema-19
/// (SqliteCompactMarket via its tasks() bound repo).
fn build_check_fixture<T>(
    repo: &SqliteTasks,
    market: &T,
    with_result: bool,
) where
    T: crate::publication_verification::EvidenceRepository
        + crate::publication_verification::decision::AuditStore,
{
    // target task + committed no_change result + committed check publication-check fixture — a Daily-contract target
    // task T owning a committed (reused/no_change) market_update_results
    // row, plus a succeeded verify_publication_result check task with a
    // committed publication_checks audit row built by the real
    // decision::prepare + commit_check_tx path.
    {
        let occ = "[\"market\",\"sh.600006\",\"day\"]";
        let acc = repo
            .accept(
                "req-check-target",
                &serde_json::from_value::<Command>(json!({
                    "key":{"operation":"update_data","dataset":"market.daily",
                           "contract_version":"explicit-range-task-v1"},
                    "intent":{}
                }))
                .unwrap(),
                &serde_json::from_value::<FrozenWork>(json!({
                    "items":[{"item_id":"i1","target":{"symbol":"sh.600006","period":"day"},
                              "progress_unit":"x","frozen_inputs":{},"scope_keys":[occ]}],
                    "scopes":[occ]
                }))
                .unwrap(),
                "run-check",
                None,
            )
            .unwrap();
        let target = match acc.record.outcome {
            RequestOutcome::Task(s) => s.task.task_id,
            _ => panic!("task outcome"),
        };
        repo.transition(&target, 0, "run-check", Change::Claim).unwrap();
        repo.transition(&target, 1, "run-check", Change::StartItem("i1".into()))
            .unwrap();
        repo.transition(&target, 2, "run-check",
            Change::FinishItem("i1".into(),
                ItemOutcome { success: with_result, error: None, evidence: None }))
            .unwrap();
        let snap = repo
            .transition(&target, 3, "run-check", Change::FinishExecution)
            .unwrap();
        let trev = snap.task.state_revision;
        // no_change result row: reused base — needs no batch rows.
        // Compact storage has no market_update_results table; the compact
        // fixture seeds the not_applicable arm instead.
        if with_result {
        let base_sha: String = repo
            .read(|c| {
                c.query_row(
                    "SELECT content_digest FROM data_versions WHERE version_id='legacy-daily'",
                    [], |r| r.get(0))
                    .map_err(storage_task)
            })
            .unwrap();
        let seg_ref: String = repo
            .read(|c| {
                c.query_row("SELECT payload_id FROM payloads LIMIT 1", [], |r| r.get(0))
                    .map_err(storage_task)
            })
            .unwrap();
        let command = json!({
            "publication_id": "res-ck", "version_id": "legacy-daily",
            "task_id": target, "item_id": "i1", "run_id": "run-check",
            "expected_revision": 0,
            "intent": {"Daily": {"expected_head": "legacy-daily"}},
        });
        let receipt = json!({
            "publication_id": "res-ck", "version_id": "legacy-daily",
            "task_id": target, "task_revision": trev,
        });
        let audit = json!({
            "algorithm_version": "local-range-update-v1",
            "command": command,
            "base_integrity_sha256": base_sha,
            "result_integrity_sha256": base_sha,
            "segment": {"bars": {"$ref": format!("payload:{seg_ref}")}},
            "range": {"start": "2025-01-02", "end": "2025-12-31"},
            "evidence": {"base_version_id": "legacy-daily"},
            "combined": true,
            "no_change": true,
        });
        let audit_json = serde_json::to_string(&audit).unwrap();
        let receipt_json = serde_json::to_string(&receipt).unwrap();
        let digest = crate::hash::sha256(
            serde_json::json!([audit_json, receipt_json]).to_string().as_bytes());
        repo.write(|c| {
            c.execute(
                "INSERT INTO market_update_results VALUES(?1,?2,'i1','legacy-daily','legacy-daily',1,?3,?4,?5)",
                rusqlite::params![format!("res-ck"), target, audit_json, digest, receipt_json],
            )
            .map_err(storage_task)
        })
        .unwrap();
        }
        // Legacy-contract task owning the two publication_batches rows —
        // exercises the legacy read_facts arm (committed publication
        // evidence + receipt digest + task_publications count).
        if with_result {
            let occd = "[\"market\",\"sh.600006\",\"day\"]";
            let occy = "[\"market\",\"sh.600006\",\"year\"]";
            let acc3 = repo
                .accept(
                    "req-legacy-collect",
                    &serde_json::from_value::<Command>(json!({
                        "key":{"operation":"start_collection",
                               "dataset":"market.daily",
                               "contract_version":"v1"},
                        "intent":{}
                    }))
                    .unwrap(),
                    &serde_json::from_value::<FrozenWork>(json!({
                        "items":[
                            {"item_id":"i1","target":{"symbol":"sh.600006","period":"day"},
                             "progress_unit":"x","frozen_inputs":{},"scope_keys":[occd]},
                            {"item_id":"i2","target":{"symbol":"sh.600006","period":"year"},
                             "progress_unit":"x","frozen_inputs":{},"scope_keys":[occy]}
                        ],
                        "scopes":[occd,occy]
                    }))
                    .unwrap(),
                    "run-check",
                    None,
                )
                .unwrap();
            let t4 = match acc3.record.outcome {
                RequestOutcome::Task(s) => s.task.task_id,
                _ => panic!("task outcome"),
            };
            repo.transition(&t4, 0, "run-check", Change::Claim).unwrap();
            repo.transition(&t4, 1, "run-check", Change::StartItem("i1".into()))
                .unwrap();
            repo.transition(&t4, 2, "run-check",
                Change::FinishItem("i1".into(),
                    ItemOutcome { success: true, error: None, evidence: None }))
                .unwrap();
            repo.transition(&t4, 3, "run-check", Change::StartItem("i2".into()))
                .unwrap();
            repo.transition(&t4, 4, "run-check",
                Change::FinishItem("i2".into(),
                    ItemOutcome { success: true, error: None, evidence: None }))
                .unwrap();
            let t4snap = repo
                .transition(&t4, 5, "run-check", Change::FinishExecution)
                .unwrap();
            let t4rev = t4snap.task.state_revision;
            repo.write(|c| {
                for (pid, vid, item, intent) in [
                    ("pub-legacy-daily", "legacy-daily", "i1",
                        json!({"Daily": {"expected_head": null}})),
                    ("pub-legacy-yearly", "legacy-yearly", "i2",
                        json!("HistoricalFixedBase")),
                ] {
                    let cmd = serde_json::to_string(&json!({
                        "publication_id": pid, "version_id": vid,
                        "task_id": t4, "item_id": item, "run_id": "run-check",
                        "expected_revision": 0,
                        "intent": intent,
                    })).unwrap();
                    let rcpt = serde_json::to_string(&json!({
                        "publication_id": pid, "version_id": vid,
                        "task_id": t4, "task_revision": t4rev,
                    })).unwrap();
                    c.execute(
                        "UPDATE publication_batches SET task_id=?1,item_id=?2,command_json=?3,receipt_json=?4 WHERE publication_id=?5",
                        rusqlite::params![t4, item, cmd, rcpt, pid],
                    ).map_err(storage_task)?;
                    c.execute(
                        "UPDATE task_publications SET task_id=?1 WHERE publication_id=?2",
                        rusqlite::params![t4, pid],
                    ).map_err(storage_task)?;
                }
                Ok(())
            }).unwrap();
        }
        // the verification check task: one running item, verification
        // scope bound to the target
        let vscope = crate::task_verification::verification_scope(&target);
        let target_snap = repo.read_task(&target).unwrap().unwrap();
        let acc2 = repo
            .accept(
                "req-check",
                &serde_json::from_value::<Command>(json!({
                    "key":serde_json::to_value(
                        crate::publication_verification::admission::command_key()).unwrap(),
                    "intent":{"target_task_id":target}
                }))
                .unwrap(),
                &serde_json::from_value::<FrozenWork>(json!({
                    "items":[{"item_id":"publication-results",
                              "target":{"task_id":target},
                              "progress_unit":"evidence_check",
                              "frozen_inputs":{"target_task_id":target,
                                               "accepted_run_id":target_snap.task.accepted_run_id},
                              "scope_keys":[vscope]}],
                    "scopes":[vscope]
                }))
                .unwrap(),
                "run-check",
                None,
            )
            .unwrap();
        let check_id = match acc2.record.outcome {
            RequestOutcome::Task(s) => s.task.task_id,
            _ => panic!("task outcome"),
        };
        repo.transition(&check_id, 0, "run-check", Change::Claim).unwrap();
        let check = repo
            .transition(&check_id, 1, "run-check",
                Change::StartItem("publication-results".into()))
            .unwrap();
        
        let facts = crate::publication_verification::EvidenceRepository::publication_facts(
            market, &target).unwrap();
        let plan = crate::publication_verification::decision::prepare(
            check.clone(), facts, "run-check", "actor-check", chrono::Utc::now())
            .unwrap();
        crate::publication_verification::decision::AuditStore::commit_check(
            market, &plan).unwrap();
    }
}
