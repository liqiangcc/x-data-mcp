// NM-092 fault-injection oracle — Rust side.
// Drives the same public surfaces the Node probe exercises:
// DatabaseLease::acquire, Endpoint::bind, acquire::Service::open,
// rusqlite begin/insert/kill-self (commit_kill), post-kill reopen
// state. Compile via scripts/node_migration_p4/fault_build.sh.
// Exit codes: 0 ok; 42 expected-refusal printed as JSON; 2 harness bug.
use std::io::Read;
use std::path::PathBuf;
use std::process;
use x_data_mcp::acquire::service::{self, Source};
use x_data_mcp::local_runtime::{DatabaseLease, Endpoint};

fn out(v: serde_json::Value) {
    println!("{}", serde_json::to_string(&v).unwrap());
}
fn err_json(e: &dyn std::fmt::Display) -> serde_json::Value {
    serde_json::json!({ "error": e.to_string() })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let code = match cmd {
        // lease_acquire <path> — acquire and hold until stdin EOF.
        "lease_hold" => {
            let path = PathBuf::from(&args[1]);
            match DatabaseLease::acquire(&path) {
                Ok(_lease) => {
                    out(serde_json::json!({ "acquired": true }));
                    let mut sink = Vec::new();
                    let _ = std::io::stdin().read_to_end(&mut sink);
                    0
                }
                Err(e) => {
                    out(err_json(&e));
                    42
                }
            }
        }
        // lease_try <path> — one-shot acquire attempt.
        "lease_try" => {
            let path = PathBuf::from(&args[1]);
            match DatabaseLease::acquire(&path) {
                Ok(_) => { out(serde_json::json!({ "acquired": true })); 0 }
                Err(e) => { out(err_json(&e)); 42 }
            }
        }
        // bind <dir> <dbname> — lease + endpoint bind, hold until EOF.
        "bind" => {
            let dir = PathBuf::from(&args[1]);
            let dbp = dir.join(&args[2]);
            match DatabaseLease::acquire(&dbp)
                .and_then(|lease| Endpoint::bind(&dir.join("svc.sock"), &lease)
                    .map(|ep| (lease, ep)))
            {
                Ok((_lease, _ep)) => {
                    out(serde_json::json!({ "bound": true }));
                    let mut sink = Vec::new();
                    let _ = std::io::stdin().read_to_end(&mut sink);
                    0
                }
                Err(e) => { out(err_json(&e)); 42 }
            }
        }
        // commit_kill <db> — begin, insert, SIGKILL self mid-tx.
        "commit_kill" => {
            let db = PathBuf::from(&args[1]);
            let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
            let kill_at: usize =
                args.get(3).and_then(|s| s.parse().ok()).unwrap_or(n / 2);
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE IF NOT EXISTS k(id INTEGER PRIMARY KEY, v TEXT NOT NULL);",
            )
            .unwrap();
            c.execute_batch("BEGIN IMMEDIATE").unwrap();
            for i in 0..n {
                c.execute("INSERT INTO k(v) VALUES(?1)", [&format!("row-{i}")])
                    .unwrap();
                if i + 1 == kill_at {
                    unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
                }
            }
            c.execute_batch("COMMIT").unwrap();
            out(serde_json::json!({ "committed": true }));
            0
        }
        // reopen_state <db> — row count + sha256 of concatenated rows +
        // whether a -wal file remains post-open checkpoint.
        "reopen_state" => {
            let db = PathBuf::from(&args[1]);
            let c = rusqlite::Connection::open(&db).unwrap();
            let n: i64 = c
                .query_row("SELECT count(*) FROM k", [], |r| r.get(0))
                .unwrap_or(-1);
            let mut st = c.prepare("SELECT v FROM k ORDER BY id").unwrap();
            let vals: Vec<String> = st
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            use sha2::Digest;
            let mut h = sha2::Sha256::new();
            for v in &vals { h.update(v.as_bytes()); }
            let wal = PathBuf::from(format!("{}-wal", db.display())).exists();
            out(serde_json::json!({
                "rows": n, "sha256": hex(&h.finalize()), "wal_left": wal,
                "first": vals.first(), "last": vals.last(),
            }));
            0
        }
        // acquire_run <root> <mirror> — open + acquire_generation +
        // worker_once loop to terminal; prints task/gen ids.
        "acquire_run" => {
            let root = PathBuf::from(&args[1]);
            let mirror = PathBuf::from(&args[2]);
            match service::AcquireService::open(&root, Source::Mirror(mirror))
                .and_then(|svc| {
                    svc.acquire_generation("req-acq-1")?;
                    for _ in 0..20 {
                        match svc.worker_once()? {
                            Some(_) => continue,
                            None => break,
                        }
                    }
                    Ok(svc)
                }) {
                Ok(svc) => {
                    drop(svc);
                    let (tasks, gens) = db_rows(&root);
                    out(serde_json::json!({ "opened": true, "tasks": tasks, "generations": gens }));
                    0
                }
                Err(e) => { out(err_json(&e)); 42 }
            }
        }
        // acquire_open <root> <mirror> — Service::open (reconcile) then
        // dump sync_status + digest of acquisition_tasks/generations
        // (timestamps stripped — they legitimately differ across impls).
        "acquire_open" => {
            let root = PathBuf::from(&args[1]);
            let mirror = PathBuf::from(&args[2]);
            match service::AcquireService::open(&root, Source::Mirror(mirror)) {
                Ok(svc) => {
                    let status = svc.sync_status().unwrap();
                    let status_v = serde_json::to_value(&status).unwrap();
                    drop(svc);
                    let (tasks, gens) = db_rows(&root);
                    out(serde_json::json!({ "opened": true,
                        "status": status_v, "tasks": tasks,
                        "generations": gens }));
                    0
                }
                Err(e) => { out(err_json(&e)); 42 }
            }
        }
        _ => {
            eprintln!("usage: fault_oracle <subcommand> ...");
            2
        }
    };
    process::exit(code);
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn db_rows(root: &PathBuf) -> (Vec<String>, Vec<String>) {
    let c = rusqlite::Connection::open(root.join("acquire.sqlite")).unwrap();
    let mut ts = c.prepare(
        "SELECT task_id,kind,state,generation_id,stage,error,detail FROM acquisition_tasks ORDER BY task_id").unwrap();
    let tasks: Vec<String> = ts.query_map([], |r| {
        Ok((0..7).map(|i| r.get::<_, Option<String>>(i).unwrap_or(None).unwrap_or_default()).collect::<Vec<_>>().join("|"))
    }).unwrap().map(|r| r.unwrap()).collect();
    let mut gs = c.prepare(
        "SELECT generation_id,state,consistency_status FROM generations ORDER BY generation_id").unwrap();
    let gens: Vec<String> = gs.query_map([], |r| {
        Ok((0..3).map(|i| r.get::<_, Option<String>>(i).unwrap_or(None).unwrap_or_default()).collect::<Vec<_>>().join("|"))
    }).unwrap().map(|r| r.unwrap()).collect();
    (tasks, gens)
}
