//! v8 phase 2 PR-1 (D-56): one process, several sockets. The process holds the
//! single write lease, binds every `<socket> --entry <kind>` pair, serves each
//! listener its own allowlist and runs at most one worker.
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use x_data_mcp::{
    local_runtime::{Config, DatabaseLease, Endpoint, WorkerGate, serve_many},
    mcp::{ApiError, ToolHandler},
    sqlite_tasks::SqliteTasks,
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
}
impl Service {
    fn start<P: AsRef<Path>>(db: &Path, sockets: &[(P, &str)], poll: u64) -> Self {
        let mut args: Vec<OsString> = Vec::new();
        for (socket, entry) in sockets {
            args.push(socket.as_ref().as_os_str().to_owned());
            args.push("--entry".into());
            args.push((*entry).into());
        }
        args.push("--poll-ms".into());
        args.push(poll.to_string().into());
        let mut child = Command::new(env!("CARGO_BIN_EXE_data_service"))
            .arg(db)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "service exited before readiness"
            );
            if sockets
                .iter()
                .all(|(socket, _)| x_data_mcp::local_runtime::connect(socket.as_ref()).is_ok())
            {
                break;
            }
            assert!(Instant::now() < deadline, "service readiness deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self { child }
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
        }
    }
    /// Byte-level request/response: the same request sent to two sockets must
    /// produce the same line, not merely the same parsed value.
    fn raw(&mut self, request: Value) -> String {
        writeln!(self.stream, "{request}").unwrap();
        let mut line = String::new();
        assert!(self.reader.read_line(&mut line).unwrap() > 0);
        line
    }
    fn call(&mut self, name: &str, args: Value) -> Value {
        let line = self.raw(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":args}}));
        serde_json::from_str(&line).unwrap()
    }
}

/// `tools/list` line plus the number of tools it advertises.
fn tools_list(socket: &Path) -> (usize, String) {
    let mut session = Session::new(socket);
    session.raw(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
    let line = session.raw(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    let value: Value = serde_json::from_str(&line).unwrap();
    let names: Vec<_> = value["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    (names.len(), line)
}

#[test]
fn three_entries_in_one_process_match_the_single_socket_surfaces_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    // Baseline: one single-socket service per entry, one at a time (the write
    // lease is exclusive, so they cannot overlap).
    let mut baseline: Vec<String> = Vec::new();
    for (entry, expected) in [("research", 5usize), ("ops", 15), ("maint", 9)] {
        let socket = dir.path().join(format!("solo-{entry}.sock"));
        let mut service = Service::start(&db, &[(&socket, entry)], 60000);
        let (count, line) = tools_list(&socket);
        assert_eq!(count, expected, "{entry}");
        baseline.push(line);
        service.stop();
        assert!(!socket.exists(), "single socket removed on stop: {entry}");
    }

    // One process, three sockets, one lease.
    let research = dir.path().join("research.sock");
    let ops = dir.path().join("mcp.sock");
    let maint = dir.path().join("maint.sock");
    let mut service = Service::start(
        &db,
        &[(&research, "research"), (&ops, "ops"), (&maint, "maint")],
        60000,
    );
    for (index, socket) in [&research, &ops, &maint].into_iter().enumerate() {
        let (_, line) = tools_list(socket);
        assert_eq!(
            line,
            baseline[index],
            "{} tools/list differs from its single-socket service",
            socket.display()
        );
    }

    // A tool outside the listener's allowlist stays unknown.
    let update = Session::new(&research).call("update_data", json!({}));
    assert_eq!(
        update["result"]["structuredContent"]["code"],
        "unknown_tool"
    );
    let query = Session::new(&maint).call("query_data", json!({}));
    assert_eq!(query["result"]["structuredContent"]["code"], "unknown_tool");

    // A tool served by two entries answers with the same payload bytes on
    // both. The top-level `content[0].text` is entry-specific by design
    // (D-36: ops/maint use a bounded summary), so the comparison is on the
    // tool result itself.
    let shared = json!({
        "dataset": "market.daily",
        "contract_version": "v2",
        "selection": {"kind": "single", "entity": "sh.600000"},
        "filter": {"kind": "daily", "start_date": "2025-01-01", "end_date": "2025-12-31"}
    });
    let request = json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"query_data","arguments":shared}});
    let payload = |line: &str| -> (bool, Vec<u8>) {
        let value: Value = serde_json::from_str(line).unwrap();
        (
            value["result"]["isError"].as_bool().unwrap(),
            serde_json::to_vec(&value["result"]["structuredContent"]).unwrap(),
        )
    };
    let mut research_session = Session::new(&research);
    research_session.raw(json!({"jsonrpc":"2.0","id":8,"method":"initialize","params":{}}));
    let from_research = research_session.raw(request.clone());
    let mut ops_session = Session::new(&ops);
    ops_session.raw(json!({"jsonrpc":"2.0","id":8,"method":"initialize","params":{}}));
    let from_ops = ops_session.raw(request);
    assert_eq!(
        payload(&from_research),
        payload(&from_ops),
        "query_data payload differs by entry"
    );
    drop(research_session);
    drop(ops_session);

    service.stop();
    for socket in [&research, &ops, &maint] {
        assert!(
            !socket.exists(),
            "socket removed on stop: {}",
            socket.display()
        );
    }
}

