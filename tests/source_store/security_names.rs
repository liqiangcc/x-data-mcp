//! `source_store replay --scope security_names` (M3 PR-12, issue #446 §9.4.2).
//!
//! The name history is a whole-market dataset whose evidence carries coverage
//! windows. These tests pin the deployment contract: a fresh-init database is
//! rebuilt from the frozen SZSE short-name table alone (no HTTP, no clock), two
//! replays produce bit-identical evidence, `--verify-only` compares instead of
//! writing, and "no data" is never reported as "the empty table was rebuilt".
//! Everything is a fixture in a tempdir; no network, no host path.

use std::path::{Path, PathBuf};

use x_data_mcp::exchange_source::request::SZSE_PARSER_VERSION;
use x_data_mcp::source_store::{Registration, SourceStore, StoreKind};
use x_data_mcp::sqlite_tasks::SqliteTasks;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/fixtures/exchange-security-names/{name}"))
        .expect("security-name fixture exists")
}

fn market_db(dir: &Path) -> PathBuf {
    let db = dir.join("market.db");
    SqliteTasks::new(&db).initialize_compact().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    db
}

/// One frozen SZSE `SSGSGMXX`/`tab2` page as PR-2's freeze path registers it:
/// `store_kind=http_fetch`, the request fingerprint as `origin`, the envelope
/// parser version, one acquisition time for the whole page set.
fn frozen_short_name(store: &SourceStore, body: &[u8], parser_version: &str) -> String {
    let mut facts = Registration::new(
        "szse_short_name",
        StoreKind::HttpFetch,
        "https://www.szse.cn/api/report/ShowReport/data?CATALOGID=SSGSGMXX&TABKEY=tab2&PAGENO=1",
        "2026-10-30T02:00:00Z",
    );
    facts.parser_version = Some(parser_version.to_string());
    facts.basis_status = Some("approved_source".to_string());
    store.register(body, &facts).unwrap().sha256
}

fn register_generation_member(store: &SourceStore) {
    let mut facts = Registration::new(
        "tdx.base.zip!tdxhy.cfg",
        StoreKind::GenerationMember,
        "generation:6f9f0a4f0d3e4c8b1a2c3d4e5f60718293a4b5c6d7e8f90123456789abcdef01",
        "2026-10-29T00:00:00Z",
    );
    facts.parser_version = Some("tdxhy-v1".to_string());
    store
        .register(b"1|600006|T090302|||X6102\r\n", &facts)
        .unwrap();
}

fn cli(store: &Path, db: Option<&Path>, command: &str, scope: &str) -> (i32, serde_json::Value) {
    let mut command_line = std::process::Command::new(env!("CARGO_BIN_EXE_source_store"));
    command_line
        .arg(command)
        .args(["--store", store.to_str().unwrap(), "--scope", scope]);
    if let Some(db) = db {
        command_line.args(["--db", db.to_str().unwrap()]);
    }
    let output = command_line.output().unwrap();
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{command} printed no JSON ({error}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap_or(-1), report)
}

