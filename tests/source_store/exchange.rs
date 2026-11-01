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
    SSE_PARSER_VERSION_V1, SZSE_PARSER_VERSION, SZSE_PARSER_VERSION_V1, SZSE_PARSER_VERSION_V2,
};
use x_data_mcp::exchange_source::transport::{HttpRequest, HttpResponse};
use x_data_mcp::exchange_source::validate::parse_page;
use x_data_mcp::exchange_source::{
    AcquireError, AcquisitionPlan, BasisStatus, Client, Enabled, Entry, ExchangeRequest,
    PagingFormat, RetryPolicy, ReusePolicyTable, RunCache, Segment, SelectedBy, SourceId,
    Transport, TransportError, bind, freeze_page,
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

// ---------------------------------------------------------------------------
// R1 (XDM-153): reuse policy registration and read-through reuse
// (`docs/tasks/issue-446-exchange-refresh.md` §5.1, §5.2, §5.3, §8 R1).
// Every test is a fixture: the fake transport counts requests, so "no request"
// is a number, not a claim.
// ---------------------------------------------------------------------------

/// The declared table with only one source turned on, the way R2/R3 will pass
/// it once the refresh run exists.
fn reuse_on(source: SourceId) -> ReusePolicyTable {
    ReusePolicyTable::declared().with_enabled(source, Enabled::Yes)
}

fn day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
}

/// Freeze one SZSE page at a fixed observation time, so the age gates are
/// deterministic instead of depending on the wall clock.
fn freeze_szse_page(
    store: &SourceStore,
    backend: &MarketBackend,
    page: u32,
    fetched_at: &str,
    etag: &str,
) -> String {
    let request = ExchangeRequest::new(SourceId::SzseListing, "https://example.invalid/szse-list")
        .paged(PagingFormat::SzseMetadata, page);
    let response = ok(szse_body(page, 1, 1, 1), etag);
    let envelope = parse_page(PagingFormat::SzseMetadata, None, &response.body).unwrap();
    freeze_page(
        store,
        backend,
        &request,
        &response,
        &envelope,
        SZSE_PARSER_VERSION,
        BasisStatus::ApprovedSource,
        fetched_at,
    )
    .unwrap()
    .snapshot_id()
    .to_string()
}

/// The shipped table keeps every rule off: a second run fetches again and only
/// de-duplicates the same bytes, exactly as before R1.
#[test]
fn a_disabled_reuse_rule_keeps_fetching_page_by_page() {
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
        let first = entry.acquire(&szse_plan()).unwrap();
        assert_eq!(first.reused, 0);
        assert_eq!(first.evidence.policy.enabled, Enabled::No);
        assert_eq!(first.evidence.fetched, 2);
        assert_eq!(first.evidence.reused, 0);
        let second = entry.acquire(&szse_plan()).unwrap();
        // The old "same fingerprint + same bytes" de-duplication still happens
        // (two snapshots), but both pages were downloaded again.
        assert_eq!(second.reused, 2);
        assert_eq!(second.evidence.fetched, 2);
        assert_eq!(second.evidence.reused, 0);
        assert!(
            second
                .evidence
                .selected_by
                .iter()
                .all(|selection| selection.selected_by == SelectedBy::Fetch)
        );
    }
    assert_eq!(
        fixture.calls(),
        4,
        "the disabled rule sends both runs' requests"
    );
}

