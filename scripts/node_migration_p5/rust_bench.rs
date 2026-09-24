// NM-072 P5 oracle — rlib-linked bench binary (NOT part of the crate;
// compiled on demand: rustc --edition 2024 -O rust_bench.rs
//   --extern x_data_mcp=<rlib> --extern serde_json=<rlib>
//   --extern serde=<rlib> --extern chrono=<rlib>
//   --extern rust_decimal=<rlib> --extern rusqlite=<rlib>
//   -L target/debug/deps -o <out>)
//
// Each subcommand times one surface op with warmup+N iterations and
// prints ONE JSONL stats line — the same record shape bench.mjs emits.
// Usage: rust_bench <surface> <args...>
use std::time::Instant;

use x_data_mcp::acquire::store::Db as AcquireDb;
use x_data_mcp::compact_market::CompactBar;
use x_data_mcp::market_data::{Bar, Period};
use x_data_mcp::market_storage::compat;
use x_data_mcp::mcp::{ApiError, ToolHandler, handle_json};
use x_data_mcp::sqlite_tasks::SqliteTasks;
use x_data_mcp::task_control::legacy::LegacyCollection;
use x_data_mcp::task_control::service::TaskControl;
use x_data_mcp::task_control::{Change, Contract, TaskRepository};

fn stats_line(surface: &str, samples: &mut Vec<u64>, input_bytes: u64,
              extra: &str) -> String {
    samples.sort_unstable();
    let n = samples.len();
    let mean = samples.iter().sum::<u64>() / n.max(1) as u64;
    let p50 = samples[n * 50 / 100];
    let p95 = samples[n * 95 / 100];
    let max = *samples.last().unwrap();
    format!(
        "{{\"surface\":{surface:?},\"impl\":\"rust\",\"iters\":{n},\"p50_ns\":{p50},\"p95_ns\":{p95},\"max_ns\":{max},\"mean_ns\":{mean},\"input_bytes\":{input_bytes},{extra}}}"
    )
}

fn timed<R, F: FnMut() -> R>(iters: usize, warmup: usize,
                             mut f: F) -> Vec<u64> {
    for _ in 0..warmup { f(); }
    let mut s = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        f();
        s.push(t.elapsed().as_nanos() as u64);
    }
    s
}

fn vmhwm() -> u64 {
    std::fs::read_to_string("/proc/self/status").ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmHWM"))
            .and_then(|l| l.split_whitespace().nth(1)?.parse().ok()))
        .unwrap_or(0)
}

fn db_bytes(path: &str) -> String {
    let db = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let wal = std::fs::metadata(format!("{path}-wal")).map(|m| m.len()).unwrap_or(0);
    format!("\"db_bytes\":{db},\"wal_bytes\":{wal},\"peak_rss_kb\":{}", vmhwm())
}

