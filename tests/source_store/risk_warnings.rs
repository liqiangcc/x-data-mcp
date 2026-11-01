//! `source_store replay --scope risk_warnings` selection (XDM-154, §5.5
//! AC-M3-9): replay must derive from the same pages the daily item used.
//!
//! The store keeps every frozen answer, including answers the source revised:
//! one request fingerprint (a window, a page, a plan) can have two snapshots,
//! and a fetch that failed on a later page leaves the earlier pages behind.
//! Parsing everything that is frozen would resurrect retired announcements, so
//! the scope takes the newest answer per fingerprint and refuses a page set that
//! is not the complete table. Everything here is a fixture in a tempdir; no
//! network, no host path.

use std::path::{Path, PathBuf};

use x_data_mcp::exchange_source::request::{SSE_BULLETIN_PARSER_VERSION, SZSE_PARSER_VERSION};
use x_data_mcp::source_store::{Registration, SourceStore, StoreKind};
use x_data_mcp::sqlite_tasks::SqliteTasks;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/fixtures/exchange-risk-warnings/{name}"))
        .expect("risk-warning fixture exists")
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

/// One frozen page as PR-2's freeze path registers it: `store_kind=http_fetch`,
/// the request fingerprint as `origin`, the envelope parser version, and the
/// acquisition time of the fetch that froze it.
fn frozen_page(
    store: &SourceStore,
    source_id: &str,
    body: &[u8],
    parser_version: &str,
    origin: &str,
    acquired_at: &str,
) -> String {
    let mut facts = Registration::new(source_id, StoreKind::HttpFetch, origin, acquired_at);
    facts.parser_version = Some(parser_version.to_string());
    facts.basis_status = Some("approved_source".to_string());
    store.register(body, &facts).unwrap().sha256
}

/// The one bulletin window the plan asks for; `pageHelp.beginPage` is part of
/// the fingerprint, so a re-fetch of the same window answers the same one.
const BULLETIN_WINDOW: &str =
    "https://query.sse.com.cn/security/stock/queryCompanyBulletinNew.do?beginPage=1&endPage=1";

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

/// The SH codes of the stored intervals, in key order.
fn sh_codes(db: &Path) -> Vec<String> {
    let connection = rusqlite::Connection::open(db).unwrap();
    let mut statement = connection
        .prepare("SELECT code FROM risk_warning_intervals WHERE exchange='sh' ORDER BY code")
        .unwrap();
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

fn interval_rows(db: &Path) -> i64 {
    let connection = rusqlite::Connection::open(db).unwrap();
    connection
        .query_row("SELECT COUNT(*) FROM risk_warning_intervals", [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// One window answered twice: the publisher revised it between two runs, so the
/// store holds two snapshots under one fingerprint. The live item derived from
/// the second answer only, and replay must do the same — the retired
/// announcement of the first answer must not come back as a risk-warning
/// interval.
#[test]
fn a_revised_bulletin_window_counts_only_the_newest_answer() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let store = SourceStore::new(dir.path().join("store"));
    frozen_page(
        &store,
        "sse_bulletin",
        &fixture("sse-bulletins-revised-old.json"),
        SSE_BULLETIN_PARSER_VERSION,
        BULLETIN_WINDOW,
        "2026-10-30T02:00:00Z",
    );
    let (code, report) = cli(store.root(), Some(&db), "replay", "risk_warnings");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["scopes"][0]["snapshots"], 2);
    assert_eq!(sh_codes(&db), ["600001", "600003"]);

    // Two days later the same window is fetched again and answers one
    // announcement fewer.
    let newest = frozen_page(
        &store,
        "sse_bulletin",
        &fixture("sse-bulletins-revised-new.json"),
        SSE_BULLETIN_PARSER_VERSION,
        BULLETIN_WINDOW,
        "2026-11-01T02:00:00Z",
    );
    let (code, report) = cli(store.root(), Some(&db), "replay", "risk_warnings");
    assert_eq!(code, 0, "{report}");
    let scope = &report["scopes"][0];
    assert_eq!(scope["rebuilt"], "rebuilt");
    assert_eq!(scope["database"], "match");
    // Both answers are still selected records; the *derivation* used one page:
    // one fingerprint, the newest snapshot.
    assert_eq!(scope["entries"], 2);
    assert_eq!(scope["snapshots"], 1);
    assert_eq!(
        sh_codes(&db),
        ["600003"],
        "the retired announcement is not counted again"
    );
    assert_eq!(interval_rows(&db), 1);

    // Both answers stay in the store: a revision adds an answer, it does not
    // overwrite the manifest.
    let records = store.records().unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.iter().any(|record| record.sha256 == newest));
    // And `--verify-only` compares against the same live selection.
    let (code, verify) = cli(store.root(), Some(&db), "verify", "risk_warnings");
    assert_eq!(code, 0, "{verify}");
    assert_eq!(verify["scopes"][0]["database"], "match");
    assert_eq!(verify["scopes"][0]["snapshots"], 1);
}

/// A snapshot frozen before XDM-152 holds the flat v2 `pageHelp.data` shape;
/// the v3 parser replays it unchanged (one row object = one announcement), so
/// the selection change must not make old bulletin pages unreplayable.
#[test]
fn a_v2_flat_bulletin_page_still_replays() {
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let store = SourceStore::new(dir.path().join("store"));
    frozen_page(
        &store,
        "sse_bulletin",
        &fixture("sse-bulletins.json"),
        SSE_BULLETIN_PARSER_VERSION,
        BULLETIN_WINDOW,
        "2026-10-30T02:00:00Z",
    );
    let (code, report) = cli(store.root(), Some(&db), "replay", "risk_warnings");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["scopes"][0]["rebuilt"], "rebuilt");
    // 600004's bulletin is a proposal and 900001 is a B share: neither is an
    // implementation or a revocation event.
    assert_eq!(sh_codes(&db), ["600001", "600003"]);
    let (code, verify) = cli(store.root(), Some(&db), "verify", "risk_warnings");
    assert_eq!(code, 0, "{verify}");
    assert_eq!(verify["scopes"][0]["database"], "match");
}

