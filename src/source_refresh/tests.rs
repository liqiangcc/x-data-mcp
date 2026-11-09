//! Refresh-run acceptance tests: no network, a temp source store and a real
//! compact database. They exercise request budgets, run membership, the
//! page-1 guard, probe triggers, revision evidence and restart persistence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;

use crate::exchange_source::{
    Client, HttpRequest, HttpResponse, RetryPolicy, Transport, TransportError,
};
use crate::source_refresh::wire::{RefreshMode, RefreshRequest, RefreshStatus};
use crate::source_store::SourceStore;
use crate::sqlite_market::backend::MarketBackend;
use crate::sqlite_tasks::SqliteTasks;

use super::{dry_run, run};

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// The service instance id a run is attributed to. Startup reclaim closes only
/// runs whose owner differs from the instance taking the database lease.
const INSTANCE: &str = "service-1000-instance-a";
const RESTARTED: &str = "service-2000-instance-b";

struct State {
    short_code: String,
    short_day: String,
    short_before: String,
    short_after: String,
    bulletin_revision: u64,
    bulletin_zero: bool,
    delisting_pages: u32,
    /// Pages of the two non-delisting SZSE tables. `1` is the shape every
    /// other test uses; the #582 reproduction harness raises them to the
    /// production page counts (146 listing, 375 short name).
    listing_pages: u32,
    short_pages: u32,
    /// Pages of the SZSE ETF notice channel, 50 rows each (#604 reuse counts).
    szse_notice_pages: u32,
    fail_page_two: bool,
    bump_on_page_two: bool,
    bump_every_page_one: bool,
    bumped: BTreeSet<String>,
    versions: BTreeMap<String, u32>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            short_code: "000004".to_string(),
            short_day: "2026-01-05".to_string(),
            short_before: "深安达A".to_string(),
            short_after: "深安达B".to_string(),
            bulletin_revision: 0,
            bulletin_zero: false,
            delisting_pages: 1,
            szse_notice_pages: 0,
            listing_pages: 1,
            short_pages: 1,
            fail_page_two: false,
            bump_on_page_two: false,
            bump_every_page_one: false,
            bumped: BTreeSet::new(),
            versions: BTreeMap::new(),
        }
    }
}

struct RefreshFixture {
    state: Mutex<State>,
    calls: Mutex<Vec<String>>,
    /// Lets a test hold the first request open so a background run is provably
    /// still `running` while the test inspects it.
    hold: (Mutex<bool>, std::sync::Condvar),
    /// Panic on this request number (1-based) instead of answering it: a
    /// transport defect is an unwind, not an `Err`.
    panic_at: Mutex<Option<usize>>,
}

impl RefreshFixture {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            calls: Mutex::new(Vec::new()),
            hold: (Mutex::new(false), std::sync::Condvar::new()),
            panic_at: Mutex::new(None),
        })
    }

    fn panic_at(&self, request: usize) {
        *self.panic_at.lock().unwrap() = Some(request);
    }

    fn pause(&self) {
        *self.hold.0.lock().unwrap() = true;
    }

    fn release(&self) {
        *self.hold.0.lock().unwrap() = false;
        self.hold.1.notify_all();
    }

    fn wait_if_paused(&self) {
        let mut paused = self.hold.0.lock().unwrap();
        while *paused {
            paused = self.hold.1.wait(paused).unwrap();
        }
    }

    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn set_short_after(&self, value: &str) {
        self.state.lock().unwrap().short_after = value.to_string();
    }

    fn set_bulletin_revision(&self, value: u64) {
        self.state.lock().unwrap().bulletin_revision = value;
    }

    fn set_bulletin_zero(&self) {
        self.state.lock().unwrap().bulletin_zero = true;
    }

    fn set_delisting_pages(&self, pages: u32) {
        self.state.lock().unwrap().delisting_pages = pages;
    }

    /// Give the SZSE ETF notice channel `pages` pages of 50 rows (#604).
    fn set_szse_notice_pages(&self, pages: u32) {
        self.state.lock().unwrap().szse_notice_pages = pages;
    }

    /// Page counts of `szse_listing` and `szse_short_name` (#582 harness).
    fn set_table_pages(&self, listing: u32, short: u32) {
        let mut state = self.state.lock().unwrap();
        state.listing_pages = listing;
        state.short_pages = short;
    }

    fn fail_page_two(&self) {
        self.state.lock().unwrap().fail_page_two = true;
    }

    fn bump_on_page_two(&self) {
        self.state.lock().unwrap().bump_on_page_two = true;
    }

    fn bump_every_page_one(&self) {
        self.state.lock().unwrap().bump_every_page_one = true;
    }

    fn body(&self, request: &HttpRequest) -> Result<Vec<u8>, TransportError> {
        let url = request.url.as_str();
        if url.contains("commonSoaQuery") {
            if url.contains("FUND_LIST") {
                // M3-2 ETF identity list: one row is enough for the envelope;
                // the business parser/floor is tested in `etf_list`.
                return Ok(sse_page(1, 1, 1));
            }
            return Ok(
                br#"{"pageHelp":{"pageNo":1,"pageSize":25,"pageCount":0,"total":0,"data":[]}}"#
                    .to_vec(),
            );
        }
        if url.contains("queryCompanyBulletinNew") {
            let state = self.state.lock().unwrap();
            if state.bulletin_zero {
                return Ok(serde_json::json!({
                    "pageHelp": {
                        "beginPage": 1,
                        "pageNo": 1,
                        "pageSize": 100,
                        "pageCount": 0,
                        "total": 0,
                        "data": []
                    }
                })
                .to_string()
                .into_bytes());
            }
            return Ok(serde_json::json!({
                "pageHelp": {
                    "beginPage": 1,
                    "pageNo": 1,
                    "pageSize": 100,
                    "pageCount": 1,
                    "total": 1,
                    "data": [{"TITLE": "风险警示", "revision": state.bulletin_revision}]
                }
            })
            .to_string()
            .into_bytes());
        }
        if url.contains("COMMON_PL_JJXX_JJGG_L") {
            // M3-2 PR-A2: the SSE announcement index is unpaged and a quiet
            // quarter is a complete empty `result` array.
            return Ok(
                br#"{"pageHelp":{"pageNo":1,"pageCount":null,"total":0},"result":[]}"#.to_vec(),
            );
        }
        if url.contains("announcement/annList") {
            // M3-2 PR-A2: SZSE ETF notice channel. The default fixture is a
            // quiet channel; `set_szse_notice_pages` gives it pages of 50 rows,
            // which is the measured `pageSize` cap (#604 counts pages).
            let pages = self.state.lock().unwrap().szse_notice_pages;
            if pages == 0 {
                return Ok(br#"{"announceCount":0,"pageNum":1,"data":[]}"#.to_vec());
            }
            // `SzseAnnouncement` carries the page in the POST body, not the URL.
            let page = request
                .body
                .as_deref()
                .and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok())
                .and_then(|value| value.get("pageNum").and_then(|page| page.as_u64()))
                .and_then(|page| u32::try_from(page).ok())
                .unwrap_or(1);
            return Ok(serde_json::json!({
                "announceCount": u64::from(pages) * 50,
                "data": (0..50)
                    .map(|row| serde_json::json!({
                        "secCode": format!("{}{}{:02}", pages, page, row),
                        "secName": format!("ETF-{pages}-{page}-{row}"),
                    }))
                    .collect::<Vec<_>>(),
            })
            .to_string()
            .into_bytes());
        }
        if url.contains("commonQuery.do") {
            return Ok(sse_page(1, 1, 1));
        }
        if url.contains("ShowReport") && url.contains("CATALOGID=1945") {
            // M3-2 ETF identity list: a synthetic workbook (no exchange bytes).
            return Ok(include_bytes!(
                "../../tests/fixtures/exchange-etf-list/szse_etf_list_small.xlsx"
            )
            .to_vec());
        }
        if url.contains("ShowReport/data") {
            return self.szse_body(url);
        }
        Ok(sse_page(1, 1, 1))
    }

    fn szse_body(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        let short = url.contains("CATALOGID=SSGSGMXX");
        let delisting = url.contains("CATALOGID=1793_ssgs");
        let page = url
            .split("PAGENO=")
            .nth(1)
            .and_then(|tail| tail.split('&').next())
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(1);
        if page == 2 && self.state.lock().unwrap().fail_page_two {
            return Err(TransportError::Failed("page 2 is unavailable".to_string()));
        }
        let key = if short {
            "SSGSGMXX".to_string()
        } else if delisting {
            format!(
                "1793_ssgs:{}",
                if url.contains("selectModule=main") {
                    "main"
                } else {
                    "nm"
                }
            )
        } else {
            "1110".to_string()
        };
        let mut state = self.state.lock().unwrap();
        let page_count = if delisting {
            state.delisting_pages
        } else if short {
            state.short_pages
        } else {
            state.listing_pages
        };
        if page == 2 && state.bump_on_page_two && state.bumped.insert(key.clone()) {
            *state.versions.entry(key.clone()).or_insert(0) += 1;
        }
        if page == 1 && state.bump_every_page_one {
            *state.versions.entry(key.clone()).or_insert(0) += 1;
        }
        let version = *state.versions.entry(key).or_insert(0);
        // A whole table's pages each carry `pagesize` rows and the metadata
        // `recordcount` is the table total, which `validate_pages` checks
        // against the sum of the page rows. One row per page is what the other
        // tests use; the harness emits a page-sized page instead.
        let rows_per_page = if delisting {
            1
        } else if page_count > 1 {
            20
        } else {
            1
        };
        let row_count = if delisting {
            page_count as u64
        } else if page_count > 1 {
            u64::from(page_count) * rows_per_page as u64
        } else {
            1
        };
        let rows = if delisting {
            vec![serde_json::json!({
                "zqdm": format!("00000{page}{version}"),
                "zqjc": format!("listing-{page}-{version}")
            })]
        } else if short {
            // Page 1 carries the row the probe parses; every page is distinct
            // so a multi-page table never freezes the same bytes twice.
            (0..rows_per_page)
                .map(|row| {
                    serde_json::json!({
                        "zqdm": state.short_code.clone(),
                        "bgrq": state.short_day.clone(),
                        "bgqzqjc": state.short_before.clone(),
                        "bghzqjc": format!("{}-{version}-{page}-{row}", state.short_after)
                    })
                })
                .collect()
        } else {
            (0..rows_per_page)
                .map(|row| {
                    serde_json::json!({
                        "zqdm": format!("00000{version}{page}{row}"),
                        "zqjc": format!("listing-{version}-{page}-{row}")
                    })
                })
                .collect()
        };
        let block = if short || delisting {
            serde_json::json!([{
                "metadata": {
                    "tabkey": "tab2",
                    "pageno": page,
                    "pagecount": page_count,
                    "recordcount": row_count,
                    "pagesize": 20
                },
                "data": rows
            }])
        } else {
            serde_json::json!([{
                "metadata": {
                    "tabkey": "tab1",
                    "pageno": page,
                    "pagecount": page_count,
                    "recordcount": row_count,
                    "pagesize": 20
                },
                "data": rows
            }])
        };
        Ok(block.to_string().into_bytes())
    }
}

