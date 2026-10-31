//! A-1/A-2 adapter tests (M3 PR-3). Everything is fixture: the frozen pages
//! live in `tests/fixtures/exchange-a1a2/`, the transport is scripted, and no
//! test opens a socket.
//!
//! The five codes the design names (`600001`/`000003`/`600849`/`300114`/
//! `302132`) are exercised through the real assembly and the real data
//! pipeline seams, so a parser change that breaks the merge fails here.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::exchange_source::validate;
use crate::exchange_source::{
    self, HttpRequest, HttpResponse, PagingFormat, RetryPolicy, Transport, TransportError,
};
use crate::security_master::assembly::{self, Assembly, Conflict};
use crate::security_master::{
    ArrangementFact, DelistingSource, Exchange, IssuerResolution, ListingSource,
    SecurityLifecycleFact,
};
use crate::sqlite_market::backend::MarketBackend;
use crate::sqlite_market::derived::read_version;
use crate::sqlite_market::security::{read_security_master, replace_security_master};
use crate::sqlite_tasks::SqliteTasks;

const SSE_LISTING_MAIN: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a1a2/sse_listing_main.json");
const SSE_LISTING_STAR: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a1a2/sse_listing_star.json");
const SSE_DELISTING_MAIN: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a1a2/sse_delisting_main.json");
const SSE_DELISTING_STAR: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a1a2/sse_delisting_star.json");
const SZSE_LISTING: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a1a2/szse_listing.json");
const SZSE_DELISTING_MAIN: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a1a2/szse_delisting_main.json");
const SZSE_DELISTING_MAIN_P2: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a1a2/szse_delisting_main_p2.json");
const SZSE_DELISTING_MAIN_B_ONLY: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a1a2/szse_delisting_main_b_only.json");
const SZSE_DELISTING_NM: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a1a2/szse_delisting_nm.json");
const SSE_QUERY_PAGE_IGNORED: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-a1a2/sse_query_page_ignored.json");

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// The frozen page that answers one request, chosen from the request URL the
/// same way the exchanges route it (sqlId + filters / CATALOGID + module).
fn page_for(url: &str) -> Vec<u8> {
    if url.contains("sseQuery") {
        let delisting = url.contains("COMPANY_STATUS=3");
        let star = url.contains("STOCK_TYPE=8");
        match (delisting, star) {
            (true, true) => SSE_DELISTING_STAR.to_vec(),
            (true, false) => SSE_DELISTING_MAIN.to_vec(),
            (false, true) => SSE_LISTING_STAR.to_vec(),
            (false, false) => SSE_LISTING_MAIN.to_vec(),
        }
    } else if url.contains("CATALOGID=1793_ssgs") {
        if url.contains("selectModule=nm") {
            SZSE_DELISTING_NM.to_vec()
        } else if url.contains("PAGENO=2") {
            SZSE_DELISTING_MAIN_P2.to_vec()
        } else {
            SZSE_DELISTING_MAIN.to_vec()
        }
    } else {
        SZSE_LISTING.to_vec()
    }
}

/// Scripted transport: one queued answer per request (or the same list page
/// forever). Never touches a network.
struct Lists {
    fail: Option<u16>,
    answers: Mutex<VecDeque<Vec<u8>>>,
    requests: Mutex<Vec<String>>,
}

