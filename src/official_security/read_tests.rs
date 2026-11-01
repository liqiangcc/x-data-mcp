//! R3 read-path tests (XDM-156, `issue-446` §5.1/§5.3/§5.4).
//!
//! The four source-derived items no longer acquire: they derive from the frozen
//! set a `refresh_exchange_sources` run published. Every test here freezes pages
//! directly (no transport exists in this module at all, which is the point) and
//! then calls the read path:
//!
//! - a missing snapshot, an incomplete page set or a confirmation older than
//!   `H = 14` days fails closed with a stable code,
//! - `verified_through` of the refresh run — not the selected page's `as_of` —
//!   is what the `H` gate compares (the R1 ruling this PR applies),
//! - the same frozen set reads to the same rows and the same evidence twice,
//! - the whole-table items take `as_of` from their oldest page, the windowed one
//!   from its newest.

use chrono::NaiveDate;

use super::{a1a2, a3, record_test_refresh_run, risk_warnings, security_names};
use crate::exchange_source::request::quarter_segments;
use crate::exchange_source::validate::{declared_tabkey, parse_page};
use crate::exchange_source::{
    AcquisitionPlan, BasisStatus, HttpResponse, Segment, SourceId, freeze_page,
};
use crate::security_master::{
    DelistingReason, DelistingSource, Exchange, IssuerResolution, ListingSource,
    SecurityLifecycleFact,
};
use crate::source_store::{Policy, PolicyEnv, SourceStore, TEST_ENV_LOCK};
use crate::sqlite_market::backend::MarketBackend;
use crate::sqlite_tasks::SqliteTasks;

const SSE_LISTING_MAIN: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a1a2/sse_listing_main.json");
const SSE_LISTING_STAR: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a1a2/sse_listing_star.json");
const SSE_DELISTING_MAIN: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a1a2/sse_delisting_main.json");
const SSE_DELISTING_STAR: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a1a2/sse_delisting_star.json");
const SZSE_LISTING: &[u8] = include_bytes!("../../tests/fixtures/exchange-a1a2/szse_listing.json");
const SZSE_DELISTING_MAIN: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a1a2/szse_delisting_main.json");
const SZSE_DELISTING_MAIN_P2: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a1a2/szse_delisting_main_p2.json");
const SZSE_DELISTING_NM: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a1a2/szse_delisting_nm.json");
const SZSE_NAME_CHANGES: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-security-names/szse-chain.json");
const SSE_BULLETINS: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-risk-warnings/sse-bulletins.json");
const A3_Q1_EMPTY: &[u8] = include_bytes!("../../tests/fixtures/exchange-a3/window_2008_q1.json");
const A3_Q2_PAGE1: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a3/window_2008_q2_p1.json");
const A3_Q2_PAGE2: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a3/window_2008_q2_p2.json");

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid day")
}

/// The observation time of the frozen pages: a *past* date, so a test that
/// forgets the refresh run cannot pass by accident.
const OBSERVED_AT: &str = "2026-10-30T02:00:00Z";
const OBSERVED_DAY: &str = "2026-10-30";

struct Scenario {
    _dir: tempfile::TempDir,
    store: SourceStore,
    market: MarketBackend,
}

impl Scenario {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        // The store root exists but holds nothing: `Policy::Required` then
        // reaches the read path instead of failing as unconfigured.
        std::fs::create_dir_all(dir.path().join("source-store")).unwrap();
        let store = SourceStore::new(dir.path().join("source-store"));
        let tasks = SqliteTasks::new(dir.path().join("market.sqlite"));
        tasks.initialize_compact().unwrap();
        std::fs::set_permissions(tasks.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
        let market = MarketBackend::open(&tasks).unwrap();
        Self {
            _dir: dir,
            store,
            market,
        }
    }

    fn manifest_lines(&self) -> usize {
        self.store.records().unwrap().len()
    }

    fn index_rows(&self) -> usize {
        self.market.read_exchange_snapshots().unwrap().len()
    }

    /// Bind the process to this store for `run_source`; the caller holds
    /// [`TEST_ENV_LOCK`].
    fn policy(&self) -> PolicyEnv {
        PolicyEnv::set(Some(self.store.root()), false)
    }
}