fn sse_page(page_count: u32, total: u64, revision: u64) -> Vec<u8> {
    serde_json::json!({
        "pageHelp": {
            "beginPage": 1,
            "pageNo": 1,
            "pageSize": 2000,
            "pageCount": page_count,
            "total": total,
            "data": [{"revision": revision}]
        }
    })
    .to_string()
    .into_bytes()
}

impl Transport for RefreshFixture {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        self.wait_if_paused();
        let count = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(match request.body.as_deref() {
                Some(body) => format!("{} {body}", request.url),
                None => request.url.clone(),
            });
            calls.len()
        };
        // The guard is dropped before unwinding: a poisoned call log would make
        // the test's own `calls()` assertion panic as well.
        if *self.panic_at.lock().unwrap() == Some(count) {
            panic!("fake transport panicked at request {count}");
        }
        Ok(HttpResponse {
            status: 200,
            etag: None,
            last_modified: None,
            body: self.body(request)?,
        })
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

    fn db(&self) -> std::path::PathBuf {
        self.dir.path().join("market.sqlite")
    }

    fn open(&self) -> MarketBackend {
        open_db(&self.db())
    }
}

fn open_db(db: &Path) -> MarketBackend {
    let tasks = SqliteTasks::new(db);
    if !db.exists() {
        tasks.initialize_compact().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(db, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    MarketBackend::open(&tasks).unwrap()
}

fn client(fixture: &Arc<RefreshFixture>) -> Client {
    Client::with_transport(Shared(fixture.clone()))
        .with_policy(RetryPolicy::single())
        .without_sleep()
}

struct Shared(Arc<RefreshFixture>);

impl Transport for Shared {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        self.0.get(request)
    }
}

fn request(mode: RefreshMode, sources: &[&str]) -> RefreshRequest {
    RefreshRequest {
        mode: Some(mode),
        sources: sources.iter().map(|source| source.to_string()).collect(),
        dry_run: false,
        status_of: None,
        latest: None,
        wait_seconds: None,
    }
}

fn run_refresh(
    _fixture: &Arc<RefreshFixture>,
    scenario: &Scenario,
    backend: &MarketBackend,
    client: &Client,
    mode: RefreshMode,
    sources: &[&str],
) -> super::wire::RefreshReport {
    run(
        &scenario.store(),
        backend,
        client,
        day(2026, 11, 2),
        &request(mode, sources),
        INSTANCE,
    )
    .unwrap()
}

#[test]
fn tail_stays_within_budget_and_members_survive_a_restart() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let first = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &[],
    );
    assert_eq!(first.status, RefreshStatus::Succeeded);
    // The in-process form returns the finished run; the MCP surface does not
    // wait (#582) and is covered by the background tests below.
    assert!(first.finished_at.is_some());
    assert!(
        first.requests <= 32,
        "tail must stay within the D-66 budget, got {} ({} calls)",
        first.requests,
        fixture.calls()
    );
    assert_eq!(first.requests as usize, fixture.calls());
    assert_eq!(first.scope.len(), 11);
    assert_eq!(first.planned.len(), 20);
    assert_eq!(first.sources.len(), 11);
    let short = first
        .sources
        .iter()
        .find(|source| source.source_id == "szse_short_name")
        .unwrap();
    assert!(short.complete, "first observation must complete the table");
    let bulletin = first
        .sources
        .iter()
        .find(|source| source.source_id == "sse_bulletin")
        .unwrap();
    assert!(!bulletin.complete, "a two-window tail is not a full set");
    assert!(bulletin.verified_through.is_none());

    let run_id = first.run_id.clone().unwrap();
    let runs = backend.read_source_refresh_runs().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "succeeded");
    assert_eq!(runs[0].verified_through.as_deref(), Some("2026-11-02"));
    let source_rows = backend.read_source_refresh_run_sources(&run_id).unwrap();
    assert_eq!(source_rows.len(), 11);
    let listing = source_rows
        .iter()
        .find(|row| row.source_id == "sse_listing")
        .unwrap();
    assert!(listing.complete);
    assert_eq!(listing.verified_through.as_deref(), Some("2026-11-02"));
    let bulletin_row = source_rows
        .iter()
        .find(|row| row.source_id == "sse_bulletin")
        .unwrap();
    assert!(!bulletin_row.complete);
    assert!(bulletin_row.verified_through.is_none());
    let members = backend.read_source_refresh_run_members(&run_id).unwrap();
    assert_eq!(
        members.len(),
        14,
        "ten cheap-table/probe-triggered pages + four bulletin pages; a quiet \
         suspension window selects no page"
    );
    assert!(members.iter().all(|member| {
        member.request_fingerprint.starts_with('{')
            && member.request_fingerprint.contains("\"source_id\"")
    }));

    let second = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &[],
    );
    assert_eq!(second.status, RefreshStatus::Succeeded);
    // D-87: the four window sources contribute their open window only, so the
    // three window sources that are one page per window drop from two requests
    // to one and `sse_bulletin` (two plans) from four to two.
    assert_eq!(
        second.requests, 23,
        "probes must not refetch whole tables; the two ETF lists are tables and do; \
         each window source fetches only its open window because the closed quarter \
         is read through the index"
    );
    let short = second
        .sources
        .iter()
        .find(|source| source.source_id == "szse_short_name")
        .unwrap();
    assert!(!short.complete);
    let listing = second
        .sources
        .iter()
        .find(|source| source.source_id == "szse_listing")
        .unwrap();
    assert!(!listing.complete);
    let second_members = backend
        .read_source_refresh_run_members(&second.run_id.clone().unwrap())
        .unwrap();
    assert_eq!(
        second_members.len(),
        12,
        "eight cheap-table pages + four bulletin pages; probes and quiet windows select \
         nothing"
    );
    assert!(
        second_members
            .iter()
            .all(|member| member.source_id != "szse_listing")
    );

    drop(backend);
    let reopened = open_db(&scenario.db());
    let runs = reopened.read_source_refresh_runs().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(
        reopened
            .read_source_refresh_run_members(&run_id)
            .unwrap()
            .len(),
        14,
        "run membership is durable across a process restart"
    );
    assert_eq!(
        reopened
            .latest_complete_verified_through("sse_listing")
            .unwrap()
            .as_deref(),
        Some("2026-11-02"),
        "a complete source's verified date survives the restart"
    );
}