impl Lists {
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

impl Transport for Lists {
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

/// Wraps the shared transport so `Client` can own a `'static` value.
struct Shared(Arc<Lists>);

impl Transport for Shared {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        self.0.get(request)
    }
}

fn client(lists: &Arc<Lists>) -> Client {
    Client::with_transport(Shared(lists.clone()))
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
    tasks: SqliteTasks,
    market: MarketBackend,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempdir();
        let store = SourceStore::new(dir.path().join("source-store"));
        let tasks = SqliteTasks::new(dir.path().join("market.sqlite"));
        tasks.initialize_compact().unwrap();
        // The compact lease refuses a world-readable database file.
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(tasks.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let market = MarketBackend::open(&tasks).unwrap();
        Self {
            _dir: dir,
            store,
            tasks,
            market,
        }
    }

    fn connection(&self) -> Connection {
        self.tasks.connect(false).unwrap()
    }
}

/// A TDX lifecycle fact: `SSDATE` is the subject-level listing day (#399 A-5).
fn tdx(code: &str, exchange: Exchange, ssdate: NaiveDate) -> SecurityLifecycleFact {
    SecurityLifecycleFact {
        exchange,
        code: code.to_string(),
        effective_from: ssdate,
        effective_to: None,
        instrument_class: 0,
        issuer_key: None,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_date: Some(ssdate),
        listing_date_tdx: Some(ssdate),
        listing_source: ListingSource::TdxSsdate,
        b_listing_date: None,
        delisting_date: None,
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

/// The TDX facts of the fixture generation: one interval per code, the
/// five key codes included. `302132` carries the inherited split-day SSDATE,
/// which the confirmed migration must move to 2025-02-17.
fn tdx_facts() -> Vec<SecurityLifecycleFact> {
    use Exchange::{Sh, Sz};
    vec![
        tdx("600001", Sh, day(1998, 6, 18)),
        tdx("600018", Sh, day(2006, 10, 26)),
        tdx("600680", Sh, day(1993, 10, 18)),
        tdx("600849", Sh, day(1994, 3, 24)),
        tdx("601607", Sh, day(1994, 3, 24)),
        tdx("688555", Sh, day(2020, 7, 22)),
        tdx("688981", Sh, day(2020, 7, 16)),
        tdx("000001", Sz, day(1991, 4, 3)),
        tdx("000002", Sz, day(1991, 1, 29)),
        tdx("000003", Sz, day(1991, 1, 14)),
        tdx("000004", Sz, day(1991, 1, 14)),
        tdx("300114", Sz, day(2010, 8, 27)),
        tdx("301569", Sz, day(2026, 10, 15)),
        tdx("302132", Sz, day(2010, 8, 27)),
    ]
}

fn acquired(fixture: &Fixture, lists: &Arc<Lists>) -> Admitted {
    acquire(&fixture.store, &fixture.market, &client(lists)).expect("the fixtures are admitted")
}

fn assemble(admitted: &Admitted) -> Assembly {
    assemble_with(admitted, &tdx_facts(), &[])
}

fn assemble_with(
    admitted: &Admitted,
    tdx: &[SecurityLifecycleFact],
    arrangements: &[ArrangementFact],
) -> Assembly {
    assembly::assemble_security_master(
        tdx,
        arrangements,
        assembly::exchange_source(Some(admitted.snapshot.clone())),
        &candidate_migrations(),
    )
    .expect("the fixture facts assemble")
}

fn row<'a>(assembly: &'a Assembly, exchange: Exchange, code: &str) -> &'a SecurityLifecycleFact {
    let rows: Vec<&SecurityLifecycleFact> = assembly
        .rows
        .iter()
        .filter(|row| row.exchange == exchange && row.code == code)
        .collect();
    assert_eq!(rows.len(), 1, "expected exactly one interval for {code}");
    rows[0]
}

#[test]
fn seven_requests_reuse_the_pr2_source_ids() {
    let plans = plans();
    assert_eq!(plans.len(), 7);
    let ids: Vec<&str> = plans
        .iter()
        .map(|(_, plan)| plan.source_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "sse_listing",
            "sse_listing",
            "sse_delisting",
            "sse_delisting",
            "szse_listing",
            "szse_delisting",
            "szse_delisting",
        ]
    );
    // The SSE terminated list must never accept a risk-warning row as a
    // delisting: the request filters `COMPANY_STATUS=3` (§7.2).
    let (_, delisting) = plans
        .iter()
        .find(|(kind, _)| *kind == ListKind::SseDelistingMain)
        .unwrap();
    assert_eq!(
        delisting.params.get("COMPANY_STATUS").map(String::as_str),
        Some("3")
    );
    assert_eq!(
        delisting.params.get("STOCK_TYPE").map(String::as_str),
        Some(SSE_MAIN_BOARD_A)
    );
    let (_, szse) = plans
        .iter()
        .find(|(kind, _)| *kind == ListKind::SzseDelistingMain)
        .unwrap();
    assert_eq!(
        szse.params.get("selectModule").map(String::as_str),
        Some("main")
    );
    assert_eq!(
        szse.params.get("SHOWTYPE").map(String::as_str),
        Some("JSON")
    );
    assert_eq!(szse.format, PagingFormat::SzseMetadata);
    // #446 D1: the terminated modules declare which block they mean. `1110`
    // (the current list) answers one non-empty block, so it declares none.
    for kind in [
        ListKind::SzseDelistingMain,
        ListKind::SzseDelistingShortName,
    ] {
        let (_, plan) = plans.iter().find(|(found, _)| *found == kind).unwrap();
        assert_eq!(
            plan.params.get("TABKEY").map(String::as_str),
            Some(SZSE_DELISTING_TABKEY),
            "{}",
            kind.label()
        );
    }
    let (_, listing) = plans
        .iter()
        .find(|(found, _)| *found == ListKind::SzseListing)
        .unwrap();
    assert_eq!(listing.params.get("TABKEY"), None);
    for (_, plan) in plans {
        assert_eq!(
            plan.basis_status,
            crate::exchange_source::BasisStatus::ApprovedSource
        );
        assert_eq!(
            plan.required_basis,
            crate::exchange_source::BasisStatus::ApprovedSource
        );
    }
}

#[test]
fn b_shares_are_filtered_and_star_chinext_rows_are_kept() {
    let sse_main = parse_page(ListKind::SseListingMain, None, SSE_LISTING_MAIN).unwrap();
    assert_eq!(
        sse_main.b_shares_skipped, 1,
        "the 900901 B row is not a fact"
    );
    let codes: Vec<&str> = sse_main
        .listing
        .iter()
        .map(|fact| fact.code.as_str())
        .collect();
    assert_eq!(codes, vec!["600001", "600018", "600680", "601607"]);
    assert!(sse_main.delisting.is_empty());

    // A+B company: only the A row survives, so no false listing conflict can
    // be built from the B row's different date (§7.1).
    let star = parse_page(ListKind::SseListingStar, None, SSE_LISTING_STAR).unwrap();
    assert_eq!(star.listing.len(), 2);
    assert!(star.listing.iter().all(|fact| fact.instrument_class == 0));
    assert_eq!(star.b_shares_skipped, 0);

    let szse = parse_page(ListKind::SzseListing, None, SZSE_LISTING).unwrap();
    assert_eq!(szse.b_shares_skipped, 1, "the 200002 B row is filtered");
    let codes: Vec<&str> = szse.listing.iter().map(|fact| fact.code.as_str()).collect();
    assert_eq!(
        codes,
        vec!["000001", "000002", "000004", "301569", "302132"],
        "ChiNext (301569, 302132) is A shares; the old 300114 code is not in the list"
    );
    // The SZSE list has no security-type column, so an unknown prefix fails
    // closed instead of becoming an A-share fact.
    let unknown = br#"[{"metadata":{"pageNo":1,"pagecount":1,"recordcount":1},"data":[{"zqdm":"400001","zqjc":"x","agssrq":"19910101"}]}]"#;
    let error = parse_page(ListKind::SzseListing, None, unknown).unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("400001"));

    // An unknown SSE `STOCK_TYPE` is a vocabulary change: refuse it instead of
    // treating the row as an A share.
    let sse_unknown = br#"{"pageHelp":{"pageNo":1,"pageSize":2000,"pageCount":1,"total":1,"data":[{"A_STOCK_CODE":"600001","COMPANY_CODE":"600001","LIST_DATE":"19980618","STOCK_TYPE":"9"}]}}"#;
    let error = parse_page(ListKind::SseListingMain, None, sse_unknown).unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("STOCK_TYPE"));

    let delisting = parse_page(
        ListKind::SzseDelistingMain,
        Some("tab2"),
        SZSE_DELISTING_MAIN,
    )
    .unwrap();
    assert_eq!(delisting.b_shares_skipped, 1);
    assert_eq!(delisting.delisting.len(), 1);
    assert_eq!(delisting.delisting_rows_without_date, 0);
    assert_eq!(delisting.delisting[0].code, "000003");
    assert_eq!(delisting.delisting[0].delisting_date, day(2002, 6, 14));
    assert_eq!(
        delisting.delisting[0].reason_class,
        DelistingReason::Unknown,
        "D-46: the reason class is NOT_AVAILABLE"
    );

    // A terminated-list row whose delisting day is empty is not a delisting
    // fact (the list also carries suspended companies), but the count stays
    // visible; a page where no row has the date is a shape change.
    let mixed = br#"[{"metadata":{"pageNo":1,"pagecount":1,"recordcount":2},"data":[{"zqdm":"000003","ssrq":"19910703","zzrq":"20020614"},{"zqdm":"000005","ssrq":"19920101","zzrq":""}]}]"#;
    let page = parse_page(ListKind::SzseDelistingMain, None, mixed).unwrap();
    assert_eq!(page.delisting.len(), 1);
    assert_eq!(page.delisting_rows_without_date, 1);
    let none = br#"[{"metadata":{"pageNo":1,"pagecount":1,"recordcount":1},"data":[{"zqdm":"000005","ssrq":"19920101","zzrq":""}]}]"#;
    let error = parse_page(ListKind::SzseDelistingMain, None, none).unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("delisting date"));
}

