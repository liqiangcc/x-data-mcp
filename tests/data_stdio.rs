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
    assert_eq!(tools.len(), 12);
    let schema =
        |name: &str, field: &str| tools.iter().find(|t| t["name"] == name).unwrap()[field].clone();
    for entry in x_data_mcp::data_supply::registry::entries() {
        jsonschema::validator_for(&entry.input)
            .unwrap()
            .validate(&entry.example)
            .unwrap();
        if matches!(
            entry.handler,
            x_data_mcp::data_supply::registry::Handler::Progress
                | x_data_mcp::data_supply::registry::Handler::Collection
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
    s.call("run_worker_once", json!({}), false);
    let p = s.call("get_task_progress", json!({"task_id":task}), false);
    assert_eq!(p["failed"], 1);
    let retry = json!({"contract_version":"v1","original_task_id":task,"new_request_id":"retry","item_ids":accepted["item_ids"]});
    jsonschema::validator_for(&schema("retry_task", "inputSchema"))
        .unwrap()
        .validate(&retry)
        .unwrap();
    let a = s.call("retry_task", retry, false);
    s.call("cancel_task", json!({"task_id":a["task_id"]}), false);
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
