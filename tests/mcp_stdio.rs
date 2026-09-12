use std::io::{Read, Write};
use std::process::{Command, Stdio};

#[test]
fn stdio_initialize_then_tools_list_exposes_exactly_eight_tools() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_mcp_server"))
        .env("X_DATA_MCP_STORE", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
        )
        .unwrap();
    }
    drop(child.stdin.take());

    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert!(child.wait().unwrap().success());
    let responses = output
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(
        responses[0]["result"]["capabilities"]["tools"],
        serde_json::json!({})
    );
    assert_eq!(responses[1]["id"], 2);

    let mut names = responses[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![
            "cancel_task",
            "get_market_data",
            "get_request",
            "get_task",
            "list_market_data_versions",
            "retry_task",
            "run_worker_once",
            "start_collection",
        ]
    );
}

/// Issue #55 AC-01/04: the actual binary ignores retired source configuration.
#[test]
fn production_stdio_fails_closed_without_python_and_does_not_publish() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_mcp_server"))
        .env("X_DATA_MCP_STORE", dir.path())
        .env("X_DATA_MCP_TDX_PYTHON", "/nonexistent/retired-python")
        .env("X_DATA_MCP_TDX_LIVE", "1")
        .env("PATH", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for request in [
            json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{
                "name":"start_collection", "arguments":{"request_id":"blocked", "symbols":["000001"], "periods":["day","year"]}
            }}),
            json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"run_worker_once", "arguments":{}}}),
        ] {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        responses[0]["result"]["structuredContent"]["status"],
        "pending"
    );
    assert_eq!(
        responses[1]["result"]["structuredContent"]["status"],
        "failed"
    );
    let task_id = responses[0]["result"]["structuredContent"]["task_id"]
        .as_str()
        .unwrap();
    let record = x_data_mcp::task::TaskStore::new(dir.path())
        .get_execution(task_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.scope_results.len(), 2);
    assert!(record.scope_results.iter().all(|result| !result.success));
    assert!(
        record.scope_results[0]
            .error
            .as_ref()
            .unwrap()
            .contains("FILE_SET_CONSISTENCY_NOT_VERIFIED")
    );
    assert!(
        record.scope_results[1]
            .error
            .as_ref()
            .unwrap()
            .contains("PUBLISHED_QFQ_DAILY_REQUIRED")
    );
    assert!(!dir.path().join("effective").exists());
    assert!(!dir.path().join("000001.day.qfq.json").exists());
    assert!(!dir.path().join("000001.year.qfq.json").exists());
}

#[test]
fn actual_stdio_queries_are_readonly_and_unknown_arguments_reject() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("absent");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mcp_server"))
        .env("X_DATA_MCP_STORE", &store)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let calls = [
        (
            "get_market_data",
            json!({"exchange":"sh","code":"600006","period":"day","start_date":"2027-01-01","end_date":"2027-12-31"}),
        ),
        (
            "list_market_data_versions",
            json!({"exchange":"sh","code":"600006","period":"day"}),
        ),
        ("get_request", json!({"request_id":"unbound"})),
        ("run_worker_once", json!({"unexpected":true})),
        (
            "start_collection",
            json!({"request_id":"bad","symbols":["sh.600006"],"periods":["day"],"unexpected":true}),
        ),
    ];
    {
        let mut stdin = child.stdin.take().unwrap();
        for (i, (name, args)) in calls.into_iter().enumerate() {
            writeln!(stdin,"{}",json!({"jsonrpc":"2.0","id":i,"method":"tools/call","params":{"name":name,"arguments":args}})).unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let responses: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 5);
    assert_eq!(
        responses[0]["result"]["structuredContent"]["records"],
        json!([])
    );
    assert_eq!(
        responses[1]["result"]["structuredContent"]["records"],
        json!([])
    );
    assert_eq!(responses[2]["result"]["structuredContent"]["bound"], false);
    for response in &responses[3..] {
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["structuredContent"]["code"],
            "invalid_arguments"
        );
    }
    assert!(!store.exists());
}