fn name_rows(db: &Path) -> i64 {
    let connection = rusqlite::Connection::open(db).unwrap();
    connection
        .query_row("SELECT COUNT(*) FROM security_name_history", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn registrations(db: &Path) -> i64 {
    let connection = rusqlite::Connection::open(db).unwrap();
    connection
        .query_row(
            "SELECT COUNT(*) FROM derived_dataset_versions \
             WHERE dataset_kind='security_names' AND dataset_key='a_share'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn evidence_digest(db: &Path) -> String {
    let connection = rusqlite::Connection::open(db).unwrap();
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT evidence_digest FROM derived_dataset_versions \
             WHERE dataset_kind='security_names' AND dataset_key='a_share'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn wipe(db: &Path) {
    let connection = rusqlite::Connection::open(db).unwrap();
    connection
        .execute("DELETE FROM security_name_history", [])
        .unwrap();
    connection
        .execute(
            "DELETE FROM derived_dataset_versions \
             WHERE dataset_kind='security_names' AND dataset_key='a_share'",
            [],
        )
        .unwrap();
}

/// `--db` rebuilds `security_name_history` + its registration from the store
/// alone, the write reads back as `match`, and `--verify-only` compares the
/// same summary without writing.
#[test]
fn replay_rebuilds_the_name_history_and_verifies_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let store = SourceStore::new(dir.path().join("store"));
    let sha = frozen_short_name(&store, &fixture("szse-chain.json"), SZSE_PARSER_VERSION);

    let (code, report) = cli(store.root(), Some(&db), "replay", "security_names");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["status"], "ok");
    let scope = &report["scopes"][0];
    assert_eq!(scope["scope"], "security_names");
    assert_eq!(scope["rebuilt"], "rebuilt");
    assert_eq!(scope["database"], "match");
    assert_eq!(scope["entries"], 1);
    assert_eq!(scope["blobs"], 1);
    // Three intervals for the 000004 change chain; the B-share row is skipped.
    assert_eq!(scope["snapshots"], 3);
    // The replayable as-of bound is the table's own last change day, not the
    // day the replay runs.
    assert_eq!(scope["as_of"], "2001-03-07");
    let summary = scope["summary"].as_str().unwrap().to_string();
    assert_eq!(name_rows(&db), 3);
    assert_eq!(registrations(&db), 1);
    let digest = evidence_digest(&db);

    let (code, verify) = cli(store.root(), Some(&db), "verify", "security_names");
    assert_eq!(code, 0, "{verify}");
    assert_eq!(verify["scopes"][0]["database"], "match");
    assert_eq!(verify["scopes"][0]["summary"], summary.as_str());
    assert_eq!(verify["scopes"][0]["as_of"], "2001-03-07");
    assert_eq!(verify["scopes"][0]["rebuilt"], "computed_no_db");
    assert_eq!(name_rows(&db), 3, "verify must not write");

    // "Clear the DB and replay" reproduces the same rows, the same coverage
    // evidence and therefore the same summary on another day: the evidence
    // digest is bit-identical, so no window was re-stamped with a clock.
    wipe(&db);
    assert_eq!(name_rows(&db), 0);
    let (code, again) = cli(store.root(), Some(&db), "replay", "security_names");
    assert_eq!(code, 0, "{again}");
    assert_eq!(again["scopes"][0]["rebuilt"], "rebuilt");
    assert_eq!(again["scopes"][0]["database"], "match");
    assert_eq!(again["scopes"][0]["summary"], summary.as_str());
    assert_eq!(again["scopes"][0]["as_of"], "2001-03-07");
    assert_eq!(name_rows(&db), 3);
    assert_eq!(
        evidence_digest(&db),
        digest,
        "the replayed registration evidence is bit-identical"
    );

    // The bytes stay in the store under their digest.
    assert!(
        SourceStore::new(store.root())
            .records()
            .unwrap()
            .iter()
            .any(|record| record.sha256 == sha)
    );
}

/// The frozen page keeps the real two-block shape (`tab1` empty, `tab2` with
/// rows): `source_store replay --scope security_names` reads the declared tab,
/// so the page that used to rebuild zero rows now rebuilds the change chain.
#[test]
fn replay_reads_tab2_when_the_frozen_page_starts_with_an_empty_tab1() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let store = SourceStore::new(dir.path().join("store"));
    frozen_short_name(
        &store,
        &fixture("szse-chain-tab1-empty.json"),
        SZSE_PARSER_VERSION,
    );

    let (code, report) = cli(store.root(), Some(&db), "replay", "security_names");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["status"], "ok");
    let scope = &report["scopes"][0];
    assert_eq!(scope["rebuilt"], "rebuilt");
    assert_eq!(scope["database"], "match");
    assert_eq!(scope["snapshots"], 3);
    assert_eq!(scope["as_of"], "2001-03-07");
    assert_eq!(name_rows(&db), 3);
    assert_eq!(registrations(&db), 1);
}

/// Without `--db` the scope only computes: a summary and the as-of bound, and
/// no database is opened or created by the read.
#[test]
fn replay_without_a_database_only_reports_the_summary() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    frozen_short_name(&store, &fixture("szse-chain.json"), SZSE_PARSER_VERSION);
    let (code, report) = cli(store.root(), None, "replay", "security_names");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["scopes"][0]["rebuilt"], "computed_no_db");
    assert_eq!(report["scopes"][0]["snapshots"], 3);
    assert_eq!(report["scopes"][0]["as_of"], "2001-03-07");
    assert!(report["scopes"][0].get("database").is_none());
    assert!(report.get("database").is_none());
}

/// A blob that does not read back to its digest fails the scope with
/// `source_store_corrupt` and nothing is written.
#[test]
fn a_tampered_blob_is_source_store_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let store = SourceStore::new(dir.path().join("store"));
    let sha = frozen_short_name(&store, &fixture("szse-chain.json"), SZSE_PARSER_VERSION);
    std::fs::write(store.blob_path(&sha), b"tampered").unwrap();

    let (code, report) = cli(store.root(), Some(&db), "replay", "security_names");
    assert_eq!(code, 1);
    assert_eq!(report["status"], "source_store_corrupt");
    assert_eq!(report["problems"][0]["reason"], "source_store_corrupt");
    assert_eq!(report["scopes"][0]["status"], "source_store_corrupt");
    assert_eq!(name_rows(&db), 0);
    assert_eq!(registrations(&db), 0);
}