/// §5.2's acceptance: the first run sends `N` requests, the second sends none
/// and returns the very same snapshots; reuse advances neither `fetched_at`
/// nor `as_of` and writes neither a manifest line nor an index row.
#[test]
fn an_enabled_reuse_rule_reads_the_same_snapshots_without_a_request() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![
        Ok(ok(szse_body(1, 2, 3, 2), "page-1")),
        Ok(ok(szse_body(2, 2, 3, 1), "page-2")),
    ]);
    let client = client(&fixture);
    let first = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client);
        entry.acquire(&szse_plan()).unwrap()
    };
    assert_eq!(fixture.calls(), 2, "the first run fetches both pages");
    assert_eq!(store.records().unwrap().len(), 2);

    let second = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client)
            .with_reuse_policies(reuse_on(SourceId::SzseListing));
        entry.acquire(&szse_plan()).unwrap()
    };
    assert_eq!(fixture.calls(), 2, "the second run sends no request at all");
    assert_eq!(second.row_count, 3);
    assert_eq!(second.page_count, 2);
    assert_eq!(second.evidence.fetched, 0);
    assert_eq!(second.evidence.reused, 2);
    assert_eq!(second.evidence.policy.enabled, Enabled::Yes);
    assert_eq!(second.evidence.policy.source_id, SourceId::SzseListing);
    assert_eq!(
        second
            .evidence
            .selected_by
            .iter()
            .map(|selection| selection.selected_by)
            .collect::<Vec<_>>(),
        vec![SelectedBy::Reuse, SelectedBy::Reuse]
    );

    let first_ids: Vec<&str> = first.snapshots.iter().map(|s| s.snapshot_id()).collect();
    let second_ids: Vec<&str> = second.snapshots.iter().map(|s| s.snapshot_id()).collect();
    assert_eq!(first_ids, second_ids, "the same snapshots, not new ones");
    for (before, after) in first.snapshots.iter().zip(&second.snapshots) {
        assert_eq!(
            before.row.fetched_at, after.row.fetched_at,
            "reuse does not re-date bytes"
        );
        assert_eq!(
            before.row.as_of, after.row.as_of,
            "reuse does not advance as_of"
        );
        assert!(!after.appended_manifest_line && !after.inserted_index_row);
    }
    assert_eq!(second.evidence.as_of, first.evidence.as_of);
    assert_eq!(
        second.evidence.verified_through, first.evidence.verified_through,
        "without a refresh run the fallback is the selected as_of"
    );

    // Reuse is read-only: no blob, no manifest line, no index row.
    assert_eq!(store.records().unwrap().len(), 2);
    assert_eq!(snapshot_count(&db, SourceId::SzseListing), 2);

    // The replayed index still matches, so reuse did not fork the evidence.
    let replay = cli(&store, &db, "replay");
    assert_eq!(replay["scopes"][0]["database"], "match");
    assert_eq!(replay["scopes"][0]["snapshots"], 2);
}

/// §5.1's `H = 14` gate: exactly `H` days is still reusable, one day more
/// fails closed with `source_refresh_overdue` and sends no request.
#[test]
fn reuse_beyond_the_hard_age_bound_fails_closed() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![]);
    let client = client(&fixture);
    {
        let backend = open(&db);
        freeze_szse_page(&store, &backend, 1, "2026-10-31T02:00:00Z", "old-page");
    }

    let boundary = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client)
            .with_today(day("2026-11-14"))
            .with_reuse_policies(reuse_on(SourceId::SzseListing));
        entry.acquire(&szse_plan()).unwrap()
    };
    assert_eq!(boundary.evidence.reused, 1, "14 days is still within H");
    assert_eq!(
        boundary.evidence.verified_through.as_deref(),
        Some("2026-10-31")
    );

    let error = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client)
            .with_today(day("2026-11-15"))
            .with_reuse_policies(reuse_on(SourceId::SzseListing));
        entry.acquire(&szse_plan()).unwrap_err()
    };
    assert_eq!(error.code(), "source_refresh_overdue");
    assert!(
        error.to_string().contains("2026-10-31"),
        "the failure names the stale observation: {error}"
    );
    assert_eq!(
        fixture.calls(),
        0,
        "an overdue snapshot is not refetched here"
    );
    assert_eq!(store.records().unwrap().len(), 1);
    assert_eq!(snapshot_count(&db, SourceId::SzseListing), 1);
}

/// §5.2: bytes are read back with `read_verified` before reuse, so a tampered
/// blob is `source_store_corrupt` instead of a silent fallback.
#[test]
fn a_tampered_blob_fails_closed_on_reuse() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![]);
    let client = client(&fixture);
    let snapshot_id = {
        let backend = open(&db);
        freeze_szse_page(&store, &backend, 1, "2026-11-01T02:00:00Z", "page-1")
    };
    std::fs::write(store.blob_path(&snapshot_id), b"tampered").unwrap();

    let error = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client)
            .with_today(day("2026-11-02"))
            .with_reuse_policies(reuse_on(SourceId::SzseListing));
        entry.acquire(&szse_plan()).unwrap_err()
    };
    assert_eq!(error.code(), "source_store_corrupt");
    assert_eq!(fixture.calls(), 0);
}