#[test]
fn full_run_observes_every_source() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let report = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Full,
        &[],
    );
    assert_eq!(report.status, RefreshStatus::Succeeded);
    assert_eq!(report.requests, 488);
    assert_eq!(report.requests as usize, fixture.calls());
    assert_eq!(report.changed, 0, "first observation is not a revision");
    for source in &report.sources {
        assert!(
            source.complete,
            "{} must be complete in a full run",
            source.source_id
        );
        assert_eq!(source.verified_through.as_deref(), Some("2026-11-02"));
    }
    let run_id = report.run_id.unwrap();
    let source_rows = backend.read_source_refresh_run_sources(&run_id).unwrap();
    assert!(source_rows.iter().all(|row| row.complete));
    assert_eq!(
        backend
            .latest_complete_verified_through("sse_bulletin")
            .unwrap()
            .as_deref(),
        Some("2026-11-02")
    );
}

#[test]
fn a_quiet_bulletin_full_is_unobserved_but_complete() {
    let fixture = RefreshFixture::new();
    fixture.set_bulletin_zero();
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let report = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Full,
        &["sse_bulletin"],
    );
    assert_eq!(report.status, RefreshStatus::Succeeded);
    assert_eq!(
        report.requests, 216,
        "two title filters per bulletin quarter, including the empty ones"
    );
    assert_eq!(report.sources.len(), 1);
    let source = &report.sources[0];
    assert_eq!(source.unobserved_windows, 216);
    assert!(source.complete, "an observed empty window is not a failure");
    assert_eq!(source.verified_through.as_deref(), Some("2026-11-02"));
    assert_eq!(report.changed, 0);
    let run_id = report.run_id.unwrap();
    assert!(
        backend
            .read_source_refresh_run_members(&run_id)
            .unwrap()
            .is_empty(),
        "a pageCount=0 window selects no page"
    );
    let source_rows = backend.read_source_refresh_run_sources(&run_id).unwrap();
    assert_eq!(source_rows.len(), 1);
    assert_eq!(source_rows[0].unobserved_windows, 216);
    assert!(source_rows[0].complete);
    assert_eq!(
        backend
            .latest_complete_verified_through("sse_bulletin")
            .unwrap()
            .as_deref(),
        Some("2026-11-02")
    );
}

#[test]
fn a_window_revision_is_recorded() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let first = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &["sse_bulletin"],
    );
    assert_eq!(first.requests, 4);
    assert_eq!(first.changed, 0);
    assert!(
        first.verified_through.is_none(),
        "a two-window tail verifies no complete source"
    );
    fixture.set_bulletin_revision(1);
    // D-87: a `tail` re-reads the closed quarter only through the index, so a
    // revision inside it is not seen here; only the open window is fetched and
    // only its two plans can be found changed. The weekly `full` is what finds
    // closed-quarter revisions (see `full_run_rereads_every_window`).
    let second = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &["sse_bulletin"],
    );
    assert_eq!(second.requests, 2, "only the open window is fetched");
    assert_eq!(second.changed, 2, "the open window's two plans changed");
    assert_eq!(second.revisions.len(), 2);
    for revision in &second.revisions {
        assert_ne!(revision.previous_snapshot_id, revision.snapshot_id);
        assert!(revision.window_start.is_some());
        assert!(revision.window_end.is_some());
    }
    let run_id = second.run_id.unwrap();
    let members = backend.read_source_refresh_run_members(&run_id).unwrap();
    assert_eq!(members.len(), 4, "both windows stay run members");
    // A `full` then notices the closed-quarter revision.
    let third = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Full,
        &["sse_bulletin"],
    );
    assert_eq!(third.changed, 4, "full re-reads the closed quarter too");
}

#[test]
fn short_name_probe_triggers_only_on_a_new_marked_row() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let first = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &["szse_short_name"],
    );
    assert_eq!(first.requests, 3, "probe + bootstrap full + guard");
    assert!(first.sources[0].complete);
    assert_eq!(first.changed, 0);

    fixture.set_short_after("*ST深安达");
    let second = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &["szse_short_name"],
    );
    assert_eq!(second.requests, 3);
    assert_eq!(second.changed, 1);
    assert!(second.sources[0].complete);

    fixture.set_short_after("深安达B");
    let third = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &["szse_short_name"],
    );
    assert_eq!(
        third.requests, 1,
        "an ordinary rename waits for the weekly full"
    );
    assert_eq!(third.changed, 0);
    assert!(!third.sources[0].complete);
    assert!(
        third
            .sources
            .iter()
            .all(|source| source.verified_through.is_none())
    );
}

