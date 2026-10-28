//! v8 phase 2 PR-2 (D-56, XDM-73 §2.2(c)): maint and ops share one write
//! boundary. One process holds the only `DatabaseLease` and one compact
//! `WriteCoordinator`, so every session of every entry commits through the same
//! writer under the same lease; the single worker turn is serialized by the one
//! shared `WorkerGate`, and only the ops entry runs the background worker.
//!
//! The controlled executor stands in for the range/collection contracts, so the
//! task-registry write path is exercised without a data source or a generation.
//! It deliberately overlaps nothing: each turn writes at most one item and then
//! returns, exactly like the production worker.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use x_data_mcp::{
    data_supply::{
        Access,
        mcp::DataApi,
        registry::{self, EntryKind},
    },
    local_runtime::{Config, serve_many},
    mcp::ApiError,
    sqlite_market::backend::MarketBackend,
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
            evidence: None,
        }
    }
}
fn error(code: &str) -> ApiError {
    ApiError {
        code: code.into(),
        message: code.into(),
        scopes: None,
        outcome_certainty: None,
        details: None,
    }
}
/// A session handler for one deployed entry: real registry dispatch, plus the
/// controlled single turn behind the process-wide `WorkerGate`.
struct Boundary {
    entry: EntryKind,
    api: DataApi,
    gate: Arc<WorkerGate>,
    executor: Arc<Blocking>,
}
impl Boundary {
    fn turn(&self) -> Result<Value, ApiError> {
        let _permit = self.gate.acquire().map_err(error)?;
        let task = self
            .api
            .legacy
            .control
            .run_once(self.executor.as_ref())
            .map_err(|_| error("fixture_execution_error"))?;
        Ok(json!({
            "idle": task.is_none(),
            "task_id": task.as_ref().map(|t| &t.task.task_id),
            "status": task.as_ref().map(|t| t.visible_status(&self.api.legacy.control.run_id)),
        }))
    }
}
impl ToolHandler for Boundary {
    fn tools(&self) -> Value {
        self.api.tools()
    }
    fn call(&self, name: &str, args: Value) -> Result<Value, ApiError> {
        // Only an entry that actually serves `run_worker_once` reaches the
        // shared gate; every other entry keeps its own surface answer.
        if name == "run_worker_once" && args == json!({}) && self.entry.allows(name) {
            return self.turn();
        }
        self.api.call(name, args)
    }
    fn worker_once(&self) -> Result<Value, ApiError> {
        self.turn()
    }
}
struct BoundaryService {
    dir: PathBuf,
    /// Coordinator-bound task handle: the same writer the sessions use, so a
    /// test can read state without opening a second writer.
    tasks: SqliteTasks,
    executor: Arc<Blocking>,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}
