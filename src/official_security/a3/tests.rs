//! A-3 suspension/resumption adapter tests (M3 PR-11). Everything is fixture:
//! the frozen pages live in `tests/fixtures/exchange-a3/`, the transport is
//! scripted, and no test opens a socket.
//!
//! The one real page (`sse_gw_pl_jyts_tfpxx_20261001_20261024_p1.json`) is the
//! 2026-10-01…24 answer D-60 sampled (`total=28`, LXTP 20 / LSTP 8); page 1
//! carries 25 rows, 7 of them SSE A shares. It pins the field names and the
//! `endStopDate` semantics against the live shape, while the synthetic 2008
//! windows pin the failure and skip rules.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use serde_json::json;

use super::*;
use crate::exchange_source::{HttpRequest, HttpResponse, RetryPolicy, Transport, TransportError};
use crate::security_master::suspensions::{assemble_suspensions, suspension_source};
use crate::security_master::{
    DelistingSource, Exchange, IssuerResolution, ListingSource, SecurityLifecycleFact,
    SuspensionCertainty, SuspensionReason,
};
use crate::source_store::SourceStore;
use crate::sqlite_market::backend::MarketBackend;
use crate::sqlite_tasks::SqliteTasks;

const Q1_EMPTY: &[u8] = include_bytes!("../../../tests/fixtures/exchange-a3/window_2008_q1.json");
const Q2_PAGE1: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a3/window_2008_q2_p1.json");
const Q2_PAGE2: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a3/window_2008_q2_p2.json");
const REAL_20261024: &[u8] = include_bytes!(
    "../../../tests/fixtures/exchange-a3/sse_gw_pl_jyts_tfpxx_20261001_20261024_p1.json"
);

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// Scripted transport: answers one frozen page per request and remembers the
/// URLs, so a paging or window regression is visible in the assertions.
struct Pages {
    fail: Option<u16>,
    answers: Mutex<VecDeque<Vec<u8>>>,
    requests: Mutex<Vec<String>>,
}