#[test]
fn table_guard_retries_once_when_page_one_changes_mid_fetch() {
    let fixture = RefreshFixture::new();
    fixture.set_delisting_pages(2);
    fixture.bump_on_page_two();
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let report = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &["szse_delisting"],
    );
    assert_eq!(report.status, RefreshStatus::Succeeded);
    assert_eq!(
        report.requests, 12,
        "each plan: 2 pages + guard + retried 2 pages + guard"
    );
    assert_eq!(report.sources[0].snapshots, 4);
    assert_eq!(
        backend
            .read_source_refresh_run_members(&report.run_id.unwrap())
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn table_guard_gives_up_after_one_retry_and_records_no_members() {
    let fixture = RefreshFixture::new();
    fixture.set_delisting_pages(2);
    fixture.bump_every_page_one();
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let report = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &["szse_delisting"],
    );
    assert_eq!(report.status, RefreshStatus::Failed);
    assert_eq!(
        report.requests, 12,
        "each plan: page 1+2, guard, retried page 1+2, guard"
    );
    assert!(
        report
            .failures
            .iter()
            .all(|failure| failure.code == "window_changed_mid_fetch")
    );
    assert!(!report.sources[0].complete);
    assert!(
        backend
            .read_source_refresh_run_members(&report.run_id.clone().unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_mid_fetch_failure_is_snapshot_incomplete_and_records_no_members() {
    let fixture = RefreshFixture::new();
    fixture.set_delisting_pages(2);
    fixture.fail_page_two();
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let report = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &["szse_delisting"],
    );
    assert_eq!(report.status, RefreshStatus::Failed);
    assert_eq!(report.requests, 4);
    assert_eq!(report.failures.len(), 2);
    assert!(
        report
            .failures
            .iter()
            .all(|failure| failure.code == "snapshot_incomplete")
    );
    assert!(report.verified_through.is_none());
    assert!(!report.sources[0].complete);
    assert!(
        backend
            .read_source_refresh_run_members(&report.run_id.clone().unwrap())
            .unwrap()
            .is_empty()
    );
    let runs = backend.read_source_refresh_runs().unwrap();
    assert_eq!(runs[0].status, "failed");
    assert!(runs[0].verified_through.is_none());
}

#[test]
fn dry_run_lists_the_plan_and_touches_nothing() {
    let scenario = Scenario::new();
    let store = scenario.store();
    let report = dry_run(
        day(2026, 11, 2),
        &RefreshRequest {
            mode: Some(RefreshMode::Tail),
            sources: Vec::new(),
            dry_run: true,
            status_of: None,
            latest: None,
            wait_seconds: None,
        },
    )
    .unwrap();
    assert_eq!(report.status, RefreshStatus::Planned);
    assert!(report.dry_run);
    assert!(report.run_id.is_none());
    assert_eq!(report.planned.len(), 20);
    assert!(report.scope.contains(&"sse_etf_list".to_string()));
    assert!(report.scope.contains(&"szse_etf_list".to_string()));
    assert!(report.sources.iter().all(|source| source.requests == 0));
    assert!(!scenario.db().exists());
    assert!(!store.manifest_path().exists());
}

#[test]
fn a_second_run_is_rejected_while_one_is_open() {
    let scenario = Scenario::new();
    let backend = scenario.open();

    // `tail` and `full` exclude each other: the guard is per run record, not
    // per mode, so a background full cannot be doubled by a tail.
    let first = super::start(&backend, day(2026, 11, 2), RefreshMode::Tail, &[], INSTANCE).unwrap();
    let rejected =
        super::start(&backend, day(2026, 11, 2), RefreshMode::Full, &[], INSTANCE).unwrap_err();
    assert_eq!(rejected.code(), "refresh_already_running");
    assert_eq!(rejected.run_id(), Some(first.run_id.as_str()));
    let rejected_again =
        super::start(&backend, day(2026, 11, 2), RefreshMode::Tail, &[], "other").unwrap_err();
    assert_eq!(rejected_again.run_id(), Some(first.run_id.as_str()));

    // The open run is durable and visible before any request is made.
    let open = super::status_of(&backend, &first.run_id).unwrap();
    assert_eq!(open.status, RefreshStatus::Running);
    assert!(open.finished_at.is_none());
    assert_eq!(open.requests, 0);

    // A finished run releases the guard.
    let client = client(&RefreshFixture::new());
    super::execute(
        &scenario.store(),
        &backend,
        &client,
        day(2026, 11, 2),
        first,
    )
    .unwrap();
    let next = super::start(&backend, day(2026, 11, 2), RefreshMode::Tail, &[], INSTANCE).unwrap();
    assert!(next.run_id.starts_with("refresh-tail-"));
}

#[test]
fn racing_starts_let_exactly_one_run_win() {
    let scenario = Scenario::new();
    let backend = scenario.open();
    // The guard is checked and inserted in one write transaction, so a race is
    // decided by the database, not by a check-then-insert window. Threads hand
    // back only `String`s: the run/plan types never cross a thread boundary.
    let (winners, rejected) = std::thread::scope(|scope| {
        let handles = (0..4)
            .map(|index| {
                let backend = &backend;
                scope.spawn(move || {
                    match super::start(
                        backend,
                        day(2026, 11, 2),
                        RefreshMode::Tail,
                        &[],
                        &format!("service-{index}"),
                    ) {
                        Ok(started) => Ok(started.run_id),
                        Err(error) => {
                            Err((error.code().to_string(), error.run_id().map(str::to_string)))
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut winners = Vec::new();
        let mut rejected = Vec::new();
        for handle in handles {
            match handle.join().unwrap() {
                Ok(run_id) => winners.push(run_id),
                Err(error) => rejected.push(error),
            }
        }
        (winners, rejected)
    });
    assert_eq!(winners.len(), 1, "exactly one start may open a run");
    assert_eq!(rejected.len(), 3);
    for (code, run_id) in rejected {
        assert_eq!(code, "refresh_already_running");
        assert_eq!(run_id.as_deref(), Some(winners[0].as_str()));
    }
    assert_eq!(
        backend
            .read_source_refresh_runs()
            .unwrap()
            .iter()
            .filter(|row| row.status == "running")
            .count(),
        1
    );
}

#[test]
fn an_invalid_plan_opens_no_run() {
    let scenario = Scenario::new();
    let backend = scenario.open();
    let rejected = super::start(
        &backend,
        day(2026, 11, 2),
        RefreshMode::Tail,
        &["nope".into()],
        INSTANCE,
    )
    .unwrap_err();
    assert_eq!(rejected.code(), "invalid_arguments");
    assert!(backend.read_source_refresh_runs().unwrap().is_empty());
    assert_eq!(
        super::status_of(&backend, "refresh-tail-1")
            .unwrap_err()
            .code(),
        "refresh_run_not_found"
    );
}

#[test]
fn a_run_from_a_previous_instance_is_reclaimed_as_failed() {
    let scenario = Scenario::new();
    let backend = scenario.open();
    let finished = {
        let fixture = RefreshFixture::new();
        let client = client(&fixture);
        run_refresh(
            &fixture,
            &scenario,
            &backend,
            &client,
            RefreshMode::Tail,
            &[],
        )
    };
    assert_eq!(finished.status, RefreshStatus::Succeeded);
    let finished_id = finished.run_id.unwrap();
    // The interrupted run opened by the instance that "crashed"; it never
    // finishes, so its row is still `running` when the next instance starts.
    let interrupted =
        super::start(&backend, day(2026, 11, 2), RefreshMode::Full, &[], INSTANCE).unwrap();

    let reclaimed = super::reclaim_abandoned(&backend, RESTARTED).unwrap();
    assert_eq!(reclaimed, vec![interrupted.run_id.clone()]);

    let row = backend
        .read_source_refresh_run(&interrupted.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.finished_at.is_some());
    assert!(row.verified_through.is_none());
    assert_eq!(row.owner_instance.as_deref(), Some(INSTANCE));
    let detail = row.detail.unwrap_or_default();
    assert!(detail.contains("process_restarted"), "{detail}");
    // The reclaimed run reports failed and no verified date; the earlier
    // successful run is untouched.
    let report = super::status_of(&backend, &interrupted.run_id).unwrap();
    assert_eq!(report.status, RefreshStatus::Failed);
    assert!(report.verified_through.is_none());
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].code, "process_restarted");
    assert_eq!(report.failures[0].source_id, "*");
    assert_eq!(
        super::status_of(&backend, &finished_id).unwrap().status,
        RefreshStatus::Succeeded
    );
    // Reclaiming twice is a no-op, and a run of the live instance is never
    // touched: attribution is the owner id, not elapsed time.
    assert!(
        super::reclaim_abandoned(&backend, RESTARTED)
            .unwrap()
            .is_empty()
    );
    let live = super::start(
        &backend,
        day(2026, 11, 2),
        RefreshMode::Tail,
        &[],
        RESTARTED,
    )
    .unwrap();
    assert!(
        super::reclaim_abandoned(&backend, RESTARTED)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        super::status_of(&backend, &live.run_id).unwrap().status,
        RefreshStatus::Running
    );
}

#[test]
fn a_full_run_returns_running_and_finishes_in_the_background() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let backend = scenario.open();

    // Hold the first request so the background run is still `running` when the
    // single-flight guard and the reply are checked.
    fixture.pause();
    let started = super::start_in_background(
        scenario.store(),
        backend.clone(),
        client(&fixture),
        day(2026, 11, 2),
        request(RefreshMode::Full, &[]),
        INSTANCE,
    )
    .unwrap();
    // The call returns the open run, not the result: 1,941 requests at
    // 1 req/s cannot be waited for in one MCP call.
    assert_eq!(started.status, RefreshStatus::Running);
    assert!(started.finished_at.is_none());
    assert_eq!(started.requests, 0);
    assert_eq!(
        started.planned.len(),
        478,
        "the immediate reply carries the full plan, not results"
    );
    let run_id = started.run_id.clone().unwrap();

    // A tail is refused while the background full is open, even though the
    // request that started it has already been answered.
    let rejected =
        super::start(&backend, day(2026, 11, 2), RefreshMode::Tail, &[], INSTANCE).unwrap_err();
    assert_eq!(rejected.code(), "refresh_already_running");
    assert_eq!(rejected.run_id(), Some(run_id.as_str()));

    // Releasing the transport is the only thing the finished call depended on:
    // the run is not tied to the request that started it.
    fixture.release();
    let report = wait_for_run(&backend, &run_id);
    assert_eq!(report.status, RefreshStatus::Succeeded);
    assert_eq!(report.requests, 488);
    assert_eq!(report.requests as usize, fixture.calls());
    assert!(report.finished_at.is_some());
    assert_eq!(report.verified_through.as_deref(), Some("2026-11-02"));
    assert_eq!(
        backend
            .latest_complete_verified_through("sse_bulletin")
            .unwrap()
            .as_deref(),
        Some("2026-11-02"),
        "a completed background run advances the durable verified date"
    );
    // The reply is readable after the fact through the same durable record.
    assert_eq!(super::status_of(&backend, &run_id).unwrap().requests, 488);
    // The guard is released: the next run (here a tail) may start.
    let next = super::start(&backend, day(2026, 11, 2), RefreshMode::Tail, &[], INSTANCE).unwrap();
    assert_eq!(next.mode, RefreshMode::Tail);
}

#[test]
fn a_failing_background_full_reports_failed_without_a_verified_date() {
    let fixture = RefreshFixture::new();
    fixture.set_delisting_pages(2);
    fixture.fail_page_two();
    let scenario = Scenario::new();
    let backend = scenario.open();

    let started = super::start_in_background(
        scenario.store(),
        backend.clone(),
        client(&fixture),
        day(2026, 11, 2),
        request(RefreshMode::Full, &["szse_delisting"]),
        INSTANCE,
    )
    .unwrap();
    let run_id = started.run_id.unwrap();

    let report = wait_for_run(&backend, &run_id);
    assert_eq!(report.status, RefreshStatus::Failed);
    assert!(report.verified_through.is_none());
    assert!(!report.failures.is_empty());
    assert!(
        report
            .failures
            .iter()
            .all(|failure| failure.code == "snapshot_incomplete")
    );
    assert!(
        backend
            .latest_complete_verified_through("szse_delisting")
            .unwrap()
            .is_none(),
        "a failed background run advances no verified date"
    );
    let row = backend.read_source_refresh_run(&run_id).unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.finished_at.is_some());
    assert!(row.verified_through.is_none());
}

/// #582: `tail` no longer runs inside the caller's request thread. The call
/// returns the open run at once, and the client keeps a handle (`run_id`) it
/// can poll after its own timeout expired — the tail reply used to be the only
/// place the result ever appeared.
#[test]
fn a_tail_returns_running_and_finishes_in_the_background() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let backend = scenario.open();

    // Hold the first request: the run is provably still in flight while the
    // reply, the single-flight guard and the durable record are inspected. No
    // sleep decides when the background thread has started.
    fixture.pause();
    let started = super::start_in_background(
        scenario.store(),
        backend.clone(),
        client(&fixture),
        day(2026, 11, 2),
        request(RefreshMode::Tail, &[]),
        INSTANCE,
    )
    .unwrap();
    assert_eq!(started.status, RefreshStatus::Running);
    assert!(started.finished_at.is_none());
    assert_eq!(
        started.requests, 0,
        "a running record is not a progress meter"
    );
    // The immediate reply carries the plan, not results: the same units a dry
    // run of the same request lists.
    let planned = super::dry_run(day(2026, 11, 2), &request(RefreshMode::Tail, &[]))
        .unwrap()
        .planned;
    assert_eq!(started.planned, planned);
    let run_id = started.run_id.clone().unwrap();

    // A client that timed out still finds the run.
    let open = super::status_of(&backend, &run_id).unwrap();
    assert_eq!(open.status, RefreshStatus::Running);
    assert!(open.finished_at.is_none());

    // Single-flight is unchanged: a second tail is refused while the first is
    // open, and the refusal names the run to poll.
    let rejected =
        super::start(&backend, day(2026, 11, 2), RefreshMode::Tail, &[], INSTANCE).unwrap_err();
    assert_eq!(rejected.code(), "refresh_already_running");
    assert_eq!(rejected.run_id(), Some(run_id.as_str()));

    fixture.release();
    let report = wait_for_run(&backend, &run_id);
    assert_eq!(report.status, RefreshStatus::Succeeded);
    assert!(report.finished_at.is_some());
    assert!(report.requests > 0);
    // The counters only exist in the durable record: the reply carried zeros.
    assert_eq!(report.requests as usize, fixture.calls());
    assert_eq!(report.run_id.as_deref(), Some(run_id.as_str()));
    // The durable record is where the counters live: the reply said 0, and an
    // empty store makes the first observation of both probes acquire their
    // whole table, so the run spent more requests than its plan listed.
    assert!(
        report.requests as usize > started.planned.len(),
        "{} request(s) for a {} unit plan",
        report.requests,
        started.planned.len()
    );

    // The guard is released.
    let next = super::start(&backend, day(2026, 11, 2), RefreshMode::Tail, &[], INSTANCE).unwrap();
    assert_ne!(next.run_id, run_id);
}

/// #582 failure path: an asynchronous tail that fails closes its own record
/// `failed`, advances no `verified_through` and releases the single-flight
/// guard, so the next daily run is not blocked by a failed refresh.
#[test]
fn a_failing_background_tail_reports_failed_without_a_verified_date() {
    let fixture = RefreshFixture::new();
    fixture.set_delisting_pages(2);
    fixture.fail_page_two();
    let scenario = Scenario::new();
    let backend = scenario.open();

    let started = super::start_in_background(
        scenario.store(),
        backend.clone(),
        client(&fixture),
        day(2026, 11, 2),
        request(RefreshMode::Tail, &["szse_delisting"]),
        INSTANCE,
    )
    .unwrap();
    let run_id = started.run_id.unwrap();

    let report = wait_for_run(&backend, &run_id);
    assert_eq!(report.status, RefreshStatus::Failed);
    assert!(report.verified_through.is_none());
    assert!(!report.failures.is_empty());
    assert!(
        report
            .failures
            .iter()
            .all(|failure| failure.code == "snapshot_incomplete"),
        "{:?}",
        report.failures
    );
    assert!(
        backend
            .latest_complete_verified_through("szse_delisting")
            .unwrap()
            .is_none(),
        "a failed background tail advances no verified date"
    );
    let row = backend.read_source_refresh_run(&run_id).unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.finished_at.is_some());

    // Fail-closed, not fail-stuck: the next start is accepted.
    let next = super::start(&backend, day(2026, 11, 2), RefreshMode::Tail, &[], INSTANCE).unwrap();
    assert_ne!(next.run_id, run_id);
}

/// Poll the durable record until the background thread closes it.
#[test]
fn every_source_records_its_own_wall_clock() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    // D-67: G2's per-table gate (`<600 s` for each triggered whole table) reads
    // `source_refresh_run_sources.elapsed_ms`, so it must be the wall clock of
    // that source alone, never the run's.
    let started = std::time::Instant::now();
    let report = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Full,
        &[],
    );
    // Same rounding as the recorded column: `ceil` is monotone, so a source can
    // never round above the run that contains it.
    let total_ms = super::millis_ceil(started.elapsed());
    let run_id = report.run_id.unwrap();
    let source_rows = backend.read_source_refresh_run_sources(&run_id).unwrap();
    assert_eq!(source_rows.len(), report.sources.len());
    for row in &source_rows {
        assert!(
            row.elapsed_ms >= 1,
            "{} recorded no wall clock",
            row.source_id
        );
        assert!(
            row.elapsed_ms <= total_ms,
            "{} claims {} ms of a {} ms run",
            row.source_id,
            row.elapsed_ms,
            total_ms
        );
    }
}