impl BoundaryService {
    fn start(dir: &Path, entries: &[(&str, EntryKind)], poll: Duration) -> Self {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        let db = dir.join("boundary.sqlite");
        let tasks = SqliteTasks::new(&db);
        tasks.initialize_compact().unwrap();
        fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
        let lease = Arc::new(DatabaseLease::acquire(&db).unwrap());
        let backend = MarketBackend::open_shared(&SqliteTasks::new(&db), lease.clone()).unwrap();
        assert!(matches!(backend, MarketBackend::Compact(_)));
        let mut endpoints = Vec::new();
        for (name, entry) in entries {
            endpoints.push((Endpoint::bind(&dir.join(name), &lease).unwrap(), *entry));
        }
        // Mirror the service wiring: only a process with an ops listener runs
        // the background worker (v8 §3.4).
        let has_ops = entries.iter().any(|(_, entry)| *entry == EntryKind::Ops);
        let tasks = backend.tasks();
        let gate = Arc::new(WorkerGate::default());
        let stop = Arc::new(AtomicBool::new(false));
        let executor = Arc::new(Blocking::default());
        let factory = {
            let gate = gate.clone();
            let backend = backend.clone();
            let executor = executor.clone();
            move |entry: EntryKind| -> std::io::Result<Boundary> {
                let api = DataApi::new_with_backend(
                    backend.tasks(),
                    backend.clone(),
                    "boundary-run".into(),
                    Access::local(),
                    None,
                )
                .map_err(std::io::Error::other)?
                .with_entry(entry)
                .with_worker_gate(gate.clone());
                Ok(Boundary {
                    entry,
                    api,
                    gate: gate.clone(),
                    executor: executor.clone(),
                })
            }
        };
        // Every listener shares the lease, backend and gate; the background
        // worker is started once and always serves the ops entry.
        let server = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                serve_many(
                    endpoints,
                    EntryKind::Ops,
                    lease,
                    factory,
                    gate,
                    stop,
                    Config {
                        poll_interval: poll,
                        run_worker: has_ops,
                        ..Config::default()
                    },
                )
            })
        };
        let service = Self {
            dir: dir.to_path_buf(),
            tasks,
            executor,
            stop,
            server: Some(server),
        };
        for (name, _) in entries {
            let socket = service.socket(name);
            let deadline = Instant::now() + Duration::from_secs(15);
            while x_data_mcp::local_runtime::connect(&socket).is_err() {
                assert!(Instant::now() < deadline, "listener readiness deadline");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        service
    }
    fn socket(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
    fn session(&self, name: &str) -> Session {
        Session::new(&self.socket(name))
    }
    /// Lets a subsequent explicit turn finish immediately instead of blocking
    /// on the fixture barrier.
    fn release(&self) {
        self.executor.release.store(true, Ordering::SeqCst);
    }
    fn status(&self, id: &str) -> Status {
        self.tasks.read_task(id).unwrap().unwrap().task.status
    }
    fn entered(&self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.executor.entered.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "worker did not enter the item");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn finish(&mut self) {
        self.executor.release.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            server.join().unwrap().unwrap();
        }
    }
}
impl Drop for BoundaryService {
    fn drop(&mut self) {
        self.executor.release.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn tool_names(session: &mut Session) -> Vec<String> {
    let value = session.request("tools/list", json!({}));
    value["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}
fn collection(request_id: &str, code: &str) -> Value {
    json!({
        "request_id": request_id,
        "dataset": "market.daily",
        "contract_version": "v1",
        "selection": {"kind": "explicit", "securities": [{"exchange": "sh", "code": code}]},
    })
}
/// Polls one task through the ops socket until it is terminal.
fn wait_terminal(ops: &mut Session, id: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let view = ops.call(
            "get_task",
            json!({"contract_version":"v1","task_id":id}),
            false,
        );
        let status = view["status"].as_str().unwrap().to_string();
        if matches!(
            status.as_str(),
            "succeeded" | "partially_failed" | "failed" | "cancelled" | "interrupted"
        ) {
            return status;
        }
        assert!(Instant::now() < deadline, "task not terminal: {view}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Retries the explicit turn until the fast-ticking ops worker is not holding
/// the gate. A `worker_busy` here only means the moment, never lost work.
fn wait_idle(service: &BoundaryService) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut maint = service.session("maint.sock");
        let raw = maint.request(
            "tools/call",
            json!({"name":"run_worker_once","arguments":{}}),
        );
        let value = raw["result"]["structuredContent"].clone();
        if raw["result"]["isError"] == json!(false) {
            return value;
        }
        assert_eq!(value["code"], "worker_busy", "{value}");
        assert!(Instant::now() < deadline, "worker never turned idle");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn maint_keeps_its_nine_tools_and_the_operator_only_flags() {
    let dir = tempfile::tempdir().unwrap();
    let mut service = BoundaryService::start(
        dir.path(),
        &[
            ("research.sock", EntryKind::Research),
            ("maint.sock", EntryKind::Maint),
        ],
        Duration::from_secs(60),
    );
    // The nine maint tools are unchanged: the write boundary moved to the
    // process, it did not remove the operator surface.
    let mut maint = service.session("maint.sock");
    let expected: Vec<String> = registry::MAINT_TOOLS
        .iter()
        .map(|n| n.to_string())
        .collect();
    assert_eq!(tool_names(&mut maint), expected);
    assert_eq!(registry::MAINT_TOOLS.len(), 9);
    for name in [
        "run_worker_once",
        "inspect_verification_task",
        "backfill_data",
        "cancel_task",
    ] {
        let view = maint.call("get_data_capabilities", json!({"operation":name}), false);
        let item = &view["items"][0];
        assert_eq!(item["tool_name"], name);
        assert_eq!(item["entry"], "maint");
        assert_eq!(
            item["operator_only"],
            json!(name == "run_worker_once" || name == "inspect_verification_task"),
            "{name} operator_only"
        );
    }
    // The write tools stay off the tunnel-exposed research entry.
    let mut research = service.session("research.sock");
    assert_eq!(tool_names(&mut research).len(), 5);
    assert_eq!(
        research.call("backfill_data", collection("r", "600000"), true)["code"],
        "unknown_tool"
    );
    service.finish();
}

#[test]
fn maint_run_worker_once_returns_worker_busy_while_the_ops_worker_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mut service = BoundaryService::start(
        dir.path(),
        &[
            ("mcp.sock", EntryKind::Ops),
            ("maint.sock", EntryKind::Maint),
        ],
        Duration::from_millis(10),
    );
    let mut ops = service.session("mcp.sock");
    let mut maint = service.session("maint.sock");
    let accepted = ops.call("start_collection", collection("inflight", "600000"), false);
    let id = accepted["task_id"].as_str().unwrap().to_string();
    service.entered();
    // One gate for both entries: neither socket can start a second turn while
    // the ops worker owns it.
    assert_eq!(
        maint.call("run_worker_once", json!({}), true)["code"],
        "worker_busy"
    );
    assert_eq!(
        ops.call("run_worker_once", json!({}), true)["code"],
        "worker_busy"
    );
    // A maint *write* is not rejected by the gate: it queues on the single
    // write coordinator and keeps its normal semantics.
    assert_eq!(
        maint.call(
            "cancel_task",
            json!({"contract_version":"v1","task_id":id}),
            false
        )["status"],
        "cancel_requested"
    );
    service.executor.release.store(true, Ordering::SeqCst);
    let status = wait_terminal(&mut ops, &id);
    assert!(status == "cancelled" || status == "failed", "{status}");
    assert_eq!(
        service.executor.count.load(Ordering::SeqCst),
        1,
        "the item executed exactly once"
    );
    // The same shared task store: maint sees no further work.
    let idle = wait_idle(&service);
    assert_eq!(idle["idle"], true, "{idle}");
    assert_eq!(service.executor.count.load(Ordering::SeqCst), 1);
    service.finish();
}

#[test]
fn concurrent_maint_and_ops_writes_share_one_registry_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let mut service = BoundaryService::start(
        dir.path(),
        &[
            ("mcp.sock", EntryKind::Ops),
            ("maint.sock", EntryKind::Maint),
        ],
        Duration::from_secs(60),
    );
    // The same request_id accepted at the same time from both entries: the
    // replay binding converges on one task, and the single write coordinator
    // rejects nothing on the way.
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let callers: Vec<_> = ["mcp.sock", "maint.sock"]
        .into_iter()
        .map(|socket| {
            let barrier = barrier.clone();
            let path = service.socket(socket);
            std::thread::spawn(move || {
                let mut session = Session::new(&path);
                barrier.wait();
                session.call("start_collection", collection("shared", "600000"), false)
            })
        })
        .collect();
    let accepted: Vec<Value> = callers
        .into_iter()
        .map(|caller| caller.join().unwrap())
        .collect();
    assert_eq!(accepted[0]["task_id"], accepted[1]["task_id"]);
    let id = accepted[0]["task_id"].as_str().unwrap().to_string();
    let items = accepted[0]["item_ids"].clone();

    let mut ops = service.session("mcp.sock");
    let mut maint = service.session("maint.sock");
    assert_eq!(
        ops.call("list_tasks", json!({}), false)["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // One turn through maint's shared gate executes the single item once.
    service.release();
    let turn = maint.call("run_worker_once", json!({}), false);
    assert_eq!(turn["task_id"], id.as_str());
    assert_eq!(service.executor.count.load(Ordering::SeqCst), 1);
    wait_terminal(&mut ops, &id);

    // A maint write creates a linked task that ops reads back: one store, one
    // writer, no shadow registry per entry.
    let retried = maint.call(
        "retry_task",
        json!({
            "contract_version":"v1",
            "original_task_id":id,
            "new_request_id":"retry",
            "item_ids":items,
        }),
        false,
    );
    let retry_id = retried["task_id"].as_str().unwrap();
    assert_ne!(retry_id, id.as_str());
    assert_eq!(
        ops.call("list_tasks", json!({}), false)["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let turn = maint.call("run_worker_once", json!({}), false);
    assert_eq!(turn["task_id"], retry_id);
    assert_eq!(service.executor.count.load(Ordering::SeqCst), 2);
    service.finish();
}

/// Ops is the only entry that advances tasks on its own; a maint-only process
/// accepts the write and leaves the task pending until an explicit turn.
#[test]
fn a_maint_only_process_accepts_writes_but_never_runs_the_background_worker() {
    let dir = tempfile::tempdir().unwrap();
    let mut service = BoundaryService::start(
        dir.path(),
        &[("maint.sock", EntryKind::Maint)],
        Duration::from_millis(10),
    );
    let mut maint = service.session("maint.sock");
    // `verify_data_gap` only needs a target: acceptance writes a task without
    // reading any published data.
    let accepted = maint.call(
        "verify_data_gap",
        json!({
            "contract_version":"v1",
            "request_id":"idle",
            "target":{"symbol":"sh.600006","period":"day","start_date":"2025-01-01","end_date":"2025-01-10"},
        }),
        false,
    );
    let id = accepted["task_id"].as_str().unwrap();
    assert_eq!(accepted["status"], "pending");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(service.executor.count.load(Ordering::SeqCst), 0);
    assert_eq!(service.status(id), Status::Pending);
    // The explicit single turn still works from this entry.
    service.release();
    assert_eq!(
        maint.call("run_worker_once", json!({}), false)["task_id"],
        id
    );
    assert!(service.status(id).terminal());
    assert_eq!(service.executor.count.load(Ordering::SeqCst), 1);
    service.finish();
}
