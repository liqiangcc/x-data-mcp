//! End-to-end tests of the exchange entry against a real (temp) source store
//! and a schema-25 database. Everything is a fixture: no network, no host path.
//!
//! The four design claims exercised here are §6.3's ("freeze before parse",
//! "one row per response byte identity", "0 rows is a failure", bounded
//! pagination) and §9.4.2's ("replay rebuilds the index from the store alone
//! and the summary matches").

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use chrono::NaiveDate;
use x_data_mcp::exchange_source::request::{
    SSE_PARSER_VERSION_V1, SZSE_PARSER_VERSION_V1, SZSE_PARSER_VERSION_V2,
};
use x_data_mcp::exchange_source::transport::{HttpRequest, HttpResponse};
use x_data_mcp::exchange_source::validate::parse_page;
use x_data_mcp::exchange_source::{
    AcquireError, AcquisitionPlan, BasisStatus, Client, Entry, ExchangeRequest, PagingFormat,
    RetryPolicy, RunCache, Segment, SourceId, Transport, TransportError, bind, freeze_page,
};
use x_data_mcp::source_store::SourceStore;
use x_data_mcp::sqlite_market::backend::MarketBackend;
use x_data_mcp::sqlite_tasks::SqliteTasks;

/// A `pageHelp` page in the dialect written since 2026-10-30
/// (`pageHelp.pageNo`/`pageSize`).
fn sse_body(page: u32, page_count: u32, total: u64, rows: usize) -> Vec<u8> {
    let data: Vec<serde_json::Value> = (0..rows)
        .map(|index| serde_json::json!({"code": format!("60000{index}")}))
        .collect();
    serde_json::json!({
        "pageHelp": {
            "beginPage": 1,
            "endPage": null,
            "pageNo": page,
            "pageSize": 2000,
            "pageCount": page_count,
            "total": total,
            "data": data,
        }
    })
    .to_string()
    .into_bytes()
}

/// A `pageHelp` page in the shape frozen before 2026-10-30
/// (`exchange-sse-page-v1`: the page number is `beginPage`, there is no
/// `pageHelp.pageNo`). Only the legacy-replay test uses it.
fn sse_v1_body(page: u32, page_count: u32, total: u64, rows: usize) -> Vec<u8> {
    let data: Vec<serde_json::Value> = (0..rows)
        .map(|index| serde_json::json!({"code": format!("60000{index}")}))
        .collect();
    serde_json::json!({
        "pageHelp": {
            "beginPage": page,
            "endPage": page,
            "pageCount": page_count,
            "total": total,
            "data": data,
        }
    })
    .to_string()
    .into_bytes()
}

/// A SZSE page: `metadata.pageNo`/`pagecount`/`recordcount` plus `PAGENO` as the
/// request page. This is the multi-page dialect this suite uses for the paging
/// invariants (the SSE list dialect is single-page since 2026-10-30).
fn szse_body(page: u32, page_count: u32, total: u64, rows: usize) -> Vec<u8> {
    let data: Vec<serde_json::Value> = (0..rows)
        .map(|index| serde_json::json!({"zqdm": format!("00000{index}")}))
        .collect();
    serde_json::json!([{
        "metadata": {
            "pageno": page,
            "pagecount": page_count,
            "recordcount": total,
            "pagesize": 20,
        },
        "data": data,
    }])
    .to_string()
    .into_bytes()
}

/// A two-block SZSE page in the shape frozen before the block-selection change
/// (`exchange-szse-page-v1`): `tab1` is the empty suspended list, `tab2` the
/// terminated list. v1 replay must keep reading the first block.
fn szse_two_block_body(page: u32, page_count: u32, total: u64, rows: usize) -> Vec<u8> {
    let data: Vec<serde_json::Value> = (0..rows)
        .map(|index| serde_json::json!({"zqdm": format!("00000{index}")}))
        .collect();
    serde_json::json!([
        {
            "metadata": {
                "tabkey": "tab1",
                "pageno": page,
                "pagecount": 0,
                "recordcount": 0,
                "pagesize": 20,
            },
            "data": [],
        },
        {
            "metadata": {
                "tabkey": "tab2",
                "pageno": page,
                "pagecount": page_count,
                "recordcount": total,
                "pagesize": 20,
            },
            "data": data,
        }
    ])
    .to_string()
    .into_bytes()
}