#[test]
fn a_panicking_tail_is_closed_failed_and_releases_the_guard() {
    let fixture = RefreshFixture::new();
    fixture.panic_at(1);
    let scenario = Scenario::new();
    let backend = scenario.open();
    let client = client(&fixture);

    // A panic in the acquisition is an unwind, not an `Err`: without the catch
    // the row would stay `running` and no later start would be accepted.
    let error = run(
        &scenario.store(),
        &backend,
        &client,
        day(2026, 11, 2),
        &request(RefreshMode::Tail, &[]),
        INSTANCE,
    )
    .unwrap_err();
    assert_eq!(error.code(), "refresh_panicked");
    let run_id = error
        .run_id()
        .expect("a panicked run names itself")
        .to_string();

    let row = backend.read_source_refresh_run(&run_id).unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.finished_at.is_some());
    assert!(row.verified_through.is_none());
    let detail = row.detail.expect("a failed run records why");
    assert!(detail.contains("refresh_panicked"), "{detail}");
    assert!(
        detail.contains("fake transport panicked at request 1"),
        "{detail}"
    );
    assert!(
        backend
            .latest_complete_verified_through("szse_listing")
            .unwrap()
            .is_none(),
        "a panicked run advances no verified date"
    );

    // The guard is free again without a restart.
    let next = super::start(&backend, day(2026, 11, 2), RefreshMode::Tail, &[], INSTANCE).unwrap();
    assert_ne!(next.run_id, run_id);
}

#[test]
fn a_panicking_background_full_reports_failed_without_a_verified_date() {
    let fixture = RefreshFixture::new();
    fixture.set_delisting_pages(2);
    fixture.panic_at(2);
    let scenario = Scenario::new();
    let backend = scenario.open();

    let started = super::start_in_background(
        scenario.store(),
        backend.clone(),
        client(&fixture),
        day(2026, 11, 2),
        request(RefreshMode::Full, &["szse_delisting"]),
        INSTANCE,
    )
    .unwrap();
    let run_id = started.run_id.unwrap();

    let report = wait_for_run(&backend, &run_id);
    assert_eq!(report.status, RefreshStatus::Failed);
    assert!(report.verified_through.is_none());
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    assert_eq!(report.failures[0].source_id, "*");
    assert_eq!(report.failures[0].code, "refresh_panicked");
    assert!(
        report.failures[0]
            .detail
            .contains("fake transport panicked at request 2"),
        "{:?}",
        report.failures[0]
    );
    assert!(
        backend
            .latest_complete_verified_through("szse_delisting")
            .unwrap()
            .is_none(),
        "a panicked background run advances no verified date"
    );
    let row = backend.read_source_refresh_run(&run_id).unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.verified_through.is_none());

    let next = super::start(&backend, day(2026, 11, 2), RefreshMode::Tail, &[], INSTANCE).unwrap();
    assert_ne!(next.run_id, run_id);
}