#[test]
fn the_szse_terminated_list_is_read_from_the_tab2_block() {
    // Both fixtures start with the empty `tab1` (暂停上市) block whose
    // `pagecount=0`; reading `[0]` would fail as `page_count is 0` (main) or
    // drop every row.
    let first = parse_page(
        ListKind::SzseDelistingMain,
        Some("tab2"),
        SZSE_DELISTING_MAIN,
    )
    .unwrap();
    assert_eq!(first.b_shares_skipped, 1);
    assert_eq!(first.delisting.len(), 1);
    assert_eq!(first.delisting[0].code, "000003");

    let second = parse_page(
        ListKind::SzseDelistingMain,
        Some("tab2"),
        SZSE_DELISTING_MAIN_P2,
    )
    .unwrap();
    assert_eq!(second.delisting.len(), 1);
    assert_eq!(second.delisting[0].code, "000005");
    assert_eq!(second.delisting[0].delisting_date, day(2024, 4, 26));

    // A tab1 that carries rows must not win over tab2.
    let two_blocks = br#"[
      {"metadata":{"tabkey":"tab1","pageno":1,"pagecount":1,"recordcount":1},
       "data":[{"zqdm":"000001","ssrq":"1991-04-03","zzrq":"2020-01-01"}]},
      {"metadata":{"tabkey":"tab2","pageno":1,"pagecount":1,"recordcount":1},
       "data":[{"zqdm":"000003","ssrq":"1991-01-14","zzrq":"2002-06-14"}]}
    ]"#;
    let page = parse_page(ListKind::SzseDelistingMain, Some("tab2"), two_blocks).unwrap();
    assert_eq!(page.delisting.len(), 1);
    assert_eq!(page.delisting[0].code, "000003");

    // No block carries the declared tabkey: fail closed, never fall back to
    // the first block or to "tab2".
    let ambiguous = br#"[
      {"metadata":{"tabkey":"tab1","pageno":1,"pagecount":1,"recordcount":1},
       "data":[{"zqdm":"000001","ssrq":"1991-04-03","zzrq":"2020-01-01"}]},
      {"metadata":{"tabkey":"tab3","pageno":1,"pagecount":1,"recordcount":1},
       "data":[{"zqdm":"000003","ssrq":"1991-01-14","zzrq":"2002-06-14"}]}
    ]"#;
    let error = parse_page(ListKind::SzseDelistingMain, Some("tab2"), ambiguous).unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("declared tabkey"), "{error}");
    assert!(error.to_string().contains("tab1"), "{error}");
}

#[test]
fn the_current_szse_list_is_read_from_the_only_non_empty_block() {
    // #446 D1: `CATALOGID=1110` answers four blocks and only `tab1` carries the
    // current list (`tab2`/`tab3`/`tab4` are empty with `pagecount=0`). The
    // request declares no `TABKEY`, so the only non-empty block decides; the
    // old "tab2 first" rule read the empty block and later failed
    // `page_count is 0`.
    let envelope = validate::parse_page(PagingFormat::SzseMetadata, None, SZSE_LISTING).unwrap();
    assert_eq!(
        (
            envelope.page,
            envelope.page_count,
            envelope.rows,
            envelope.total
        ),
        (1, 1, 6, Some(6)),
        "the counts and the rows come from tab1, the block with data"
    );
    let page = parse_page(ListKind::SzseListing, None, SZSE_LISTING).unwrap();
    assert_eq!(page.b_shares_skipped, 1);
    let codes: Vec<&str> = page.listing.iter().map(|fact| fact.code.as_str()).collect();
    assert_eq!(
        codes,
        vec!["000001", "000002", "000004", "301569", "302132"]
    );
    let selection = page.block_selection.as_ref().expect("SZSE block evidence");
    assert_eq!(selection.basis.as_str(), "unique_non_empty");
    assert_eq!(selection.tabkey.as_deref(), Some("tab1"));
    assert_eq!(selection.blocks.len(), 4);
    assert_eq!(selection.blocks[0].tabkey.as_deref(), Some("tab1"));
    assert_eq!(selection.blocks[0].rows, 6);
    assert_eq!(selection.blocks[0].page_count, Some(1));
    assert_eq!(selection.blocks[1].tabkey.as_deref(), Some("tab2"));
    assert_eq!(selection.blocks[1].rows, 0);
    assert_eq!(selection.blocks[1].page_count, Some(0));

    // A declaration wins over "the only non-empty block": tab2 is empty
    // here, and the selection follows the request instead of silently
    // switching to tab1. The window then fails the zero-row policy, which is
    // the honest answer to "the block I asked for has nothing".
    let declared = validate::parse_page(PagingFormat::SzseMetadata, Some("tab2"), SZSE_LISTING)
        .expect("the declared block exists, it is just empty");
    assert_eq!((declared.page_count, declared.rows), (0, 0));

    // A declared tabkey no block carries is a source change, not a fallback.
    let error =
        validate::parse_page(PagingFormat::SzseMetadata, Some("tab9"), SZSE_LISTING).unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("tab9"), "{error}");
}

