//! Controlled executor exists only in this test binary. No runtime source switch.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use x_data_mcp::{
    local_runtime::{Config, serve},
    task_control::{CommandKey, Executor, ItemOutcome, ItemSpec},
};
#[derive(Default)]
struct Blocking {
    entered: AtomicBool,
    release: AtomicBool,
    count: AtomicUsize,
}
impl Executor for Blocking {
    fn execute(&self, _: &CommandKey, _: &ItemSpec) -> ItemOutcome {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.entered.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.release.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "bounded fixture release deadline"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        ItemOutcome {
            success: false,
            error: Some("test-only controlled execution".into()),
        }
    }
}
struct FixtureApi {
    api: x_data_mcp::data_supply::mcp::DataApi,
    gate: Arc<WorkerGate>,
    executor: Arc<Blocking>,
}
fn error(code: &str) -> x_data_mcp::mcp::ApiError {
    x_data_mcp::mcp::ApiError {
        code: code.into(),
        message: code.into(),
        scopes: None,
        outcome_certainty: None,
        next_actions: None,
        details: None,
    }
}
impl ToolHandler for FixtureApi {
    fn tools(&self) -> Value {
        self.api.tools()
    }
    fn call(&self, name: &str, args: Value) -> Result<Value, x_data_mcp::mcp::ApiError> {
        if name != "run_worker_once" || args != json!({}) {
            return self.api.call(name, args);
        }
        let _permit = self.gate.acquire().map_err(error)?;
        let task = self
            .api
            .legacy
            .control
            .run_once(self.executor.as_ref())
            .map_err(|_| error("fixture_execution_error"))?;
        Ok(
            json!({"idle":task.is_none(),"task_id":task.as_ref().map(|t|&t.task.task_id),"status":task.as_ref().map(|t|t.visible_status(&self.api.legacy.control.run_id))}),
        )
    }
}
fn fixture_server(
    db: &Path,
    socket: &Path,
    executor: Arc<Blocking>,
    stop: Arc<AtomicBool>,
    config: Config,
) -> std::thread::JoinHandle<std::io::Result<()>> {
    let lease = Arc::new(DatabaseLease::acquire(db).unwrap());
    let endpoint = Endpoint::bind(socket, &lease).unwrap();
    let db = db.to_path_buf();
    let gate = Arc::new(WorkerGate::default());
    let factory = {
        let gate = gate.clone();
        move || {
            Ok(FixtureApi {
                api: x_data_mcp::data_supply::mcp::DataApi::new(
                    SqliteTasks::new(&db),
                    "controlled-service".into(),
                    x_data_mcp::data_supply::Access::local(),
                )
                .map_err(std::io::Error::other)?
                .with_worker_gate(gate.clone()),
                gate: gate.clone(),
                executor: executor.clone(),
            })
        }
    };
    std::thread::spawn(move || serve(endpoint, lease, factory, gate, stop, config))
}
fn entered(executor: &Blocking) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !executor.entered.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn executing_sqlite_task_survives_disconnect_allows_concurrent_cancel_and_stops_new_claims() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let socket = dir.path().join("socket");
    let executor = Arc::new(Blocking::default());
    let stop = Arc::new(AtomicBool::new(false));
    let server = fixture_server(
        &db,
        &socket,
        executor.clone(),
        stop.clone(),
        Config {
            poll_interval: Duration::from_millis(10),
            ..Config::default()
        },
    );
    let mut a = Session::new(&socket);
    let accepted = a.call(
        "start_collection",
        json!({"request_id":"inflight","symbols":["sh.600000","sz.000001"],"periods":["day"]}),
        false,
    );
    let id = accepted["task_id"].as_str().unwrap();
    entered(&executor);
    drop(a);
    let tasks = SqliteTasks::new(&db);
    assert_eq!(
        tasks.read_task(id).unwrap().unwrap().task.status,
        Status::Running
    );
    let mut b = Session::new(&socket);
    assert_eq!(
        b.call("get_task", json!({"task_id":id}), false)["status"],
        "running"
    );
    assert_eq!(
        b.call("run_worker_once", json!({}), true)["code"],
        "worker_busy"
    );
    assert_eq!(
        b.call("cancel_task", json!({"task_id":id}), false)["status"],
        "cancel_requested"
    );
    let queued = b.call(
        "start_collection",
        json!({"request_id":"still-pending","symbols":["sz.000002"],"periods":["day"]}),
        false,
    );
    // Stop new turns while the real SQLite item is still running. Release only
    // after both the explicit cancellation and service stop have been requested.
    stop.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(30));
    executor.release.store(true, Ordering::SeqCst);
    let completed = wait_terminal(&db, id);
    assert_eq!(completed.task.status, Status::Cancelled);
    assert!(completed.claims.is_empty());
    drop(b);
    assert!(server.join().unwrap().is_ok());
    let queued = tasks
        .read_task(queued["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(queued.task.status, Status::Pending);
    assert!(!queued.claims.is_empty());
    assert_eq!(executor.count.load(Ordering::SeqCst), 1);
}
#[test]
fn shutdown_child() {
    let Ok(db) = std::env::var("X_DATA_MCP_TEST_RUNTIME_SHUTDOWN_DB") else {
        return;
    };
    let db = PathBuf::from(db);
    let socket = db.with_file_name("child.sock");
    let flag = db.with_file_name("stop-test-child");
    let stop = Arc::new(AtomicBool::new(false));
    let monitor = stop.clone();
    std::thread::spawn(move || {
        while !flag.exists() {
            std::thread::sleep(Duration::from_millis(5));
        }
        monitor.store(true, Ordering::SeqCst);
    });
    let server = fixture_server(
        &db,
        &socket,
        Arc::new(Blocking::default()),
        stop,
        Config {
            poll_interval: Duration::from_millis(10),
            stop_budget: Duration::from_millis(100),
            ..Config::default()
        },
    );
    let failure = server.join().unwrap().unwrap_err();
    assert!(failure.to_string().contains("shutdown budget expired"));
    // Real process exit kills the in-flight fixture executor; it does not run a
    // made-up FinishCancellation or alter any persistent task completion.
    std::process::exit(79);
}
#[test]
fn real_shutdown_budget_exit_retains_running_task_and_occupancy() {
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let socket = dir.path().join("child.sock");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lifecycle::shutdown_child", "--nocapture"])
        .env("X_DATA_MCP_TEST_RUNTIME_SHUTDOWN_DB", &db)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while x_data_mcp::local_runtime::connect(&socket).is_err() {
        assert!(child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut session = Session::new(&socket);
    let accepted = session.call(
        "start_collection",
        json!({"request_id":"timeout","symbols":["sh.600000"],"periods":["day"]}),
        false,
    );
    let id = accepted["task_id"].as_str().unwrap();
    let tasks = SqliteTasks::new(&db);
    let before = loop {
        let s = tasks.read_task(id).unwrap().unwrap();
        if s.items[0].stage == x_data_mcp::task_control::Stage::Running {
            break s;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    fs::write(dir.path().join("stop-test-child"), b"stop").unwrap();
    drop(session);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(79));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(tasks.read_task(id).unwrap().unwrap(), before);
    assert!(!before.claims.is_empty());
    let mut real = Service::start(&db, &socket, 10);
    let mut session = Session::new(&socket);
    assert_eq!(
        session.call("get_task", json!({"task_id":id}), false)["status"],
        "needs_verification"
    );
    drop(session);
    real.stop();
}
#[test]
fn ipc_frame_and_client_limits_close_only_the_session_without_database_writes() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let db = database(dir.path());
    let socket = dir.path().join("socket");
    let stop = Arc::new(AtomicBool::new(false));
    let server = fixture_server(
        &db,
        &socket,
        Arc::new(Blocking::default()),
        stop.clone(),
        Config {
            max_clients: 1,
            max_frame_bytes: 128,
            io_timeout: Duration::from_millis(500),
            ..Config::default()
        },
    );
    let mut first = Session::new(&socket);
    assert!(first.request("initialize", json!({}))["result"].is_object());
    let mut second = x_data_mcp::local_runtime::connect(&socket).unwrap();
    second
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut byte = [0];
    assert_eq!(second.read(&mut byte).unwrap(), 0);
    first.stream.write_all(&[b'x'; 129]).unwrap();
    assert_eq!(first.reader.read(&mut byte).unwrap(), 0);
    drop(first);
    stop.store(true, Ordering::SeqCst);
    assert!(server.join().unwrap().is_ok());
    let c = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(
        c.query_row("select count(*) from requests", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
