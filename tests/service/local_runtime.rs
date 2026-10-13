use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use x_data_mcp::{
    local_runtime::{DatabaseLease, Endpoint, WorkerGate},
    mcp::ToolHandler,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Change, Contract, RequestOutcome, Status, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
};
fn database(dir: &Path) -> PathBuf {
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join("db.sqlite");
    SqliteTasks::new(&path).initialize().unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}
struct Service {
    child: Child,
    socket: PathBuf,
}
impl Service {
    fn start(db: &Path, socket: &Path, poll: u64) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_data_service"))
            .arg(db)
            .arg(socket)
            .args(["--poll-ms", &poll.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "service exited before readiness"
            );
            if x_data_mcp::local_runtime::connect(socket).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "service readiness deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self {
            child,
            socket: socket.into(),
        }
    }
    fn stop(&mut self) {
        // SAFETY: test owns this child PID and only sends SIGTERM to it.
        assert_eq!(
            unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                break;
            }
            assert!(Instant::now() < deadline, "service stop deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
struct Session {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    seq: u64,
}
impl Session {
    fn new(socket: &Path) -> Self {
        let stream = x_data_mcp::local_runtime::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Self {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
            seq: 0,
        }
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.seq += 1;
        writeln!(
            self.stream,
            "{}",
            json!({"jsonrpc":"2.0","id":self.seq,"method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        assert!(self.reader.read_line(&mut line).unwrap() > 0);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], self.seq);
        v
    }
    fn call(&mut self, name: &str, args: Value, error: bool) -> Value {
        let v = self.request("tools/call", json!({"name":name,"arguments":args}));
        assert_eq!(v["result"]["isError"], error, "{v}");
        let result = &v["result"];
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            result["structuredContent"]
        );
        result["structuredContent"].clone()
    }
}
fn wait_terminal(db: &Path, id: &str) -> x_data_mcp::task_control::TaskSnapshot {
    // The restarted service ticks its worker every few milliseconds and may
    // hold the SQLite write lock across the poller's busy budget — WAL-index
    // recovery after a SIGKILL makes a fresh read connection especially
    // sensitive. Transient lock contention is not a missing terminal state,
    // so transient storage errors are retried within the deadline.
    let tasks = SqliteTasks::new(db).with_busy_timeout(Duration::from_secs(5));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match tasks.read_task(id) {
            Ok(Some(task)) => {
                if task.task.status.terminal() {
                    return task;
                }
            }
            Ok(None) => panic!("task missing: {id}"),
            Err(x_data_mcp::task_control::Error::StorageUnavailable(_)) => {}
            Err(e) => panic!("task read failed: {e}"),
        }
        assert!(Instant::now() < deadline, "task not terminal");
        std::thread::sleep(Duration::from_millis(20));
    }
}
/// Same transient-contention tolerance for snapshot reads taken while a
/// fast-ticking restarted service may hold the write lock.
fn read_task_retried(tasks: &SqliteTasks, id: &str) -> x_data_mcp::task_control::TaskSnapshot {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match tasks.read_task(id) {
            Ok(Some(task)) => return task,
            Ok(None) => panic!("task missing: {id}"),
            Err(x_data_mcp::task_control::Error::StorageUnavailable(_)) => {
                assert!(Instant::now() < deadline, "task read deadline");
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("task read failed: {e}"),
        }
    }
}
#[test]
fn real_service_proxy_disconnect_keeps_accepted_sqlite_task_and_source_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let socket = dir.path().join("mcp.sock");
    let mut service = Service::start(&db, &socket, 5000);
    let mut child = Command::new(env!("CARGO_BIN_EXE_mcp_connect"))
        .arg(&socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(child.stdin.as_mut().unwrap(),"{}",json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"start_collection","arguments":{"request_id":"disconnected","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600000"}]}}}})).unwrap();
    let mut line = String::new();
    assert!(output.read_line(&mut line).unwrap() > 0);
    let accepted: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(accepted["result"]["isError"], false, "{accepted}");
    let task_id = accepted["result"]["structuredContent"]["task_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        SqliteTasks::new(&db)
            .read_task(task_id)
            .unwrap()
            .unwrap()
            .task
            .status,
        Status::Pending
    );
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    let finished = wait_terminal(&db, task_id);
    assert_eq!(finished.task.status, Status::Failed);
    assert!(
        finished.items[0]
            .outcome
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("SOURCE_NOT_VERIFIED")
    );
    let mut session = Session::new(&service.socket);
    assert!(session.request("initialize", json!({}))["result"]["serverInfo"].is_object());
    let tools = session.request("tools/list", json!({}))["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tools.len(), 23);
    let caps = session.call("get_data_capabilities", json!({}), false);
    assert_eq!(caps["production_ready"], false);
    let names: std::collections::BTreeSet<_> =
        tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let registry: std::collections::BTreeSet<_> = caps["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["tool_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, registry);
    assert_eq!(
        session.call(
            "get_task",
            json!({"contract_version":"v1","task_id":task_id}),
            false
        )["status"],
        "failed"
    );
    assert_eq!(
        session.call("run_worker_once", json!({"extra":true}), true)["code"],
        "invalid_arguments"
    );
    let replay = session.call(
        "start_collection",
        json!({"request_id":"disconnected","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600000"}]}}),
        false,
    );
    assert_eq!(replay["task_id"], task_id);
    assert_eq!(replay["reused"], true);
    drop(session);
    service.stop();
}
#[test]
fn real_service_restart_does_not_resume_old_nonterminal_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let socket = dir.path().join("mcp.sock");
    let tasks = SqliteTasks::new(&db);
    let control = TaskControl::new(
        tasks.clone(),
        "old-fixture-run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let mut historical = Vec::new();
    for (i, status) in [Status::Running, Status::CancelRequested]
        .into_iter()
        .enumerate()
    {
        let RequestOutcome::Task(task) = control
            .accept(
                &format!("old-{i}"),
                LegacyCollection.key(),
                json!({"symbols":[format!("sh.60000{}",i+1)],"periods":["day"]}),
            )
            .unwrap()
            .record
            .outcome
        else {
            panic!()
        };
        let mut s = tasks
            .transition(&task.task.task_id, 0, "old-fixture-run", Change::Claim)
            .unwrap();
        if status == Status::CancelRequested {
            s = tasks
                .transition(
                    &s.task.task_id,
                    s.task.state_revision,
                    "old-fixture-run",
                    Change::RequestCancel,
                )
                .unwrap();
        }
        historical.push(s);
    }
    let mut service = Service::start(&db, &socket, 60000);
    let mut session = Session::new(&socket);
    let pending = session.call(
        "start_collection",
        json!({"request_id":"old-pending","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sz","code":"000001"}]}}),
        false,
    );
    historical.push(
        tasks
            .read_task(pending["task_id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
    );
    drop(session);
    service.child.kill().unwrap();
    assert!(!service.child.wait().unwrap().success());
    assert!(socket.exists()); // Our stale socket remains after actual SIGKILL.
    let mut restarted = Service::start(&db, &socket, 10);
    let mut session = Session::new(&socket);
    let new = session.call(
        "start_collection",
        json!({"request_id":"new-run","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sz","code":"000002"}]}}),
        false,
    );
    wait_terminal(&db, new["task_id"].as_str().unwrap());
    let reader = tasks.clone().with_busy_timeout(Duration::from_secs(5));
    for s in historical {
        assert_eq!(read_task_retried(&reader, &s.task.task_id), s);
        assert_eq!(
            session.call(
                "get_task",
                json!({"contract_version":"v1","task_id":s.task.task_id}),
                false
            )["status"],
            "needs_verification"
        );
        assert!(!s.claims.is_empty());
    }
    drop(session);
    restarted.stop();
}
#[test]
fn real_service_same_database_exclusion_and_no_init_or_migration() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let socket = dir.path().join("mcp.sock");
    let mut service = Service::start(&db, &socket, 100);
    let second = Command::new(env!("CARGO_BIN_EXE_data_service"))
        .arg(&db)
        .arg(dir.path().join("other.sock"))
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(!dir.path().join("other.sock").exists());
    service.stop();
    for (name, contents) in [("missing", None), ("bad", Some(b"bad sqlite".as_slice()))] {
        let file = dir.path().join(name);
        if let Some(bytes) = contents {
            fs::write(&file, bytes).unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let result = Command::new(env!("CARGO_BIN_EXE_data_service"))
            .arg(&file)
            .arg(dir.path().join(format!("{name}.sock")))
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert_eq!(file.exists(), contents.is_some());
    }
    let c = rusqlite::Connection::open(&db).unwrap();
    c.pragma_update(None, "user_version", 1234).unwrap();
    drop(c);
    assert!(
        !Command::new(env!("CARGO_BIN_EXE_data_service"))
            .arg(&db)
            .arg(&socket)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        rusqlite::Connection::open(&db)
            .unwrap()
            .query_row("pragma user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        1234
    );
}
#[test]
fn private_endpoint_rejects_symlink_unregistered_files_and_database_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let lease = DatabaseLease::acquire(&db).unwrap();
    let path = dir.path().join("socket");
    fs::write(&path, b"keep this file").unwrap();
    assert!(Endpoint::bind(&path, &lease).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"keep this file");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(Endpoint::bind(&link, &lease).is_err());
    assert!(x_data_mcp::local_runtime::connect(&link).is_err());
    let public = dir.path().join("public");
    fs::create_dir(&public).unwrap();
    fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Endpoint::bind(&public.join("socket"), &lease).is_err());
    fs::rename(&db, dir.path().join("preserved-db")).unwrap();
    fs::write(&db, b"new path different inode").unwrap();
    fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(lease.validate().is_err());
}
#[test]
fn shared_execution_gate_keeps_invalid_wire_validation_and_read_access() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let gate = Arc::new(WorkerGate::default());
    let api = x_data_mcp::data_supply::mcp::DataApi::new(
        SqliteTasks::new(db),
        "runtime".into(),
        x_data_mcp::data_supply::Access::local(),
    )
    .unwrap()
    .with_worker_gate(gate.clone());
    let held = gate.acquire().unwrap();
    assert_eq!(
        api.call("run_worker_once", json!({})).unwrap_err().code,
        "worker_busy"
    );
    assert_eq!(
        api.call("run_worker_once", json!({"extra":true}))
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    assert!(api.call("get_data_capabilities", json!({})).is_ok());
    drop(held);
    gate.stop();
    assert_eq!(
        api.call("run_worker_once", json!({})).unwrap_err().code,
        "worker_stopping"
    );
}