#[test]
fn the_szse_code_column_is_zqdm_or_agdm() {
    // #446 D2: the current list (`CATALOGID=1110`) carries `agdm`, the
    // terminated lists carry `zqdm`. The row states which one it has; a code
    // is never taken from the plan name.
    let body = |tabkey: &str, fields: &str| {
        format!(
            "[{{\"metadata\":{{\"tabkey\":\"{tabkey}\",\"pageno\":1,\"pagecount\":1,\"recordcount\":1}},\n               \"data\":[{{{fields}}}]}}]"
        )
        .into_bytes()
    };
    // `agdm` alone (the 1110 row shape).
    let page = parse_page(
        ListKind::SzseListing,
        None,
        &body("tab1", r#""agdm":"000001","agssrq":"19910403""#),
    )
    .unwrap();
    assert_eq!(page.listing[0].code, "000001");
    // `zqdm` alone (the terminated-list row shape).
    let page = parse_page(
        ListKind::SzseDelistingMain,
        Some("tab2"),
        &body(
            "tab2",
            r#""zqdm":"000003","ssrq":"1991-01-14","zzrq":"2002-06-14""#,
        ),
    )
    .unwrap();
    assert_eq!(page.delisting[0].code, "000003");
    // Both present and agreeing is one code.
    let page = parse_page(
        ListKind::SzseListing,
        None,
        &body(
            "tab1",
            r#""zqdm":"000001","agdm":"000001","agssrq":"19910403""#,
        ),
    )
    .unwrap();
    assert_eq!(page.listing[0].code, "000001");
    // Both present and disagreeing is a shape change, not a guess.
    let error = parse_page(
        ListKind::SzseListing,
        None,
        &body(
            "tab1",
            r#""zqdm":"000001","agdm":"000002","agssrq":"19910403""#,
        ),
    )
    .unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("disagree"), "{error}");
    // Neither column: fail closed with both names.
    let error = parse_page(
        ListKind::SzseListing,
        None,
        &body("tab1", r#""agjc":"x","agssrq":"19910403""#),
    )
    .unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("zqdm nor agdm"), "{error}");
}

#[test]
fn a_page_of_only_b_shares_is_not_a_shape_change() {
    // #446 D3: the 9th SZSE main page is ten B shares (200160/200168/200413/
    // 200418/200513/200613/200706/200770/200771/200986). Filtering leaves no
    // fact, but the page is not a renamed date column.
    let page = parse_page(
        ListKind::SzseDelistingMain,
        Some("tab2"),
        SZSE_DELISTING_MAIN_B_ONLY,
    )
    .unwrap();
    assert!(page.delisting.is_empty());
    assert_eq!(page.b_shares_skipped, 10);
    assert_eq!(page.delisting_rows_without_date, 0);

    // One A-share row without the date is still a shape change, and the
    // message carries the three counts the operator needs.
    let a_only = br#"[{"metadata":{"tabkey":"tab2","pageno":9,"pagecount":9,"recordcount":1},
      "data":[{"zqdm":"000005","ssrq":"1992-01-01","zzrq":""}]}]"#;
    let error = parse_page(ListKind::SzseDelistingMain, Some("tab2"), a_only).unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    let message = error.to_string();
    assert!(message.contains("1 row(s)"), "{message}");
    assert!(message.contains("1 A-share row(s)"), "{message}");
    assert!(message.contains("0 B-share row(s)"), "{message}");
    assert!(message.contains("delisting date"), "{message}");

    // A mixed page is filtered first: only the A row can trigger the guard.
    let mixed = br#"[{"metadata":{"tabkey":"tab2","pageno":9,"pagecount":9,"recordcount":2},
      "data":[{"zqdm":"200160"},{"zqdm":"000005","ssrq":"1992-01-01","zzrq":""}]}]"#;
    let error = parse_page(ListKind::SzseDelistingMain, Some("tab2"), mixed).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("2 row(s)"), "{message}");
    assert!(message.contains("1 A-share row(s)"), "{message}");
    assert!(message.contains("1 B-share row(s)"), "{message}");
}

#[test]
fn a_synthetic_shared_code_is_merged_across_the_two_szse_modules() {
    // Constructed: the 2026-10-30 main/nm first pages share no code, so the
    // real fixtures no longer claim an overlap. This keeps the merge rule
    // covered with a made-up duplicate (noted in the fixture README).
    let page = |kind: ListKind, code: &str| {
        let body = format!(
            "[{{\"metadata\":{{\"tabkey\":\"tab2\",\"pageno\":1,\"pagecount\":1,\"recordcount\":1}},\
               \"data\":[{{\"zqdm\":\"{code}\",\"ssrq\":\"1991-01-14\",\"zzrq\":\"2002-06-14\"}}]}}]"
        );
        parse_page(kind, Some("tab2"), body.as_bytes()).unwrap()
    };
    let mut facts = page(ListKind::SzseDelistingMain, "000003").delisting;
    facts.extend(page(ListKind::SzseDelistingShortName, "000003").delisting);
    let merged = merge_delisting(facts).unwrap();
    assert_eq!(merged.len(), 1, "the same code and day is one fact");
    assert_eq!(merged[0].code, "000003");
}