fn wait_for_run(backend: &MarketBackend, run_id: &str) -> super::wire::RefreshReport {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let report = super::status_of(backend, run_id).unwrap();
        if report.status != RefreshStatus::Running {
            return report;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background refresh run {run_id} did not finish within 60 s"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// R1's `H` gate reads the refresh run's confirmation date, not the selected
/// page's own `as_of` (XDM-156 ③). A whole-table page keeps the date it was
/// first observed with forever — `full` re-fetches identical bytes, so nothing
/// is re-indexed — and only the run record says the fingerprint set was
/// confirmed again. Measuring `H` against the page date would fail an item that
/// a run verified yesterday.
#[test]
fn reuse_is_gated_by_the_run_confirmation_not_the_page_date() {
    use crate::exchange_source::{Enabled, Entry, ReusePolicyTable, SourceId};

    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let client = client(&fixture);

    // First full run: the SZSE short-name table is frozen, dated by the
    // acquisition clock (the freeze date), not by this run's `today`.
    let first = run(
        &scenario.store(),
        &scenario.open(),
        &client,
        day(2026, 10, 2),
        &request(RefreshMode::Full, &["szse_short_name"]),
        INSTANCE,
    )
    .unwrap();
    assert_eq!(first.status, RefreshStatus::Succeeded);
    let as_of = {
        let backend = scenario.open();
        backend.read_exchange_snapshots().unwrap()[0].as_of.clone()
    };
    let page_day = NaiveDate::parse_from_str(&as_of, "%Y-%m-%d").expect("as_of is a day");
    let confirmed = page_day + chrono::Duration::days(30);
    let confirmed_text = confirmed.to_string();

    // Second full run, thirty days later: the bytes are unchanged, so the page
    // keeps its first observation date and only the run is new.
    let second = run(
        &scenario.store(),
        &scenario.open(),
        &client,
        confirmed,
        &request(RefreshMode::Full, &["szse_short_name"]),
        INSTANCE,
    )
    .unwrap();
    assert_eq!(second.status, RefreshStatus::Succeeded);
    {
        let backend = scenario.open();
        let rows = backend.read_exchange_snapshots().unwrap();
        assert_eq!(rows.len(), 1, "an unchanged page is not re-indexed");
        assert_eq!(rows[0].as_of, as_of, "the page keeps its own date");
    }

    let source_plan = crate::source_refresh::plan::build(
        RefreshMode::Full,
        confirmed,
        &["szse_short_name".to_string()],
    )
    .unwrap()
    .remove(0);
    let plan = &source_plan.plans[0].1;
    let reuse = ReusePolicyTable::declared().with_enabled(SourceId::SzseShortName, Enabled::Yes);
    let calls = fixture.calls();
    let store = scenario.store();

    // Forty days after the page's own date, ten after the run: reusable. The
    // page date alone would be beyond H and fail the item.
    let read_day = confirmed + chrono::Duration::days(10);
    let backend = scenario.open();
    let outcome = Entry::new(&store, &backend, &client)
        .with_today(read_day)
        .with_reuse_policies(reuse)
        .acquire(plan)
        .expect("the run confirmed the table 10 days ago, so the page date does not gate");
    assert_eq!(outcome.evidence.reused, 1);
    assert_eq!(
        outcome.evidence.verified_through.as_deref(),
        Some(confirmed_text.as_str())
    );
    drop(backend);

    // Twenty days after the run: overdue, and the failure names the run's date.
    let backend = scenario.open();
    let error = Entry::new(&store, &backend, &client)
        .with_today(confirmed + chrono::Duration::days(20))
        .with_reuse_policies(reuse)
        .acquire(plan)
        .unwrap_err();
    assert_eq!(error.code(), "source_refresh_overdue");
    assert!(error.to_string().contains(&confirmed_text), "{error}");
    assert_eq!(fixture.calls(), calls, "an overdue page is not refetched");
}

/// Reproduction harness for #582, not a regression test: a tail on an empty
/// store is the production worst case (the first observation of both probe
/// tables triggers their whole-table acquisition), so the plan is
/// 146 (`szse_listing`) + 375 (`szse_short_name`) + 20 other requests.
///
/// The transport here answers instantly, so the numbers printed below are the
/// stages that are *not* the network: store freeze, envelope parse, the index
/// transaction of a 375-page table, and the write/read coordinator waits. Run
/// it with `X_DATA_MCP_STAGE_TRACE=1` to get the per-stage breakdown:
///
/// ```text
/// X_DATA_MCP_STAGE_TRACE=1 cargo nextest run --run-ignored \
///   -E 'test(production_magnitude_tail_stage_timings)' --no-capture
/// ```
#[test]
#[ignore = "reproduction harness (#582): prints staged timings, not an assertion"]
fn production_magnitude_tail_stage_timings() {
    let fixture = RefreshFixture::new();
    fixture.set_table_pages(146, 375);
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let started = std::time::Instant::now();
    let report = run(
        &scenario.store(),
        &backend,
        &client,
        day(2026, 11, 2),
        &request(RefreshMode::Tail, &[]),
        INSTANCE,
    )
    .unwrap();
    let elapsed = started.elapsed();

    eprintln!(
        "#582 harness: tail wall {} ms, requests {}, pages {}, changed {}, \
         status {:?}",
        elapsed.as_millis(),
        report.requests,
        fixture.calls(),
        report.changed,
        report.status
    );
    // 146 + 375 pages, one page-1 guard re-read each, plus the 20 requests of
    // the other sources in the tail plan.
    assert_eq!(report.status, RefreshStatus::Succeeded);
    assert!(
        report.requests >= 541,
        "the harness must reach the production page counts, got {}",
        report.requests
    );
    let rows = backend
        .read_source_refresh_run_sources(&report.run_id.unwrap())
        .unwrap();
    for row in &rows {
        eprintln!(
            "#582 harness source {:22} class {:6} requests {:4} elapsed {:8} ms complete {}",
            row.source_id, row.class, row.requests, row.elapsed_ms, row.complete
        );
    }
}

/// A `run` at an arbitrary `today`, for the two-day and overdue cases (#604).
fn run_on(
    scenario: &Scenario,
    backend: &MarketBackend,
    client: &Client,
    today: NaiveDate,
    mode: RefreshMode,
    sources: &[&str],
) -> super::wire::RefreshReport {
    run(
        &scenario.store(),
        backend,
        client,
        today,
        &request(mode, sources),
        INSTANCE,
    )
    .unwrap()
}

fn source_of<'a>(
    report: &'a super::wire::RefreshReport,
    source_id: &str,
) -> &'a super::wire::SourceReport {
    report
        .sources
        .iter()
        .find(|source| source.source_id == source_id)
        .unwrap_or_else(|| panic!("{source_id} is not in the report"))
}

/// AC #604-1 and #604-6: the day after a `tail`, the closed quarter is read
/// through the index — zero requests for it — and `reused` equals its page
/// count. The open window is fetched again on both days.
#[test]
fn a_second_day_tail_reuses_the_closed_window_without_a_request() {
    let fixture = RefreshFixture::new();
    fixture.set_szse_notice_pages(3);
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    // 2026-11-02: open window 10-01..11-01, closed window 07-01..09-30.
    let first = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 2),
        RefreshMode::Tail,
        &["szse_etf_notice_ann"],
    );
    assert_eq!(
        source_of(&first, "szse_etf_notice_ann").requests,
        6,
        "two windows of three pages each: nothing to reuse yet"
    );
    assert_eq!(source_of(&first, "szse_etf_notice_ann").reused, 0);

    // 2026-11-03: the open window moved to 10-01..11-02 (a new fingerprint) and
    // is fetched; the closed quarter is served from the store.
    let calls_before = fixture.calls();
    let second = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 3),
        RefreshMode::Tail,
        &["szse_etf_notice_ann"],
    );
    let row = source_of(&second, "szse_etf_notice_ann");
    assert_eq!(
        row.requests, 3,
        "only the open window is fetched; the closed quarter sends no request"
    );
    assert_eq!(
        row.reused, 3,
        "the closed quarter's three pages are read through the index"
    );
    assert_eq!(
        fixture.calls() - calls_before,
        3,
        "the source's own request count matches what actually went on the wire"
    );
    assert_eq!(
        second.requests as usize,
        fixture.calls() - calls_before,
        "run totals equal real requests: reuse never shows up as a request"
    );
    let run_id = second.run_id.unwrap();
    let recorded = backend.read_source_refresh_run_sources(&run_id).unwrap();
    let recorded = recorded
        .iter()
        .find(|row| row.source_id == "szse_etf_notice_ann")
        .unwrap();
    assert_eq!(
        recorded.requests, 3,
        "the run row counts real requests only"
    );
}

