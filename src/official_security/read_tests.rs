//! R3 read-path tests (XDM-156, `issue-446` §5.1/§5.3/§5.4).
//!
//! The four source-derived items no longer acquire: they derive from the frozen
//! set a `refresh_exchange_sources` run published. Every test here freezes pages
//! directly and then calls the read path; the one test that owns a transport
//! owns a *counting fake* and asserts it stays at zero:
//!
//! - a missing snapshot, an incomplete page set or a confirmation older than
//!   `H = 14` days fails closed with a stable code,
//! - `verified_through` of the refresh run — not the selected page's `as_of` —
//!   is what the `H` gate compares (the R1 ruling this PR applies),
//! - the same frozen set reads to the same rows and the same evidence twice,
//! - the whole-table items take `as_of` from their oldest page, the windowed one
//!   from its newest.

use chrono::NaiveDate;

use super::{a1a2, a3, etf_list, record_test_refresh_run, risk_warnings, security_names};
use crate::exchange_source::request::quarter_segments;
use crate::exchange_source::transport::with_helper_transport;
use crate::exchange_source::validate::{declared_tabkey, parse_page};
use crate::exchange_source::{
    AcquisitionPlan, BasisStatus, Client, HttpRequest, HttpResponse, Segment, SourceId, Transport,
    TransportError, freeze_page,
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
/// The same table one round later: one more change row (`000005`), so every
/// previous row is unchanged and one row is new (XDM-157's acceptance).
const SZSE_NAME_CHANGES_ROUND2: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-security-names/szse-chain-round2.json");
const SSE_BULLETINS: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-risk-warnings/sse-bulletins.json");
const A3_Q1_EMPTY: &[u8] = include_bytes!("../../tests/fixtures/exchange-a3/window_2008_q1.json");
const A3_Q2_PAGE1: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a3/window_2008_q2_p1.json");
const A3_Q2_PAGE2: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-a3/window_2008_q2_p2.json");
const SSE_ETF_LIST: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-etf-list/sse_etf_list_synthetic.json");
const SZSE_ETF_LIST: &[u8] =
    include_bytes!("../../tests/fixtures/exchange-etf-list/szse_etf_list_synthetic.xlsx");

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

/// M3-2 PR-A: the two ETF identity lists are read from the frozen store with no
/// request, produce the same rows twice, and carry the real member counts.
#[test]
fn etf_identity_reads_both_frozen_lists_and_is_idempotent() {
    let scenario = Scenario::new();
    for (kind, plan) in etf_list::plans() {
        let body = match kind {
            etf_list::ListKind::Sse => SSE_ETF_LIST,
            etf_list::ListKind::Szse => SZSE_ETF_LIST,
        };
        freeze(&scenario, &plan, 1, None, "", "", body, OBSERVED_AT);
    }
    record_test_refresh_run(
        &scenario.market,
        SourceId::SseEtfList,
        OBSERVED_DAY,
        OBSERVED_AT,
    );
    record_test_refresh_run(
        &scenario.market,
        SourceId::SzseEtfList,
        OBSERVED_DAY,
        OBSERVED_AT,
    );
    let today = day(2026, 10, 31);
    let first = etf_list::read(&scenario.store, &scenario.market, today).unwrap();
    let second = etf_list::read(&scenario.store, &scenario.market, today).unwrap();
    assert_eq!(first.rows, second.rows);
    assert_eq!(first.evidence(), second.evidence());
    assert_eq!(first.sse_rows(), 947);
    assert_eq!(first.szse_rows(), 770);
    assert_eq!(first.rows.len(), 1_717);

    let identity = etf_list::identity_rows(&first, &std::collections::BTreeMap::new()).unwrap();
    assert_eq!(identity.len(), 1_717);
    assert!(identity.iter().all(|row| row.instrument_class == 2));
    assert!(
        identity
            .iter()
            .all(|row| row.effective_from == day(2026, 10, 30)),
        "effective_from is the first frozen snapshot's acquisition date"
    );
    assert!(
        identity
            .iter()
            .all(|row| row.known_at == Some(day(2026, 10, 30)))
    );
}

/// The main-table item keeps the A-share assembly and the previous class=2 rows
/// when the ETF branch fails; these are the two source-level errors it must be
/// able to see (`source_invalid` for a broken workbook, `source_refresh_overdue`
/// for a stale frozen set).
#[test]
fn an_invalid_or_overdue_etf_list_is_reported_as_a_source_error() {
    let scenario = Scenario::new();
    for (kind, plan) in etf_list::plans() {
        let body = match kind {
            etf_list::ListKind::Sse => SSE_ETF_LIST,
            etf_list::ListKind::Szse => {
                &include_bytes!("../../tests/fixtures/exchange-etf-list/szse_etf_list_small.xlsx")[..]
            }
        };
        freeze(&scenario, &plan, 1, None, "", "", body, OBSERVED_AT);
    }
    record_test_refresh_run(
        &scenario.market,
        SourceId::SseEtfList,
        OBSERVED_DAY,
        OBSERVED_AT,
    );
    record_test_refresh_run(
        &scenario.market,
        SourceId::SzseEtfList,
        OBSERVED_DAY,
        OBSERVED_AT,
    );
    let error = etf_list::read(&scenario.store, &scenario.market, day(2026, 10, 31)).unwrap_err();
    assert_eq!(error.code(), "etf_row_count_below_floor");

    // A stale confirmation is the other failure the item must localize.
    let scenario = Scenario::new();
    for (kind, plan) in etf_list::plans() {
        let body = match kind {
            etf_list::ListKind::Sse => SSE_ETF_LIST,
            etf_list::ListKind::Szse => SZSE_ETF_LIST,
        };
        freeze(
            &scenario,
            &plan,
            1,
            None,
            "",
            "",
            body,
            "2026-09-01T02:00:00Z",
        );
    }
    record_test_refresh_run(
        &scenario.market,
        SourceId::SseEtfList,
        "2026-09-01",
        "2026-09-01T02:00:00Z",
    );
    record_test_refresh_run(
        &scenario.market,
        SourceId::SzseEtfList,
        "2026-09-01",
        "2026-09-01T02:00:00Z",
    );
    let error = etf_list::read(&scenario.store, &scenario.market, day(2026, 10, 31)).unwrap_err();
    assert_eq!(error.code(), "source_refresh_overdue");
}

fn sse_etf_list_body(rows: &[(&str, &str)]) -> Vec<u8> {
    let data: Vec<serde_json::Value> = rows
        .iter()
        .map(|(code, listing)| {
            serde_json::json!({
                "fundCode": code,
                "secNameFull": format!("合成ETF{code}"),
                "listingDate": listing,
                "subClass": "01",
                "INDEX_CODE": "000001",
            })
        })
        .collect();
    serde_json::json!({
        "pageHelp": {
            "pageNo": 1,
            "pageCount": 1,
            "total": data.len(),
            "data": data,
        }
    })
    .to_string()
    .into_bytes()
}

/// PR-A2 monotonic identity: the newest list no longer contains a code, but the
/// union keeps its class=2 row and `current` excludes it.
#[test]
fn a_code_that_disappears_from_the_newest_list_keeps_its_class_two_row() {
    let scenario = Scenario::new();
    let sse_plan = etf_list::plans()
        .into_iter()
        .find(|(kind, _)| *kind == etf_list::ListKind::Sse)
        .unwrap()
        .1;
    let szse_plan = etf_list::plans()
        .into_iter()
        .find(|(kind, _)| *kind == etf_list::ListKind::Szse)
        .unwrap()
        .1;
    let older = sse_etf_list_body(&[("510050", "2005-02-23"), ("510070", "2010-01-01")]);
    let newer = sse_etf_list_body(&[("510050", "2005-02-23")]);
    freeze(
        &scenario,
        &sse_plan,
        1,
        None,
        "",
        "",
        &older,
        "2026-10-30T02:00:00Z",
    );
    freeze(
        &scenario,
        &sse_plan,
        1,
        None,
        "",
        "",
        &newer,
        "2026-10-31T02:00:00Z",
    );
    freeze(
        &scenario,
        &szse_plan,
        1,
        None,
        "",
        "",
        SZSE_ETF_LIST,
        "2026-10-31T02:00:00Z",
    );
    record_test_refresh_run(
        &scenario.market,
        SourceId::SseEtfList,
        "2026-10-31",
        "2026-10-31T03:00:00Z",
    );
    record_test_refresh_run(
        &scenario.market,
        SourceId::SzseEtfList,
        "2026-10-31",
        "2026-10-31T03:00:00Z",
    );
    let admitted = etf_list::read(&scenario.store, &scenario.market, day(2026, 11, 1)).unwrap();
    assert_eq!(admitted.rows.len(), 1 + 770);
    assert_eq!(admitted.history.len(), 2 + 770);
    assert_eq!(
        admitted.effective_to[&(Exchange::Sh, "510070".to_string())],
        day(2026, 10, 31)
    );
    let identity = etf_list::identity_rows(&admitted, &std::collections::BTreeMap::new()).unwrap();
    let removed = identity
        .iter()
        .find(|row| row.exchange == Exchange::Sh && row.code == "510070")
        .expect("the historical row stays in class=2");
    assert_eq!(removed.instrument_class, 2);
    assert_eq!(removed.effective_to, Some(day(2026, 10, 31)));
    assert!(
        identity
            .iter()
            .any(|row| row.exchange == Exchange::Sh && row.code == "510050")
    );
}

// ------------------------------------------------- daily items, no request ---

/// A fake exchange transport that counts every attempt and refuses it. The
/// daily items must never reach it (R3, `issue-446` §5.1); the acquiring form of
/// the same adapter does, which is what keeps the zero below honest.
#[derive(Default)]
struct NoRequests {
    calls: std::sync::Mutex<u64>,
}

impl NoRequests {
    fn calls(&self) -> u64 {
        *self
            .calls
            .lock()
            .expect("the fake's counter is never poisoned")
    }
}

impl Transport for NoRequests {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        *self
            .calls
            .lock()
            .expect("the fake's counter is never poisoned") += 1;
        Err(TransportError::Unavailable(format!(
            "the daily items must not send exchange requests; this one asked for {}",
            request.url
        )))
    }
}