#[test]
fn the_sse_code_is_a_stock_code_with_a_company_code_fallback() {
    let body = |row: &str| {
        format!(
            "{{\"pageHelp\":{{\"pageNo\":1,\"pageSize\":2000,\"pageCount\":1,\"total\":1,\"data\":[{row}]}}}}"
        )
        .into_bytes()
    };
    // `COMPANY_CODE` alone still yields the code (the real rows carry both).
    let page = parse_page(
        ListKind::SseListingMain,
        None,
        &body(r#"{"COMPANY_CODE":"600018","LIST_DATE":"20000719","STOCK_TYPE":"1"}"#),
    )
    .unwrap();
    assert_eq!(page.listing[0].code, "600018");
    // `A_STOCK_CODE` wins when both exist and agree.
    let page = parse_page(
        ListKind::SseListingMain,
        None, &body(
            r#"{"A_STOCK_CODE":"600018","COMPANY_CODE":"600018","LIST_DATE":"20000719","STOCK_TYPE":"1"}"#,
        ),
    )
    .unwrap();
    assert_eq!(page.listing[0].code, "600018");
    // Both and disagreeing is a shape change: never guessed.
    let error = parse_page(
        ListKind::SseListingMain,
        None, &body(
            r#"{"A_STOCK_CODE":"600018","COMPANY_CODE":"600019","LIST_DATE":"20000719","STOCK_TYPE":"1"}"#,
        ),
    )
    .unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("disagree"), "{error}");
    // Neither field is a shape change too.
    let error = parse_page(
        ListKind::SseListingMain,
        None,
        &body(r#"{"COMPANY_ABBR":"x","LIST_DATE":"20000719","STOCK_TYPE":"1"}"#),
    )
    .unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("neither"), "{error}");
}

