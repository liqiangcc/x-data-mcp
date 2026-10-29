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

/// M3 PR-6: `--db` turns the replay into a rebuild of the fresh-init database,
/// and `--verify-only` then compares the recomputed summary with the stored
/// rows instead of writing them. This is the deployment's "replay after fresh
/// init" contract, exercised through the real binary.
#[test]
fn replay_with_a_database_rebuilds_the_industry_table_and_verifies_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("market.db");
    x_data_mcp::sqlite_tasks::SqliteTasks::new(&db)
        .initialize_compact()
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let (_, sha) = registered_store(dir.path());
    let store = dir.path().join("store");
    let rebuild = std::process::Command::new(cli())
        .args([
            "replay",
            "--store",
            store.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
            "--scope",
            "industry",
        ])
        .output()
        .unwrap();
    assert!(
        rebuild.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rebuild.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&rebuild.stdout).unwrap();
    assert_eq!(report["status"], "ok");
    assert_eq!(report["scopes"][0]["rebuilt"], "rebuilt");
    assert_eq!(report["scopes"][0]["database"], "match");
    assert_eq!(report["scopes"][0]["snapshots"], 1);
    assert_eq!(report["scopes"][0]["assignments"], 1);
    let summary = report["scopes"][0]["summary"].as_str().unwrap().to_string();

    // The bytes stay in the store under their digest, which is what lets a
    // later fresh init reproduce the same table.
    let records = SourceStore::new(&store).records().unwrap();
    assert!(records.iter().any(|record| record.sha256 == sha));

    // `--verify-only` recomputes and compares instead of writing again.
    let verify = std::process::Command::new(cli())
        .args([
            "verify",
            "--store",
            store.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
            "--scope",
            "industry",
        ])
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&verify.stdout).unwrap();
    assert_eq!(report["status"], "ok");
    assert_eq!(report["scopes"][0]["database"], "match");
    assert_eq!(report["scopes"][0]["summary"], summary.as_str());
    assert_eq!(report["scopes"][0]["rebuilt"], "computed_no_db");
}
