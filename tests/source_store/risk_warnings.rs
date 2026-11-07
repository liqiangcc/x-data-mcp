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
    seed_weekdays(&db);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    db
}

/// D-84 reads the trading calendar from the database. The fixtures announce in
/// 2024, so a weekday calendar is enough to place every event.
fn seed_weekdays(db: &Path) {
    use chrono::{Datelike, NaiveDate};
    let connection = rusqlite::Connection::open(db).unwrap();
    let transaction = connection.unchecked_transaction().unwrap();
    let mut cursor = NaiveDate::from_ymd_opt(1990, 1, 1).unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    while cursor <= end {
        if cursor.weekday().number_from_monday() <= 5 {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO trading_days(trade_day,sources) VALUES(?1,1)",
                    [cursor.signed_duration_since(epoch).num_days()],
                )
                .unwrap();
        }
        cursor = cursor.succ_opt().unwrap();
    }
    transaction.commit().unwrap();
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

/// D-84: without a trading calendar the SH effective day cannot be placed. A
/// standalone replay fails with a stable code and writes no row; inside a
/// writing `--scope all` (fresh init) it is `not_ready` and the deployment
/// still succeeds.
#[test]
fn a_database_without_a_calendar_is_not_ready_and_writes_no_sh_rows() {
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
    {
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute("DELETE FROM trading_days", []).unwrap();
    }

    let (code, report) = cli(store.root(), Some(&db), "replay", "risk_warnings");
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["status"], "risk_warnings_replay_not_ready");
    let scope = &report["scopes"][0];
    assert_eq!(scope["status"], "risk_warnings_replay_not_ready");
    assert_eq!(scope["rebuilt"], "not_rebuilt");
    assert!(scope["summary"].is_null());
    assert_eq!(interval_rows(&db), 0);

    // A fresh-init `all` must not fail the deployment for the missing calendar.
    let (code, report) = cli(store.root(), Some(&db), "replay", "all");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["status"], "ok");
    let risk = report["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["scope"] == "risk_warnings")
        .unwrap();
    assert_eq!(risk["status"], "not_ready");
    assert!(
        risk["not_ready_reason"]
            .as_str()
            .unwrap()
            .contains("calendar"),
        "{risk}"
    );
    assert_eq!(interval_rows(&db), 0);

    // Verifying that database compares nothing, so it keeps the stable failing
    // code and a non-zero exit.
    let (code, report) = cli(store.root(), Some(&db), "verify", "risk_warnings");
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["status"], "risk_warnings_replay_not_ready");
}

/// D-84: an announcement on the calendar's last day has no next trading day
/// yet, so no row is written; when the calendar advances, the same store writes
/// the row on the new day. This is the "do not guess" half of the rule.
#[test]
fn an_announcement_past_the_calendar_end_waits_for_the_next_trading_day() {
    use chrono::NaiveDate;

    let dir = tempfile::tempdir().unwrap();
    let db = market_db(dir.path());
    let store = SourceStore::new(dir.path().join("store"));
    let body = r#"{"pageHelp":{"beginPage":1,"endPage":1,"pageNo":1,"pageSize":100,
      "pageCount":1,"total":1,"data":[[{"SSEDATE":"2024-05-06","SECURITY_CODE":"600002",
      "TITLE":"示例二关于公司股票被实施其他风险警示的公告",
      "ORG_BULLETIN_ID":"d84-last-day"}]]}}"#
        .as_bytes();
    frozen_page(
        &store,
        "sse_bulletin",
        body,
        SSE_BULLETIN_PARSER_VERSION,
        BULLETIN_WINDOW,
        "2026-10-30T02:00:00Z",
    );
    let epoch = |day: NaiveDate| {
        day.signed_duration_since(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
            .num_days()
    };
    {
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute(
                "DELETE FROM trading_days WHERE trade_day >= ?1",
                [epoch(NaiveDate::from_ymd_opt(2024, 5, 7).unwrap())],
            )
            .unwrap();
    }

    let (code, report) = cli(store.root(), Some(&db), "replay", "risk_warnings");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["scopes"][0]["rebuilt"], "rebuilt");
    assert_eq!(interval_rows(&db), 0, "no next trading day, no row");

    {
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute(
                "INSERT INTO trading_days(trade_day,sources) VALUES(?1,1)",
                [epoch(NaiveDate::from_ymd_opt(2024, 5, 7).unwrap())],
            )
            .unwrap();
    }
    let (code, report) = cli(store.root(), Some(&db), "replay", "risk_warnings");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["scopes"][0]["rebuilt"], "rebuilt");
    assert_eq!(interval_rows(&db), 1, "the row appears on the new day");
    let connection = rusqlite::Connection::open(&db).unwrap();
    let from: i64 = connection
        .query_row(
            "SELECT effective_from FROM risk_warning_intervals WHERE exchange='sh' AND code='600002'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(from, epoch(NaiveDate::from_ymd_opt(2024, 5, 7).unwrap()));
}