/// Freeze one page exactly as a refresh run does: the same request the plan
/// builds, the same envelope the parser derives, the same basis tier.
#[allow(clippy::too_many_arguments)]
fn freeze(
    scenario: &Scenario,
    plan: &AcquisitionPlan,
    page: u32,
    window: Option<Segment>,
    from: &str,
    to: &str,
    body: &[u8],
    fetched_at: &str,
) {
    let request = plan.request(page, window, from, to);
    let response = HttpResponse {
        status: 200,
        etag: None,
        last_modified: None,
        body: body.to_vec(),
    };
    let envelope = parse_page(plan.format, declared_tabkey(&plan.params), body)
        .expect("the fixture page has a readable envelope");
    freeze_page(
        &scenario.store,
        &scenario.market,
        &request,
        &response,
        &envelope,
        plan.format.parser_version(),
        BasisStatus::ApprovedSource,
        fetched_at,
    )
    .expect("the frozen page is admitted");
}

// ---------------------------------------------------------------- A-1/A-2 ---

fn a1a2_body(plan: &AcquisitionPlan, page: u32) -> Vec<u8> {
    let param = |name: &str| plan.params.get(name).map(String::as_str);
    if param("sqlId") == Some(a1a2::SSE_LIST_SQL_ID) {
        let delisting = param("COMPANY_STATUS") == Some("3");
        let star = param("STOCK_TYPE") == Some("8");
        return match (delisting, star) {
            (true, true) => SSE_DELISTING_STAR.to_vec(),
            (true, false) => SSE_DELISTING_MAIN.to_vec(),
            (false, true) => SSE_LISTING_STAR.to_vec(),
            (false, false) => SSE_LISTING_MAIN.to_vec(),
        };
    }
    match (param("CATALOGID"), param("selectModule"), page) {
        (Some(a1a2::SZSE_DELISTING_CATALOG), Some("nm"), _) => SZSE_DELISTING_NM.to_vec(),
        (Some(a1a2::SZSE_DELISTING_CATALOG), _, 1) => SZSE_DELISTING_MAIN.to_vec(),
        (Some(a1a2::SZSE_DELISTING_CATALOG), _, _) => SZSE_DELISTING_MAIN_P2.to_vec(),
        _ => SZSE_LISTING.to_vec(),
    }
}

fn a1a2_pages(plan: &AcquisitionPlan) -> u32 {
    let nm = plan.params.get("selectModule").map(String::as_str) == Some("nm");
    if plan.source_id == SourceId::SzseDelisting && !nm {
        2
    } else {
        1
    }
}

fn freeze_a1a2_except(
    scenario: &Scenario,
    fetched_at: &str,
    keep: impl Fn(&AcquisitionPlan, u32) -> bool,
) {
    for (_, plan) in a1a2::plans() {
        for page in 1..=a1a2_pages(&plan) {
            if keep(&plan, page) {
                let body = a1a2_body(&plan, page);
                freeze(scenario, &plan, page, None, "", "", &body, fetched_at);
            }
        }
    }
}

/// Every planned A-1/A-2 page.
fn every_a1a2_page(_: &AcquisitionPlan, _: u32) -> bool {
    true
}

/// Every page of the SZSE terminated list *except* its page 2.
fn without_szse_terminated_page_two(plan: &AcquisitionPlan, page: u32) -> bool {
    !(plan.source_id == SourceId::SzseDelisting
        && plan.params.get("selectModule").map(String::as_str) != Some("nm")
        && page == 2)
}

fn freeze_a1a2(scenario: &Scenario, fetched_at: &str) {
    freeze_a1a2_except(scenario, fetched_at, every_a1a2_page);
}

fn record_a1a2_runs(scenario: &Scenario, verified_through: &str) {
    for source in [
        SourceId::SseListing,
        SourceId::SseDelisting,
        SourceId::SzseListing,
        SourceId::SzseDelisting,
    ] {
        record_test_refresh_run(
            &scenario.market,
            source,
            verified_through,
            &format!("{verified_through}T03:00:00Z"),
        );
    }
}