#[test]
fn one_process_holds_the_only_write_lease_with_several_sockets_bound() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let research = dir.path().join("research.sock");
    let ops = dir.path().join("mcp.sock");
    let mut service = Service::start(&db, &[(&research, "research"), (&ops, "ops")], 60000);
    // A second process on the same database is refused at the lease, before it
    // binds its socket: the multi-socket process is still the single writer.
    let other = dir.path().join("other.sock");
    let second = Command::new(env!("CARGO_BIN_EXE_data_service"))
        .arg(&db)
        .arg(&other)
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already active"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(!other.exists());
    service.stop();
}

#[test]
fn any_bind_failure_refuses_the_whole_start_and_removes_earlier_sockets() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let good = dir.path().join("good.sock");
    let bad = dir.path().join("bad.sock");
    fs::write(&bad, b"keep this file").unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_data_service"))
        .arg(&db)
        .arg(&good)
        .args(["--entry", "research"])
        .arg(&bad)
        .args(["--entry", "ops"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("endpoint"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !good.exists(),
        "a partial start must not leave the first socket behind"
    );
    assert_eq!(fs::read(&bad).unwrap(), b"keep this file");

    // An unknown entry anywhere in the list refuses the start too.
    let unknown = Command::new(env!("CARGO_BIN_EXE_data_service"))
        .arg(&db)
        .arg(dir.path().join("one.sock"))
        .args(["--entry", "research"])
        .arg(dir.path().join("two.sock"))
        .args(["--entry", "all"])
        .output()
        .unwrap();
    assert!(!unknown.status.success());
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("--entry"),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );
    assert!(!dir.path().join("one.sock").exists());
}

#[test]
fn research_and_maint_start_without_an_ops_socket() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let research = dir.path().join("research.sock");
    let maint = dir.path().join("maint.sock");
    let mut service = Service::start(&db, &[(&research, "research"), (&maint, "maint")], 60000);
    assert_eq!(tools_list(&research).0, 5);
    assert_eq!(tools_list(&maint).0, 9);
    assert!(
        service.child.try_wait().unwrap().is_none(),
        "no-ops process must stay up"
    );
    service.stop();
}

/// A test-only session handler: it records which thread executed worker turns,
/// so several listeners can be checked for exactly one worker.
struct Counting {
    label: &'static str,
    turns: Arc<AtomicUsize>,
    threads: Arc<Mutex<Vec<std::thread::ThreadId>>>,
}
impl ToolHandler for Counting {
    fn tools(&self) -> Value {
        json!([{"name": self.label, "description": "test-only", "inputSchema": {"type": "object"}}])
    }
    fn call(&self, _: &str, _: Value) -> Result<Value, ApiError> {
        Ok(json!({"entry": self.label}))
    }
    fn worker_once(&self) -> Result<Value, ApiError> {
        self.turns.fetch_add(1, Ordering::SeqCst);
        let id = std::thread::current().id();
        let mut threads = self.threads.lock().unwrap();
        if !threads.contains(&id) {
            threads.push(id);
        }
        Ok(json!({"idle": true}))
    }
}

#[test]
fn several_listeners_share_one_worker_and_keep_their_own_session() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let db = database(dir.path());
    let lease = Arc::new(DatabaseLease::acquire(&db).unwrap());
    let paths = [
        dir.path().join("research.sock"),
        dir.path().join("mcp.sock"),
        dir.path().join("maint.sock"),
    ];
    // Two of the three listeners are ops: still exactly one worker turn loop.
    let entries = ["research", "ops", "ops"];
    let mut listeners = Vec::new();
    for (path, entry) in paths.iter().zip(entries) {
        listeners.push((Endpoint::bind(path, &lease).unwrap(), entry));
    }
    let turns = Arc::new(AtomicUsize::new(0));
    let threads = Arc::new(Mutex::new(Vec::new()));
    let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate = Arc::new(WorkerGate::default());
    let factory = {
        let turns = turns.clone();
        let threads = threads.clone();
        move |label: &'static str| -> std::io::Result<Counting> {
            Ok(Counting {
                label,
                turns: turns.clone(),
                threads: threads.clone(),
            })
        }
    };
    let server = {
        let stopping = stopping.clone();
        let gate = gate.clone();
        std::thread::spawn(move || {
            serve_many(
                listeners,
                "ops",
                lease,
                factory,
                gate,
                stopping,
                Config {
                    poll_interval: Duration::from_millis(10),
                    ..Config::default()
                },
            )
        })
    };
    for (index, path) in paths.iter().enumerate() {
        let deadline = Instant::now() + Duration::from_secs(10);
        while x_data_mcp::local_runtime::connect(path).is_err() {
            assert!(Instant::now() < deadline, "listener readiness deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut session = Session::new(path);
        let line = session.raw(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"any","arguments":{}}}));
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            value["result"]["structuredContent"]["entry"],
            entries[index],
            "listener {} served another entry's session",
            path.display()
        );
        let tools = session.raw(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
        let value: Value = serde_json::from_str(&tools).unwrap();
        assert_eq!(value["result"]["tools"][0]["name"], entries[index]);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while turns.load(Ordering::SeqCst) < 3 {
        assert!(Instant::now() < deadline, "worker did not run");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        threads.lock().unwrap().len(),
        1,
        "more than one worker thread ran"
    );
    stopping.store(true, Ordering::SeqCst);
    server.join().unwrap().unwrap();
}