/// R2b's single-flight guard covers the refresh task only: a daily worker that
/// still acquired would add its own stream on top of a running refresh and
/// double the exchanges' 1 request/s budget. Every daily `run_source` therefore
/// has to read the frozen set, and this test measures that instead of reading
/// it: a fake transport counts the attempts, and the daily entry points are the
/// same four calls `data_supply::derived` makes.
#[test]
fn the_daily_items_send_no_exchange_request() {
    use crate::exchange_source::SourceId as Id;

    let _env_lock = TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let scenario = Scenario::new();
    // One fixed `today` for every item, the way `derived::run` takes it once
    // from the market calendar (XDM-156 ruling A). It is the first day of a
    // quarter, so the A-3 window list of a read (`today - 1`) ends on a quarter
    // boundary and the frozen set needs no partial trailing quarter. The pages
    // stay six days older, well inside `H`.
    let today = day(2026, 10, 1);
    let observed_at = "2026-09-25T02:00:00Z";
    let confirmed = "2026-09-25";

    freeze_a1a2(&scenario, observed_at);
    record_a1a2_runs(&scenario, confirmed);
    freeze_sz_short_name(&scenario, observed_at);
    record_test_refresh_run(
        &scenario.market,
        Id::SzseShortName,
        confirmed,
        &format!("{confirmed}T03:00:00Z"),
    );
    freeze_bulletins(&scenario, today, observed_at);
    record_test_refresh_run(
        &scenario.market,
        Id::SseBulletin,
        confirmed,
        &format!("{confirmed}T03:00:00Z"),
    );
    freeze_a3_except(&scenario, today, |_| true);

    let members: Vec<SecurityLifecycleFact> = ["600001", "600018", "600849", "601607"]
        .into_iter()
        .map(sh_member)
        .collect();

    let _policy = scenario.policy();
    let fake = std::sync::Arc::new(NoRequests::default());
    let fake_for_hook = std::sync::Arc::clone(&fake);
    let (admitted, daily_calls) = with_helper_transport(fake_for_hook, || {
        let admitted = (
            matches!(
                a1a2::run_source(&scenario.market, today, Policy::Required)
                    .expect("the security-master item reads its frozen set"),
                a1a2::RunSource::Admitted(_)
            ),
            matches!(
                security_names::run_source(&scenario.market, today, Policy::Required)
                    .expect("the name-history item reads its frozen table"),
                security_names::RunSource::Admitted(_)
            ),
            matches!(
                risk_warnings::run_source(&scenario.market, today, Policy::Required)
                    .expect("the risk-warning item reads its frozen tables"),
                risk_warnings::RunSource::Admitted(_)
            ),
            matches!(
                a3::run_source(&scenario.market, &members, today, Policy::Required)
                    .expect("the suspension item reads its frozen windows"),
                a3::RunSource::Admitted(_)
            ),
        );
        let daily_calls = fake.calls();

        // The hook is live, not dead wire: the acquiring form of the same
        // adapter does reach the fake, and `Client::attempts()` counts exactly
        // those transport calls.
        let client = Client::helper().expect("the installed fake serves helper()");
        let error = a1a2::acquire(&scenario.store, &scenario.market, &client).unwrap_err();
        assert_eq!(error.code(), "source_unavailable");
        assert!(
            fake.calls() > daily_calls,
            "the fake counted nothing at all"
        );
        assert_eq!(client.attempts(), fake.calls());
        (admitted, daily_calls)
    });

    assert_eq!(
        admitted,
        (true, true, true, true),
        "every daily item read its frozen set instead of failing"
    );
    assert_eq!(
        daily_calls, 0,
        "the daily items sent {daily_calls} exchange request(s); acquisition belongs to \
         refresh_exchange_sources (issue-446 §5.1)"
    );
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
    let error = a1a2::run_source(&scenario.market, day(2026, 11, 1), Policy::Required).unwrap_err();
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
    freeze_sz_short_name_body(scenario, SZSE_NAME_CHANGES, fetched_at);
}

