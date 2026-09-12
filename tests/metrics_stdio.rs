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
#[path = "support/metrics.rs"]
mod fixture;
#[test]
fn compute_real_stdio_roundtrips_schema_and_fixed_version_reference() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let tasks = x_data_mcp::sqlite_tasks::SqliteTasks::new(&path);
    tasks.initialize().unwrap();
    let reference = fixture::publish(&x_data_mcp::sqlite_market::SqliteMarket::new(tasks), true);
    let mut s = Session::new(&path);
    s.request("initialize", json!({}));
    let tools = s.request("tools/list", json!({}));
    let tool = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "compute_metrics")
        .unwrap();
    let request = fixture::request(reference);
    jsonschema::validator_for(&tool["inputSchema"])
        .unwrap()
        .validate(&request)
        .unwrap();
    let response = s.call("compute_metrics", request.clone(), false);
    jsonschema::validator_for(&tool["outputSchema"])
        .unwrap()
        .validate(&response)
        .unwrap();
    assert_eq!(response["result"]["cumulative_change"], "-0.300000000000");
    assert_eq!(response["result"]["run_length_kind"], "lower_bound");
    assert_eq!(response, s.call("compute_metrics", request, false));
    let details = s.call(
        "get_data_capabilities",
        json!({"dataset":"market.yearly","operation":"compute_metrics"}),
        false,
    );
    assert_eq!(details["items"][0]["input_schema"], tool["inputSchema"]);
}
#[test]
fn missing_completion_is_an_explicit_mcp_failure_with_diagnostics() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let t = x_data_mcp::sqlite_tasks::SqliteTasks::new(&path);
    t.initialize().unwrap();
    let reference = fixture::publish(&x_data_mcp::sqlite_market::SqliteMarket::new(t), false);
    let mut s = Session::new(&path);
    let result = s.call("compute_metrics", fixture::request(reference), true);
    assert_eq!(result["code"], "not_computable");
    assert_eq!(result["details"]["result"]["reason"], "unknown_completion");
    assert_eq!(result["outcome_certainty"], "not_applicable");
}
