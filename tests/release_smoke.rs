#[test]
fn real_binary_release_smoke_exercises_the_explicit_fixture_flow() {
    let dir = tempfile::tempdir().unwrap();
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_mcp_data"))
        .parent()
        .unwrap();
    let report = dir.path().join("report.json");
    let result = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/smoke_release.py"
        ))
        .arg("--bin-dir")
        .arg(bin)
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    assert_eq!(value["status"], "PASS");
    for required in [
        "initialize_discovery_18_tools",
        "persistent_service_proxy_discovery_and_reconnect",
        "publication_inspection_discovery_and_never_started_refusal_without_binding",
        "bounded_inspection_historical_request_without_recursive_task_or_rewrite",
    ] {
        assert!(
            value["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == required)
        );
    }
    assert_eq!(value["checks"].as_array().unwrap().len(), 10);
    assert!(
        value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "yearly_update_frozen_input_and_blocked_worker_preserves_fixture")
    );
}