#[test]
fn acquire_merges_the_four_lists_and_the_five_key_codes() {
    let fixture = Fixture::new();
    let lists = Lists::serving_fixtures();
    let admitted = acquired(&fixture, &lists);
    assert_eq!(
        lists.requests().len(),
        8,
        "seven plans; the SZSE main terminated list has two pages"
    );

    let codes: Vec<String> = admitted
        .snapshot
        .listing
        .iter()
        .map(|fact| format!("{}.{}", fact.exchange.as_str(), fact.code))
        .collect();
    for expected in [
        "sh.600001",
        "sh.601607",
        "sh.688555",
        "sh.688981",
        "sz.000001",
        "sz.000002",
        "sz.000003",
        "sz.000004",
        "sz.301569",
    ] {
        assert!(codes.contains(&expected.to_string()), "missing {expected}");
    }
    for absent in ["sh.600849", "sz.300114", "sz.302132"] {
        assert!(
            !codes.contains(&absent.to_string()),
            "{absent} must not come from an exchange listing fact"
        );
    }
    assert_eq!(admitted.suppressed_listing_targets, vec!["sz.302132"]);
    assert_eq!(admitted.b_shares_skipped, 3);

    let delistings: Vec<(String, NaiveDate)> = admitted
        .snapshot
        .delisting
        .iter()
        .map(|fact| {
            (
                format!("{}.{}", fact.exchange.as_str(), fact.code),
                fact.delisting_date,
            )
        })
        .collect();
    assert_eq!(
        delistings,
        vec![
            ("sh.600001".to_string(), day(2009, 12, 29)),
            ("sh.600680".to_string(), day(2010, 11, 5)),
            ("sh.688555".to_string(), day(2023, 7, 5)),
            ("sz.000003".to_string(), day(2002, 6, 14)),
            ("sz.000005".to_string(), day(2024, 4, 26)),
            ("sz.300023".to_string(), day(2022, 6, 29)),
        ],
        "the SZSE main list spans PAGENO=1/2 and its two modules are merged; B rows are not facts"
    );

    // One confirmed migration (D-46) plus the candidate from A-2.
    assert_eq!(admitted.snapshot.migrations.len(), 1);
    let migration = &admitted.snapshot.migrations[0];
    assert_eq!(
        (migration.from_code.as_str(), migration.to_code.as_str()),
        ("300114", "302132")
    );
    assert_eq!(migration.effective_from, day(2025, 2, 17));
    let candidate = candidate_migrations();
    assert_eq!(candidate.len(), 1);
    assert_eq!(candidate[0].certainty, MigrationCertainty::Candidate);
    assert_eq!(
        (
            candidate[0].from_code.as_str(),
            candidate[0].to_code.as_str()
        ),
        ("600849", "601607")
    );

    // Binding evidence: four sources, every snapshot id named, the consumer
    // parser version recorded.
    let evidence = admitted.evidence();
    assert_eq!(evidence["parser_version"], PARSER_VERSION);
    assert_eq!(evidence["b_shares_skipped"], 3);
    assert_eq!(evidence["listing_facts"], admitted.snapshot.listing.len());
    let bindings = evidence["source_bindings"].as_array().unwrap();
    let sources: Vec<&str> = bindings
        .iter()
        .map(|binding| binding["source_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        sources,
        vec![
            "sse_delisting",
            "sse_listing",
            "szse_delisting",
            "szse_listing"
        ]
    );
    let ids = evidence["snapshot_ids"].as_object().unwrap();
    assert_eq!(ids.len(), 7, "one snapshots entry per plan kind");
    assert!(
        ids.values()
            .all(|list| !list.as_array().unwrap().is_empty())
    );
    assert_eq!(
        admitted.bindings.len(),
        4,
        "binding is per source_id, not per request"
    );
}

#[test]
fn frozen_szse_bytes_replay_to_the_same_row_level_facts() {
    // Regression for #446 D1/D2/D3: `source_store replay` only re-derives the
    // envelope, and all three defects passed it. This test re-reads every
    // frozen SZSE page from the store (no network) and re-derives the row-level
    // facts with the same block selection the acquisition used.
    let fixture = Fixture::new();
    let lists = Lists::serving_fixtures();
    let admitted = acquired(&fixture, &lists);

    struct Expected {
        label: &'static str,
        kind: ListKind,
        declared: Option<&'static str>,
        page_count: u32,
        rows: usize,
        listing: usize,
        delisting: usize,
        b_shares: usize,
    }
    let expected = [
        Expected {
            label: "szse_listing",
            kind: ListKind::SzseListing,
            declared: None,
            page_count: 1,
            rows: 6,
            listing: 5,
            delisting: 0,
            b_shares: 1,
        },
        Expected {
            label: "szse_delisting_main",
            kind: ListKind::SzseDelistingMain,
            declared: Some("tab2"),
            page_count: 2,
            rows: 3,
            listing: 2,
            delisting: 2,
            b_shares: 2,
        },
        Expected {
            label: "szse_delisting_nm",
            kind: ListKind::SzseDelistingShortName,
            declared: Some("tab2"),
            page_count: 1,
            rows: 1,
            listing: 1,
            delisting: 1,
            b_shares: 0,
        },
    ];
    for case in expected {
        let ids = &admitted.snapshot_ids[case.label];
        assert_eq!(
            ids.len() as u32,
            case.page_count,
            "{}: frozen page count",
            case.label
        );
        let mut rows = 0;
        let mut listing = 0;
        let mut delisting = 0;
        let mut b_shares = 0;
        for id in ids {
            let bytes = fixture.store.read_verified(id).unwrap();
            let envelope =
                validate::parse_page(PagingFormat::SzseMetadata, case.declared, &bytes).unwrap();
            let page = parse_page(case.kind, case.declared, &bytes).unwrap();
            rows += envelope.rows;
            listing += page.listing.len();
            delisting += page.delisting.len();
            b_shares += page.b_shares_skipped;
            if case.kind.listing() {
                assert_eq!(
                    envelope.rows,
                    page.listing.len() + page.b_shares_skipped,
                    "{}: envelope rows are the A-share facts plus the skipped B rows",
                    case.label
                );
            } else {
                assert_eq!(
                    envelope.rows,
                    page.delisting.len() + page.b_shares_skipped + page.delisting_rows_without_date,
                    "{}: envelope rows are the delisting facts plus skipped/undated rows",
                    case.label
                );
            }
        }
        assert_eq!(
            (rows, listing, delisting, b_shares),
            (case.rows, case.listing, case.delisting, case.b_shares),
            "{}: row-level replay",
            case.label
        );
    }

    // The evidence names the selected block and every block's row/page counts,
    // so a mis-selected block is visible without downloading the page again.
    let evidence = admitted.evidence();
    let listing = &evidence["block_selection"]["szse_listing"];
    assert_eq!(listing["pages"], json!(1));
    assert_eq!(listing["basis"]["unique_non_empty"], json!(1));
    assert_eq!(listing["selected_tabkey"]["tab1"], json!(1));
    assert_eq!(listing["blocks"][0]["tabkey"], json!("tab1"));
    assert_eq!(listing["blocks"][0]["rows"], json!(6));
    assert_eq!(listing["blocks"][0]["page_count"], json!(1));
    assert_eq!(listing["blocks"][1]["tabkey"], json!("tab2"));
    assert_eq!(listing["blocks"][1]["rows"], json!(0));
    assert_eq!(listing["blocks"][1]["page_count"], json!(0));
    let main = &evidence["block_selection"]["szse_delisting_main"];
    assert_eq!(main["pages"], json!(2));
    assert_eq!(main["basis"]["declared_tabkey"], json!(2));
    assert_eq!(main["selected_tabkey"]["tab2"], json!(2));
    assert_eq!(main["blocks"][0]["tabkey"], json!("tab1"));
    assert_eq!(main["blocks"][1]["tabkey"], json!("tab2"));
    assert_eq!(main["blocks"][1]["rows"], json!(3));
    assert_eq!(main["blocks"][1]["page_count"], json!(2));
}

#[test]
fn a_listing_page_that_ignores_the_page_size_fails_closed() {
    // 2026-10-30 evidence: the endpoint ignored `beginPage`/`endPage` and
    // answered page 1 again. The list plans are single-page, so a `pageCount=2`
    // envelope means the request's `pageHelp.pageSize` was ignored too; taking
    // page 1 as the list would silently drop rows.
    let fixture = Fixture::new();
    let lists = Lists::serving_fixtures();
    lists
        .answers
        .lock()
        .unwrap()
        .push_back(SSE_QUERY_PAGE_IGNORED.to_vec());
    let error = acquire(&fixture.store, &fixture.market, &client(&lists)).unwrap_err();
    assert_eq!(error.code(), "page_size_ignored");
    let detail = error.to_string();
    assert!(
        detail.contains("declared total 100 but collected 50 row(s)"),
        "{detail}"
    );
    assert!(detail.contains("pageCount 2"), "{detail}");
    assert_eq!(
        lists.requests().len(),
        1,
        "the single-page plan must not send a second request"
    );
    // The frozen page is still indexed (a window verdict, not index state).
    assert_eq!(fixture.market.read_exchange_snapshots().unwrap().len(), 1);
}

#[test]
fn assembly_merges_the_exchange_lists_with_the_tdx_facts() {
    let fixture = Fixture::new();
    let lists = Lists::serving_fixtures();
    let admitted = acquired(&fixture, &lists);
    let assembled = assemble(&admitted);
    assert!(assembled.pending_sources.is_empty());
    assert_eq!(assembled.exchange_as_of, Some(admitted.snapshot.as_of));

    // 600001 / 000003: the exchange terminated list supplies the delisting day
    // and closes the interval (§7.2 priority: exchange > TDX arrangement).
    for (code, delisted) in [("600001", day(2009, 12, 29)), ("000003", day(2002, 6, 14))] {
        let exchange = if code.starts_with('6') {
            Exchange::Sh
        } else {
            Exchange::Sz
        };
        let row = row(&assembled, exchange, code);
        assert_eq!(row.delisting_date, Some(delisted));
        assert_eq!(row.delisting_source, DelistingSource::ExchangeList);
        assert_eq!(row.effective_to, Some(delisted));
        assert!(!row.delisting_conflict);
    }

    // 302132: the confirmed migration owns the interval start; the inherited
    // exchange "A股上市日期" (2010-08-27) did not open it.
    let old = row(&assembled, Exchange::Sz, "300114");
    assert_eq!(old.effective_to, Some(day(2025, 2, 17)));
    let new = row(&assembled, Exchange::Sz, "302132");
    assert_eq!(new.effective_from, day(2025, 2, 17));
    assert_eq!(new.effective_to, None);
    assert_eq!(
        new.listing_date,
        Some(day(2010, 8, 27)),
        "the exchange listing day is the code's inherited subject-level value"
    );

    // 600849 -> 601607 stays a candidate: both intervals survive untouched
    // (no collapse, no pool de-duplication, D-46).
    let old = row(&assembled, Exchange::Sh, "600849");
    assert_eq!(old.effective_from, day(1994, 3, 24));
    assert_eq!(old.effective_to, None);
    let new = row(&assembled, Exchange::Sh, "601607");
    assert_eq!(new.effective_from, day(1994, 3, 24));
    assert_eq!(new.effective_to, None);
    let candidates: Vec<&CodeMigrationFact> = assembled
        .migrations
        .iter()
        .filter(|migration| migration.certainty == MigrationCertainty::Candidate)
        .collect();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].from_entity(), "sh.600849");
    assert_eq!(candidates[0].to_entity(), "sh.601607");
}