/// Freeze one content version of the whole SZSE short-name table. Two calls
/// with different bodies are two snapshots of the same request fingerprint:
/// exactly what a table that gained a row looks like in the store.
fn freeze_sz_short_name_body(scenario: &Scenario, body: &[u8], fetched_at: &str) {
    let plan = risk_warnings::szse_short_name_plan();
    freeze(scenario, &plan, 1, None, "", "", body, fetched_at);
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
    // XDM-157: the SZ intervals carry the day the frozen table entered the
    // store (the page's `as_of`), never the change day.
    let sz_rows: Vec<_> = admitted
        .set
        .rows
        .iter()
        .filter(|row| row.exchange == crate::security_master::Exchange::Sz)
        .collect();
    assert!(!sz_rows.is_empty());
    assert!(
        sz_rows
            .iter()
            .all(|row| row.known_at == Some(day(2024, 6, 30))),
        "the SZ ST rows are known from the day their snapshot was stored"
    );
    assert_eq!(admitted.first_capture_day, Some(day(2024, 6, 30)));
    // §5.3 over two sources: the oldest confirmation wins.
    assert_eq!(admitted.freshness.as_of.as_deref(), Some("2024-06-30"));
    assert_eq!(scenario.manifest_lines(), manifest);
    assert_eq!(scenario.index_rows(), index);
}

/// One synthetic SSE bulletin page: `(id, code, title, day)` announcements.
fn bulletin_page(rows: &[(&str, &str, &str, &str)]) -> Vec<u8> {
    let data: Vec<_> = rows
        .iter()
        .map(|(id, code, title, day)| {
            serde_json::json!({
                "ORG_BULLETIN_ID": id,
                "SECURITY_CODE": code,
                "SECURITY_NAME": "示例",
                "TITLE": title,
                "BULLETIN_TYPE_DESC": "风险警示",
                "SSEDATE": day,
                "URL": format!("/disclosure/{id}.pdf"),
            })
        })
        .collect();
    serde_json::json!({
        "pageHelp": {
            "beginPage": 1,
            "endPage": 1,
            "pageNo": 1,
            "pageSize": 100,
            "pageCount": 1,
            "total": data.len(),
            "data": data,
        }
    })
    .to_string()
    .into_bytes()
}