#[test]
fn the_security_master_item_reads_the_frozen_set_and_writes_nothing() {
    let scenario = Scenario::new();
    freeze_a1a2(&scenario, OBSERVED_AT);
    record_a1a2_runs(&scenario, OBSERVED_DAY);
    let manifest = scenario.manifest_lines();
    let index = scenario.index_rows();

    let admitted = a1a2::read(&scenario.store, &scenario.market, day(2026, 11, 1)).unwrap();

    assert!(!admitted.snapshot.listing.is_empty());
    assert!(!admitted.snapshot.delisting.is_empty());
    assert_eq!(admitted.snapshot.as_of, day(2026, 10, 30));
    // §5.3: a table set is as new as its *oldest* page, and the freshness is
    // the refresh run's, not the run day's.
    assert_eq!(admitted.freshness.as_of.as_deref(), Some(OBSERVED_DAY));
    assert_eq!(
        admitted.freshness.verified_through.as_deref(),
        Some(OBSERVED_DAY)
    );
    assert_eq!(admitted.freshness.staleness_days, Some(2));
    assert_eq!(admitted.freshness.selected_by, "refresh_run");
    // The read is read-only: no manifest line and no index row was added.
    assert_eq!(scenario.manifest_lines(), manifest);
    assert_eq!(scenario.index_rows(), index);
}

#[test]
fn a_confirmation_older_than_h_days_fails_closed() {
    let scenario = Scenario::new();
    freeze_a1a2(&scenario, OBSERVED_AT);
    // The newest complete run was confirmed on 2026-10-17: 14 days before
    // 2026-10-31 (still inside H) and 15 days before 2026-11-01.
    record_a1a2_runs(&scenario, "2026-10-17");

    a1a2::read(&scenario.store, &scenario.market, day(2026, 10, 31))
        .expect("H = 14 days is still a current input");

    let error = a1a2::read(&scenario.store, &scenario.market, day(2026, 11, 1)).unwrap_err();
    assert_eq!(error.code(), "source_refresh_overdue");
    assert!(
        error.to_string().contains("2026-10-17"),
        "the failure names the last complete confirmation: {error}"
    );
}

/// The R1 ruling this PR applies: `H` is measured against the refresh run's
/// `verified_through`, never against the selected page's `as_of`. A whole-table
/// page keeps its first observation date forever, so the page date alone would
/// fail the item even though the run re-confirmed it yesterday.
#[test]
fn the_run_confirms_the_set_not_the_observation_date_of_a_page() {
    let scenario = Scenario::new();
    freeze_a1a2(&scenario, "2026-10-02T02:00:00Z");
    record_a1a2_runs(&scenario, OBSERVED_DAY);

    let admitted = a1a2::read(&scenario.store, &scenario.market, day(2026, 11, 1))
        .expect("the run confirmed the set yesterday, so the page date does not gate");

    // The dataset date is still the page's first observation; the gate looked
    // at the run (2 days) and passed.
    assert_eq!(admitted.freshness.as_of.as_deref(), Some("2026-10-02"));
    assert_eq!(
        admitted.freshness.verified_through.as_deref(),
        Some(OBSERVED_DAY)
    );
    assert_eq!(admitted.freshness.staleness_days, Some(2));
}

#[test]
fn a_missing_snapshot_fails_closed() {
    let _lock = TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let scenario = Scenario::new();
    let _env = scenario.policy();
    let error = a1a2::run_source(&scenario.market, Policy::Required).unwrap_err();
    assert_eq!(error.code(), "source_snapshot_not_found");
}

#[test]
fn a_page_set_that_is_not_complete_fails_closed() {
    let scenario = Scenario::new();
    // The SZSE terminated list answers two pages; only page 1 is frozen.
    freeze_a1a2_except(&scenario, OBSERVED_AT, without_szse_terminated_page_two);
    record_a1a2_runs(&scenario, OBSERVED_DAY);

    let error = a1a2::read(&scenario.store, &scenario.market, day(2026, 11, 1)).unwrap_err();
    assert_eq!(error.code(), "snapshot_incomplete");
    assert!(
        error.to_string().contains("page 2"),
        "the failure names the missing page: {error}"
    );
}

#[test]
fn the_same_frozen_set_reads_identically_twice() {
    let scenario = Scenario::new();
    freeze_a1a2(&scenario, OBSERVED_AT);
    record_a1a2_runs(&scenario, OBSERVED_DAY);
    let today = day(2026, 11, 1);

    let first = a1a2::read(&scenario.store, &scenario.market, today).unwrap();
    let second = a1a2::read(&scenario.store, &scenario.market, today).unwrap();

    assert_eq!(first.snapshot, second.snapshot);
    assert_eq!(first.evidence(), second.evidence());
    assert_eq!(first.bindings, second.bindings);
}

// -------------------------------------------------------------- PR-10/PR-12 ---