#[test]
fn a_listing_date_disagreement_keeps_both_sides() {
    let fixture = Fixture::new();
    let lists = Lists::serving_fixtures();
    let admitted = acquired(&fixture, &lists);
    let assembled = assemble(&admitted);

    // sh.600018: the exchange `LIST_DATE` is 2000-07-19 (the code's first
    // listing) while TDX `SSDATE` is 2006-10-26 (the current subject). Two
    // intervals, both flagged, both values kept (#396 finding 1).
    let rows: Vec<&SecurityLifecycleFact> = assembled
        .rows
        .iter()
        .filter(|row| row.exchange == Exchange::Sh && row.code == "600018")
        .collect();
    assert_eq!(rows.len(), 2);
    let earlier = rows
        .iter()
        .find(|row| row.effective_from == day(2000, 7, 19))
        .expect("the exchange-dated interval exists");
    let later = rows
        .iter()
        .find(|row| row.effective_from == day(2006, 10, 26))
        .expect("the TDX-dated interval exists");
    assert_eq!(earlier.effective_to, Some(day(2006, 10, 26)));
    assert_eq!(earlier.listing_date, Some(day(2000, 7, 19)));
    assert_eq!(later.listing_date, Some(day(2006, 10, 26)));
    assert_eq!(later.listing_date_tdx, Some(day(2006, 10, 26)));
    assert!(earlier.listing_conflict && later.listing_conflict);
    assert!(assembled.conflicts.contains(&Conflict::ListingDate {
        exchange: Exchange::Sh,
        code: "600018".into(),
        exchange_date: day(2000, 7, 19),
        tdx_date: day(2006, 10, 26),
    }));
    assert_eq!(assembled.conflicts.len(), 1);
}

#[test]
fn a_delisting_date_disagreement_keeps_both_sides() {
    let fixture = Fixture::new();
    let lists = Lists::serving_fixtures();
    let admitted = acquired(&fixture, &lists);
    // TDX's arrangement ends 2009-12-28; the exchange says 2009-12-29. The
    // exchange wins the delisting day, TDX's value stays, the row is flagged.
    let arrangements = vec![ArrangementFact {
        exchange: Exchange::Sh,
        code: "600001".into(),
        start: Some(day(2009, 12, 1)),
        end: Some(day(2009, 12, 28)),
    }];
    let assembled = assemble_with(&admitted, &tdx_facts(), &arrangements);
    let row = row(&assembled, Exchange::Sh, "600001");
    assert!(row.delisting_conflict);
    assert_eq!(row.delisting_date, Some(day(2009, 12, 29)));
    assert_eq!(row.delisting_date_tdx, Some(day(2009, 12, 28)));
    assert_eq!(row.arrangement_start, Some(day(2009, 12, 1)));
    assert_eq!(row.arrangement_end, Some(day(2009, 12, 28)));
    assert!(assembled.conflicts.contains(&Conflict::DelistingDate {
        exchange: Exchange::Sh,
        code: "600001".into(),
        exchange_date: day(2009, 12, 29),
        tdx_date: day(2009, 12, 28),
    }));
}

#[test]
fn zero_listing_conflicts_against_ssdate_is_the_regression_baseline() {
    let fixture = Fixture::new();
    let lists = Lists::serving_fixtures();
    let admitted = acquired(&fixture, &lists);
    // The same-day facts: the exchange value wins, the TDX value is kept as
    // `listing_date_tdx`, and no conflict is recorded (§7.2).
    let tdx = [
        tdx("600001", Exchange::Sh, day(1998, 6, 18)),
        tdx("000003", Exchange::Sz, day(1991, 1, 14)),
    ];
    let assembled = assembly::assemble_security_master(
        &tdx,
        &[],
        assembly::exchange_source(Some(admitted.snapshot.clone())),
        &candidate_migrations(),
    )
    .unwrap();
    assert!(assembled.conflicts.is_empty());
    let sh = row(&assembled, Exchange::Sh, "600001");
    assert!(!sh.listing_conflict);
    assert_eq!(sh.listing_date, Some(day(1998, 6, 18)));
    assert_eq!(sh.listing_date_tdx, Some(day(1998, 6, 18)));
    assert_eq!(sh.listing_source, ListingSource::ExchangeList);
    let sz = row(&assembled, Exchange::Sz, "000003");
    assert_eq!(sz.listing_date, Some(day(1991, 1, 14)));
    assert!(!sz.listing_conflict);
    assert!(!sz.delisting_conflict);
    assert_eq!(sz.delisting_source, DelistingSource::ExchangeList);
}

#[test]
fn replaying_the_same_frozen_snapshot_keeps_the_master_digest() {
    let fixture = Fixture::new();
    let first_lists = Lists::serving_fixtures();
    let first = acquired(&fixture, &first_lists);
    let first_assembly = assemble(&first);
    let rows_before = fixture.market.read_exchange_snapshots().unwrap().len();

    // A second acquisition from the same store: the bytes are already frozen,
    // so no new manifest line or index row is written.
    let second_lists = Lists::serving_fixtures();
    let second = acquired(&fixture, &second_lists);
    let rows_after = fixture.market.read_exchange_snapshots().unwrap().len();
    assert_eq!(rows_before, rows_after);
    assert_eq!(first.snapshot, second.snapshot);
    assert_eq!(first.bindings, second.bindings);
    let second_assembly = assemble(&second);
    assert_eq!(
        assembly::assembly_digest(&first_assembly.rows, &first_assembly.migrations),
        assembly::assembly_digest(&second_assembly.rows, &second_assembly.migrations)
    );
}