/// The same announcement answered by both title filters of one window: the
/// event is admitted once, and the row cites the first page that states it.
/// The daily read and `replay --scope risk_warnings` must pick the same page,
/// so the rebuilt set has the daily item's `dataset_digest` and `--verify-only`
/// reports `match` against the rows the daily item wrote.
#[test]
fn an_announcement_on_several_query_pages_cites_the_same_page_in_replay_and_the_daily_item() {
    use crate::security_master::risk_warnings::dataset_digest;
    use crate::source_store::replay::{ReplayRequest, Scope, replay};

    let scenario = Scenario::new();
    let today = day(2024, 7, 1);
    let shared = (
        "fix-shared",
        "600001",
        "示例一关于公司股票被实施其他风险警示的公告",
        "2024-05-06",
    );
    let only_risk = (
        "fix-risk",
        "600002",
        "示例二关于公司股票被实施退市风险警示的公告",
        "2024-05-07",
    );
    let only_special = (
        "fix-special",
        "600003",
        "示例三关于公司股票被实施其他风险警示的公告",
        "2024-05-08",
    );
    // One page per title filter, with different bytes: the shared announcement
    // sits on both, and which page it cites is the only thing that can differ.
    let risk_page = bulletin_page(&[shared, only_risk]);
    let special_page = bulletin_page(&[only_special, shared]);
    assert_ne!(risk_page, special_page);

    freeze_sz_short_name(&scenario, "2024-06-30T02:00:00Z");
    let quiet = quiet_bulletin_page();
    let windows = quarter_segments(day(2000, 1, 1), today).unwrap();
    for (kind, plan) in risk_warnings::plans() {
        let page = match kind {
            risk_warnings::Kind::SzseShortName => continue,
            risk_warnings::Kind::SseRiskWarning => &risk_page,
            risk_warnings::Kind::SseSpecialTreatment => &special_page,
        };
        for window in &windows {
            let body = if window.start() == day(2024, 4, 1) {
                page.clone()
            } else {
                quiet.clone()
            };
            freeze(
                &scenario,
                &plan,
                1,
                Some(*window),
                "START_DATE",
                "END_DATE",
                &body,
                "2024-06-30T02:00:00Z",
            );
        }
    }
    for source in [SourceId::SzseShortName, SourceId::SseBulletin] {
        record_test_refresh_run(
            &scenario.market,
            source,
            "2024-06-30",
            "2024-06-30T02:00:00Z",
        );
    }

    // The daily item: the first filter's page is the first one read.
    let admitted = risk_warnings::read_set(&scenario.store, &scenario.market, today).unwrap();
    let cited = |code: &str| {
        admitted
            .set
            .rows
            .iter()
            .find(|row| row.exchange == Exchange::Sh && row.code == code)
            .and_then(|row| row.source_document_sha256.clone())
    };
    let risk_sha = crate::hash::sha256(&risk_page);
    let special_sha = crate::hash::sha256(&special_page);
    assert_eq!(cited("600001"), Some(risk_sha.clone()));
    assert_eq!(cited("600002"), Some(risk_sha));
    assert_eq!(cited("600003"), Some(special_sha));
    let daily_digest = crate::compact_market::digest_hex(&dataset_digest(&admitted.set.rows));

    // The daily item's write, then the replay of the same store against it.
    let evidence = serde_json::json!({"fixture": true});
    let (manifest, member, event) = (
        crate::hash::sha256(b"manifest"),
        crate::hash::sha256(b"member"),
        crate::hash::sha256(b"event"),
    );
    scenario
        .market
        .commit_risk_warnings(&crate::sqlite_market::risk_warnings::RiskWarningWrite {
            set: &admitted.set,
            task_id: "fixture",
            item_id: "derived:risk_warnings",
            generation_id: "fixture-generation",
            generation_manifest_sha256: &manifest,
            member_sha256: &member,
            security_event_sha256: &event,
            generation_evidence: &evidence,
        })
        .unwrap();
    let Scenario {
        _dir: dir,
        store,
        market,
    } = scenario;
    // The replay opens the database itself and takes its lease.
    drop(market);
    let database = dir.path().join("market.sqlite");

    let report = replay(
        &store,
        &ReplayRequest::new(Some(Scope::RiskWarnings), true, Some(&database)),
    )
    .unwrap();
    let scope = &report.scopes[0];
    assert_eq!(scope.summary.as_deref(), Some(daily_digest.as_str()));
    assert_eq!(scope.database, Some("match"), "{scope:?}");
    assert!(scope.drift.is_empty(), "{:?}", scope.drift);
    // And without a database the rebuilt digest is the same one.
    let computed = replay(
        &store,
        &ReplayRequest::new(Some(Scope::RiskWarnings), false, None),
    )
    .unwrap();
    assert_eq!(
        computed.scopes[0].summary.as_deref(),
        Some(daily_digest.as_str())
    );
}

/// Freeze `risk_page`/`special_page` as the two title filters' answers of the
/// 2024Q2 window (every other quarter quiet) and record the refresh runs.
fn freeze_two_filter_bulletins(
    scenario: &Scenario,
    today: NaiveDate,
    risk_page: &[u8],
    special_page: &[u8],
) {
    freeze_sz_short_name(scenario, "2024-06-30T02:00:00Z");
    let quiet = quiet_bulletin_page();
    let windows = quarter_segments(day(2000, 1, 1), today).unwrap();
    for (kind, plan) in risk_warnings::plans() {
        let page = match kind {
            risk_warnings::Kind::SzseShortName => continue,
            risk_warnings::Kind::SseRiskWarning => risk_page,
            risk_warnings::Kind::SseSpecialTreatment => special_page,
        };
        for window in &windows {
            let body = if window.start() == day(2024, 4, 1) {
                page.to_vec()
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
                "2024-06-30T02:00:00Z",
            );
        }
    }
    for source in [SourceId::SzseShortName, SourceId::SseBulletin] {
        record_test_refresh_run(
            &scenario.market,
            source,
            "2024-06-30",
            "2024-06-30T02:00:00Z",
        );
    }
}

/// Write `set` the way the daily item does.
fn commit_daily(scenario: &Scenario, set: &crate::security_master::risk_warnings::RiskWarningSet) {
    let evidence = serde_json::json!({"fixture": true});
    let (manifest, member, event) = (
        crate::hash::sha256(b"manifest"),
        crate::hash::sha256(b"member"),
        crate::hash::sha256(b"event"),
    );
    scenario
        .market
        .commit_risk_warnings(&crate::sqlite_market::risk_warnings::RiskWarningWrite {
            set,
            task_id: "fixture",
            item_id: "derived:risk_warnings",
            generation_id: "fixture-generation",
            generation_manifest_sha256: &manifest,
            member_sha256: &member,
            security_event_sha256: &event,
            generation_evidence: &evidence,
        })
        .unwrap();
}

