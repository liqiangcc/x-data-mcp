//! CLI contract of the selected PR-1a entry form: one JSON object on stdout,
//! exit 0 for `ok`/`store_empty`, exit 1 for a corrupt store.

use std::fs;

use x_data_mcp::source_store::{Registration, SourceStore, StoreKind};

fn cli() -> &'static str {
    env!("CARGO_BIN_EXE_source_store")
}

const BODY: &[u8] = b"0|000001|T1001|||X500102\r\n";

fn registered_store(dir: &std::path::Path) -> (SourceStore, String) {
    let store = SourceStore::new(dir.join("store"));
    let mut facts = Registration::new(
        "tdx.base.zip!tdxhy.cfg",
        StoreKind::ArchiveImport,
        "import:cli-fixture",
        "2026-10-24T02:00:00Z",
    );
    facts.parser_version = Some("tdxhy-v1".to_string());
    store.register(BODY, &facts).unwrap();
    let sha = x_data_mcp::hash::sha256(BODY);
    (store, sha)
}

#[test]
fn empty_store_reports_store_empty_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("store");
    let output = std::process::Command::new(cli())
        .args([
            "replay",
            "--store",
            store_path.to_str().unwrap(),
            "--scope",
            "all",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "store_empty");
    assert_eq!(report["scope"], "all");
    assert_eq!(report["scopes"].as_array().unwrap().len(), 3);
    assert!(!store_path.exists(), "a read must not create the store");
}

#[test]
fn corrupt_blob_makes_verify_exit_one_with_the_corrupt_code() {
    let dir = tempfile::tempdir().unwrap();
    let (store, sha) = registered_store(dir.path());
    fs::write(store.blob_path(&sha), b"tampered").unwrap();
    let output = std::process::Command::new(cli())
        .args(["verify", "--store", store.root().to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "source_store_corrupt");
    assert_eq!(report["problems"][0]["reason"], "source_store_corrupt");
}

#[test]
fn bad_usage_exits_two() {
    let output = std::process::Command::new(cli())
        .args(["replay", "--scope", "industry"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--store is required"));
}
