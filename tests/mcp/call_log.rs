//! XDM-167: every `tools/call` leaves exactly one identity-only audit line on
//! stderr. This drives the real stdio entry so the assertion covers the
//! production path (`handle_json` → `tools/call`), not just the formatter.

use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
};
use x_data_mcp::call_log::{MAX_LINE_BYTES, PREFIX};

struct Session {
    child: Child,
    stdout: BufReader<ChildStdout>,
    stderr: BufReader<ChildStderr>,
    sequence: u64,
}

impl Session {
    fn new(path: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcp_data"))
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The audit channel must stay separate from protocol output.
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = BufReader::new(child.stderr.take().unwrap());
        Self {
            child,
            stdout,
            stderr,
            sequence: 0,
        }
    }

    /// One `tools/call` round trip; returns the tool result and the raw audit
    /// line the call must have produced.
    fn call(&mut self, name: &str, args: Value) -> (Value, String) {
        self.sequence += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": self.sequence,
            "method": "tools/call",
            "params": {"name": name, "arguments": args},
        });
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        assert!(self.stdout.read_line(&mut line).unwrap() > 0);
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], self.sequence);
        let mut audit = String::new();
        assert!(self.stderr.read_line(&mut audit).unwrap() > 0);
        assert!(audit.starts_with(PREFIX), "not an audit line: {audit}");
        assert!(
            audit.trim_end().len() <= MAX_LINE_BYTES,
            "audit line over budget: {} bytes: {audit}",
            audit.trim_end().len()
        );
        (response["result"].clone(), audit)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

fn audit_line(raw: &str) -> Value {
    serde_json::from_str(&raw[PREFIX.len()..]).unwrap()
}

#[test]
fn successful_and_failed_calls_each_leave_one_identity_only_audit_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    x_data_mcp::sqlite_tasks::SqliteTasks::new(&path)
        .initialize()
        .unwrap();
    let mut session = Session::new(&path);

    // Success: all fixed fields, request identifier carried over.
    let (result, raw) = session.call(
        "get_request",
        json!({"contract_version": "v1", "request_id": "audit-ok"}),
    );
    assert_eq!(result["isError"], false);
    let audit = audit_line(&raw);
    assert_eq!(audit["entry"], "stdio");
    assert_eq!(audit["tool"], "get_request");
    assert_eq!(audit["request_id"], "audit-ok");
    assert_eq!(audit["outcome"], "ok");
    assert!(audit["elapsed_ms"].is_u64(), "{audit}");
    assert!(audit.get("task_id").is_none(), "{audit}");

    // Error: the audit outcome is the stable error code, and the securities
    // list in the arguments never reaches the log.
    let entities = ["sh.600000", "sz.000001"];
    let (result, raw) = session.call(
        "query_data",
        json!({
            "dataset": "market.unknown",
            "contract_version": "v1",
            "selection": {"kind": "batch", "entities": entities},
            "filter": {"kind": "daily", "start_date": "2025-01-01", "end_date": "2025-01-31"},
        }),
    );
    assert_eq!(result["isError"], true, "{result}");
    let code = result["structuredContent"]["code"].as_str().unwrap();
    assert_eq!(code, "unknown_dataset", "{result}");
    let audit = audit_line(&raw);
    assert_eq!(audit["entry"], "stdio");
    assert_eq!(audit["tool"], "query_data");
    assert_eq!(audit["outcome"], code);
    assert!(audit["elapsed_ms"].is_u64(), "{audit}");
    for sensitive in ["entities", "600000", "000001", "2025-01-01"] {
        assert!(!raw.contains(sensitive), "{sensitive} leaked: {raw}");
    }

    // A task-bound error still names the task, so follow-up can find the row.
    let (result, raw) = session.call(
        "get_task",
        json!({"contract_version": "v1", "task_id": "task-audit-missing"}),
    );
    assert_eq!(result["isError"], true, "{result}");
    let audit = audit_line(&raw);
    assert_eq!(audit["tool"], "get_task");
    assert_eq!(audit["task_id"], "task-audit-missing");
    assert_eq!(audit["outcome"], result["structuredContent"]["code"]);
}

#[test]
fn a_call_without_a_tool_name_is_still_audited() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    x_data_mcp::sqlite_tasks::SqliteTasks::new(&path)
        .initialize()
        .unwrap();
    let mut session = Session::new(&path);
    let input = session.child.stdin.as_mut().unwrap();
    writeln!(
        input,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {}})
    )
    .unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    assert!(session.stdout.read_line(&mut line).unwrap() > 0);
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["error"]["code"], -32602);
    let mut raw = String::new();
    assert!(session.stderr.read_line(&mut raw).unwrap() > 0);
    let audit = audit_line(&raw);
    assert_eq!(audit["tool"], "?");
    assert_eq!(audit["outcome"], "invalid_tool_call");
}