/// D-81 R5 end to end: `sh.600001` states an implementation and a revocation
/// on one day, `sh.600002` is ordinary. The daily item publishes the dataset,
/// the conflicting code is `marker_only_unresolved` for that day, and
/// `replay --scope risk_warnings` rebuilds the very same rows (digest and
/// row-by-row `--verify-only`), without a database too.
#[test]
fn a_same_day_conflict_isolates_its_code_and_replay_writes_the_same_rows() {
    use crate::security_master::RiskStatus;
    use crate::security_master::risk_warnings::dataset_digest;
    use crate::source_store::replay::{ReplayRequest, Scope, replay};

    let scenario = Scenario::new();
    let today = day(2024, 7, 1);
    let implement = "关于公司股票被实施其他风险警示的公告";
    let revoke = "关于撤销公司股票其他风险警示的公告";
    let risk_page = bulletin_page(&[
        ("c-impl", "600001", implement, "2024-05-06"),
        ("c-rev", "600001", revoke, "2024-05-06"),
        ("n-impl", "600002", implement, "2024-05-08"),
    ]);
    let special_page = bulletin_page(&[
        ("n-rev", "600002", revoke, "2024-06-03"),
        ("c-rev", "600001", revoke, "2024-05-06"),
    ]);
    assert_ne!(risk_page, special_page);
    freeze_two_filter_bulletins(&scenario, today, &risk_page, &special_page);

    let admitted = risk_warnings::read_set(&scenario.store, &scenario.market, today)
        .expect("one conflicting code does not fail the dataset");
    let sh: Vec<_> = admitted
        .set
        .rows
        .iter()
        .filter(|row| row.exchange == Exchange::Sh)
        .collect();
    let conflict = sh
        .iter()
        .find(|row| row.code == "600001")
        .expect("the conflicting code is written");
    assert_eq!(sh.iter().filter(|row| row.code == "600001").count(), 1);
    assert_eq!(conflict.status, RiskStatus::MarkerOnlyUnresolved);
    assert_eq!(conflict.effective_from, day(2024, 5, 6));
    assert_eq!(conflict.effective_to, Some(day(2024, 5, 7)));
    let ordinary = sh
        .iter()
        .find(|row| row.code == "600002")
        .expect("the other code is published");
    assert_eq!(ordinary.status, RiskStatus::OtherRiskWarning);
    assert_eq!(ordinary.effective_from, day(2024, 5, 8));
    assert_eq!(ordinary.effective_to, Some(day(2024, 6, 3)));

    let evidence = admitted.evidence();
    assert_eq!(evidence["same_day_conflicts"]["count"], 1);
    assert_eq!(
        evidence["same_day_conflicts"]["sample"][0]["code"],
        "600001"
    );
    assert_eq!(
        evidence["same_day_conflicts"]["sample"][0]["day"],
        "2024-05-06"
    );
    assert_eq!(evidence["parser_version"], "risk-warnings-v6");

    let daily_digest = crate::compact_market::digest_hex(&dataset_digest(&admitted.set.rows));
    commit_daily(&scenario, &admitted.set);
    let Scenario {
        _dir: dir,
        store,
        market,
    } = scenario;
    drop(market);
    let database = dir.path().join("market.sqlite");
    let report = replay(
        &store,
        &ReplayRequest::new(Some(Scope::RiskWarnings), true, Some(&database)),
    )
    .unwrap();
    let scope = &report.scopes[0];
    assert_eq!(scope.summary.as_deref(), Some(daily_digest.as_str()));
    assert_eq!(scope.database, Some("match"), "{scope:?}");
    assert!(scope.drift.is_empty(), "{:?}", scope.drift);
    let computed = replay(
        &store,
        &ReplayRequest::new(Some(Scope::RiskWarnings), false, None),
    )
    .unwrap();
    assert_eq!(
        computed.scopes[0].summary.as_deref(),
        Some(daily_digest.as_str())
    );
}

/// D-81 R4 end to end: the two title filters state one announcement with
/// different dates. The daily item fails with `source_invalid` instead of
/// keeping the first date.
#[test]
fn the_daily_item_refuses_one_announcement_stated_with_two_dates() {
    let scenario = Scenario::new();
    let today = day(2024, 7, 1);
    let implement = "关于公司股票被实施其他风险警示的公告";
    let risk_page = bulletin_page(&[("a1", "600001", implement, "2024-05-06")]);
    let special_page = bulletin_page(&[("a1", "600001", implement, "2024-05-07")]);
    freeze_two_filter_bulletins(&scenario, today, &risk_page, &special_page);
    let error = risk_warnings::read_set(&scenario.store, &scenario.market, today)
        .expect_err("a corrected date is not silently resolved");
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("D-81 R4"), "{error}");
}