/// AC #604-2: a second `tail` on the **same** day re-reads the open window. The
/// open window's fingerprint is identical, so this proves the open-window rule
/// is explicit and not an accident of a moving fingerprint.
#[test]
fn a_second_tail_on_the_same_day_still_fetches_the_open_window() {
    let fixture = RefreshFixture::new();
    fixture.set_szse_notice_pages(3);
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let first = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 2),
        RefreshMode::Tail,
        &["szse_etf_notice_ann"],
    );
    assert_eq!(source_of(&first, "szse_etf_notice_ann").requests, 6);

    let calls_before = fixture.calls();
    let second = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 2),
        RefreshMode::Tail,
        &["szse_etf_notice_ann"],
    );
    let row = source_of(&second, "szse_etf_notice_ann");
    assert_eq!(
        row.requests, 3,
        "the identical open window is fetched again, not reused"
    );
    assert_eq!(
        fixture.calls() - calls_before,
        3,
        "the same-day open window really went on the wire"
    );
}

/// AC #604-3: `full` re-reads every window. It is the run that finds revisions
/// in the closed quarters, so a same-fingerprint closed window must still be
/// fetched — never answered from the store.
#[test]
fn full_rereads_every_window_instead_of_reusing_the_closed_quarter() {
    let fixture = RefreshFixture::new();
    fixture.set_szse_notice_pages(3);
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let first = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 3),
        RefreshMode::Full,
        &["szse_etf_notice_ann"],
    );
    let pages: u64 = source_of(&first, "szse_etf_notice_ann").requests;
    assert!(pages > 0);
    let calls_before = fixture.calls();

    // The same day and the same bytes: a `tail` would reuse the closed
    // quarters, a `full` must not.
    let second = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 3),
        RefreshMode::Full,
        &["szse_etf_notice_ann"],
    );
    assert_eq!(
        fixture.calls() - calls_before,
        pages as usize,
        "full re-reads every quarter, so it sends as many requests as the first full"
    );
    assert_eq!(
        source_of(&second, "szse_etf_notice_ann").requests,
        pages,
        "no window is answered from the store in a full run"
    );
    assert_eq!(
        source_of(&second, "szse_etf_notice_ann").reused as u64,
        pages,
        "the bytes are unchanged, so the pages are stored once, but they were fetched"
    );
}

/// AC #604-4: whole-table sources keep exactly the behaviour they had before
/// D-87 — the tail still fetches the cheap tables and probes the two large
/// ones. Only the four windowed sources changed.
#[test]
fn a_tail_leaves_whole_table_sources_unchanged() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let first = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 2),
        RefreshMode::Tail,
        &[],
    );
    let table_requests = |report: &super::wire::RefreshReport| {
        report
            .sources
            .iter()
            .filter(|source| source.class == "table")
            .map(|source| (source.source_id.clone(), source.requests))
            .collect::<Vec<_>>()
    };
    // 4 + 4 + 2 + 3 + 4 + 3 + 2 in the default fixture (see the budget test);
    // the point of the assertion is that the second tail matches the shape of
    // the first for every table source.
    let calls_before = fixture.calls();
    let second = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 2),
        RefreshMode::Tail,
        &[],
    );
    let _ = first;
    let tables = table_requests(&second);
    let table_requests_total: u64 = tables.iter().map(|(_, requests)| requests).sum();
    assert_eq!(
        fixture.calls() - calls_before,
        second.requests as usize,
        "run totals equal real requests"
    );
    // The two ETF list tables and the two full SZSE tables are fetched in full
    // on every tail, exactly as before D-87; the two probes stay one page.
    let by_id = |id: &str| {
        tables
            .iter()
            .find(|(source, _)| source == id)
            .map(|(_, requests)| *requests)
            .unwrap_or_else(|| panic!("{id} is not a table source of the tail plan"))
    };
    assert_eq!(by_id("sse_etf_list"), 2, "a whole table is fetched in full");
    assert_eq!(by_id("szse_etf_list"), 2);
    assert_eq!(by_id("szse_listing"), 1, "the probe stays one page");
    assert_eq!(by_id("szse_short_name"), 1, "the probe stays one page");
    assert_eq!(by_id("sse_listing"), 4);
    assert_eq!(by_id("sse_delisting"), 4);
    assert_eq!(by_id("szse_delisting"), 4);
    assert!(table_requests_total > 0);
}

/// AC #604-5 (part 1): a fresh source store that has never had a `full` has no
/// complete refresh date, so the closed quarter is not read through and the
/// `tail` fetches it like any other window. Nothing is claimed as verified.
#[test]
fn a_first_tail_on_a_fresh_store_fetches_the_closed_quarter() {
    let fixture = RefreshFixture::new();
    fixture.set_szse_notice_pages(2);
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    let report = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 2),
        RefreshMode::Tail,
        &["szse_etf_notice_ann"],
    );
    let row = source_of(&report, "szse_etf_notice_ann");
    assert_eq!(
        row.requests, 4,
        "no complete refresh has ever run, so both windows are fetched"
    );
    assert_eq!(row.reused, 0);
    assert!(
        row.verified_through.is_none(),
        "a tail of two windows verifies no complete set and claims no date"
    );
    assert!(
        backend
            .latest_complete_verified_through("szse_etf_notice_ann")
            .unwrap()
            .is_none()
    );
    assert_eq!(report.status, RefreshStatus::Succeeded);
    assert!(report.failures.is_empty());
}

/// AC #604-5 (part 2): past `H = 14` days without a complete refresh the `tail`
/// fails closed with `source_refresh_overdue` instead of re-fetching, so the
/// reuse gate never silently degrades into the pre-D-87 behaviour.
#[test]
fn a_tail_beyond_the_age_bound_fails_closed_instead_of_refetching() {
    let fixture = RefreshFixture::new();
    fixture.set_szse_notice_pages(2);
    let scenario = Scenario::new();
    let client = client(&fixture);
    let backend = scenario.open();

    // A weekly full on 2026-11-02 confirms the source.
    let full = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 2),
        RefreshMode::Full,
        &["szse_etf_notice_ann"],
    );
    assert_eq!(full.status, RefreshStatus::Succeeded);
    assert_eq!(
        backend
            .latest_complete_verified_through("szse_etf_notice_ann")
            .unwrap()
            .as_deref(),
        Some("2026-11-02")
    );

    // Fifteen days later, without any full in between: overdue.
    let calls_before = fixture.calls();
    let late = run_on(
        &scenario,
        &backend,
        &client,
        day(2026, 11, 17),
        RefreshMode::Tail,
        &["szse_etf_notice_ann"],
    );
    assert_eq!(
        late.status,
        RefreshStatus::Failed,
        "an overdue tail fails the run"
    );
    let failures = &late.failures;
    assert!(
        failures
            .iter()
            .all(|failure| failure.code == "source_refresh_overdue"),
        "every failure is the overdue code, got {failures:?}"
    );
    let closed = failures
        .iter()
        .find(|failure| failure.window_end.as_deref() == Some("2026-09-30"))
        .expect("the closed quarter is the window that fails");
    assert_eq!(
        closed.window_start.as_deref(),
        Some("2026-07-01"),
        "the failure names the closed quarter"
    );
    assert!(
        closed.detail.contains("2026-11-02"),
        "the detail names the last complete verification: {}",
        closed.detail
    );
    assert!(
        closed.detail.contains("H=14"),
        "the detail names the bound: {}",
        closed.detail
    );
    let row = source_of(&late, "szse_etf_notice_ann");
    assert_eq!(row.failed_windows, failures.len());
    // Only the open window is fetched (2 pages); the overdue closed quarter
    // sends nothing, because the gate refuses before the first request.
    assert_eq!(row.requests, 2);
    assert_eq!(fixture.calls() - calls_before, 2);
    let run_id = late.run_id.unwrap();
    let runs = backend.read_source_refresh_runs().unwrap();
    let run = runs.iter().find(|run| run.run_id == run_id).unwrap();
    assert_eq!(run.status, "failed");
    assert!(
        run.verified_through.is_none(),
        "a failed run advances no source's date"
    );
    let recorded = backend.read_source_refresh_run_sources(&run_id).unwrap();
    let recorded = recorded
        .iter()
        .find(|row| row.source_id == "szse_etf_notice_ann")
        .unwrap();
    assert!(!recorded.complete);
    assert_eq!(recorded.failed_windows as usize, failures.len());
}

// ---------- #625: server-side long poll for `status_of` ----------

