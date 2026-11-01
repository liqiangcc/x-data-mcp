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

    fn fail_page_two(&self) {
        self.state.lock().unwrap().fail_page_two = true;
    }

    fn bump_on_page_two(&self) {
        self.state.lock().unwrap().bump_on_page_two = true;
    }

    fn bump_every_page_one(&self) {
        self.state.lock().unwrap().bump_every_page_one = true;
    }

    fn body(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        if url.contains("commonSoaQuery") {
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
        if url.contains("commonQuery.do") {
            return Ok(sse_page(1, 1, 1));
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
        let page_count = if delisting { state.delisting_pages } else { 1 };
        if page == 2 && state.bump_on_page_two && state.bumped.insert(key.clone()) {
            *state.versions.entry(key.clone()).or_insert(0) += 1;
        }
        if page == 1 && state.bump_every_page_one {
            *state.versions.entry(key.clone()).or_insert(0) += 1;
        }
        let version = *state.versions.entry(key).or_insert(0);
        let row_count = if delisting { page_count as u64 } else { 1 };
        let rows = if delisting {
            vec![serde_json::json!({
                "zqdm": format!("00000{page}{version}"),
                "zqjc": format!("listing-{page}-{version}")
            })]
        } else if short {
            vec![serde_json::json!({
                "zqdm": state.short_code.clone(),
                "bgrq": state.short_day.clone(),
                "bgqzqjc": state.short_before.clone(),
                "bghzqjc": format!("{}-{version}", state.short_after)
            })]
        } else {
            vec![serde_json::json!({
                "zqdm": format!("00000{version}"),
                "zqjc": format!("listing-{version}")
            })]
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
            calls.push(request.url.clone());
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
            body: self.body(&request.url)?,
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
    // A tail is synchronous: the reply is the finished run, not a handle.
    assert!(first.finished_at.is_some());
    assert!(
        first.requests <= 28,
        "tail must stay within the D-66 budget, got {} ({} calls)",
        first.requests,
        fixture.calls()
    );
    assert_eq!(first.requests as usize, fixture.calls());
    assert_eq!(first.scope.len(), 7);
    assert_eq!(first.planned.len(), 14);
    assert_eq!(first.sources.len(), 7);
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
    assert_eq!(source_rows.len(), 7);
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
        12,
        "eight cheap-table/probe-triggered pages + four bulletin pages; a quiet \
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
    assert_eq!(second.requests, 20, "probes must not refetch whole tables");
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
        10,
        "six cheap-table pages + four bulletin pages; probes and quiet windows select \
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
        12,
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
    assert_eq!(report.requests, 308);
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
    let second = run_refresh(
        &fixture,
        &scenario,
        &backend,
        &client,
        RefreshMode::Tail,
        &["sse_bulletin"],
    );
    assert_eq!(second.requests, 4);
    assert_eq!(second.changed, 4);
    assert_eq!(second.revisions.len(), 4);
    for revision in &second.revisions {
        assert_ne!(revision.previous_snapshot_id, revision.snapshot_id);
        assert!(revision.window_start.is_some());
        assert!(revision.window_end.is_some());
    }
    let run_id = second.run_id.unwrap();
    let members = backend.read_source_refresh_run_members(&run_id).unwrap();
    assert_eq!(members.len(), 4);
    assert!(members.iter().all(|member| member.changed));
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
        },
    )
    .unwrap();
    assert_eq!(report.status, RefreshStatus::Planned);
    assert!(report.dry_run);
    assert!(report.run_id.is_none());
    assert_eq!(report.planned.len(), 14);
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
    let started = super::start_full_in_background(
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
        300,
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
    assert_eq!(report.requests, 308);
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
    assert_eq!(super::status_of(&backend, &run_id).unwrap().requests, 308);
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

    let started = super::start_full_in_background(
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

    let started = super::start_full_in_background(
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
