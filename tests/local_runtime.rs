#![cfg(target_os = "linux")]
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
    assert_eq!(tools.len(), 20);
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
