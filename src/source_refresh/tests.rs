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

struct State {
    short_code: String,
    short_day: String,
    short_before: String,
    short_after: String,
    bulletin_revision: u64,
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
}

impl RefreshFixture {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            calls: Mutex::new(Vec::new()),
        })
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

    fn set_delisting_pages(&self, pages: u32) {
        self.state.lock().unwrap().delisting_pages = pages;
    }

    fn fail_page_two(&self) {
        self.state.lock().unwrap().fail_page_two = true;
    }

    fn bump_on_page_two(&self) {
        self.state.lock().unwrap().bump_on_page_two = true;
    }

    fn body(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        if url.contains("commonSoaQuery") {
            return Ok(
                br#"{"pageHelp":{"pageNo":1,"pageSize":25,"pageCount":0,"total":0,"data":[]}}"#
                    .to_vec(),
            );
        }
        if url.contains("queryCompanyBulletinNew") {
            let revision = self.state.lock().unwrap().bulletin_revision;
            return Ok(serde_json::json!({
                "pageHelp": {
                    "beginPage": 1,
                    "pageNo": 1,
                    "pageSize": 100,
                    "pageCount": 1,
                    "total": 1,
                    "data": [{"TITLE": "风险警示", "revision": revision}]
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
        self.calls.lock().unwrap().push(request.url.clone());
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
        mode,
        sources: sources.iter().map(|source| source.to_string()).collect(),
        dry_run: false,
    }
}

fn run_refresh(
    fixture: &Arc<RefreshFixture>,
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
        8,
        "six cheap-table pages + two first-observation probes' full tables"
    );
    assert!(
        members
            .iter()
            .all(|member| member.request_fingerprint.len() == 64)
    );

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
    assert_eq!(second_members.len(), 6);
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
        8,
        "run membership is durable across a process restart"
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
            mode: RefreshMode::Tail,
            sources: Vec::new(),
            dry_run: true,
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