fn ok(body: Vec<u8>, etag: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        etag: Some(format!("\"{etag}\"")),
        last_modified: Some("Wed, 29 Oct 2026 02:00:00 GMT".to_string()),
        body,
    }
}

struct Fixture {
    responses: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
    requests: Mutex<Vec<String>>,
}

impl Fixture {
    fn new(responses: Vec<Result<HttpResponse, TransportError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Transport for Fixture {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(request.url.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(TransportError::Failed("fixture exhausted".to_string())))
    }
}

/// Wraps the shared fixture so `Client` can own a `'static` transport.
struct Shared(Arc<Fixture>);

impl Transport for Shared {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        self.0.get(request)
    }
}

struct Scenario {
    dir: tempfile::TempDir,
}

impl Scenario {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn store(&self) -> SourceStore {
        SourceStore::new(self.dir.path().join("store"))
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("market.sqlite")
    }
}

fn open(db: &Path) -> MarketBackend {
    let tasks = SqliteTasks::new(db);
    if !db.exists() {
        tasks.initialize_compact().unwrap();
        // The compact backend's lease refuses a world-readable database file.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(db, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    MarketBackend::open(&tasks).unwrap()
}

fn client(fixture: &Arc<Fixture>) -> Client {
    Client::with_transport(Shared(fixture.clone()))
        .with_policy(RetryPolicy::single())
        .without_sleep()
}

fn plan(source: SourceId, url: &str) -> AcquisitionPlan {
    AcquisitionPlan::new(source, url, PagingFormat::SsePageHelp)
}

fn szse_plan() -> AcquisitionPlan {
    AcquisitionPlan::new(
        SourceId::SzseListing,
        "https://example.invalid/szse-list",
        PagingFormat::SzseMetadata,
    )
}

fn cli(store: &SourceStore, db: &Path, command: &str) -> serde_json::Value {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_source_store"))
        .args([
            command,
            "--store",
            store.root().to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
            "--scope",
            "exchange",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{command} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn wipe(db: &Path) {
    let connection = rusqlite::Connection::open(db).unwrap();
    connection
        .execute("DELETE FROM exchange_source_snapshots", [])
        .unwrap();
}

fn snapshot_count(db: &Path, source: SourceId) -> usize {
    let backend = open(db);
    match bind::bind(&backend, source, BasisStatus::ApprovedSource) {
        Ok(binding) => binding.snapshots.len(),
        Err(bind::BindError::NotFound { .. }) => 0,
        Err(error) => panic!("unexpected bind error: {error}"),
    }
}

/// The paging invariant: every page of a multi-page window is frozen as its own
/// snapshot, indexed as its own row, and replay rebuilds exactly that index from
/// the store alone. SZSE is the multi-page dialect (the SSE list dialect is
/// single-page since 2026-10-30: a `pageCount>1` answer fails closed with
/// `page_size_ignored`, see `a_listing_page_that_ignores_the_page_size_fails_closed`
/// in the adapter tests).
#[test]
fn a_two_page_listing_is_frozen_page_by_page() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![
        Ok(ok(szse_body(1, 2, 3, 2), "page-1")),
        Ok(ok(szse_body(2, 2, 3, 1), "page-2")),
    ]);
    let client = client(&fixture);
    {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client);
        let outcome = entry.acquire(&szse_plan()).unwrap();
        assert_eq!(outcome.row_count, 3);
        assert_eq!(outcome.page_count, 2);
        assert_eq!(outcome.snapshots.len(), 2);
        assert_eq!(outcome.reused, 0);
        assert_eq!(
            outcome
                .snapshots
                .iter()
                .map(|snapshot| snapshot.row.row_count)
                .sum::<usize>(),
            3
        );
        assert_eq!(outcome.binding.snapshots.len(), 2);
        assert_eq!(
            outcome.snapshots[0].row.http_etag.as_deref(),
            Some("\"page-1\"")
        );
        // `as_of` is the observation date of the acquisition (UTC date), so it
        // is a date, not a clock reading.
        assert_eq!(outcome.snapshots[0].row.as_of.len(), 10);
    }
    assert_eq!(fixture.calls(), 2);
    let requests = fixture.requests();
    // Each page is its own request: the page selector is what replay reads back
    // from the manifest, so it must carry the page the response belongs to.
    assert!(requests[0].contains("PAGENO=1"), "url: {}", requests[0]);
    assert!(requests[1].contains("PAGENO=2"), "url: {}", requests[1]);
    assert_ne!(requests[0], requests[1]);
    assert_eq!(store.records().unwrap().len(), 2);
    assert_eq!(snapshot_count(&db, SourceId::SzseListing), 2);
    let replay = cli(&store, &db, "replay");
    assert_eq!(replay["status"], "ok");
    assert_eq!(replay["scopes"][0]["rebuilt"], "rebuilt");
    assert_eq!(replay["scopes"][0]["database"], "match");
    assert_eq!(replay["scopes"][0]["snapshots"], 2);
}

/// The SZSE block-selection change reads `tab2` (v2), but pages frozen before
/// it (`exchange-szse-page-v1`) must replay with their old meaning: the first
/// block. `PagingFormat::from_parser_version` keeps that alias as a distinct
/// format; without it the old rows would be re-derived with the new selection.
#[test]
fn legacy_szse_pages_keep_the_first_block_meaning_on_replay() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    {
        let backend = open(&db);
        for (page, rows) in [(1_u32, 2_usize), (2, 1)] {
            let request =
                ExchangeRequest::new(SourceId::SzseDelisting, "https://example.invalid/szse")
                    .paged(PagingFormat::SzseMetadata, page);
            let response = ok(
                szse_two_block_body(page, 2, 3, rows),
                &format!("szse-page-{page}"),
            );
            // v1 reads `[0]`: the empty tab1, count 0. This is the old meaning
            // the replay must reproduce, not the new tab2 selection.
            let envelope = parse_page(PagingFormat::SzseMetadataV1, None, &response.body).unwrap();
            assert_eq!(
                (envelope.page, envelope.page_count, envelope.rows),
                (page, 0, 0)
            );
            freeze_page(
                &store,
                &backend,
                &request,
                &response,
                &envelope,
                SZSE_PARSER_VERSION_V1,
                BasisStatus::ApprovedSource,
                "2026-10-30T02:00:00Z",
            )
            .unwrap();
        }
    }
    assert_eq!(store.records().unwrap().len(), 2);
    assert_eq!(snapshot_count(&db, SourceId::SzseDelisting), 2);
    let replay = cli(&store, &db, "replay");
    assert_eq!(replay["status"], "ok");
    assert_eq!(replay["scopes"][0]["rebuilt"], "rebuilt");
    assert_eq!(replay["scopes"][0]["database"], "match");
    assert_eq!(replay["scopes"][0]["snapshots"], 2);
    assert_eq!(snapshot_count(&db, SourceId::SzseDelisting), 2);
}