/// The daily item reads the newest `END_DATE` answer of a window only. An older
/// answer that dates an announcement differently is something only replay
/// holds, so replay must not fail where the daily item succeeds, and both write
/// the newest answer's date.
#[test]
fn replay_does_not_refuse_a_date_only_a_superseded_answer_states() {
    use crate::security_master::risk_warnings::dataset_digest;
    use crate::source_store::replay::{ReplayRequest, Scope, replay};

    let scenario = Scenario::new();
    let today = day(2024, 7, 1);
    let implement = "关于公司股票被实施其他风险警示的公告";
    let newest = bulletin_page(&[("a1", "600001", implement, "2024-05-07")]);
    freeze_two_filter_bulletins(&scenario, today, &newest, &newest);
    let older = bulletin_page(&[("a1", "600001", implement, "2024-05-06")]);
    for (kind, plan) in risk_warnings::plans() {
        if kind == risk_warnings::Kind::SzseShortName {
            continue;
        }
        freeze(
            &scenario,
            &plan,
            1,
            Some(Segment::new(day(2024, 4, 1), day(2024, 5, 31)).unwrap()),
            "START_DATE",
            "END_DATE",
            &older,
            "2024-05-31T02:00:00Z",
        );
    }

    let admitted = risk_warnings::read_set(&scenario.store, &scenario.market, today)
        .expect("the daily item reads the newest answer only");
    let row = admitted
        .set
        .rows
        .iter()
        .find(|row| row.exchange == Exchange::Sh)
        .unwrap();
    assert_eq!(row.effective_from, day(2024, 5, 7));
    let daily_digest = crate::compact_market::digest_hex(&dataset_digest(&admitted.set.rows));
    commit_daily(&scenario, &admitted.set);
    let Scenario {
        _dir: dir,
        store,
        market,
    } = scenario;
    drop(market);
    let database = dir.path().join("market.sqlite");
    let report = replay(
        &store,
        &ReplayRequest::new(Some(Scope::RiskWarnings), true, Some(&database)),
    )
    .unwrap();
    let scope = &report.scopes[0];
    assert_eq!(scope.status, "ok", "{scope:?}");
    assert_eq!(scope.summary.as_deref(), Some(daily_digest.as_str()));
    assert_eq!(scope.database, Some("match"), "{scope:?}");
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

// ------------------------------------------------ R5: known_at (XDM-157) ---

// Shanghai days. The store records UTC instants: 2026-10-31T18:43:40Z is
// already 2026-11-01 in Beijing, and that is the day a row became known here.
const FIRST_CAPTURE: &str = "2026-11-01";
const SECOND_CAPTURE: &str = "2026-11-02";

/// Write the admitted name set the way `derived:security_names` does: one
/// generation binding, then the whole-table replace.
fn write_names(scenario: &Scenario, set: &crate::security_master::names::NameSet) {
    scenario
        .market
        .commit_security_names(&crate::sqlite_market::security_names::SecurityNameWrite {
            set,
            task_id: "task-known-at",
            item_id: "derived:security_names",
            generation_id: "gen-known-at",
            generation_manifest_sha256: &"11".repeat(32),
            member_sha256: &"22".repeat(32),
            security_event_sha256: &"33".repeat(32),
            generation_evidence: &serde_json::json!({"generation_id": "gen-known-at"}),
        })
        .expect("the name history writes");
}

fn stored_names(scenario: &Scenario) -> Vec<crate::sqlite_market::security::SecurityNameRow> {
    scenario
        .market
        .read(crate::sqlite_market::security::read_security_names)
        .expect("the stored rows read back")
}

/// XDM-157 (`issue-446` §5.3/Q4): two collection rounds, offline.
///
/// - A row that was already in the first frozen table keeps the first round's
///   day as its `known_at`, even though the second round rewrites the whole
///   table (the day is the *first* snapshot containing it, not the write day).
/// - A row that only appears in the second snapshot takes the second day.
/// - The read model's `known_at` filter answers accordingly: a PIT query known
///   by the first day does not see the second round's row and counts exactly
///   that one as excluded.
///
/// No transport is involved: both rounds are frozen pages plus a refresh run
/// record, which is the production read path.
#[test]
fn two_collection_rounds_date_new_rows_and_keep_the_first_day_for_unchanged_ones() {
    let scenario = Scenario::new();
    let first_day = day(2026, 11, 1);
    let second_day = day(2026, 11, 2);

    // Round 1: the table is captured for the first time.
    freeze_sz_short_name_body(&scenario, SZSE_NAME_CHANGES, "2026-10-31T18:43:40Z");
    record_test_refresh_run(
        &scenario.market,
        SourceId::SzseShortName,
        FIRST_CAPTURE,
        "2026-10-31T18:50:00Z",
    );
    let first = security_names::read_table(&scenario.store, &scenario.market, first_day)
        .expect("the first round reads its frozen table");
    assert_eq!(
        first.first_capture_day,
        Some(first_day),
        "the store's acquired_at is a UTC instant: 18:43:40Z is 2026-11-01 in Shanghai"
    );
    assert!(
        !first.set.rows.is_empty(),
        "the fixture admitted no name row"
    );
    assert!(
        first
            .set
            .rows
            .iter()
            .all(|row| row.known_at == Some(first_day)),
        "every row of the first table is known from the first capture day"
    );
    write_names(&scenario, &first.set);
    let before = stored_names(&scenario);
    assert_eq!(before.len(), first.set.rows.len());
    assert!(before.iter().all(|row| row.known_at == Some(first_day)));

    // Round 2: same request, new bytes — one change row was appended.
    freeze_sz_short_name_body(&scenario, SZSE_NAME_CHANGES_ROUND2, "2026-11-01T18:10:00Z");
    record_test_refresh_run(
        &scenario.market,
        SourceId::SzseShortName,
        SECOND_CAPTURE,
        "2026-11-01T18:20:00Z",
    );
    let second = security_names::read_table(&scenario.store, &scenario.market, second_day)
        .expect("the second round reads the newest complete table");
    assert_eq!(
        second.first_capture_day,
        Some(first_day),
        "a later snapshot does not move the first capture day"
    );
    let new_rows: Vec<_> = second
        .set
        .rows
        .iter()
        .filter(|row| row.code == "000005")
        .collect();
    assert_eq!(new_rows.len(), 1, "the added change opens one interval");
    assert_eq!(new_rows[0].known_at, Some(second_day));
    assert!(
        second
            .set
            .rows
            .iter()
            .filter(|row| row.code != "000005")
            .all(|row| row.known_at == Some(first_day)),
        "an unchanged row keeps the day of the snapshot that first carried it"
    );

    write_names(&scenario, &second.set);
    let after = stored_names(&scenario);
    assert_eq!(after.len(), before.len() + 1);
    let added = after
        .iter()
        .find(|row| row.code == "000005")
        .expect("the new row is written");
    assert_eq!(added.known_at, Some(second_day));
    assert!(
        after
            .iter()
            .filter(|row| row.code != "000005")
            .all(|row| row.known_at == Some(first_day))
    );

    // The filter the read model applies (`research::temporal`), on the stored
    // rows: known by the first capture day, the second round's row is
    // invisible; known by the second day, every row is admitted.
    use crate::research::temporal::{Temporal, TemporalReport};
    let as_of = day(2026, 11, 30);
    let mut first_pit = TemporalReport::new(Temporal::known_by(as_of, first_day));
    let mut admitted = 0;
    for row in &after {
        if first_pit.observe("security_names", row.known_at) {
            admitted += 1;
        }
    }
    assert_eq!(admitted, after.len() - 1);
    assert_eq!(
        first_pit
            .dataset("security_names")
            .map(|entry| entry.excluded_known_at_rows),
        Some(1),
        "exactly the row first seen in the second round is excluded"
    );
    assert_eq!(first_pit.unknown_known_at_rows(), 0);

    let mut second_pit = TemporalReport::new(Temporal::known_by(as_of, second_day));
    assert!(
        after
            .iter()
            .all(|row| second_pit.observe("security_names", row.known_at))
    );
    assert_eq!(
        second_pit
            .dataset("security_names")
            .map(|entry| entry.excluded_known_at_rows),
        Some(0)
    );
}

/// The knowledge day is a **Shanghai** day, not the UTC date of `acquired_at`
/// (Claude review, XDM-157). A snapshot stored between 16:00Z and 24:00Z is
/// already the next day in Beijing; dating it by the UTC day would let a
/// `known_at` filter admit the row a day before we had it.
#[test]
fn a_snapshot_is_dated_in_shanghai_not_in_utc() {
    for (fetched_at, expected) in [
        // The production first capture: 02:43 Beijing time on 2026-11-01.
        ("2026-10-31T18:43:40Z", day(2026, 11, 1)),
        ("2026-11-01T15:59:59Z", day(2026, 11, 1)),
        ("2026-11-01T16:00:00Z", day(2026, 11, 2)),
    ] {
        let scenario = Scenario::new();
        freeze_sz_short_name_body(&scenario, SZSE_NAME_CHANGES, fetched_at);
        let read = security_names::read_table(&scenario.store, &scenario.market, day(2026, 11, 2))
            .expect("the frozen table reads");
        assert_eq!(read.first_capture_day, Some(expected), "{fetched_at}");
        assert!(
            read.set
                .rows
                .iter()
                .all(|row| row.known_at == Some(expected)),
            "{fetched_at}"
        );
    }
}

// ------------------------------------------------- ETF name chains (v2) ---

const SZSE_NAME_HEADER: [&str; 3] = ["证券代码", "证券简称", "基金管理人"];

/// An SZSE workbook that clears the 700-row shape floor. `named` rows come
/// first with the given names; the rest are filler. `generated_at` varies the
/// bytes without varying the cells.
fn szse_named_book(named: &[(&str, &str)], generated_at: &str) -> Vec<u8> {
    let mut rows: Vec<Vec<String>> = named
        .iter()
        .map(|(code, name)| vec![code.to_string(), name.to_string(), "合成基金".to_string()])
        .collect();
    for index in 0..etf_list::SZSE_MIN_ROWS {
        rows.push(vec![
            format!("16{index:04}"),
            format!("填充ETF{index}"),
            "合成基金".to_string(),
        ]);
    }
    super::test_szse_xlsx(&SZSE_NAME_HEADER, &rows, generated_at)
}

fn sse_named_body(named: &[(&str, &str)]) -> Vec<u8> {
    let data: Vec<serde_json::Value> = named
        .iter()
        .map(|(code, name)| {
            serde_json::json!({
                "fundCode": code,
                "secNameFull": name,
                "fundAbbr": "短名",
                "listingDate": "20100101",
                "subClass": "01",
            })
        })
        .collect();
    serde_json::json!({
        "pageHelp": {"pageNo": 1, "pageCount": 1, "total": data.len(), "data": data}
    })
    .to_string()
    .into_bytes()
}

fn plan_of(kind: etf_list::ListKind) -> AcquisitionPlan {
    etf_list::plans()
        .into_iter()
        .find(|(candidate, _)| *candidate == kind)
        .unwrap()
        .1
}

fn chain_of(
    admitted: &etf_list::Admitted,
    exchange: Exchange,
    code: &str,
) -> Vec<(String, String)> {
    admitted
        .names
        .get(&(exchange, code.to_string()))
        .map(|chain| {
            chain
                .iter()
                .map(|step| (step.observed_from.to_string(), step.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn pair(day: &str, name: &str) -> (String, String) {
    (day.to_string(), name.to_string())
}

/// Evidence §1: the SZSE workbook embeds its generation time, so identical
/// cells still freeze as a new sha. Every refresh is a manifest line; the
/// chain merges the repeats, and a rename and a return (A→B→A) are all there.
#[test]
fn szse_names_follow_every_refresh_including_a_to_b_to_a() {
    let scenario = Scenario::new();
    let szse = plan_of(etf_list::ListKind::Szse);
    let sse = plan_of(etf_list::ListKind::Sse);
    let a = "创业板ETF易方达";
    let b = "创业板ETF";
    let rounds: [(&str, &str, &str); 5] = [
        // Same cells twice, different generation stamp: two shas, one name.
        ("2026-11-05T02:00:00Z", "2026-11-05T00:10:00Z", a),
        ("2026-11-06T02:00:00Z", "2026-11-06T00:10:00Z", a),
        ("2026-11-07T02:00:00Z", "2026-11-07T00:10:00Z", b),
        ("2026-11-08T02:00:00Z", "2026-11-08T00:10:00Z", b),
        ("2026-11-09T02:00:00Z", "2026-11-09T00:10:00Z", a),
    ];
    for (fetched_at, generated_at, name) in rounds {
        let body = szse_named_book(&[("159915", name), ("159916", "深证成指ETF")], generated_at);
        freeze(&scenario, &szse, 1, None, "", "", &body, fetched_at);
    }
    freeze(
        &scenario,
        &sse,
        1,
        None,
        "",
        "",
        &sse_named_body(&[("510300", "沪深300ETF华泰柏瑞")]),
        "2026-11-09T02:00:00Z",
    );
    for source in [SourceId::SseEtfList, SourceId::SzseEtfList] {
        record_test_refresh_run(
            &scenario.market,
            source,
            "2026-11-09",
            "2026-11-09T03:00:00Z",
        );
    }
    assert_eq!(
        scenario
            .store
            .records()
            .unwrap()
            .iter()
            .filter(|record| record.source_id == "szse_etf_list")
            .count(),
        5,
        "each SZSE refresh is a new manifest line even with unchanged cells"
    );
    let admitted = etf_list::read(&scenario.store, &scenario.market, day(2026, 11, 10)).unwrap();
    assert_eq!(
        chain_of(&admitted, Exchange::Sz, "159915"),
        vec![
            pair("2026-11-05", a),
            pair("2026-11-07", b),
            pair("2026-11-09", a),
        ]
    );
    assert_eq!(
        chain_of(&admitted, Exchange::Sz, "159916"),
        vec![pair("2026-11-05", "深证成指ETF")],
        "a code whose name never changed is one segment, not one per refresh"
    );
    assert_eq!(
        chain_of(&admitted, Exchange::Sh, "510300"),
        vec![pair("2026-11-09", "沪深300ETF华泰柏瑞")]
    );
    assert!(admitted.names_behind_current().is_empty());
    // The selected current row carries the newest name; history rows carry none.
    let current = admitted
        .rows
        .iter()
        .find(|row| row.code == "159915")
        .unwrap();
    assert_eq!(current.name.as_deref(), Some(a));
    assert!(admitted.history.iter().all(|row| row.name.is_none()));
}

/// Design §1.2 limit 2 / evidence §5: the SSE response is deterministic JSON,
/// so the third fetch (back to A) is byte-identical to the first and the store
/// adds no manifest line. The chain ends at B while the selected list says A,
/// and that disagreement is reported instead of repaired with an invented day.
#[test]
fn sse_a_to_b_to_a_is_swallowed_by_dedupe_and_reported_not_repaired() {
    let scenario = Scenario::new();
    let szse = plan_of(etf_list::ListKind::Szse);
    let sse = plan_of(etf_list::ListKind::Sse);
    let a = "沪深300ETF华泰柏瑞";
    let b = "沪深300ETF";
    let body_a = sse_named_body(&[("510300", a), ("510310", "沪深300ETF易方达")]);
    let body_b = sse_named_body(&[("510300", b), ("510310", "沪深300ETF易方达")]);
    let szse_body = szse_named_book(&[("159915", "创业板ETF易方达")], "2026-11-05T00:10:00Z");
    freeze(
        &scenario,
        &sse,
        1,
        None,
        "",
        "",
        &body_a,
        "2026-11-05T02:00:00Z",
    );
    freeze(
        &scenario,
        &sse,
        1,
        None,
        "",
        "",
        &body_b,
        "2026-11-06T02:00:00Z",
    );
    let lines_before = scenario.manifest_lines();
    // Day 3: the exchange publishes A again.
    freeze(
        &scenario,
        &sse,
        1,
        None,
        "",
        "",
        &body_a,
        "2026-11-07T02:00:00Z",
    );
    assert_eq!(
        scenario.manifest_lines(),
        lines_before,
        "the repeat of A adds no manifest line"
    );
    freeze(
        &scenario,
        &szse,
        1,
        None,
        "",
        "",
        &szse_body,
        "2026-11-07T02:00:00Z",
    );
    // The day-3 run fetched A's bytes, so it selected A's snapshot.
    let sha_a = crate::hash::sha256(&body_a);
    super::record_test_refresh_run_selecting(
        &scenario.market,
        SourceId::SseEtfList,
        Some(&sha_a),
        "2026-11-07",
        "2026-11-07T03:00:00Z",
    );
    record_test_refresh_run(
        &scenario.market,
        SourceId::SzseEtfList,
        "2026-11-07",
        "2026-11-07T03:00:00Z",
    );
    let admitted = etf_list::read(&scenario.store, &scenario.market, day(2026, 11, 8)).unwrap();
    let current = admitted
        .rows
        .iter()
        .find(|row| row.code == "510300")
        .unwrap();
    assert_eq!(current.name.as_deref(), Some(a), "the selected list says A");
    assert_eq!(
        chain_of(&admitted, Exchange::Sh, "510300"),
        vec![pair("2026-11-05", a), pair("2026-11-06", b)],
        "the manifest has A on day 1 and B on day 2 and nothing after"
    );
    assert_eq!(
        admitted.names_behind_current(),
        vec![(Exchange::Sh, "510300".to_string())],
        "only the code whose name actually returned is flagged"
    );
    assert_eq!(
        chain_of(&admitted, Exchange::Sh, "510310"),
        vec![pair("2026-11-05", "沪深300ETF易方达")]
    );
}

/// The read is a pure function of the frozen bytes: the same store reads to the
/// same chains, and an SSE rename that does round-trip through the manifest
/// (A→B on distinct bytes) is a plain two-segment chain.
#[test]
fn name_chains_are_deterministic_and_an_sse_rename_is_two_segments() {
    let scenario = Scenario::new();
    let sse = plan_of(etf_list::ListKind::Sse);
    let szse = plan_of(etf_list::ListKind::Szse);
    freeze(
        &scenario,
        &sse,
        1,
        None,
        "",
        "",
        &sse_named_body(&[("510300", "沪深300ETF华泰柏瑞")]),
        "2026-11-05T02:00:00Z",
    );
    freeze(
        &scenario,
        &sse,
        1,
        None,
        "",
        "",
        &sse_named_body(&[("510300", "沪深300ETF")]),
        "2026-11-06T02:00:00Z",
    );
    freeze(
        &scenario,
        &szse,
        1,
        None,
        "",
        "",
        &szse_named_book(&[], "2026-11-06T00:10:00Z"),
        "2026-11-06T02:00:00Z",
    );
    for source in [SourceId::SseEtfList, SourceId::SzseEtfList] {
        record_test_refresh_run(
            &scenario.market,
            source,
            "2026-11-06",
            "2026-11-06T03:00:00Z",
        );
    }
    let first = etf_list::read(&scenario.store, &scenario.market, day(2026, 11, 7)).unwrap();
    let second = etf_list::read(&scenario.store, &scenario.market, day(2026, 11, 7)).unwrap();
    assert_eq!(first.names, second.names);
    assert_eq!(
        chain_of(&first, Exchange::Sh, "510300"),
        vec![
            pair("2026-11-05", "沪深300ETF华泰柏瑞"),
            pair("2026-11-06", "沪深300ETF"),
        ]
    );
    assert!(first.names_behind_current().is_empty());
}

/// A code whose name is missing in one snapshot keeps its other names, and the
/// gap neither ends nor starts a segment.
#[test]
fn a_snapshot_without_a_usable_name_does_not_break_the_chain() {
    let scenario = Scenario::new();
    let sse = plan_of(etf_list::ListKind::Sse);
    let szse = plan_of(etf_list::ListKind::Szse);
    let named = |name: serde_json::Value| {
        serde_json::json!({
            "pageHelp": {"pageNo": 1, "pageCount": 1, "total": 1, "data": [
                {"fundCode": "510300", "secNameFull": name, "listingDate": "20100101"}
            ]}
        })
        .to_string()
        .into_bytes()
    };
    for (at, name) in [
        ("2026-11-05T02:00:00Z", serde_json::json!("甲ETF")),
        ("2026-11-06T02:00:00Z", serde_json::json!("")),
        ("2026-11-07T02:00:00Z", serde_json::json!("甲ETF")),
        ("2026-11-08T02:00:00Z", serde_json::json!(null)),
    ] {
        freeze(&scenario, &sse, 1, None, "", "", &named(name), at);
    }
    freeze(
        &scenario,
        &szse,
        1,
        None,
        "",
        "",
        &szse_named_book(&[], "2026-11-08T00:10:00Z"),
        "2026-11-08T02:00:00Z",
    );
    for source in [SourceId::SseEtfList, SourceId::SzseEtfList] {
        record_test_refresh_run(
            &scenario.market,
            source,
            "2026-11-08",
            "2026-11-08T03:00:00Z",
        );
    }
    let admitted = etf_list::read(&scenario.store, &scenario.market, day(2026, 11, 9)).unwrap();
    assert_eq!(
        chain_of(&admitted, Exchange::Sh, "510300"),
        vec![pair("2026-11-05", "甲ETF")]
    );
    let current = admitted
        .rows
        .iter()
        .find(|row| row.code == "510300")
        .unwrap();
    assert_eq!(
        current.name, None,
        "no name is invented for the current row"
    );
    assert!(
        admitted.names_behind_current().is_empty(),
        "a missing current name is not a disagreement"
    );
}