/// The SZSE side is a whole-table snapshot: an incomplete page set cannot
/// rebuild the ST intervals (and must not become a shorter table), so the scope
/// fails closed and leaves the previous version readable.
#[test]
fn an_incomplete_short_name_page_set_is_snapshot_incomplete() {
    const ORIGIN: &str =
        "https://www.szse.cn/api/report/ShowReport/data?CATALOGID=SSGSGMXX&TABKEY=tab2&PAGENO=1";
    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let store = SourceStore::new(dir.path().join("store"));
    frozen_page(
        &store,
        "szse_short_name",
        &fixture("szse-name-changes.json"),
        SZSE_PARSER_VERSION,
        ORIGIN,
        "2026-10-30T02:00:00Z",
    );
    let (code, report) = cli(store.root(), Some(&db), "replay", "risk_warnings");
    assert_eq!(code, 0, "{report}");
    let published = interval_rows(&db);
    assert!(published > 0, "the first replay published a version");

    // The table grew to two pages; the fetch froze page 1 and then failed, so
    // the newest answer to page 1 is all the store holds of that revision.
    let mut grown: serde_json::Value =
        serde_json::from_slice(&fixture("szse-name-changes.json")).unwrap();
    grown[0]["metadata"]["pagecount"] = serde_json::json!(2);
    grown[0]["data"].as_array_mut().unwrap().truncate(2);
    frozen_page(
        &store,
        "szse_short_name",
        serde_json::to_vec(&grown).unwrap().as_slice(),
        SZSE_PARSER_VERSION,
        ORIGIN,
        "2026-11-01T02:00:00Z",
    );

    let (code, report) = cli(store.root(), Some(&db), "replay", "risk_warnings");
    assert_eq!(code, 1);
    assert_eq!(report["status"], "snapshot_incomplete");
    assert_eq!(report["scopes"][0]["rebuilt"], "not_rebuilt");
    assert_eq!(report["problems"][0]["reason"], "snapshot_incomplete");
    assert_eq!(report["problems"][0]["source_id"], "szse_short_name");
    assert!(
        report["problems"][0]["actual"]
            .as_str()
            .unwrap()
            .contains("page(s) 2 of 2"),
        "{report}"
    );
    assert_eq!(interval_rows(&db), published, "the previous version stays");
}