/// `exchange-szse-page-v2` chose `tab2` first. Pages frozen under it must keep
/// that meaning after the v3 rule (declared tabkey / unique non-empty block,
/// #446 D1), otherwise a replay would silently rewrite their row counts.
#[test]
fn szse_v2_pages_keep_the_tab2_meaning_on_replay() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    {
        let backend = open(&db);
        let request = ExchangeRequest::new(SourceId::SzseDelisting, "https://example.invalid/szse")
            .paged(PagingFormat::SzseMetadata, 1);
        let response = ok(szse_two_block_body(1, 1, 2, 2), "szse-v2-page-1");
        // v2 reads tab2 even though the request carries no TABKEY.
        let envelope = parse_page(PagingFormat::SzseMetadataV2, None, &response.body).unwrap();
        assert_eq!((envelope.page_count, envelope.rows), (1, 2));
        freeze_page(
            &store,
            &backend,
            &request,
            &response,
            &envelope,
            SZSE_PARSER_VERSION_V2,
            BasisStatus::ApprovedSource,
            "2026-10-31T02:00:00Z",
        )
        .unwrap();
    }
    assert_eq!(store.records().unwrap().len(), 1);
    let replay = cli(&store, &db, "replay");
    assert_eq!(replay["status"], "ok");
    assert_eq!(replay["scopes"][0]["rebuilt"], "rebuilt");
    assert_eq!(replay["scopes"][0]["database"], "match");
    assert_eq!(replay["scopes"][0]["snapshots"], 1);
    assert_eq!(snapshot_count(&db, SourceId::SzseDelisting), 1);
}