/// An unknown `parser_version` is fail-closed (§9.4.1): the bytes stay
/// registered, nothing is parsed and nothing is written.
#[test]
fn an_unknown_parser_version_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let store = SourceStore::new(dir.path().join("store"));
    frozen_short_name(&store, &fixture("szse-chain.json"), "exchange-szse-page-v9");

    let (code, report) = cli(store.root(), Some(&db), "replay", "security_names");
    assert_eq!(code, 1);
    assert_eq!(report["status"], "unknown_parser");
    assert_eq!(report["problems"][0]["reason"], "unknown_parser");
    assert_eq!(report["problems"][0]["actual"], "exchange-szse-page-v9");
    assert_eq!(report["scopes"][0]["status"], "unknown_parser");
    assert_eq!(name_rows(&db), 0);
    assert_eq!(registrations(&db), 0);
}

/// An empty store is `store_empty`, not "an empty name history was rebuilt":
/// the report says `nothing_selected` and the fresh-init table stays untouched.
#[test]
fn an_empty_store_rebuilds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let empty = SourceStore::new(dir.path().join("empty-store"));
    let (code, report) = cli(empty.root(), Some(&db), "replay", "security_names");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["status"], "store_empty");
    assert_eq!(report["scopes"][0]["status"], "store_empty");
    assert_eq!(report["scopes"][0]["rebuilt"], "nothing_selected");
    assert_eq!(report["scopes"][0]["entries"], 0);
    assert_eq!(name_rows(&db), 0);
    assert_eq!(registrations(&db), 0);
    assert!(!empty.root().exists(), "a read must not create the store");
}

/// A store with no `szse_short_name` record selects nothing for this scope: it
/// reports `store_empty`/`nothing_selected` and does not clear a table another
/// store already rebuilt.
#[test]
fn a_store_without_the_short_name_source_leaves_the_table_alone() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let source = SourceStore::new(dir.path().join("store"));
    frozen_short_name(&source, &fixture("szse-chain.json"), SZSE_PARSER_VERSION);
    let (code, report) = cli(source.root(), Some(&db), "replay", "security_names");
    assert_eq!(code, 0, "{report}");
    assert_eq!(name_rows(&db), 3);

    // Only a generation member: nothing for the name-history scope.
    let other = SourceStore::new(dir.path().join("other-store"));
    register_generation_member(&other);
    let (code, report) = cli(other.root(), Some(&db), "replay", "security_names");
    assert_eq!(code, 0, "{report}");
    // No matching record is not a failure, and it is certainly not a rebuild:
    // the scope says `store_empty`/`nothing_selected` and the table is intact.
    assert_eq!(report["status"], "ok");
    assert_eq!(report["scopes"][0]["status"], "store_empty");
    assert_eq!(report["scopes"][0]["rebuilt"], "nothing_selected");
    assert_eq!(
        name_rows(&db),
        3,
        "no record means no write, not an empty table"
    );
    assert_eq!(registrations(&db), 1);
}

/// A frozen table with no A-share change row is a source defect: the scope
/// fails closed with `empty_name_set`, writes nothing and leaves the previous
/// dataset readable.
#[test]
fn a_zero_row_table_is_not_reported_as_an_empty_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let source = SourceStore::new(dir.path().join("store"));
    frozen_short_name(&source, &fixture("szse-chain.json"), SZSE_PARSER_VERSION);
    let (code, report) = cli(source.root(), Some(&db), "replay", "security_names");
    assert_eq!(code, 0, "{report}");
    assert_eq!(name_rows(&db), 3);

    let empty = SourceStore::new(dir.path().join("empty-page-store"));
    frozen_short_name(&empty, &fixture("szse-empty.json"), SZSE_PARSER_VERSION);
    let (code, report) = cli(empty.root(), Some(&db), "replay", "security_names");
    assert_eq!(code, 1);
    assert_eq!(report["status"], "security_names_rebuild_failed");
    assert_eq!(
        report["problems"][0]["reason"],
        "security_names_rebuild_failed"
    );
    assert!(
        report["problems"][0]["actual"]
            .as_str()
            .unwrap()
            .contains("empty_name_set"),
        "the reason code names the empty table: {report}"
    );
    assert_eq!(name_rows(&db), 3, "the previous version stays readable");
    assert_eq!(registrations(&db), 1);
}
