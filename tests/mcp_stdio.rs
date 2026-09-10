use std::io::{Read, Write};
use std::process::{Command, Stdio};

#[test]
fn stdio_initialize_then_tools_list_exposes_exactly_five_tools() {
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
            "get_task",
            "retry_task",
            "run_worker_once",
            "start_collection",
        ]
    );
}
