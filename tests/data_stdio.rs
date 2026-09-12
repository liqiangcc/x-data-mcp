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
    fn new(path: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mcp_data"));
        command.arg(path);
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
#[test]
fn real_data_stdio_discovery_queries_and_control_schema_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    x_data_mcp::sqlite_tasks::SqliteTasks::new(&path)
        .initialize()
        .unwrap();
    let mut s = Session::new(&path);
    assert!(s.request("initialize", json!({}))["result"]["serverInfo"].is_object());
    let listing = s.request("tools/list", json!({}));
    let tools = listing["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 15);
    let caps = s.call("get_data_capabilities", json!({}), false);
    let listed: std::collections::BTreeSet<_> =
        tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let discovered: std::collections::BTreeSet<_> = caps["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tool_name"].as_str().unwrap())
        .collect();
    assert_eq!(listed, discovered);
    let worker = s.call(
        "get_data_capabilities",
        json!({"operation":"run_worker_once"}),
        false,
    );
    assert_eq!(worker["items"].as_array().unwrap().len(), 1);
    assert!(
        worker["items"][0]["description"]
            .as_str()
            .unwrap()
            .contains("never downloads or publishes")
    );
    let worker_tool = tools
        .iter()
        .find(|t| t["name"] == "run_worker_once")
        .unwrap();
    assert_eq!(
        worker["items"][0]["input_schema"],
        worker_tool["inputSchema"]
    );
    assert_eq!(
        worker["items"][0]["output_schema"],
        worker_tool["outputSchema"]
    );
    assert_eq!(
        s.call("run_worker_once", json!({"extra":true}), true)["code"],
        "invalid_arguments"
    );
    let schema =
        |name: &str, field: &str| tools.iter().find(|t| t["name"] == name).unwrap()[field].clone();
    for entry in x_data_mcp::data_supply::registry::entries() {
        jsonschema::validator_for(&entry.input)
            .unwrap()
            .validate(&entry.example)
            .unwrap();
        if matches!(
            entry.handler,
            x_data_mcp::data_supply::registry::Handler::Compute
                | x_data_mcp::data_supply::registry::Handler::Progress
                | x_data_mcp::data_supply::registry::Handler::Collection
                | x_data_mcp::data_supply::registry::Handler::Range(_)
                | x_data_mcp::data_supply::registry::Handler::Retry
                | x_data_mcp::data_supply::registry::Handler::Task
                | x_data_mcp::data_supply::registry::Handler::Cancel
        ) {
            continue;
        }
        let response = s.call(entry.name, entry.example, false);
        jsonschema::validator_for(&entry.output)
            .unwrap()
            .validate(&response)
            .unwrap();
    }
    let input = json!({"request_id":"r","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600006"}]}});
    jsonschema::validator_for(&schema("start_collection", "inputSchema"))
        .unwrap()
        .validate(&input)
        .unwrap();
    let accepted = s.call("start_collection", input.clone(), false);
    let task = accepted["task_id"].clone();
    let progress = s.call("get_task_progress", json!({"task_id":task}), false);
    jsonschema::validator_for(&schema("get_task_progress", "outputSchema"))
        .unwrap()
        .validate(&progress)
        .unwrap();
    assert_eq!(progress["total"], 1);
    let request = s.call("get_request", json!({"request_id":"r"}), false);
    assert_eq!(request["bound"], true);
    let mut extra = input.clone();
    extra["symbols"] = json!(["sh.600006"]);
    assert!(
        jsonschema::validator_for(&schema("start_collection", "inputSchema"))
            .unwrap()
            .validate(&extra)
            .is_err()
    );
    assert_eq!(
        s.call("start_collection", extra, true)["code"],
        "invalid_arguments"
    );
    let executed = s.call("run_worker_once", json!({}), false);
    assert_eq!(executed["idle"], false);
    assert_eq!(executed["status"], "failed");
    jsonschema::validator_for(&worker["items"][0]["output_schema"])
        .unwrap()
        .validate(&executed)
        .unwrap();
    let p = s.call("get_task_progress", json!({"task_id":task}), false);
    assert_eq!(p["failed"], 1);
    let retry = json!({"contract_version":"v1","original_task_id":task,"new_request_id":"retry","item_ids":accepted["item_ids"]});
    jsonschema::validator_for(&schema("retry_task", "inputSchema"))
        .unwrap()
        .validate(&retry)
        .unwrap();
    let a = s.call("retry_task", retry, false);
    s.call("cancel_task", json!({"task_id":a["task_id"]}), false);
    let view = s.call(
        "get_task",
        json!({"contract_version":"v1","task_id":a["task_id"]}),
        false,
    );
    let request = s.call(
        "get_request",
        json!({"contract_version":"v1","request_id":"retry"}),
        false,
    );
    assert_eq!(request["task"]["task_id"], a["task_id"]);
    let rev = view["state_revision"].as_u64().unwrap();
    assert!(rev > 0);
    assert_eq!(
        s.call(
            "cancel_task",
            json!({"contract_version":"v1","task_id":a["task_id"],"expected_revision":rev-1}),
            true
        )["code"],
        "precondition_conflict"
    );
    s.call(
        "cancel_task",
        json!({"contract_version":"v1","task_id":a["task_id"],"expected_revision":rev}),
        false,
    );
    let detail = s.call(
        "get_data_capabilities",
        json!({"dataset":"market.daily","operation":"query_data"}),
        false,
    );
    assert_eq!(
        detail["items"][0]["input_schema"],
        schema("query_data", "inputSchema")
    );
    assert_eq!(detail["production_ready"], false);
}
#[test]
fn real_data_stdio_read_missing_database_does_not_initialize() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("missing");
    let mut s = Session::new(&p);
    s.call("list_tasks", json!({}), true);
    assert!(!p.exists());
}