struct Stub;
impl ToolHandler for Stub {
    fn tools(&self) -> serde_json::Value { serde_json::json!([]) }
    fn call(&self, _n: &str, _a: serde_json::Value)
        -> Result<serde_json::Value, ApiError> {
        Ok(serde_json::json!({"ok": true}))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let surface = args.get(1).map(String::as_str).unwrap_or("");
    let iters: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(200);
    let warmup: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(20);
    let arg = args.get(2).map(String::as_str).unwrap_or("");

    match surface {
        // --- day_file parse over a real retained member ---
        "day_parse" => {
            let bytes = std::fs::read(arg).unwrap();
            let n = bytes.len() as u64;
            let s = timed(iters, warmup, ||
                std::hint::black_box(x_data_mcp::day_file::parse(
                    std::hint::black_box(&bytes)).unwrap()));
            println!("{}", stats_line("day_parse", &mut { s }, n,
                &format!("\"peak_rss_kb\":{}", vmhwm())));
        }
        // --- serde_json decode / encode on a fixed doc ---
        "serde_dec" => {
            let bytes = std::fs::read(arg).unwrap();
            let n = bytes.len() as u64;
            let s = timed(iters, warmup, || {
                let v: serde_json::Value =
                    serde_json::from_slice(std::hint::black_box(&bytes)).unwrap();
                std::hint::black_box(v);
            });
            println!("{}", stats_line("serde_json_decode", &mut { s }, n,
                &format!("\"peak_rss_kb\":{}", vmhwm())));
        }
        "serde_enc" => {
            let bytes = std::fs::read(arg).unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let n = bytes.len() as u64;
            let s = timed(iters, warmup, ||
                std::hint::black_box(serde_json::to_string(
                    std::hint::black_box(&v)).unwrap()));
            println!("{}", stats_line("serde_json_encode", &mut { s }, n,
                &format!("\"peak_rss_kb\":{}", vmhwm())));
        }
        // --- MCP transport roundtrip on a tools/call line ---
        "mcp_roundtrip" => {
            let api = Stub;
            let line = "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"x\",\"arguments\":{}}}";
            let n = line.len() as u64;
            let s = timed(iters, warmup, ||
                std::hint::black_box(handle_json(&api,
                    std::hint::black_box(line))));
            println!("{}", stats_line("mcp_roundtrip", &mut { s }, n,
                &format!("\"peak_rss_kb\":{}", vmhwm())));
        }
        // --- acquire store: accept_request fresh ids ---
        "acquire_accept" => {
            let dirp = std::path::Path::new(arg);
            std::fs::create_dir_all(dirp).unwrap();
            let dbp = dirp.join("acquire.sqlite");
            let _ = std::fs::remove_file(&dbp);
            let db = AcquireDb::new(&dbp);
            db.open().unwrap();
            let now = chrono::Utc::now();
            let s = timed(iters, warmup, || {
                let i = NEXT.with(|c| { let v = c.get(); c.set(v + 1); v });
                std::hint::black_box(db.accept_request(
                    &format!("bench-{i}"), "acquire_generation", "{}", now)
                    .unwrap());
            });
            println!("{}", stats_line("acquire_store_accept", &mut { s }, 0,
                &db_bytes(dbp.to_str().unwrap())));
        }
        // --- acquire store: reads (request + task + tasks_in) ---
        "acquire_read" => {
            let dirp = std::path::Path::new(arg);
            std::fs::create_dir_all(dirp).unwrap();
            let dbp = dirp.join("acquire.sqlite");
            let db = AcquireDb::new(&dbp);
            let s = timed(iters, warmup, || {
                let i = NEXT.with(|c| c.get());
                std::hint::black_box(db.request(
                    std::hint::black_box(&format!("bench-{i}"))).unwrap());
            });
            println!("{}", stats_line("acquire_store_read", &mut { s }, 0,
                &db_bytes(dbp.to_str().unwrap())));
        }
        // --- compact envelope pack / expand over bars ---
        "envelope_pack" | "envelope_expand" => {
            let bars: Vec<Bar> = (0..1000).map(|i| Bar {
                date: chrono::NaiveDate::from_ymd_opt(2024, 1, 2)
                    .unwrap() + chrono::Duration::days(i),
                open: rust_decimal::Decimal::new(1000 + i, 2),
                high: rust_decimal::Decimal::new(1100 + i, 2),
                low: rust_decimal::Decimal::new(900 + i, 2),
                close: rust_decimal::Decimal::new(1050 + i, 2),
                volume: Some(rust_decimal::Decimal::new(100_000, 0)),
                amount: Some(rust_decimal::Decimal::new(105_000_000, 2)),
                r7_raw: Some(7),
            }).collect();
            if surface == "envelope_pack" {
                let s = timed(iters, warmup, || {
                    let v: Vec<CompactBar> = bars.iter().map(|b|
                        CompactBar::from_bar(b, Period::Day).unwrap())
                        .collect();
                    std::hint::black_box(v);
                });
                println!("{}", stats_line("envelope_pack", &mut { s },
                    (bars.len() * 32) as u64,
                    &format!("\"peak_rss_kb\":{}", vmhwm())));
            } else {
                let cs: Vec<CompactBar> = bars.iter().map(|b|
                    CompactBar::from_bar(b, Period::Day).unwrap()).collect();
                let s = timed(iters, warmup, || {
                    let v: Vec<Bar> = cs.iter().map(|c|
                        c.to_bar(Period::Day).unwrap()).collect();
                    std::hint::black_box(v);
                });
                println!("{}", stats_line("envelope_expand", &mut { s },
                    (cs.len() * 32) as u64,
                    &format!("\"peak_rss_kb\":{}", vmhwm())));
            }
        }
        // --- record codec check over valid legacy records (JSONL) ---
        "record_check" => {
            let text = std::fs::read_to_string(arg).unwrap();
            let n = text.len() as u64;
            let recs: Vec<serde_json::Value> = text.lines()
                .map(|l| serde_json::from_str(l).unwrap()).collect();
            let s = timed(iters, warmup, || {
                for r in &recs {
                    std::hint::black_box(compat::check(
                        std::hint::black_box(r)).unwrap());
                }
            });
            println!("{}", stats_line("record_check", &mut { s }, n,
                &format!("\"records\":{},\"peak_rss_kb\":{}",
                    recs.len(), vmhwm())));
        }
        // --- task repo: accept (admission) on a schema-19 db.
        // arg = plans.json from emit_plan: {db, plans:[{command_json,
        //   work_json, ...}]} — per-iter distinct commands (symbols a{i}).
        "task_admit" => {
            let doc: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(arg).unwrap())
                    .unwrap();
            let dbp = doc["admit_db"].as_str().unwrap().to_string();
            let db = SqliteTasks::new(&dbp);
            let parsed: Vec<(x_data_mcp::task_control::Command,
                x_data_mcp::task_control::FrozenWork)> =
                doc["plans"].as_array().unwrap().iter().map(|p| (
                    serde_json::from_str(p["command_json"].as_str().unwrap())
                        .unwrap(),
                    serde_json::from_str(p["work_json"].as_str().unwrap())
                        .unwrap(),
                )).collect();
            // plans[] has iters+warmup distinct entries — warmup and
            // timed iterations each consume a fresh plan (no wrap).
            let s = timed(iters, warmup, || {
                let i = NEXT.with(|c| { let v = c.get(); c.set(v + 1); v });
                let (c2, w) = &parsed[i];
                std::hint::black_box(db.accept(&format!("ra-{i}"),
                    std::hint::black_box(c2),
                    std::hint::black_box(w), "bench", None).unwrap());
            });
            println!("{}", stats_line("task_admit", &mut { s }, 0,
                &db_bytes(&dbp)));
        }
        // --- claim transition on tasks admitted by emit_plan ---
        "task_claim" => {
            let doc: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(arg).unwrap())
                    .unwrap();
            let dbp = doc["db"].as_str().unwrap().to_string();
            let ids: Vec<String> = doc["claim_task_ids"]
                .as_array().unwrap().iter()
                .map(|v| v.as_str().unwrap().to_string()).collect();
            let db = SqliteTasks::new(&dbp);
            let s = timed(ids.len(), 0, || {
                let i = NEXT.with(|c| { let v = c.get(); c.set(v + 1); v });
                std::hint::black_box(db.transition(&ids[i], 0, "bench",
                    Change::Claim).unwrap());
            });
            println!("{}", stats_line("task_claim", &mut { s }, 0,
                &db_bytes(&dbp)));
        }
        // --- emit plan templates: accept N tasks via TaskControl (the
        // real admission path), then dump the stored row bytes verbatim
        // so the Node admit() inserts identical rows. First `iters`
        // plans carry distinct symbols (per-iter variation); the next
        // `iters` tasks seed the claim set.
        "emit_plan" => {
            let dbp = arg;
            let _ = std::fs::remove_file(dbp);
            let _ = std::fs::remove_file(format!("{dbp}-wal"));
            let _ = std::fs::remove_file(format!("{dbp}-shm"));
            // Schema 18 (legacy): the compact schema-19 write path is a
            // different repository; SqliteTasks refuses 19 by design
            // (check_schema current-gate), and the Node admission repo
            // accepts both — 18 is the shared-layer fixture.
            let db = SqliteTasks::new(dbp);
            db.initialize().unwrap();
            let ctl = TaskControl::new(db.clone(), "bench".into(),
                vec![Box::new(LegacyCollection)]).unwrap();
            let n: usize = args.get(3).and_then(|s| s.parse().ok())
                .unwrap_or(1);
            let c = rusqlite::Connection::open(dbp).unwrap();
            let mut plans = vec![];
            let mut claim_ids = vec![];
            for i in 0..(n * 2).max(2) {
                let rid = format!("emit-{i}");
                let acc = ctl.accept(&rid, LegacyCollection.key(),
                    serde_json::json!({"symbols":[format!("a{i}")],
                        "periods":["day"],"adjustment":"qfq"})).unwrap();
                let x_data_mcp::task_control::RequestOutcome::Task(snap) =
                    acc.record.outcome else { panic!("task outcome") };
                let tid = snap.task.task_id.clone();
                let command_json: String = c.query_row(
                    "SELECT command_json FROM requests WHERE request_id=?1",
                    [&rid], |r| r.get(0)).unwrap();
                let work_json: String = c.query_row(
                    "SELECT frozen_work_json FROM tasks WHERE task_id=?1",
                    [&tid], |r| r.get(0)).unwrap();
                let key_json: String = c.query_row(
                    "SELECT command_key_json FROM tasks WHERE task_id=?1",
                    [&tid], |r| r.get(0)).unwrap();
                let mut st = c.prepare(
                    "SELECT item_id,target_json,progress_unit,frozen_inputs_json,scope_keys_json FROM task_items WHERE task_id=?1 ORDER BY item_id").unwrap();
                let items: Vec<serde_json::Value> = st.query_map(
                    [&tid], |r| Ok(serde_json::json!({
                        "item_id": r.get::<_,String>(0)?,
                        "target_json": r.get::<_,String>(1)?,
                        "progress_unit": r.get::<_,String>(2)?,
                        "frozen_inputs_json": r.get::<_,String>(3)?,
                        "scope_keys_json": r.get::<_,String>(4)?,
                    }))).unwrap().map(|r| r.unwrap()).collect();
                let work: serde_json::Value =
                    serde_json::from_str(&work_json).unwrap();
                if i < n {
                    plans.push(serde_json::json!({
                        "command_json": command_json,
                        "work_json": work_json,
                        "execution_key_json": key_json,
                        "items": items,
                        "scopes": work["scopes"],
                        "task_id": tid,
                    }));
                } else {
                    claim_ids.push(tid);
                }
            }
            // Empty same-schema dbs for admit benches — admits into the
            // seeded db would range-conflict with emit tasks' scopes.
            let mk_empty = |suffix: &str| {
                let p = format!("{dbp}.{suffix}");
                for f in [&p, &format!("{p}-wal"), &format!("{p}-shm")] {
                    let _ = std::fs::remove_file(f);
                }
                SqliteTasks::new(&p).initialize().unwrap();
                p
            };
            println!("{}", serde_json::to_string(&serde_json::json!({
                "db": dbp,                    // seeded — rust claims
                "admit_db": mk_empty("radmit"), // rust admits
                "node_db": mk_empty("node"),    // node admits+claims
                "plans": plans, "claim_task_ids": claim_ids,
            })).unwrap());
        }
        _ => { eprintln!("unknown surface {surface}"); std::process::exit(2); }
    }
}

thread_local! { static NEXT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