#[test]
fn a_fetch_failure_leaves_the_previous_master_version_readable() {
    let fixture = Fixture::new();
    let connection = fixture.connection();
    let binding = seed_binding(&connection);

    // A committed main table from an earlier run (A-1/A-2 still pending).
    let earlier =
        assembly::assemble_security_master(&tdx_facts(), &[], assembly::exchange_source(None), &[])
            .unwrap();
    let written =
        replace_security_master(&connection, &earlier, binding, "task-1", "item-1").unwrap();
    let before = read_security_master(&connection).unwrap();
    assert!(!before.is_empty());

    let lists = Lists::failing(500);
    let error = acquire(&fixture.store, &fixture.market, &client(&lists)).unwrap_err();
    assert_eq!(error.code(), "source_unavailable");

    // The item failed, so nothing replaced the dataset: same version id and
    // same rows are still readable.
    let version = read_version(&connection, DATASET_KIND, assembly::DATASET_KEY)
        .unwrap()
        .expect("the earlier registration is still there");
    assert_eq!(version.version_id, written.version_id);
    assert_eq!(read_security_master(&connection).unwrap(), before);
}

#[test]
fn a_frozen_snapshot_below_the_required_tier_is_source_not_admitted() {
    let fixture = Fixture::new();
    // A snapshot frozen as OWNER_RISK_ACCEPTED for the same source: the writer
    // requires APPROVED_SOURCE, so the entry refuses before/after fetching.
    let request =
        crate::exchange_source::request::ExchangeRequest::new(SourceId::SseListing, SSE_URL)
            .param("sqlId", SSE_LIST_SQL_ID)
            .param("STOCK_TYPE", SSE_MAIN_BOARD_A)
            .paged(PagingFormat::SsePageHelp, 1);
    let response = HttpResponse {
        status: 200,
        etag: None,
        last_modified: None,
        body: SSE_LISTING_MAIN.to_vec(),
    };
    let envelope =
        exchange_source::validate::parse_page(PagingFormat::SsePageHelp, None, SSE_LISTING_MAIN)
            .unwrap();
    exchange_source::freeze_page(
        &fixture.store,
        &fixture.market,
        &request,
        &response,
        &envelope,
        PagingFormat::SsePageHelp.parser_version(),
        crate::exchange_source::BasisStatus::OwnerRiskAccepted,
        "2026-10-30T02:00:00Z",
    )
    .unwrap();

    let lists = Lists::serving_fixtures();
    let error = acquire(&fixture.store, &fixture.market, &client(&lists)).unwrap_err();
    assert_eq!(error.code(), "source_not_admitted");
    assert!(error.to_string().contains("approved_source"));
}

#[test]
fn an_unconfigured_source_store_keeps_the_branch_pending() {
    let fixture = Fixture::new();
    // No `X_DATA_MCP_SOURCE_STORE` in the test process: the item must keep the
    // TDX-only result instead of failing every daily run.
    if std::env::var_os(SOURCE_STORE_ENV).is_some() {
        return;
    }
    let run = run_source(&fixture.market, Policy::Optional).unwrap();
    match run {
        RunSource::Pending(pending) => {
            assert_eq!(pending.dataset, DATASET_KIND);
            assert!(pending.reason.contains(SOURCE_STORE_ENV));
        }
        RunSource::Admitted(_) => panic!("an unconfigured store cannot be admitted"),
    }
    assert!(fixture.market.read_exchange_snapshots().unwrap().is_empty());
}

#[test]
fn a_strict_run_without_a_source_store_fails_and_keeps_the_previous_master() {
    // `silent-path #446` §14.5: with `Policy::Required` the missing store is a
    // configuration failure of this item, not a successful zero-row answer.
    let _lock = crate::source_store::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = crate::source_store::PolicyEnv::set(None, false);
    let fixture = Fixture::new();
    let connection = fixture.connection();
    let binding = seed_binding(&connection);

    // A committed main table from an earlier (Optional) run.
    let earlier =
        assembly::assemble_security_master(&tdx_facts(), &[], assembly::exchange_source(None), &[])
            .unwrap();
    let written =
        replace_security_master(&connection, &earlier, binding, "task-1", "item-1").unwrap();
    let before = read_security_master(&connection).unwrap();
    assert!(!before.is_empty());

    let error = run_source(&fixture.market, Policy::Required).unwrap_err();
    assert_eq!(error.code(), "source_store_unconfigured");
    assert!(
        error.to_string().starts_with("source_store_unconfigured: "),
        "the stable code leads the item error text: {error}"
    );
    assert!(error.to_string().contains(SOURCE_STORE_ENV));
    // The refusal happens before any acquisition or store write.
    assert!(!fixture.store.has_entries().unwrap());
    assert!(fixture.market.read_exchange_snapshots().unwrap().is_empty());

    // Failure before commit: the earlier version is still the readable one.
    let version = read_version(&connection, DATASET_KIND, assembly::DATASET_KEY)
        .unwrap()
        .expect("the earlier registration is still there");
    assert_eq!(version.version_id, written.version_id);
    assert_eq!(read_security_master(&connection).unwrap(), before);
}

fn seed_binding(c: &Connection) -> crate::compact_market::Digest {
    use crate::compact_market::{EvidenceKind, SourceBinding};
    let generation = crate::sqlite_market::evidence_store::put_evidence(
        c,
        EvidenceKind::Generation,
        &json!({"generation": "gen-1"}),
    )
    .unwrap();
    crate::sqlite_market::evidence_store::put_source_binding(
        c,
        &SourceBinding {
            generation_id: "gen-1".into(),
            generation_manifest_sha256: [0x0b; 32],
            day_member_sha256: [0x0c; 32],
            security_event_sha256: [0x0e; 32],
            evidence_digest: generation,
        },
    )
    .unwrap()
}