impl Pages {
    fn serving_fixtures() -> Arc<Self> {
        Arc::new(Self {
            fail: None,
            answers: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn failing(status: u16) -> Arc<Self> {
        Arc::new(Self {
            fail: Some(status),
            answers: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Transport for Pages {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(request.url.clone());
        if let Some(status) = self.fail {
            return Ok(HttpResponse {
                status,
                etag: None,
                last_modified: None,
                body: Vec::new(),
            });
        }
        let body = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| page_for(&request.url));
        Ok(HttpResponse {
            status: 200,
            etag: None,
            last_modified: None,
            body,
        })
    }
}

/// Route a request the way the endpoint does: by window and page number.
fn page_for(url: &str) -> Vec<u8> {
    if url.contains("endStopDate=20080331") {
        Q1_EMPTY.to_vec()
    } else if url.contains("endStopDate=20080630") {
        if url.contains("pageHelp.pageNo=2") {
            Q2_PAGE2.to_vec()
        } else {
            Q2_PAGE1.to_vec()
        }
    } else {
        panic!("the test transport was asked for an unexpected window: {url}")
    }
}

struct Shared(Arc<Pages>);

impl Transport for Shared {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        self.0.get(request)
    }
}

fn client(pages: &Arc<Pages>) -> Client {
    Client::with_transport(Shared(pages.clone()))
        .with_policy(RetryPolicy::single())
        .without_sleep()
}

fn tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: SourceStore,
    market: MarketBackend,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempdir();
        let store = SourceStore::new(dir.path().join("source-store"));
        let tasks = SqliteTasks::new(dir.path().join("market.sqlite"));
        tasks.initialize_compact().unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(tasks.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let market = MarketBackend::open(&tasks).unwrap();
        Self {
            _dir: dir,
            store,
            market,
        }
    }
}

/// A main-table interval for an SSE A share (or any exchange, for the SZ
/// checks).
fn member(
    exchange: Exchange,
    code: &str,
    listing: NaiveDate,
    delisting: Option<NaiveDate>,
) -> SecurityLifecycleFact {
    SecurityLifecycleFact {
        exchange,
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
        delisting_date: delisting,
        delisting_date_tdx: None,
        delisting_source: DelistingSource::Unknown,
        delisting_reason_class: crate::security_master::DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: false,
        delisting_conflict: false,
        known_at: None,
    }
}

fn sh(code: &str) -> SecurityLifecycleFact {
    member(Exchange::Sh, code, day(1995, 1, 2), None)
}

fn sh_members() -> Vec<SecurityLifecycleFact> {
    vec![
        sh("600001"),
        sh("600018"),
        sh("600680"),
        sh("600849"),
        sh("601607"),
    ]
}

fn acquired(fixture: &Fixture, pages: &Arc<Pages>, members: &[SecurityLifecycleFact]) -> Admitted {
    acquire(
        &fixture.store,
        &fixture.market,
        &client(pages),
        members,
        day(2008, 7, 10),
    )
    .expect("the fixtures are admitted")
}

#[test]
fn windows_cover_2008_to_yesterday_in_quarters() {
    let windows = windows(day(2008, 7, 10)).unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].start(), day(2008, 1, 1));
    assert_eq!(windows[0].end(), day(2008, 3, 31));
    assert_eq!(windows[1].start(), day(2008, 4, 1));
    assert_eq!(windows[1].end(), day(2008, 6, 30));

    // Today is never queried: an incomplete day cannot prove anything.
    let later = windows(day(2026, 10, 30)).unwrap();
    assert_eq!(later.last().unwrap().end(), day(2026, 10, 29));

    // 2008-01-01 has no complete day before it.
    assert!(windows(day(2008, 1, 1)).is_err());
}

#[test]
fn plan_uses_the_soa_dialect_and_treats_quiet_windows_as_not_observed() {
    let plan = plan();
    assert_eq!(plan.source_id, SourceId::SseSuspension);
    assert_eq!(plan.format, PagingFormat::SseSoaPageNo);
    assert_eq!(plan.zero_policy, ZeroPolicy::NotObserved);
    assert_eq!(plan.params.get("sqlId").map(String::as_str), Some(SQL_ID));
    assert_eq!(
        plan.params.get("isPagination").map(String::as_str),
        Some("true")
    );
    assert_eq!(plan.params.get("productCode").map(String::as_str), Some(""));
    assert_eq!(plan.params.get("keyWords").map(String::as_str), Some(""));

    let window = Segment::parse("2008-04-01", "2008-06-30").unwrap();
    let request = plan.request(2, Some(window), FROM_PARAM, TO_PARAM);
    assert_eq!(
        request.params.get("pageHelp.pageNo").map(String::as_str),
        Some("2")
    );
    assert_eq!(
        request.params.get("pageHelp.pageSize").map(String::as_str),
        Some("25")
    );
    assert_eq!(
        request.params.get(FROM_PARAM).map(String::as_str),
        Some("2008-04-01")
    );
    assert_eq!(
        request.params.get(TO_PARAM).map(String::as_str),
        Some("2008-06-30")
    );
}

#[test]
fn acquire_keeps_only_full_day_sse_a_share_events() {
    let fixture = Fixture::new();
    let pages = Pages::serving_fixtures();
    let admitted = acquired(&fixture, &pages, &sh_members());

    assert_eq!(admitted.windows, 2);
    assert_eq!(
        admitted.not_observed_windows,
        vec!["2008-01-01..2008-03-31".to_string()]
    );
    assert_eq!(admitted.snapshot.as_of, day(2008, 7, 10));
    assert_eq!(admitted.counters.rows_read, 6);
    assert_eq!(
        admitted.counters.non_a_share_skipped, 1,
        "the bond is not an A share"
    );
    assert_eq!(
        admitted.counters.intraday_skipped, 1,
        "stopTime=930 is an intraday halt"
    );
    assert_eq!(
        admitted.counters.unclassified_skipped, 1,
        "a multi-day LSTP is not a whole-day halt"
    );
    assert_eq!(admitted.unclosed_rows, 1);

    // 600001 (LXTP range), 600018 (LSTP whole day), 601607 (not resumed yet).
    let facts = &admitted.snapshot.facts;
    assert_eq!(facts.len(), 3, "{facts:#?}");
    assert_eq!(facts[0].entity(), "sh.600001");
    assert_eq!(facts[0].start_day, day(2008, 5, 5));
    assert_eq!(facts[0].end_day, day(2008, 5, 7));
    assert_eq!(facts[1].entity(), "sh.600018");
    assert_eq!(facts[1].start_day, day(2008, 6, 2));
    assert_eq!(facts[1].end_day, day(2008, 6, 2));
    assert_eq!(facts[2].entity(), "sh.601607");
    assert_eq!(facts[2].start_day, day(2008, 6, 10));
    assert_eq!(
        facts[2].end_day,
        day(2008, 6, 30),
        "an unclosed interval ends with the window it was observed in"
    );
    for fact in facts {
        assert_eq!(fact.exchange, Exchange::Sh);
        assert_eq!(fact.reason, SuspensionReason::ExchangeAnnouncement);
        assert_eq!(fact.source_kind, SOURCE_KIND);
        assert!(
            fact.known_at.is_none(),
            "the event table carries no announcement day (D-49)"
        );
        assert!(fact.source_record_id.is_some());
    }

    // Every request is a closed quarter, never "today".
    let requests = pages.requests();
    assert_eq!(
        requests.len(),
        3,
        "two Q1 pages? no: Q1 has one page, Q2 two"
    );
    assert!(
        requests
            .iter()
            .all(|url| url.contains("sqlId=GW_PL_JYTS_TFPXX"))
    );
    assert!(
        !requests
            .iter()
            .any(|url| url.contains("endStopDate=20080710"))
    );

    let evidence = admitted.evidence();
    assert_eq!(evidence["adapter"], json!("A-3"));
    assert_eq!(evidence["parser_version"], json!(PARSER_VERSION));
    assert_eq!(
        evidence["not_observed_windows"][0],
        json!("2008-01-01..2008-03-31")
    );
    assert_eq!(evidence["confirmed_rows"], json!(3));
}

#[test]
fn a_quiet_window_writes_no_confirmed_row() {
    let fixture = Fixture::new();
    let pages = Pages::serving_fixtures();
    // 2008-04-10: only the empty 2008 Q1 window is complete.
    let admitted = acquire(
        &fixture.store,
        &fixture.market,
        &client(&pages),
        &sh_members(),
        day(2008, 4, 10),
    )
    .unwrap();
    assert_eq!(admitted.windows, 1);
    assert_eq!(admitted.confirmed_rows(), 0);
    assert_eq!(
        admitted.not_observed_windows,
        vec!["2008-01-01..2008-03-31".to_string()]
    );
    // The evidence says "not observed", never "no suspension".
    assert_eq!(admitted.evidence()["confirmed_rows"], json!(0));
}

#[test]
fn a_source_failure_is_not_a_zero_row_answer() {
    let fixture = Fixture::new();
    let pages = Pages::failing(500);
    let error = acquire(
        &fixture.store,
        &fixture.market,
        &client(&pages),
        &sh_members(),
        day(2008, 7, 10),
    )
    .expect_err("a 500 is a failure, not an empty window");
    assert_eq!(error.code(), "partially_failed");
}

#[test]
fn an_event_of_an_entity_outside_the_main_table_is_counted_not_written() {
    let fixture = Fixture::new();
    let pages = Pages::serving_fixtures();
    let mut members = sh_members();
    members.retain(|fact| fact.code != "601607");
    let admitted = acquired(&fixture, &pages, &members);
    assert_eq!(admitted.confirmed_rows(), 2);
    assert_eq!(admitted.counters.unknown_entities, 1);
    assert_eq!(admitted.counters.unknown_entity_samples, vec!["sh.601607"]);
}

#[test]
fn an_interval_is_clipped_to_the_member_window() {
    let fixture = Fixture::new();
    let pages = Pages::serving_fixtures();
    let mut members = sh_members();
    // 600001 lists on 2008-05-06, so the 05-05..05-07 event may only claim
    // 05-06 and 05-07 (BR-M3-2: no day before listing).
    for fact in &mut members {
        if fact.code == "600001" {
            fact.listing_date = Some(day(2008, 5, 6));
            fact.listing_date_tdx = Some(day(2008, 5, 6));
            fact.effective_from = day(2008, 5, 6);
        }
    }
    let admitted = acquired(&fixture, &pages, &members);
    let fact = admitted
        .snapshot
        .facts
        .iter()
        .find(|fact| fact.code == "600001")
        .expect("600001 is still suspended inside its member window");
    assert_eq!(fact.start_day, day(2008, 5, 6));
    assert_eq!(fact.end_day, day(2008, 5, 7));
    assert_eq!(admitted.counters.clipped, 1);
}

#[test]
fn a_member_window_before_the_event_drops_it() {
    let fixture = Fixture::new();
    let pages = Pages::serving_fixtures();
    let mut members = sh_members();
    // 600018 delists before its 2008-06-02 suspension: the interval has no
    // day inside the member window and is dropped, counted.
    for fact in &mut members {
        if fact.code == "600018" {
            fact.delisting_date = Some(day(2008, 1, 31));
        }
    }
    let admitted = acquired(&fixture, &pages, &members);
    assert_eq!(admitted.confirmed_rows(), 2);
    assert_eq!(admitted.counters.outside_window_dropped, 1);
}

#[test]
fn confirmed_and_unresolved_gaps_are_never_double_counted() {
    let fixture = Fixture::new();
    let pages = Pages::serving_fixtures();
    let members = vec![sh("600001"), sh("600018")];
    let admitted = acquired(&fixture, &pages, &members);

    let calendar = [
        day(2008, 5, 2),
        day(2008, 5, 5),
        day(2008, 5, 6),
        day(2008, 5, 7),
        day(2008, 5, 8),
        day(2008, 5, 9),
        day(2008, 6, 2),
        day(2008, 6, 3),
        day(2008, 6, 4),
        day(2008, 6, 5),
    ];
    let mut bars: BTreeMap<String, Vec<NaiveDate>> = BTreeMap::new();
    // 600001 has bars before and after the confirmed 05-05..05-07 halt.
    bars.insert(
        "sh.600001".to_string(),
        vec![day(2008, 5, 2), day(2008, 5, 8), day(2008, 5, 9)],
    );
    // 600018 has a real, unexplained hole on 06-03..06-04.
    bars.insert(
        "sh.600018".to_string(),
        vec![day(2008, 6, 2), day(2008, 6, 5)],
    );
    let set = assemble_suspensions(
        &suspension_source(Some(admitted.snapshot.clone())),
        &members,
        &calendar,
        &bars,
    )
    .expect("the admitted facts assemble");

    assert_eq!(set.confirmed_rows, 2, "{:#?}", set.rows);
    assert_eq!(set.gap_rows, 1, "{:#?}", set.rows);
    // The explained days are counted once, as the confirmed row's
    // `missing_days`; the derived rows only count the unexplained day pair.
    assert_eq!(set.missing_days(), 3 + 1 + 2);
    let confirmed: Vec<_> = set
        .rows
        .iter()
        .filter(|row| row.certainty == SuspensionCertainty::Confirmed)
        .collect();
    let gaps: Vec<_> = set
        .rows
        .iter()
        .filter(|row| row.certainty == SuspensionCertainty::UnresolvedGap)
        .collect();
    for gap in &gaps {
        for fact in &confirmed {
            assert!(
                gap.code != fact.code
                    || gap.end_day < fact.start_day
                    || fact.end_day < gap.start_day,
                "gap {gap:?} overlaps confirmed {fact:?}"
            );
        }
        assert_eq!(gap.known_at, None);
    }
}

#[test]
fn without_the_admitted_source_sz_stays_unresolved_gap() {
    let calendar = [day(2008, 6, 2), day(2008, 6, 3), day(2008, 6, 4)];
    let members = vec![member(Exchange::Sz, "000001", day(1991, 4, 3), None)];
    let mut bars: BTreeMap<String, Vec<NaiveDate>> = BTreeMap::new();
    bars.insert(
        "sz.000001".to_string(),
        vec![day(2008, 6, 2), day(2008, 6, 4)],
    );
    let set = assemble_suspensions(&suspension_source(None), &members, &calendar, &bars).unwrap();
    assert_eq!(set.confirmed_rows, 0);
    assert_eq!(set.gap_rows, 1);
    assert_eq!(set.pending_sources.len(), 1);
    assert!(
        set.rows
            .iter()
            .all(|row| row.certainty == SuspensionCertainty::UnresolvedGap)
    );
}

#[test]
fn reparse_from_the_source_store_reproduces_the_facts() {
    let fixture = Fixture::new();
    let pages = Pages::serving_fixtures();
    let members = sh_members();
    let admitted = acquired(&fixture, &pages, &members);

    let index = fixture.market.read_exchange_snapshots().unwrap();
    assert!(
        index
            .iter()
            .all(|row| row.source_id == SourceId::SseSuspension.as_str()),
        "only this adapter's pages are in the fresh index"
    );
    let replayed = reparse(&fixture.store, &index, &members, day(2008, 7, 10)).unwrap();
    assert_eq!(replayed.snapshot.facts, admitted.snapshot.facts);
    assert_eq!(replayed.not_observed_windows, admitted.not_observed_windows);
    assert_eq!(replayed.counters, admitted.counters);
    assert_eq!(
        replayed.snapshot_ids.len(),
        2,
        "the empty page is not re-parsed as a fact page"
    );
}

#[test]
fn the_real_2026_10_page_is_read_with_the_recorded_field_names() {
    let window = Segment::parse("2026-10-01", "2026-10-24").unwrap();
    let page = parse_page(REAL_20261024, window, "fixture").expect("the sampled page parses");
    assert_eq!(page.rows_read, 25);
    assert_eq!(
        page.non_a_share_skipped, 18,
        "bonds, preferred stock, funds"
    );
    assert_eq!(page.intraday_skipped, 0);
    assert_eq!(page.unclassified_skipped, 0);
    assert_eq!(page.events.len(), 7, "{:#?}", page.events);
    let fact = page
        .events
        .iter()
        .find(|event| event.code == "603183")
        .expect("600183-style main-board halt");
    // D-60: `endStopDate` is the day before resumption, used as-is (inclusive).
    assert_eq!(fact.start_day, day(2026, 9, 30));
    assert_eq!(fact.end_day, day(2026, 10, 8));
    assert!(!fact.unclosed);
    let unclosed: Vec<&str> = page
        .events
        .iter()
        .filter(|event| event.unclosed)
        .map(|event| event.code.as_str())
        .collect();
    assert_eq!(unclosed, vec!["601326"]);
    assert!(
        page.events
            .iter()
            .all(|event| event.code.starts_with('6') && event.snapshot_id == "fixture")
    );
    assert_eq!(
        page.events
            .iter()
            .filter(|event| event.code == "603517")
            .count(),
        1,
        "a one-day LSTP/WH halt is a full-day suspension"
    );
}

#[test]
fn a_row_with_an_unknown_shape_fails_closed() {
    let body = json!({
        "pageHelp": {"data": [{"productCode": "600001", "type": "LXTP", "startStopDate": "20080505"}]}
    });
    let window = Segment::parse("2008-04-01", "2008-06-30").unwrap();
    let error = parse_page(&body.to_string().into_bytes(), window, "fixture")
        .expect_err("missing row fields are not guessed");
    assert_eq!(error.code(), "source_invalid");
}

#[test]
fn an_unknown_event_type_fails_closed() {
    let body = json!({
        "pageHelp": {"data": [{
            "startStopDate": "20080505",
            "stopReason": "重要公告",
            "productCode": "600001",
            "endStopReason": "重要公告",
            "controlType": "TR",
            "endStopDate": "20080507",
            "stopTime": "",
            "type": "XXXX",
            "productName": "邯郸钢铁"
        }]}
    });
    let window = Segment::parse("2008-04-01", "2008-06-30").unwrap();
    let error = parse_page(&body.to_string().into_bytes(), window, "fixture")
        .expect_err("an unknown type is a vocabulary change");
    assert_eq!(error.code(), "source_invalid");
}

#[test]
fn the_plan_is_registered_under_the_suspension_source_id() {
    assert_eq!(SourceId::SseSuspension.as_str(), "sse_suspension");
    assert_eq!(
        crate::exchange_source::SourceId::parse("sse_suspension"),
        Some(SourceId::SseSuspension)
    );
}