#[path = "support/market.rs"]
mod range_fixture;
#[test]
fn real_range_stdio_acceptance_replay_conflicts_and_blocked_execution() {
    for operation in ["update_data", "backfill_data"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let tasks = x_data_mcp::sqlite_tasks::SqliteTasks::new(&path);
        tasks.initialize().unwrap();
        let market = x_data_mcp::sqlite_market::SqliteMarket::new(tasks);
        range_fixture::published_fixture(&market, &range_fixture::samples()[0]["record"]);
        let mut s = Session::new(&path);
        s.request("initialize", json!({}));
        let capabilities = s.call(
            "get_data_capabilities",
            json!({"operation":operation}),
            false,
        );
        let cap = &capabilities["items"][0];
        assert_eq!(capabilities["items"].as_array().unwrap().len(), 1);
        assert_eq!(cap["dataset"], "market.daily");
        assert_eq!(cap["readiness"], "blocked");
        assert!(
            cap["description"]
                .as_str()
                .unwrap()
                .contains("Explicit market.daily")
        );
        let mut input = json!({"request_id":"range-write","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"legacy-daily","start_date":"2025-01-02","end_date":"2025-01-02"}]});
        jsonschema::validator_for(&cap["input_schema"])
            .unwrap()
            .validate(&input)
            .unwrap();
        let accepted = s.call(operation, input.clone(), false);
        jsonschema::validator_for(&cap["output_schema"])
            .unwrap()
            .validate(&accepted)
            .unwrap();
        assert_eq!(accepted["readiness"], "blocked");
        assert_eq!(accepted["contract_version"], "explicit-range-task-v1");
        let duplicate = input["items"][0].clone();
        input["items"].as_array_mut().unwrap().push(duplicate);
        let replay = s.call(operation, input.clone(), false);
        assert_eq!(replay["task_id"], accepted["task_id"]);
        assert_eq!(replay["reused"], true);
        let other_operation = if operation == "update_data" {
            "backfill_data"
        } else {
            "update_data"
        };
        assert_eq!(
            s.call(other_operation, input.clone(), true)["code"],
            "request_conflict"
        );
        input["request_id"] = json!("range-competing");
        assert_eq!(
            s.call(operation, input.clone(), true)["code"],
            "range_conflict"
        );
        assert_eq!(
            s.call(
                "get_request",
                json!({"contract_version":"v1","request_id":"range-competing"}),
                false
            )["bound"],
            false
        );
        input["request_id"] = json!("bad-field");
        input["items"][0]["extra"] = json!(true);
        assert_eq!(s.call(operation, input, true)["code"], "invalid_arguments");
        assert_eq!(
            s.call(
                "get_request",
                json!({"contract_version":"v1","request_id":"range-write"}),
                false
            )["task"]["task_id"],
            accepted["task_id"]
        );
        let worker = s.call("run_worker_once", json!({}), false);
        assert_eq!(worker["task_id"], accepted["task_id"]);
        let progress = s.call(
            "get_task_progress",
            json!({"task_id":accepted["task_id"]}),
            false,
        );
        assert_eq!(progress["failed"], 1);
        assert!(
            progress["unfinished"][0]["error"]
                .as_str()
                .unwrap()
                .contains("SOURCE_NOT_VERIFIED")
        );
        let retried=s.call("retry_task",json!({"contract_version":"v1","original_task_id":accepted["task_id"],"new_request_id":"range-retry","item_ids":accepted["item_ids"]}),false);
        let view = s.call(
            "get_task",
            json!({"contract_version":"v1","task_id":retried["task_id"]}),
            false,
        );
        s.call("cancel_task",json!({"contract_version":"v1","task_id":retried["task_id"],"expected_revision":view["state_revision"]}),false);
        s.call("run_worker_once", json!({}), false);
        let versions: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM data_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(versions, 1);
        assert!(
            s.call(
                "get_data_capabilities",
                json!({"operation":operation,"dataset":"market.yearly"}),
                false
            )["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