/// The 2026-10-30 dialect change writes `pageHelp.pageNo`/`pageSize`, but pages
/// frozen before it (`exchange-sse-page-v1`) must still replay from the store
/// alone. `PagingFormat::from_parser_version` keeps that alias; without it the
/// replay would report every old SSE page as `unknown_parser` and drop its index
/// row.
#[test]
fn legacy_sse_pages_still_replay_after_the_dialect_change() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    {
        let backend = open(&db);
        for (page, rows) in [(1_u32, 2_usize), (2, 1)] {
            let request =
                ExchangeRequest::new(SourceId::SseListing, "https://example.invalid/list")
                    .paged(PagingFormat::SsePageHelp, page);
            let response = ok(sse_v1_body(page, 2, 3, rows), &format!("page-{page}"));
            let envelope = parse_page(PagingFormat::SsePageHelp, None, &response.body).unwrap();
            freeze_page(
                &store,
                &backend,
                &request,
                &response,
                &envelope,
                SSE_PARSER_VERSION_V1,
                BasisStatus::ApprovedSource,
                "2026-10-30T02:00:00Z",
            )
            .unwrap();
        }
    }
    assert_eq!(store.records().unwrap().len(), 2);
    assert_eq!(snapshot_count(&db, SourceId::SseListing), 2);
    let replay = cli(&store, &db, "replay");
    assert_eq!(replay["status"], "ok");
    assert_eq!(replay["scopes"][0]["rebuilt"], "rebuilt");
    assert_eq!(replay["scopes"][0]["database"], "match");
    assert_eq!(replay["scopes"][0]["snapshots"], 2);
    assert_eq!(snapshot_count(&db, SourceId::SseListing), 2);
}

#[test]
fn one_quarter_bulletin_window_sends_one_bounded_request() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![Ok(ok(sse_body(1, 1, 1, 1), "q1"))]);
    let client = client(&fixture);
    let window = Segment::parse("2000-01-01", "2000-03-31").unwrap();
    {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client);
        let outcome = entry
            .acquire_window(
                &plan(SourceId::SseBulletin, "https://example.invalid/bulletin"),
                window,
                "beginDate",
                "endDate",
            )
            .unwrap();
        assert_eq!(outcome.row_count, 1);
        assert_eq!(outcome.snapshots.len(), 1);
    }
    assert_eq!(fixture.calls(), 1);
    let request = &fixture.requests()[0];
    assert!(request.contains("beginDate=2000-01-01"), "url: {request}");
    assert!(request.contains("endDate=2000-03-31"), "url: {request}");
    // The quarter is one request: its fingerprint already carries both bounds.
    assert_eq!(store.records().unwrap().len(), 1);
    assert_eq!(snapshot_count(&db, SourceId::SseBulletin), 1);
}

#[test]
fn over_one_quarter_window_is_refused_without_a_request() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![]);
    let client = client(&fixture);
    let start = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap();
    let end = NaiveDate::from_ymd_opt(2000, 7, 1).unwrap();
    let error = Segment::new(start, end).unwrap_err();
    assert_eq!(error.code(), "segment_too_long");
    // A window that fails construction can never reach `acquire_window`: the
    // bound is an entry condition, not a check after the request.
    let backend = open(&db);
    let _entry = Entry::new(&store, &backend, &client);
    assert_eq!(fixture.calls(), 0);
    assert!(!store.root().exists());
}