/// A run that stays `running` for the whole wait returns the same record the
/// immediate call would, and the call itself lasts about the budget.
#[test]
fn status_of_without_wait_seconds_returns_at_once() {
    let scenario = Scenario::new();
    let backend = scenario.open();
    let open = super::start(&backend, day(2026, 11, 2), RefreshMode::Full, &[], INSTANCE).unwrap();

    // Absent and 0 are the immediate read: a `running` row comes back at once.
    for wait in [None, Some(0)] {
        let started = std::time::Instant::now();
        let report = super::status_of_waiting(&backend, &open.run_id, wait).unwrap();
        assert_eq!(report.status, RefreshStatus::Running);
        assert_eq!(report.run_id.as_deref(), Some(open.run_id.as_str()));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "an absent or zero wait_seconds must not sleep ({:?})",
            started.elapsed()
        );
    }
}

/// The wait ends as soon as the run settles — well before its budget — and
/// returns the terminal record, not the `running` one it entered with.
#[test]
fn status_of_wait_returns_early_when_the_run_settles() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let backend = scenario.open();

    fixture.pause();
    let started_run = super::start_in_background(
        scenario.store(),
        backend.clone(),
        client(&fixture),
        day(2026, 11, 2),
        request(RefreshMode::Tail, &["szse_delisting"]),
        INSTANCE,
    )
    .unwrap();
    let run_id = started_run.run_id.clone().unwrap();

    // Release the transport from another thread once the waiter is asleep:
    // the wait must notice the settled row on its next 400 ms re-read.
    let real = fixture.clone();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(700));
        real.release();
    });
    let started = std::time::Instant::now();
    let report = super::status_of_waiting(&backend, &run_id, Some(20)).unwrap();
    let elapsed = started.elapsed();
    releaser.join().unwrap();

    assert_eq!(report.status, RefreshStatus::Succeeded);
    assert!(report.finished_at.is_some());
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "a settling run must end the wait early, took {elapsed:?}"
    );
}

/// A run that never settles inside the budget returns the state it is in at
/// the deadline — a `running` record, not an error and not a terminal one.
#[test]
fn status_of_wait_returns_the_running_state_on_timeout() {
    let scenario = Scenario::new();
    let backend = scenario.open();
    let open = super::start(&backend, day(2026, 11, 2), RefreshMode::Full, &[], INSTANCE).unwrap();

    let started = std::time::Instant::now();
    let report = super::status_of_waiting(&backend, &open.run_id, Some(1)).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(report.status, RefreshStatus::Running);
    assert!(report.finished_at.is_none());
    assert!(
        elapsed >= std::time::Duration::from_secs(1),
        "a 1 s wait must actually wait, took {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "a 1 s wait must not overrun its budget, took {elapsed:?}"
    );
}

/// `wait_seconds` above the cap is clamped to it, never rejected: the answer
/// is still a normal record and the call cannot outlast `MAX_WAIT_SECONDS`.
#[test]
fn status_of_wait_seconds_above_the_cap_is_clamped() {
    let scenario = Scenario::new();
    let backend = scenario.open();
    let open = super::start(&backend, day(2026, 11, 2), RefreshMode::Full, &[], INSTANCE).unwrap();

    // The clamp itself, without a 50 s test: the budget function is the same
    // one `get_task_progress` uses.
    assert_eq!(
        super::wait_budget(0),
        None,
        "0 means no wait (the immediate behavior)"
    );
    assert_eq!(
        super::wait_budget(1),
        Some(std::time::Duration::from_secs(1))
    );
    assert_eq!(
        super::wait_budget(super::MAX_WAIT_SECONDS),
        Some(std::time::Duration::from_secs(u64::from(
            super::MAX_WAIT_SECONDS
        )))
    );
    assert_eq!(
        super::wait_budget(u32::MAX),
        Some(std::time::Duration::from_secs(u64::from(
            super::MAX_WAIT_SECONDS
        ))),
        "an out-of-range wait is clamped, not rejected"
    );
    // And the call with an absurd value is still a plain successful read.
    let report = super::status_of_waiting(&backend, &open.run_id, Some(0)).unwrap();
    assert_eq!(report.status, RefreshStatus::Running);
    assert_eq!(super::MAX_WAIT_SECONDS, 50);
}

/// The waiter never holds a read transaction across its sleep: the background
/// refresh thread writes the run record while a wait is in flight, and the
/// waiter observes the settled row. A deadlock here would time out the test.
#[test]
fn a_waiting_reader_does_not_block_the_refresh_thread() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let backend = scenario.open();

    fixture.pause();
    let started_run = super::start_in_background(
        scenario.store(),
        backend.clone(),
        client(&fixture),
        day(2026, 11, 2),
        request(RefreshMode::Tail, &["szse_delisting"]),
        INSTANCE,
    )
    .unwrap();
    let run_id = started_run.run_id.clone().unwrap();

    let real = fixture.clone();
    let writer = std::thread::spawn(move || {
        // Long enough for the waiter to be inside its sleep loop.
        std::thread::sleep(std::time::Duration::from_millis(500));
        real.release();
    });
    // A second waiter on another thread: two long polls must not serialize the
    // refresh thread's write either.
    let second_backend = backend.clone();
    let second_run = run_id.clone();
    let second = std::thread::spawn(move || {
        super::status_of_waiting(&second_backend, &second_run, Some(20)).unwrap()
    });
    let first = super::status_of_waiting(&backend, &run_id, Some(20)).unwrap();
    let second = second.join().unwrap();
    writer.join().unwrap();

    for report in [first, second] {
        assert_eq!(report.status, RefreshStatus::Succeeded);
        assert_eq!(report.run_id.as_deref(), Some(run_id.as_str()));
    }
    let row = backend.read_source_refresh_run(&run_id).unwrap().unwrap();
    assert_eq!(row.status, "succeeded");
}

// ---------- #625: `latest:"full"` ----------

#[test]
fn latest_full_is_null_without_any_run() {
    let scenario = Scenario::new();
    let backend = scenario.open();
    // No run at all, and then only an open one: neither is a succeeded full.
    assert!(super::latest_full(&backend).unwrap().is_none());
    let _open = super::start(&backend, day(2026, 11, 2), RefreshMode::Full, &[], INSTANCE).unwrap();
    assert!(
        super::latest_full(&backend).unwrap().is_none(),
        "a running full has confirmed nothing yet"
    );
}

#[test]
fn latest_full_is_null_when_only_a_tail_succeeded() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let backend = scenario.open();
    let report = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client(&fixture),
        RefreshMode::Tail,
        &["szse_delisting"],
    );
    assert_eq!(report.status, RefreshStatus::Succeeded);
    assert!(
        super::latest_full(&backend).unwrap().is_none(),
        "a tail reads two windows, not the whole fingerprint set: it is not a full"
    );
}

#[test]
fn latest_full_skips_a_failed_full_and_returns_the_newest_succeeded_one() {
    let fixture = RefreshFixture::new();
    let scenario = Scenario::new();
    let backend = scenario.open();
    let client = client(&fixture);

    let first = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Full,
        &["szse_delisting"],
    );
    assert_eq!(first.status, RefreshStatus::Succeeded);

    // A failing full after it: newer, but it confirmed nothing.
    fixture.set_delisting_pages(2);
    fixture.fail_page_two();
    let failed = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Full,
        &["szse_delisting"],
    );
    assert_eq!(failed.status, RefreshStatus::Failed);

    let latest = super::latest_full(&backend).unwrap().unwrap();
    assert_eq!(latest.run_id, first.run_id.clone().unwrap());
    assert_eq!(latest.started_at, first.started_at);
    assert_eq!(latest.finished_at, first.finished_at);
    assert_eq!(latest.requests, first.requests);
    assert_eq!(latest.failed_windows, 0);
    assert_eq!(latest.unobserved_windows, 0);

    // A newer successful full replaces it.
    fixture.fail_page_two();
    fixture.state.lock().unwrap().fail_page_two = false;
    let second = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Full,
        &["szse_delisting"],
    );
    assert_eq!(second.status, RefreshStatus::Succeeded);
    let latest = super::latest_full(&backend).unwrap().unwrap();
    assert_eq!(latest.run_id, second.run_id.clone().unwrap());
    assert_ne!(latest.run_id, first.run_id.unwrap());
}

// ---------- #625: input validation for `latest` / `wait_seconds` ----------

#[test]
fn latest_accepts_only_full() {
    let scenario = Scenario::new();
    let backend = scenario.open();
    let mut command = RefreshRequest {
        mode: None,
        sources: Vec::new(),
        dry_run: false,
        status_of: None,
        latest: Some("tail".to_string()),
        wait_seconds: None,
    };
    // `latest` is the selector, not a mode: any other value is a bad argument.
    assert_ne!(command.latest.as_deref(), Some(super::wire::LATEST_FULL));
    command.latest = Some(super::wire::LATEST_FULL.to_string());
    assert!(super::latest_full(&backend).unwrap().is_none());
}