#[path = "local_runtime/lifecycle.rs"]
mod lifecycle;

#[test]
fn real_proxy_exits_on_service_eof_even_when_client_stdin_stays_open() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let socket = dir.path().join("socket");
    let mut service = Service::start(&db, &socket, 100);
    let mut proxy = Command::new(env!("CARGO_BIN_EXE_mcp_connect"))
        .arg(&socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        proxy.stdin.as_mut().unwrap(),
        "{}",
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})
    )
    .unwrap();
    let mut reader = BufReader::new(proxy.stdout.take().unwrap());
    let mut line = String::new();
    assert!(reader.read_line(&mut line).unwrap() > 0);
    service.stop();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = proxy.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "proxy waited for still-open client stdin after service EOF"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(proxy.stdin.is_some());
}

/// XP-025 A: terminal-history corruption no longer blocks accept; it is
/// caught by the startup sweep (service refuses to start) and by the
/// read-only `task_sqlite check`.
#[test]
fn startup_sweep_and_task_sqlite_check_reject_corrupt_history() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let check = |db: &Path| {
        Command::new(env!("CARGO_BIN_EXE_task_sqlite"))
            .arg("check")
            .arg(db)
            .output()
            .unwrap()
    };
    assert!(check(&db).status.success(), "clean database passes check");
    // A terminal task with no request: an "unbound execution record".
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute(
        "INSERT INTO tasks(task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of,item_count,items_pending,items_running,items_finished,items_succeeded) VALUES('hist-0','{}','succeeded',3,'seed-run','seed-run','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','{}','{}',NULL,0,0,0,0,0)",
        [],
    )
    .unwrap();
    drop(c);
    let checked = check(&db);
    assert!(!checked.status.success());
    assert!(
        String::from_utf8_lossy(&checked.stderr).contains("unbound execution record"),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let socket = dir.path().join("mcp.sock");
    let started = Command::new(env!("CARGO_BIN_EXE_data_service"))
        .arg(&db)
        .arg(&socket)
        .output()
        .unwrap();
    assert!(!started.status.success());
    assert!(
        String::from_utf8_lossy(&started.stderr).contains("registry check failed"),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    assert!(!socket.exists());
}

fn compact_database(dir: &Path) -> PathBuf {
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join("compact.sqlite");
    let init = Command::new(env!("CARGO_BIN_EXE_task_sqlite"))
        .args(["init20"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}
fn task_check(db: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_task_sqlite"))
        .arg("check")
        .arg(db)
        .output()
        .unwrap()
}
/// XP-025 A on the production schema (compact v20): healthy database starts
/// and `task_sqlite check` runs read-only while the service holds the lease.
#[test]
fn compact_startup_sweep_passes_and_check_runs_beside_a_live_service() {
    let dir = tempfile::tempdir().unwrap();
    let db = compact_database(dir.path());
    assert!(
        task_check(&db).status.success(),
        "healthy check before start"
    );
    let socket = dir.path().join("mcp.sock");
    let mut service = Service::start(&db, &socket, 100);
    let checked = task_check(&db);
    assert!(
        checked.status.success(),
        "check beside live service: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(
        service.child.try_wait().unwrap().is_none(),
        "check must not disturb the service"
    );
    assert!(x_data_mcp::local_runtime::connect(&socket).is_ok());
    service.stop();
}
#[test]
fn compact_corrupt_history_is_rejected_by_check_and_startup() {
    let dir = tempfile::tempdir().unwrap();
    let db = compact_database(dir.path());
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute(
        "INSERT INTO tasks(task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of,item_count,items_pending,items_running,items_finished,items_succeeded) VALUES('hist-0','{}','succeeded',3,'seed-run','seed-run','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','{}','{}',NULL,0,0,0,0,0)",
        [],
    )
    .unwrap();
    drop(c);
    let checked = task_check(&db);
    assert!(!checked.status.success());
    assert!(
        String::from_utf8_lossy(&checked.stderr).contains("unbound execution record"),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let socket = dir.path().join("mcp.sock");
    let started = Command::new(env!("CARGO_BIN_EXE_data_service"))
        .arg(&db)
        .arg(&socket)
        .output()
        .unwrap();
    assert!(!started.status.success());
    assert!(
        String::from_utf8_lossy(&started.stderr).contains("registry check failed"),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    assert!(!socket.exists());
}

/// Seeds `n` self-consistent succeeded tasks (request, item, scope, start
/// marker) straight into the tasks tables; each touches `symbol`/day.
/// `created_at` carries six fractional digits ending in 1 so the stored
/// text equals chrono's canonical rendering (keyset cursors compare text).
fn seed_history(db: &Path, symbol: &str, period: &str, first: usize, n: usize) {
    let mut c = rusqlite::Connection::open(db).unwrap();
    let tx = c.transaction().unwrap();
    let scope = x_data_mcp::task_control::legacy::scope_id(symbol, period);
    let key =
        r#"{"operation":"start_collection","dataset":"market.daily","contract_version":"v1"}"#;
    let command = format!(r#"{{"key":{key},"intent":{{}}}}"#);
    let digest = x_data_mcp::hash::sha256(command.as_bytes());
    let work = serde_json::json!({
        "items":[{"item_id":"i","target":{"symbol":symbol,"period":period},"progress_unit":"u",
                  "frozen_inputs":{},"scope_keys":[scope]}],
        "scopes":[scope]
    })
    .to_string();
    for i in first..first + n {
        let id = format!("task-seed-{symbol}-{period}-{i}");
        let created = format!("2026-01-01T00:00:00.{:06}+00:00", i * 10 + 1);
        tx.execute(
            "INSERT INTO tasks(task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of,item_count,items_pending,items_running,items_finished,items_succeeded) \
             VALUES(?1,?2,'succeeded',3,'seed-run','seed-run',?3,?3,?4,'{}',NULL,1,0,0,1,1)",
            rusqlite::params![id, key, created, work],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO requests VALUES(?1,?2,?3,'task',?4,NULL)",
            rusqlite::params![format!("req-{id}"), command, digest, id],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO task_items VALUES(?1,'i',?2,'u','{}',?3,'finished','{\"success\":true}')",
            rusqlite::params![
                id,
                format!(r#"{{"symbol":"{symbol}","period":"{period}"}}"#),
                format!(r#"["{}"]"#, scope.replace('"', "\\\""))
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO task_scopes VALUES(?1,?2)",
            rusqlite::params![id, scope],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO task_item_execution_starts VALUES(?1,'i','seed-run',2,'2026-01-01T00:00:00Z')",
            rusqlite::params![id],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

/// XP-025 D (server side): with >10,000 historical tasks (the old hard cap)
/// `list_tasks` pages from SQL and `get_data_availability` relates only the
/// requested entity's tasks; both stay correct and small, on schema 20.
#[test]
fn compact_availability_and_list_tasks_survive_more_than_ten_thousand_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let db = compact_database(dir.path());
    // 10,050 tasks for unrelated symbols, 41 for the queried one.
    seed_history(&db, "sz.000001", "day", 0, 10_050);
    seed_history(&db, "sh.600006", "day", 20_000, 40);
    // Gap-verification tasks key their scope with period "daily" (not "day");
    // the newest one must still be related to the entity (regression).
    seed_history(&db, "sh.600006", "daily", 30_000, 1);
    let socket = dir.path().join("mcp.sock");
    let mut service = Service::start(&db, &socket, 100);
    let mut session = Session::new(&socket);
    // list_tasks: newest-first keyset pages, no snapshot budget failure.
    let page = session.call("list_tasks", json!({"page_size": 3}), false);
    assert_eq!(page["page_complete"], false, "{page}");
    let first: Vec<String> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["created_at"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(first.len(), 3);
    assert!(
        first.windows(2).all(|w| w[0] > w[1]),
        "descending: {first:?}"
    );
    assert!(page.to_string().len() < 20_000, "page size is bounded");
    let cursor = page["next_cursor"].as_str().unwrap().to_string();
    let next = session.call(
        "list_tasks",
        json!({"page_size": 3, "cursor": cursor}),
        false,
    );
    let second: Vec<String> = next["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["created_at"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(second.len(), 3);
    assert!(
        first.last().unwrap() > second.first().unwrap(),
        "no overlap"
    );
    // availability: only the queried entity's tasks are related.
    let availability = session.call(
        "get_data_availability",
        json!({"dataset":"market.daily","contract_version":"v1",
               "selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},
               "filter":{"kind":"daily","start_date":"2025-01-01","end_date":"2025-01-05"},
               "version":{"kind":"latest_per_entity"}}),
        false,
    );
    let item = &availability["items"][0];
    let related = item["related_tasks"].as_array().unwrap();
    // generic-data-v5: nothing is in flight here, so only the newest 5
    // finished tasks are listed; total/truncated say what was cut.
    assert_eq!(related.len(), 5, "{availability}");
    assert_eq!(item["related_tasks_total"], 41, "{availability}");
    assert_eq!(item["related_tasks_truncated"], true, "{availability}");
    assert!(
        related
            .iter()
            .all(|t| t["task_id"].as_str().unwrap().contains("sh.600006")),
        "unrelated symbols must not leak in"
    );
    let created: Vec<&str> = related
        .iter()
        .map(|t| t["created_at"].as_str().unwrap())
        .collect();
    assert!(created.windows(2).all(|w| w[0] > w[1]), "newest first");
    assert!(
        related[0]["task_id"]
            .as_str()
            .unwrap()
            .ends_with("-daily-30000"),
        "the 'daily'-scoped (gap verification) task is related: {availability}"
    );
    service.stop();
}