fn freeze_sz_short_name(scenario: &Scenario, fetched_at: &str) {
    let plan = risk_warnings::szse_short_name_plan();
    freeze(
        scenario,
        &plan,
        1,
        None,
        "",
        "",
        SZSE_NAME_CHANGES,
        fetched_at,
    );
}

/// A quiet-quarter page: `pageCount = 0`, no rows (the shape 2000Q1 answers).
fn quiet_bulletin_page() -> Vec<u8> {
    serde_json::json!({
        "pageHelp": {
            "beginPage": 1,
            "pageNo": 1,
            "pageCount": 0,
            "total": 0,
            "data": [],
        }
    })
    .to_string()
    .into_bytes()
}

/// Freeze every bulletin window of one title filter; the fixture announcements
/// live in 2024, so only that window answers with rows and every other quarter
/// is a quiet page.
fn freeze_bulletins(scenario: &Scenario, today: NaiveDate, fetched_at: &str) {
    let windows = quarter_segments(day(2000, 1, 1), today).unwrap();
    let announcements = day(2024, 4, 1);
    let quiet = quiet_bulletin_page();
    for (kind, plan) in risk_warnings::plans() {
        if kind == risk_warnings::Kind::SzseShortName {
            continue;
        }
        for window in &windows {
            let body = if window.start() == announcements {
                SSE_BULLETINS.to_vec()
            } else {
                quiet.clone()
            };
            freeze(
                scenario,
                &plan,
                1,
                Some(*window),
                "START_DATE",
                "END_DATE",
                &body,
                fetched_at,
            );
        }
    }
}

fn freeze_named_sources(scenario: &Scenario, today: NaiveDate, fetched_at: &str) {
    freeze_sz_short_name(scenario, fetched_at);
    freeze_bulletins(scenario, today, fetched_at);
    record_test_refresh_run(
        &scenario.market,
        SourceId::SzseShortName,
        "2024-06-30",
        fetched_at,
    );
    record_test_refresh_run(
        &scenario.market,
        SourceId::SseBulletin,
        "2024-06-30",
        fetched_at,
    );
}

#[test]
fn the_name_history_item_reads_the_frozen_short_name_table() {
    let scenario = Scenario::new();
    let today = day(2024, 7, 1);
    freeze_sz_short_name(&scenario, "2024-06-30T02:00:00Z");
    record_test_refresh_run(
        &scenario.market,
        SourceId::SzseShortName,
        "2024-06-30",
        "2024-06-30T03:00:00Z",
    );
    let manifest = scenario.manifest_lines();

    let admitted = security_names::read_table(&scenario.store, &scenario.market, today).unwrap();

    assert!(admitted.sz_rows > 0);
    assert_eq!(admitted.freshness.as_of.as_deref(), Some("2024-06-30"));
    assert_eq!(
        admitted.freshness.verified_through.as_deref(),
        Some("2024-06-30")
    );
    assert_eq!(admitted.freshness.selected_by, "refresh_run");
    assert_eq!(scenario.manifest_lines(), manifest);
}

#[test]
fn the_risk_warning_item_reads_the_table_and_the_bulletin_quarters() {
    let scenario = Scenario::new();
    let today = day(2024, 7, 1);
    freeze_named_sources(&scenario, today, "2024-06-30T02:00:00Z");
    let manifest = scenario.manifest_lines();
    let index = scenario.index_rows();

    let admitted = risk_warnings::read_set(&scenario.store, &scenario.market, today).unwrap();

    assert!(admitted.sz_rows > 0);
    // The fixture page holds five announcements carrying three admitted
    // events; both title filters read it, and the `(id, event)` de-duplication
    // keeps each event once, exactly as the acquisition does.
    assert_eq!(admitted.sh_events, 3, "the 2024Q2 announcements are read");
    assert_eq!(admitted.sh_bulletins, 5);
    // Every quiet quarter stays a gap: no row is read as "no warning".
    assert!(
        admitted
            .set
            .coverage
            .iter()
            .filter(|window| window.exchange == crate::security_master::Exchange::Sh)
            .filter(|window| window.observed)
            .count()
            == 1,
        "only the 2024Q2 SH coverage window is observed"
    );
    assert_eq!(
        admitted.freshness.verified_through.as_deref(),
        Some("2024-06-30")
    );
    assert_eq!(admitted.freshness.selected_by, "refresh_run");
    // §5.3 over two sources: the oldest confirmation wins.
    assert_eq!(admitted.freshness.as_of.as_deref(), Some("2024-06-30"));
    assert_eq!(scenario.manifest_lines(), manifest);
    assert_eq!(scenario.index_rows(), index);
}

