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
    assert_eq!(report["scopes"].as_array().unwrap().len(), 4);
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

const GENERATION: &str = "6f9f0a4f0d3e4c8b1a2c3d4e5f60718293a4b5c6d7e8f90123456789abcdef01";

/// The two industry members PR-6 registers for one generation.
fn generation_store(dir: &std::path::Path) -> SourceStore {
    let store = SourceStore::new(dir.join("store"));
    for (source_id, parser, bytes) in [
        (
            x_data_mcp::source_store::TDXHY_SOURCE_ID,
            x_data_mcp::source_store::TDXHY_PARSER_VERSION,
            b"1|600006|T090302|||X6102\r\n".as_slice(),
        ),
        (
            x_data_mcp::source_store::INCON_SOURCE_ID,
            x_data_mcp::source_store::INCON_PARSER_VERSION,
            b"T090302|bank\r\n".as_slice(),
        ),
    ] {
        let mut facts = Registration::new(
            source_id,
            StoreKind::GenerationMember,
            format!("generation:{GENERATION}"),
            "2026-10-29T00:00:00Z",
        );
        facts.parser_version = Some(parser.to_string());
        store.register(bytes, &facts).unwrap();
    }
    store
}

fn market_db(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("market.db");
    x_data_mcp::sqlite_tasks::SqliteTasks::new(&db)
        .initialize_compact()
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    db
}

/// M3 PR-8: the generation-facts scope rebuilds from the source store, and a
/// cleared table replays back to the same summary (§9.4.2).
#[test]
fn replay_rebuilds_generation_facts_and_survives_a_cleared_table() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let store = generation_store(dir.path());
    let replay = || -> serde_json::Value {
        let output = std::process::Command::new(cli())
            .args([
                "replay",
                "--store",
                store.root().to_str().unwrap(),
                "--db",
                db.to_str().unwrap(),
                "--scope",
                "generation_facts",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let first = replay();
    assert_eq!(first["status"], "ok");
    assert_eq!(first["scopes"][0]["rebuilt"], "rebuilt");
    assert_eq!(first["scopes"][0]["database"], "match");
    assert_eq!(first["scopes"][0]["facts"], 3);
    let summary = first["scopes"][0]["summary"].as_str().unwrap().to_string();

    // Clear the index exactly as a fresh init would leave it, then replay.
    let backend = x_data_mcp::sqlite_market::backend::MarketBackend::open(
        &x_data_mcp::sqlite_tasks::SqliteTasks::new(&db),
    )
    .unwrap();
    backend.replace_generation_facts(&[]).unwrap();
    drop(backend);
    let rebuilt = replay();
    assert_eq!(rebuilt["scopes"][0]["summary"], summary.as_str());

    let verify = std::process::Command::new(cli())
        .args([
            "verify",
            "--store",
            store.root().to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
            "--scope",
            "generation_facts",
        ])
        .output()
        .unwrap();
    assert!(verify.status.success());
    let report: serde_json::Value = serde_json::from_slice(&verify.stdout).unwrap();
    assert_eq!(report["status"], "ok");
    assert_eq!(report["scopes"][0]["database"], "match");
}

/// M3 PR-8: an evidence exemption needs the explicit owner flag; a fact
/// exemption only needs the reason and the recorder.
#[test]
fn exempt_requires_a_reason_a_recorder_and_owner_approval_for_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let db = db.to_str().unwrap().to_string();
    let run = |extra: &[&str]| {
        std::process::Command::new(cli())
            .args(["exempt", "--db", &db, "--generation", GENERATION])
            .args(extra)
            .output()
            .unwrap()
    };
    // A factual exemption is recorded as-is.
    let output = run(&[
        "--missing",
        "derived_fact:gbbq.zip:capital_events",
        "--reason",
        "owner approved: capital events verified by hand",
        "--recorded-by",
        "owner",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // A key the gate can never match is a usage error, not an exemption.
    let output = run(&[
        "--missing",
        "capital_events",
        "--reason",
        "typo",
        "--recorded-by",
        "owner",
    ]);
    assert_eq!(output.status.code(), Some(2));
    // A blank reason is not an exemption.
    let output = run(&[
        "--missing",
        "derived_fact:gbbq.zip:capital_events",
        "--reason",
        "  ",
        "--recorded-by",
        "owner",
    ]);
    assert_eq!(output.status.code(), Some(2));
    // Evidence gaps need the owner's explicit flag…
    let output = run(&[
        "--missing",
        "source_store_evidence:base.zip:tdx.base.zip!tdxhy.cfg",
        "--reason",
        "owner approved: archive re-imported",
        "--recorded-by",
        "owner",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("owner"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // …and with it the row is written.
    let output = run(&[
        "--missing",
        "source_store_evidence:base.zip:tdx.base.zip!tdxhy.cfg",
        "--reason",
        "owner approved: archive re-imported",
        "--recorded-by",
        "owner",
        "--owner-approve-evidence",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let backend = x_data_mcp::sqlite_market::backend::MarketBackend::open(
        &x_data_mcp::sqlite_tasks::SqliteTasks::new(std::path::Path::new(&db)),
    )
    .unwrap();
    let rows = backend.read_retire_exemptions(GENERATION).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.recorded_by == "owner"));
}
