//! Real CLI/stdio adapters. No fake ToolHandler or mocked SQLite connection.
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
};

struct Session {
    child: Child,
    stdout: BufReader<ChildStdout>,
    sequence: u64,
}
impl Session {
    fn new(sqlite: bool, path: &Path) -> Self {
        let mut command = Command::new(if sqlite {
            env!("CARGO_BIN_EXE_mcp_sqlite_dev")
        } else {
            env!("CARGO_BIN_EXE_mcp_server")
        });
        if sqlite {
            command.arg(path);
        } else {
            command.env("X_DATA_MCP_STORE", path);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdout,
            sequence: 0,
        }
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.sequence += 1;
        let request = json!({"jsonrpc":"2.0","id":self.sequence,"method":method,"params":params});
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        assert!(self.stdout.read_line(&mut line).unwrap() > 0);
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], self.sequence);
        response
    }
    fn call(&mut self, name: &str, args: Value, error: bool) -> Value {
        let response = self.request("tools/call", json!({"name":name,"arguments":args}));
        assert_eq!(response["result"]["isError"], error, "{response}");
        let result = &response["result"];
        assert_eq!(result["content"][0]["type"], "text");
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"]);
        text
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}
fn cli(operation: &str, path: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_task_sqlite"))
        .arg(operation)
        .arg(path)
        .output()
        .unwrap()
}
fn start(s: &mut Session, id: &str) -> Value {
    s.call(
        "start_collection",
        json!({"request_id":id,"symbols":["a"],"periods":["day","year"]}),
        false,
    )
}

#[test]
fn sqlite_cli_init_upgrade_no_clobber_and_stdio_read_never_initializes_or_upgrades() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.sqlite");
    {
        let mut session = Session::new(true, &path);
        assert_eq!(
            session.call("get_request", json!({"request_id":"absent"}), true)["code"],
            "storage_error"
        );
        assert!(!path.exists());
    }
    assert!(!cli("upgrade", &path).status.success());
    assert!(!path.exists());
    assert!(cli("init", &path).status.success());
    let bytes = std::fs::read(&path).unwrap();
    assert!(!cli("init", &path).status.success());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(cli("upgrade", &path).status.success());
    let v1 = dir.path().join("v1.sqlite");
    let c = Connection::open(&v1).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    let sql = include_str!("../migrations/tasks/001.sql");
    c.execute_batch(sql).unwrap();
    c.execute(
        "INSERT INTO schema_migrations VALUES(1,?1,'test')",
        [x_data_mcp::hash::sha256(sql.as_bytes())],
    )
    .unwrap();
    c.pragma_update(None, "user_version", 1).unwrap();
    {
        let mut session = Session::new(true, &v1);
        assert_eq!(
            session.call("get_request", json!({"request_id":"r"}), true)["code"],
            "storage_error"
        );
        let version: u32 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }
    assert!(cli("upgrade", &v1).status.success());
    let version: u32 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, x_data_mcp::sqlite_tasks::SCHEMA_VERSION);
    c.pragma_update(None, "user_version", 999).unwrap();
    assert!(!cli("upgrade", &v1).status.success());
    assert!(!cli("wrong-operation", &path).status.success());
}
#[test]
fn sqlite_stdio_six_tools_and_complete_task_control_flow() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.sqlite");
    assert!(cli("init", &path).status.success());
    let mut session = Session::new(true, &path);
    assert_eq!(
        session.request("initialize", json!({}))["result"]["capabilities"]["tools"],
        json!({})
    );
    let listing = session.request("tools/list", json!({}));
    let mut names = listing["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![
            "cancel_task",
            "get_request",
            "get_task",
            "retry_task",
            "run_worker_once",
            "start_collection"
        ]
    );
    let accepted = start(&mut session, "original");
    let task = accepted["task_id"].clone();
    assert_eq!(accepted["status"], "pending");
    assert_eq!(accepted["frozen_range"].as_array().unwrap().len(), 2);
    assert_eq!(
        session.call("get_task", json!({"task_id":task}), false)["status"],
        "pending"
    );
    assert_eq!(
        session.call("get_request", json!({"request_id":"original"}), false)["task"]["task_id"],
        task
    );
    assert_eq!(
        session.call("cancel_task", json!({"task_id":task}), false)["status"],
        "cancel_requested"
    );
    assert_eq!(
        session.call("cancel_task", json!({"task_id":task}), false)["changed"],
        false
    );
    assert_eq!(
        session.call("run_worker_once", json!({}), false)["status"],
        "cancelled"
    );
    let retry = json!({"original_task_id":task,"new_request_id":"retry","selected_scopes":[{"symbol":"a","period":"day"}]});
    assert_eq!(
        session.call("retry_task", retry.clone(), false)["status"],
        "pending"
    );
    assert_eq!(
        session.call("run_worker_once", json!({}), false)["status"],
        "failed"
    );
    let replay = session.call("retry_task", retry, false);
    assert_eq!(replay["status"], "failed");
    assert_eq!(replay["reused"], true);
    assert_eq!(
        session.call("get_request", json!({"request_id":"retry"}), false)["task"]["status"],
        "failed"
    );
    assert_eq!(
        session.call("get_task", json!({"task_id":task}), false)["status"],
        "cancelled"
    );
    assert_eq!(
        session.call("run_worker_once", json!({}), false)["idle"],
        true
    );
    assert_eq!(
        session.call("get_request", json!({"request_id":"unbound"}), false)["bound"],
        false
    );
    for tool in names {
        assert_eq!(
            session.call(tool, json!({"extra":true}), true)["code"],
            "invalid_arguments"
        );
    }
    assert_eq!(
        session.call("get_task", json!({"task_id":"missing"}), true)["code"],
        "task_not_found"
    );
    assert_eq!(
        session.call(
            "start_collection",
            json!({"request_id":"bad","symbols":["a"],"periods":["week"]}),
            true
        )["code"],
        "invalid_period"
    );
}
#[test]
fn real_legacy_and_sqlite_stdio_retry_errors_have_identical_code_scopes_and_shape() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.sqlite");
    assert!(cli("init", &path).status.success());
    let mut old = Session::new(false, &dir.path().join("legacy"));
    let mut new = Session::new(true, &path);
    let old_id = start(&mut old, "original")["task_id"].clone();
    let new_id = start(&mut new, "original")["task_id"].clone();
    for (request, selection, code) in [
        ("original", json!([]), "retry_request_id_conflict"),
        (
            "original",
            json!([{"symbol":"a","period":"day"}]),
            "retry_request_id_conflict",
        ),
        ("retry", json!([]), "empty_selected_scopes"),
        (
            "",
            json!([{"symbol":"a","period":"day"}]),
            "empty_request_id",
        ),
        (
            "retry",
            json!([{"symbol":"z","period":"year"},{"symbol":"a","period":"day"},{"symbol":"b","period":"day"},{"symbol":"z","period":"year"}]),
            "invalid_retry_selection",
        ),
    ] {
        let old_error = old.call(
            "retry_task",
            json!({"original_task_id":old_id,"new_request_id":request,"selected_scopes":selection}),
            true,
        );
        let new_error = new.call(
            "retry_task",
            json!({"original_task_id":new_id,"new_request_id":request,"selected_scopes":selection}),
            true,
        );
        assert_eq!(old_error["code"], code);
        assert_eq!(new_error, old_error);
        if code == "invalid_retry_selection" {
            assert_eq!(
                new_error["scopes"],
                json!([{"symbol":"b","period":"day"},{"symbol":"z","period":"year"}])
            );
        }
    }
}