/// §5.2's selection: when the same request fingerprint was answered with a new
/// sha (a revision), the newest observation is the one reused.
#[test]
fn a_revised_answer_for_the_same_fingerprint_is_the_one_reused() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![]);
    let client = client(&fixture);
    let (old_sha, new_sha) = {
        let backend = open(&db);
        let old = freeze_szse_page(&store, &backend, 1, "2026-10-30T02:00:00Z", "old-page");
        // A revision of the same request: different bytes, later observation.
        let request =
            ExchangeRequest::new(SourceId::SzseListing, "https://example.invalid/szse-list")
                .paged(PagingFormat::SzseMetadata, 1);
        let response = ok(szse_body(1, 1, 2, 2), "new-page");
        let envelope = parse_page(PagingFormat::SzseMetadata, None, &response.body).unwrap();
        let new = freeze_page(
            &store,
            &backend,
            &request,
            &response,
            &envelope,
            SZSE_PARSER_VERSION,
            BasisStatus::ApprovedSource,
            "2026-10-31T02:00:00Z",
        )
        .unwrap()
        .snapshot_id()
        .to_string();
        (old, new)
    };
    assert_eq!(
        snapshot_count(&db, SourceId::SzseListing),
        2,
        "two shas, one fingerprint"
    );

    let outcome = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client)
            .with_today(day("2026-11-01"))
            .with_reuse_policies(reuse_on(SourceId::SzseListing));
        entry.acquire(&szse_plan()).unwrap()
    };
    assert_eq!(fixture.calls(), 0);
    assert_eq!(outcome.snapshots.len(), 1);
    assert_eq!(outcome.snapshots[0].snapshot_id(), new_sha);
    assert_ne!(outcome.snapshots[0].snapshot_id(), old_sha);
    assert_eq!(
        outcome.row_count, 2,
        "the revised bytes are the derived input"
    );
    assert_eq!(outcome.evidence.as_of.as_deref(), Some("2026-10-31"));
    assert_eq!(outcome.evidence.reused, 1);
}

/// A multi-window run merges the per-window evidence in window order and keeps
/// §5.3's windowed `as_of` rule (newest window).
#[test]
fn a_multi_window_run_reuses_every_quarter_and_merges_the_evidence() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let db = scenario.db();
    let fixture = Fixture::new(vec![
        Ok(ok(sse_body(1, 1, 1, 1), "q1")),
        Ok(ok(sse_body(1, 1, 2, 2), "q2")),
    ]);
    let client = client(&fixture);
    let first = Segment::parse("2000-01-01", "2000-03-31").unwrap();
    let second = Segment::parse("2000-04-01", "2000-06-30").unwrap();
    let plan = plan(SourceId::SseBulletin, "https://example.invalid/bulletin");
    {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client);
        let outcome = entry
            .acquire_windows(&plan, &[first, second], "beginDate", "endDate")
            .unwrap();
        assert_eq!(outcome.results.len(), 2);
        assert_eq!(outcome.evidence.fetched, 2);
    }
    assert_eq!(fixture.calls(), 2);

    let reused = {
        let backend = open(&db);
        let entry = Entry::new(&store, &backend, &client)
            .with_reuse_policies(reuse_on(SourceId::SseBulletin));
        entry
            .acquire_windows(&plan, &[first, second], "beginDate", "endDate")
            .unwrap()
    };
    assert_eq!(
        fixture.calls(),
        2,
        "both quarters come from the source store"
    );
    assert_eq!(reused.results.len(), 2);
    assert_eq!(reused.evidence.reused, 2);
    assert_eq!(reused.evidence.fetched, 0);
    assert_eq!(reused.evidence.selected_by.len(), 2);
    assert!(
        reused
            .evidence
            .selected_by
            .iter()
            .all(|selection| selection.selected_by == SelectedBy::Reuse)
    );
    for result in &reused.results {
        assert_eq!(result.evidence.reused, 1);
        assert_eq!(result.evidence.fetched, 0);
    }
    assert_eq!(reused.evidence.policy.class.as_str(), "window");
    assert_eq!(store.records().unwrap().len(), 2);
}