// ------------------------------------------------------------------- A-3 ---

const A3_OBSERVED_AT: &str = "2008-04-14T02:00:00Z";

/// One SSE A-share member, listed long before the fixture windows: A-3 clips
/// every event to the member interval, so a read without members carries no
/// fact at all.
fn sh_member(code: &str) -> SecurityLifecycleFact {
    let listing = day(1995, 1, 2);
    SecurityLifecycleFact {
        exchange: Exchange::Sh,
        code: code.to_string(),
        effective_from: listing,
        effective_to: None,
        instrument_class: 0,
        issuer_key: None,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_date: Some(listing),
        listing_date_tdx: Some(listing),
        listing_source: ListingSource::TdxSsdate,
        b_listing_date: None,
        delisting_date: None,
        delisting_date_tdx: None,
        delisting_source: DelistingSource::Unknown,
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: false,
        delisting_conflict: false,
        known_at: None,
    }
}

fn a3_windows(today: NaiveDate) -> Vec<Segment> {
    quarter_segments(day(2008, 1, 1), today).unwrap()
}

fn freeze_a3_except(scenario: &Scenario, today: NaiveDate, keep: impl Fn(Segment) -> bool) {
    let plan = a3::plan();
    for window in a3_windows(today) {
        if !keep(window) {
            continue;
        }
        let pages: &[&[u8]] = if window.start() == day(2008, 4, 1) {
            &[A3_Q2_PAGE1, A3_Q2_PAGE2]
        } else {
            &[A3_Q1_EMPTY]
        };
        for (index, body) in pages.iter().enumerate() {
            freeze(
                scenario,
                &plan,
                index as u32 + 1,
                Some(window),
                a3::FROM_PARAM,
                a3::TO_PARAM,
                body,
                A3_OBSERVED_AT,
            );
        }
    }
    // The run confirmed the set the day before `today`, so the H gate measures
    // 1 day whatever `today` the test picks.
    let confirmed = today
        .pred_opt()
        .expect("a test day after 2008-01-01")
        .format("%Y-%m-%d")
        .to_string();
    record_test_refresh_run(
        &scenario.market,
        SourceId::SseSuspension,
        &confirmed,
        &format!("{confirmed}T02:00:00Z"),
    );
}

#[test]
fn the_suspension_item_reads_every_window_and_keeps_the_quiet_one_as_a_gap() {
    let scenario = Scenario::new();
    // The frozen Q2 fixture is a whole quarter, so its window must be the whole
    // quarter: `today` one day after Q2 ends keeps the open-ended row
    // (`601607`, not resumed yet) ending after it starts.
    let today = day(2008, 7, 1);
    freeze_a3_except(&scenario, today, |_| true);
    let manifest = scenario.manifest_lines();

    let members: Vec<SecurityLifecycleFact> = ["600001", "600018", "600680", "600849", "601607"]
        .into_iter()
        .map(sh_member)
        .collect();
    let admitted = a3::read(&scenario.store, &scenario.market, &members, today).unwrap();

    assert_eq!(admitted.windows, 2);
    assert_eq!(
        admitted.not_observed_windows,
        vec!["2008-01-01..2008-03-31".to_string()]
    );
    assert_eq!(admitted.counters.rows_read, 6);
    // 600001 (resumed range), 600018 (whole day), 601607 (never resumed).
    assert_eq!(
        admitted.snapshot.facts.len(),
        3,
        "{:#?}",
        admitted.snapshot.facts
    );
    assert_eq!(admitted.unclosed_rows, 1);
    // §5.3: a window set is as new as its newest page.
    assert_eq!(admitted.snapshot.as_of, day(2008, 4, 14));
    assert_eq!(admitted.freshness.staleness_days, Some(1));
    assert_eq!(admitted.freshness.selected_by, "refresh_run");
    assert_eq!(scenario.manifest_lines(), manifest);
}

#[test]
fn a_needed_window_missing_from_the_store_fails_closed() {
    let scenario = Scenario::new();
    let today = day(2008, 4, 15);
    // Only the newest quarter was ever frozen: the item cannot answer for Q1.
    freeze_a3_except(&scenario, today, |window| window.start() == day(2008, 4, 1));

    let error = a3::read(&scenario.store, &scenario.market, &[], today).unwrap_err();
    assert_eq!(error.code(), "snapshot_incomplete");
}