#[test]
fn completed_scope_retry_errors_match_real_stdio_with_persisted_test_outcomes() {
    use x_data_mcp::{
        sqlite_tasks::SqliteTasks,
        task_control::{legacy::LegacyCollection, service::TaskControl, *},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.sqlite");
    let legacy = dir.path().join("legacy");
    assert!(cli("init", &path).status.success());
    let mut old = Session::new(false, &legacy);
    let mut new = Session::new(true, &path);
    let old_id = start(&mut old, "original")["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let new_id = start(&mut new, "original")["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Bounded persisted outcome fixture, not a source/publishing correctness claim.
    old.call("run_worker_once", json!({}), false);
    let old_store = x_data_mcp::task::TaskStore::new(&legacy);
    let mut execution = old_store.get_execution(&old_id).unwrap().unwrap();
    execution.status = x_data_mcp::task::TaskStatus::PartiallyFailed;
    let day = execution
        .scope_results
        .iter_mut()
        .find(|r| r.period == x_data_mcp::market_data::Period::Day)
        .unwrap();
    day.success = true;
    day.error = None;
    old_store.save_execution(execution).unwrap();
    struct Outcomes;
    impl Executor for Outcomes {
        fn execute(&self, _: &CommandKey, item: &ItemSpec) -> ItemOutcome {
            let success = item.target["period"] == "day";
            ItemOutcome {
                success,
                error: (!success).then(|| "fixture failure".into()),
            }
        }
    }
    let db = SqliteTasks::new(&path);
    let run = db.read_task(&new_id).unwrap().unwrap().task.accepted_run_id;
    TaskControl::new(db, run, vec![Box::new(LegacyCollection)])
        .unwrap()
        .run_once(&Outcomes)
        .unwrap();
    let selection = json!([{"symbol":"a","period":"year"},{"symbol":"a","period":"day"},{"symbol":"a","period":"day"}]);
    let a = old.call(
        "retry_task",
        json!({"original_task_id":old_id,"new_request_id":"retry","selected_scopes":selection}),
        true,
    );
    let b = new.call(
        "retry_task",
        json!({"original_task_id":new_id,"new_request_id":"retry","selected_scopes":selection}),
        true,
    );
    assert_eq!(a, b);
    assert_eq!(b["code"], "invalid_retry_selection");
    assert_eq!(b["scopes"], json!([{"symbol":"a","period":"day"}]));
}