#[test]
fn empty_data_is_unverified_zero_and_bytes_stay_frozen() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![Ok(ok(sse_body(1, 1, 0, 0), "empty"))]);
    let client = client(&fixture);
    let error = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client);
        entry
            .acquire(&plan(SourceId::SseListing, "https://example.invalid/list"))
            .unwrap_err()
    };
    assert_eq!(error.code(), "unverified_zero");
    assert_eq!(store.records().unwrap().len(), 1);
    // The frozen page is indexed as a factual 0-row snapshot (the index mirrors
    // the store); the *window* is still refused, so no consumer writes.
    assert_eq!(snapshot_count(&db, SourceId::SseListing), 1);
    let replay = cli(&store, &db, "replay");
    assert_eq!(replay["scopes"][0]["database"], "match");
    assert_eq!(replay["scopes"][0]["snapshots"], 1);
}

#[test]
fn total_mismatch_is_truncated_but_the_frozen_page_stays_reproducible() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![Ok(ok(sse_body(1, 1, 5, 2), "short"))]);
    let client = client(&fixture);
    let error = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client);
        entry
            .acquire(&plan(SourceId::SseListing, "https://example.invalid/list"))
            .unwrap_err()
    };
    assert_eq!(error.code(), "truncated");
    assert_eq!(store.records().unwrap().len(), 1);
    assert_eq!(snapshot_count(&db, SourceId::SseListing), 1);
    let replay = cli(&store, &db, "replay");
    assert_eq!(replay["scopes"][0]["database"], "match");
    assert_eq!(replay["scopes"][0]["snapshots"], 1);
}

#[test]
fn http_500_is_source_unavailable_and_nothing_is_frozen() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![Ok(HttpResponse {
        status: 500,
        etag: None,
        last_modified: None,
        body: b"internal error".to_vec(),
    })]);
    let client = client(&fixture);
    let error = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client);
        entry
            .acquire(&plan(
                SourceId::SzseDelisting,
                "https://example.invalid/delisted",
            ))
            .unwrap_err()
    };
    assert_eq!(error.code(), "source_unavailable");
    assert!(!store.root().exists());
    assert_eq!(snapshot_count(&db, SourceId::SzseDelisting), 0);
}

#[test]
fn the_same_fingerprint_and_bytes_do_not_add_a_snapshot() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![
        Ok(ok(szse_body(1, 2, 3, 2), "page-1")),
        Ok(ok(szse_body(2, 2, 3, 1), "page-2")),
        Ok(ok(szse_body(1, 2, 3, 2), "page-1")),
        Ok(ok(szse_body(2, 2, 3, 1), "page-2")),
    ]);
    let client = client(&fixture);
    {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client);
        let plan = szse_plan();
        let first = entry.acquire(&plan).unwrap();
        assert_eq!(first.reused, 0);
        let second = entry.acquire(&plan).unwrap();
        assert_eq!(second.reused, 2);
        assert!(
            second
                .snapshots
                .iter()
                .all(|snapshot| !snapshot.appended_manifest_line && !snapshot.inserted_index_row)
        );
    }
    assert_eq!(store.records().unwrap().len(), 2);
    assert_eq!(snapshot_count(&db, SourceId::SzseListing), 2);
    let replay = cli(&store, &db, "replay");
    assert_eq!(replay["scopes"][0]["snapshots"], 2);
}

#[test]
fn a_run_cache_serves_the_same_request_without_a_second_download() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![
        Ok(ok(szse_body(1, 2, 3, 2), "page-1")),
        Ok(ok(szse_body(2, 2, 3, 1), "page-2")),
    ]);
    let client = client(&fixture);
    let cache = Arc::new(RunCache::new());
    {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client).with_cache(cache.clone());
        let plan = szse_plan();
        let first = entry.acquire(&plan).unwrap();
        assert_eq!(first.reused, 0);
        assert_eq!(first.row_count, 3);
        assert_eq!(cache.len(), 2, "one cached page per request identity");
        let second = entry.acquire(&plan).unwrap();
        assert_eq!(second.row_count, 3);
        assert_eq!(second.reused, 2);
    }
    assert_eq!(fixture.calls(), 2, "the cached run made no request");
    assert_eq!(store.records().unwrap().len(), 2);
}

#[test]
fn replay_rebuilds_the_exact_index_after_a_wipe() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![
        Ok(ok(szse_body(1, 2, 3, 2), "page-1")),
        Ok(ok(szse_body(2, 2, 3, 1), "page-2")),
    ]);
    let client = client(&fixture);
    {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client);
        entry.acquire(&szse_plan()).unwrap();
    }
    let before = cli(&store, &db, "replay");
    assert_eq!(before["scopes"][0]["database"], "match");
    let summary = before["scopes"][0]["summary"].clone();
    assert!(summary.is_string());

    wipe(&db);
    assert_eq!(snapshot_count(&db, SourceId::SzseListing), 0);

    let after = cli(&store, &db, "replay");
    assert_eq!(after["status"], "ok");
    assert_eq!(after["scopes"][0]["database"], "match");
    assert_eq!(after["scopes"][0]["summary"], summary);
    assert_eq!(after["scopes"][0]["snapshots"], 2);
    assert_eq!(snapshot_count(&db, SourceId::SzseListing), 2);

    let verify = cli(&store, &db, "verify");
    assert_eq!(verify["scopes"][0]["database"], "match");
    assert_eq!(verify["scopes"][0]["summary"], summary);
}

#[test]
fn a_basis_below_the_threshold_is_refused_before_any_request() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![Ok(ok(sse_body(1, 1, 1, 1), "risky"))]);
    let client = client(&fixture);
    let backend = open(&db);
    let entry = Entry::new(&store, &backend, &client);
    let plan = plan(
        SourceId::SseSuspension,
        "https://example.invalid/suspension",
    )
    .basis(BasisStatus::OwnerRiskAccepted)
    .required_basis(BasisStatus::ApprovedSource);
    let error = entry.acquire(&plan).unwrap_err();
    assert_eq!(error.code(), "source_not_admitted");
    assert_eq!(fixture.calls(), 0);
    assert!(!store.root().exists());
}

#[test]
fn one_failed_window_makes_the_run_partially_failed() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![
        Ok(ok(sse_body(1, 1, 1, 1), "q1")),
        Ok(HttpResponse {
            status: 503,
            etag: None,
            last_modified: None,
            body: b"unavailable".to_vec(),
        }),
    ]);
    let client = client(&fixture);
    let first = Segment::parse("2000-01-01", "2000-03-31").unwrap();
    let second = Segment::parse("2000-04-01", "2000-06-30").unwrap();
    let error = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client);
        entry
            .acquire_windows(
                &plan(SourceId::SseBulletin, "https://example.invalid/bulletin"),
                &[first, second],
                "beginDate",
                "endDate",
            )
            .unwrap_err()
    };
    match error {
        AcquireError::PartiallyFailed {
            windows,
            failed,
            failures,
            succeeded,
        } => {
            assert_eq!(windows, 2);
            assert_eq!(failed, 1);
            assert_eq!(failures.len(), 1);
            assert_eq!(failures[0].window, second);
            assert_eq!(failures[0].code, "source_unavailable");
            assert_eq!(succeeded.len(), 1);
            assert_eq!(succeeded[0].window, first);
            assert_eq!(succeeded[0].snapshots.len(), 1);
        }
        other => panic!("expected partially_failed, got {}", other.code()),
    }
    // The successful window keeps its frozen bytes and its index row; the
    // failed window produced neither.
    assert_eq!(store.records().unwrap().len(), 1);
    let backend = open(&db);
    let binding = bind::bind(&backend, SourceId::SseBulletin, BasisStatus::ApprovedSource).unwrap();
    assert_eq!(binding.snapshots.len(), 1);
}
