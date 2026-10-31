//! Fixture-only tests of the shared exchange entry: no network, no host path.
//!
//! The store/database flows (two-page assembly, replay match after a wipe) live
//! in `tests/source_store/exchange.rs`; this module covers the pure parts —
//! request identity, segmentation, envelope counts, retry classification,
//! rebuild determinism and the binding gate.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use chrono::NaiveDate;

use super::index::{self, SnapshotRow};
use super::request::{
    SSE_PARSER_VERSION, SSE_PARSER_VERSION_V1, SSE_SOA_PARSER_VERSION, SZSE_PARSER_VERSION,
    SZSE_PARSER_VERSION_V1, SZSE_PARSER_VERSION_V2,
};
use super::*;
use crate::source_store::{
    ManifestRecord, Registration, SourceStore, StoreError, StoreKind, known_parser_version,
};

fn sse_body(page: u32, page_count: u32, total: u64, rows: usize) -> Vec<u8> {
    let data: Vec<serde_json::Value> = (0..rows)
        .map(|index| serde_json::json!({"SECURITY_CODE": format!("60000{index}")}))
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

fn szse_body(page: u32, page_count: u32, total: u64, rows: usize) -> Vec<u8> {
    let data: Vec<serde_json::Value> = (0..rows)
        .map(|index| serde_json::json!({"zqdm": format!("00000{index}")}))
        .collect();
    serde_json::json!([{
        "metadata": {
            "pageNo": page,
            "pagecount": page_count,
            "recordcount": total,
        },
        "data": data,
    }])
    .to_string()
    .into_bytes()
}

fn envelope(body: &[u8], format: PagingFormat) -> PageEnvelope {
    validate::parse_page(format, None, body).expect("fixture envelope")
}

#[test]
fn fingerprint_is_canonical_and_round_trips() {
    let first = ExchangeRequest::new(SourceId::SseListing, "https://example.invalid/list")
        .param("STOCK_TYPE", "1")
        .param("sqlId", "COMMON_SSE_CP_GPJCTPZ_GPLB_GP_L");
    let second = ExchangeRequest::new(SourceId::SseListing, "https://example.invalid/list")
        .param("sqlId", "COMMON_SSE_CP_GPJCTPZ_GPLB_GP_L")
        .param("STOCK_TYPE", "1");
    assert_eq!(first.fingerprint(), second.fingerprint());
    let parsed = ExchangeRequest::from_fingerprint(&first.fingerprint()).unwrap();
    assert_eq!(parsed, first);
    let paged = first.clone().paged(PagingFormat::SsePageHelp, 2);
    assert_ne!(paged.fingerprint(), first.fingerprint());
    assert_eq!(
        paged.params.get("pageHelp.pageNo").map(String::as_str),
        Some("2")
    );
    assert_eq!(
        paged.params.get("pageHelp.pageSize").map(String::as_str),
        Some("2000"),
        "the list services only honour pageHelp.pageSize (2026-10-30 evidence)"
    );
    assert!(!paged.params.contains_key("beginPage"));
    assert!(!paged.params.contains_key("endPage"));
    let szse = first.paged(PagingFormat::SzseMetadata, 3);
    assert_eq!(szse.params.get("PAGENO").map(String::as_str), Some("3"));
    assert!(!szse.params.contains_key("pageHelp.pageNo"));
}

#[test]
fn request_headers_carry_referer_and_user_agent() {
    let headers = default_headers(SourceId::SseBulletin);
    assert_eq!(
        headers.get("Referer").map(String::as_str),
        Some("https://www.sse.com.cn/")
    );
    assert_eq!(
        headers.get("User-Agent").map(String::as_str),
        Some(transport::USER_AGENT)
    );
    let szse = default_headers(SourceId::SzseListing);
    assert_eq!(
        szse.get("Referer").map(String::as_str),
        Some("https://www.szse.cn/")
    );
}

#[test]
fn bulletin_segments_are_quarter_aligned_and_bounded() {
    let start = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap();
    let end = NaiveDate::from_ymd_opt(2000, 12, 31).unwrap();
    let segments = quarter_segments(start, end).unwrap();
    assert_eq!(segments.len(), 4);
    assert_eq!(segments[0].start_text(), "2000-01-01");
    assert_eq!(segments[0].end_text(), "2000-03-31");
    assert_eq!(segments[3].end_text(), "2000-12-31");
    for segment in &segments {
        assert!(segment.days() <= MAX_SEGMENT_DAYS);
    }
    // Contiguous and gap-free: each window starts the day after the previous
    // one ends.
    for pair in segments.windows(2) {
        assert_eq!(
            pair[1].start().pred_opt().unwrap(),
            pair[0].end(),
            "quarter windows must be contiguous"
        );
    }
}

#[test]
fn over_one_quarter_window_is_rejected_before_any_request() {
    let start = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap();
    let end = NaiveDate::from_ymd_opt(2000, 6, 1).unwrap();
    let error = Segment::new(start, end).unwrap_err();
    assert_eq!(error.code(), "segment_too_long");
    let mut params = std::collections::BTreeMap::new();
    // Nothing was written into a request: construction failed first.
    assert!(params.is_empty());
    let early = Segment::parse("1999-12-31", "2000-01-31").unwrap_err();
    assert_eq!(early.code(), "segment_before_archive_start");
    params.insert("beginDate".to_string(), "2000-01-01".to_string());
    assert_eq!(params.len(), 1);
}

#[test]
fn sse_and_szse_envelopes_are_parsed_without_business_fields() {
    let sse = envelope(&sse_body(1, 2, 3, 2), PagingFormat::SsePageHelp);
    assert_eq!(
        (sse.page, sse.page_count, sse.rows, sse.total),
        (1, 2, 2, Some(3))
    );
    let szse = envelope(&szse_body(2, 4, 9, 3), PagingFormat::SzseMetadata);
    assert_eq!(
        (szse.page, szse.page_count, szse.rows, szse.total),
        (2, 4, 3, Some(9))
    );
    assert_eq!(
        validate::parse_page(PagingFormat::SsePageHelp, None, b"{}")
            .unwrap_err()
            .code(),
        "source_invalid"
    );
}

#[test]
fn the_szse_envelope_reads_the_declared_or_only_non_empty_block() {
    // 2026-10-30 real shape of `1793_ssgs`: tab1 (暂停上市) is empty with
    // `pagecount=0`, tab2 (终止上市) carries the rows, and the request declares
    // `TABKEY=tab2`.
    let terminated = br#"[
      {"metadata":{"tabkey":"tab1","pageno":2,"pagesize":20,"pagecount":0,"recordcount":0},"data":[]},
      {"metadata":{"tabkey":"tab2","pageno":2,"pagesize":20,"pagecount":4,"recordcount":61},
       "data":[{"zqdm":"000418"},{"zqdm":"000502"}]}
    ]"#;
    let page = validate::parse_page(PagingFormat::SzseMetadata, Some("tab2"), terminated).unwrap();
    assert_eq!(
        (page.page, page.page_count, page.rows, page.total),
        (2, 4, 2, Some(61)),
        "page number, page count, rows and total all come from the declared tab2"
    );

    // 2026-10-31 real shape of `CATALOGID=1110`: four blocks, only tab1
    // carries the current list; the request declares no tabkey, so the only
    // non-empty block decides (#446 D1). "tab2 first" would read an empty
    // block and later fail `page_count is 0`.
    let current = br#"[
      {"metadata":{"tabkey":"tab1","pageno":1,"pagesize":20,"pagecount":146,"recordcount":2905},
       "data":[{"agdm":"000001"},{"agdm":"000002"}]},
      {"metadata":{"tabkey":"tab2","pageno":1,"pagesize":20,"pagecount":0,"recordcount":0},"data":[]},
      {"metadata":{"tabkey":"tab3","pageno":1,"pagesize":20,"pagecount":0,"recordcount":0},"data":[]},
      {"metadata":{"tabkey":"tab4","pageno":1,"pagesize":20,"pagecount":0,"recordcount":0},"data":[]}
    ]"#;
    let page = validate::parse_page(PagingFormat::SzseMetadata, None, current).unwrap();
    assert_eq!(
        (page.page, page.page_count, page.rows, page.total),
        (1, 146, 2, Some(2905)),
        "the count and rows come from tab1, the only block with data"
    );

    // A declaration wins over another block with rows: the request, not the
    // response, says which list is meant.
    let two_lists = br#"[
      {"metadata":{"tabkey":"tab1","pageno":1,"pagecount":1,"recordcount":1},"data":[{"zqdm":"000001"}]},
      {"metadata":{"tabkey":"tab2","pageno":1,"pagecount":2,"recordcount":3},"data":[{"zqdm":"000002"}]}
    ]"#;
    let page = validate::parse_page(PagingFormat::SzseMetadata, Some("tab1"), two_lists).unwrap();
    assert_eq!((page.page_count, page.rows), (1, 1));

    // No declaration and no block with rows: still unambiguous when the
    // response has one block only (the old single-block shape).
    let single = br#"[{"metadata":{"pageno":1,"pagecount":1,"recordcount":0},"data":[]}]"#;
    let page = validate::parse_page(PagingFormat::SzseMetadata, None, single).unwrap();
    assert_eq!((page.page_count, page.rows), (1, 0));

    // No declaration and several blocks with rows: never guess the first one.
    let ambiguous = br#"[
      {"metadata":{"tabkey":"tab1","pageno":1,"pagecount":1,"recordcount":1},"data":[{"zqdm":"000001"}]},
      {"metadata":{"tabkey":"tab3","pageno":1,"pagecount":1,"recordcount":1},"data":[{"zqdm":"000003"}]}
    ]"#;
    let error = validate::parse_page(PagingFormat::SzseMetadata, None, ambiguous).unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("several blocks"), "{error}");
    // The message names every block's counts, so the operator can see which
    // block changed.
    assert!(error.to_string().contains("tab1: 1 row(s)"), "{error}");
    assert!(error.to_string().contains("tab3: 1 row(s)"), "{error}");

    // A declared tabkey that no block carries fails closed.
    let error =
        validate::parse_page(PagingFormat::SzseMetadata, Some("tab2"), ambiguous).unwrap_err();
    assert_eq!(error.code(), "source_invalid");
    assert!(error.to_string().contains("declared tabkey"), "{error}");
}

#[test]
fn the_szse_envelope_versions_keep_their_old_selection_on_replay() {
    let two_blocks = br#"[
      {"metadata":{"tabkey":"tab1","pageno":1,"pagecount":0,"recordcount":0},"data":[]},
      {"metadata":{"tabkey":"tab2","pageno":1,"pagecount":2,"recordcount":3},"data":[{"zqdm":"000003"}]}
    ]"#;
    // v3 reads tab2 here because it is the only non-empty block (the request
    // declares no tabkey).
    assert_eq!(
        PagingFormat::SzseMetadata.parser_version(),
        SZSE_PARSER_VERSION
    );
    assert!(known_parser_version(SZSE_PARSER_VERSION));
    let page = envelope(two_blocks, PagingFormat::SzseMetadata);
    assert_eq!((page.page_count, page.rows), (2, 1));
    // v2 frozen pages keep their "tab2 first" meaning on replay.
    assert!(known_parser_version(SZSE_PARSER_VERSION_V2));
    assert_eq!(
        PagingFormat::from_parser_version(SZSE_PARSER_VERSION_V2),
        Some(PagingFormat::SzseMetadataV2)
    );
    let v2 = envelope(two_blocks, PagingFormat::SzseMetadataV2);
    assert_eq!((v2.page_count, v2.rows), (2, 1));
    // Old frozen pages replay with the old meaning: `[0]`.
    assert!(known_parser_version(SZSE_PARSER_VERSION_V1));
    assert_eq!(
        PagingFormat::from_parser_version(SZSE_PARSER_VERSION_V1),
        Some(PagingFormat::SzseMetadataV1)
    );
    let legacy = envelope(two_blocks, PagingFormat::SzseMetadataV1);
    assert_eq!((legacy.page_count, legacy.rows), (0, 0));
}

#[test]
fn the_sse_dialect_pages_by_page_help_page_no() {
    // The list services answer with `pageHelp.pageNo`; `beginPage` is stale
    // (2026-10-30 evidence: both `beginPage` spellings answered page 1 again).
    let body =
        br#"{"pageHelp":{"beginPage":1,"pageNo":2,"pageCount":2,"total":28,"data":[{"x":"1"}]}}"#;
    let page = envelope(body, PagingFormat::SsePageHelp);
    assert_eq!((page.page, page.page_count), (2, 2));
    // An older frozen page without `pageNo` still reads `beginPage`.
    let legacy = br#"{"pageHelp":{"beginPage":2,"pageCount":2,"total":2,"data":[{"x":"1"}]}}"#;
    let page = envelope(legacy, PagingFormat::SsePageHelp);
    assert_eq!(page.page, 2);
    let mut params = std::collections::BTreeMap::new();
    PagingFormat::SsePageHelp.apply_page(&mut params, 2);
    assert_eq!(params.get("pageHelp.pageNo").map(String::as_str), Some("2"));
    assert_eq!(
        params.get("pageHelp.pageSize").map(String::as_str),
        Some("2000")
    );
    assert!(!params.contains_key("beginPage"));
    assert_eq!(
        PagingFormat::SsePageHelp.parser_version(),
        SSE_PARSER_VERSION
    );
    assert!(known_parser_version(SSE_PARSER_VERSION));
    assert_eq!(
        PagingFormat::from_parser_version(SSE_PARSER_VERSION),
        Some(PagingFormat::SsePageHelp)
    );
    // Snapshots frozen before the dialect change still replay: they have no
    // `pageHelp.pageNo`, so `beginPage` is read exactly as before.
    assert!(known_parser_version(SSE_PARSER_VERSION_V1));
    assert_eq!(
        PagingFormat::from_parser_version(SSE_PARSER_VERSION_V1),
        Some(PagingFormat::SsePageHelp)
    );
}

#[test]
fn the_soa_dialect_pages_by_page_help_page_no() {
    // `commonSoaQuery` ignores `beginPage`/`endPage` and keeps
    // `pageHelp.beginPage=1`; the page number is `pageHelp.pageNo` (PR-11
    // evidence: `beginPage=2&endPage=2` answered page 1 again).
    let body = br#"{"pageHelp":{"beginPage":1,"pageNo":2,"pageCount":2,"total":28,"data":[{"x":"1"},{"x":"2"}]}}"#;
    let page = validate::parse_page(PagingFormat::SseSoaPageNo, None, body).unwrap();
    assert_eq!(
        (page.page, page.page_count, page.rows, page.total),
        (2, 2, 2, Some(28))
    );
    assert_eq!(
        PagingFormat::SseSoaPageNo.parser_version(),
        SSE_SOA_PARSER_VERSION
    );
    assert!(known_parser_version(SSE_SOA_PARSER_VERSION));
    assert_eq!(
        PagingFormat::from_parser_version(SSE_SOA_PARSER_VERSION),
        Some(PagingFormat::SseSoaPageNo)
    );
    let mut params = std::collections::BTreeMap::new();
    PagingFormat::SseSoaPageNo.apply_page(&mut params, 2);
    assert_eq!(params.get("pageHelp.pageNo").map(String::as_str), Some("2"));
    assert_eq!(
        params.get("pageHelp.pageSize").map(String::as_str),
        Some("25")
    );
    assert!(!params.contains_key("beginPage"));
}

#[test]
fn a_quiet_soa_window_can_be_not_observed() {
    // `commonSoaQuery` answers `pageCount=0` with `data: []` for a quiet
    // window. A consumer that stores only positive facts may record it as an
    // unobserved window; the default stays fail-closed.
    let empty = PageEnvelope {
        page: 1,
        page_count: 0,
        rows: 0,
        total: Some(0),
    };
    let set = validate::validate_pages(
        std::slice::from_ref(&empty),
        EmptyPolicy::Forbidden,
        ZeroPolicy::NotObserved,
    )
    .unwrap();
    assert_eq!((set.row_count, set.page_count), (0, 0));
    assert_eq!(
        validate::validate_pages(&[empty], EmptyPolicy::Forbidden, ZeroPolicy::FailClosed)
            .unwrap_err()
            .code(),
        "source_invalid"
    );
    // A page that claims `pageCount=0` but carries rows is never accepted.
    let liar = PageEnvelope {
        page: 1,
        page_count: 0,
        rows: 2,
        total: None,
    };
    assert!(
        validate::validate_pages(&[liar], EmptyPolicy::Forbidden, ZeroPolicy::NotObserved).is_err()
    );
}

#[test]
fn an_sse_total_of_zero_means_not_reported() {
    // Older `GPLB_GP_L` snapshots report `pageHelp.total = 0` even when rows
    // are returned (M2 A-1 evidence, #396); the 2026-10-30 responses report a
    // real total. A zero therefore stays "no total", not "no rows": reading
    // it as a declared count would reject an old frozen listing page.
    let page = envelope(&sse_body(1, 1, 0, 4), PagingFormat::SsePageHelp);
    assert_eq!(page.rows, 4);
    assert_eq!(page.total, None);
    let set =
        validate::validate_pages(&[page], EmptyPolicy::Forbidden, ZeroPolicy::FailClosed).unwrap();
    assert_eq!(set.row_count, 4);
    // The quirk must not weaken the zero-row rule.
    let empty = envelope(&sse_body(1, 1, 0, 0), PagingFormat::SsePageHelp);
    assert_eq!(
        validate::validate_pages(&[empty], EmptyPolicy::Forbidden, ZeroPolicy::FailClosed)
            .unwrap_err()
            .code(),
        "unverified_zero"
    );
}

#[test]
fn two_page_set_sums_to_the_declared_total() {
    let pages = vec![
        envelope(&sse_body(1, 2, 3, 2), PagingFormat::SsePageHelp),
        envelope(&sse_body(2, 2, 3, 1), PagingFormat::SsePageHelp),
    ];
    let set =
        validate::validate_pages(&pages, EmptyPolicy::Forbidden, ZeroPolicy::FailClosed).unwrap();
    assert_eq!(set.row_count, 3);
    assert_eq!(set.page_count, 2);
}

#[test]
fn empty_data_is_unverified_zero_unless_the_window_is_declared_empty() {
    let pages = vec![envelope(&sse_body(1, 1, 0, 0), PagingFormat::SsePageHelp)];
    let error = validate::validate_pages(&pages, EmptyPolicy::Forbidden, ZeroPolicy::FailClosed)
        .unwrap_err();
    assert_eq!(error.code(), "unverified_zero");
    assert_eq!(error.class(), FailureClass::SourceInvalid);
    let accepted = validate::validate_pages(
        &pages,
        EmptyPolicy::DeclaredEmptyWindow,
        ZeroPolicy::FailClosed,
    )
    .unwrap();
    assert_eq!(accepted.row_count, 0);
    // Only a window that starts after "today" may be declared empty.
    let future = Segment::parse("2030-01-01", "2030-03-31").unwrap();
    let past = Segment::parse("2020-01-01", "2020-03-31").unwrap();
    let today = NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
    assert_eq!(
        EmptyPolicy::for_window(Some(future), today),
        EmptyPolicy::DeclaredEmptyWindow
    );
    assert_eq!(
        EmptyPolicy::for_window(Some(past), today),
        EmptyPolicy::Forbidden
    );
    assert_eq!(EmptyPolicy::for_window(None, today), EmptyPolicy::Forbidden);
}

#[test]
fn total_and_page_count_mismatches_are_truncated() {
    let short = vec![
        envelope(&sse_body(1, 2, 5, 2), PagingFormat::SsePageHelp),
        envelope(&sse_body(2, 2, 5, 1), PagingFormat::SsePageHelp),
    ];
    assert_eq!(
        validate::validate_pages(&short, EmptyPolicy::Forbidden, ZeroPolicy::FailClosed)
            .unwrap_err()
            .code(),
        "truncated"
    );
    let missing = vec![envelope(&sse_body(1, 3, 3, 2), PagingFormat::SsePageHelp)];
    assert_eq!(
        validate::validate_pages(&missing, EmptyPolicy::Forbidden, ZeroPolicy::FailClosed)
            .unwrap_err()
            .code(),
        "truncated"
    );
    let duplicated = vec![
        envelope(&sse_body(1, 2, 3, 2), PagingFormat::SsePageHelp),
        envelope(&sse_body(1, 2, 3, 1), PagingFormat::SsePageHelp),
    ];
    assert_eq!(
        validate::validate_pages(&duplicated, EmptyPolicy::Forbidden, ZeroPolicy::FailClosed)
            .unwrap_err()
            .code(),
        "truncated"
    );
}

#[test]
fn http_status_failures_are_source_unavailable() {
    let error = validate::require_status(500).unwrap_err();
    assert_eq!(error.code(), "source_unavailable");
    assert_eq!(error.class(), FailureClass::SourceUnavailable);
    assert_eq!(validate::require_status(200), Ok(()));
    assert_eq!(
        validate::require_status(503).unwrap_err().code(),
        "source_unavailable"
    );
}

struct Scripted {
    responses: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
    calls: Mutex<usize>,
    retried_after_failure: Mutex<bool>,
}

impl Scripted {
    fn new(responses: Vec<Result<HttpResponse, TransportError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            calls: Mutex::new(0),
            retried_after_failure: Mutex::new(false),
        }
    }

    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl Transport for Scripted {
    fn get(&self, _request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        if *calls > 1 {
            *self.retried_after_failure.lock().unwrap() = true;
        }
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted response queue is exhausted")
    }
}

fn response(status: u16) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse {
        status,
        etag: None,
        last_modified: None,
        body: sse_body(1, 1, 1, 1),
    })
}

#[test]
fn retries_are_bounded_and_only_for_transient_failures() {
    let request = HttpRequest {
        url: "https://example.invalid/list".to_string(),
        headers: std::collections::BTreeMap::new(),
    };
    let flaky = std::sync::Arc::new(Scripted::new(vec![
        Err(TransportError::Failed("connection reset".to_string())),
        response(500),
        response(200),
    ]));
    let client = Client::with_transport(Flaky(flaky.clone()))
        .with_policy(RetryPolicy::bounded(3, Duration::ZERO))
        .without_sleep();
    let answered = client.get(&request).unwrap();
    assert_eq!(answered.status, 200);
    assert_eq!(flaky.calls(), 3);
    assert!(*flaky.retried_after_failure.lock().unwrap());

    let forbidden = std::sync::Arc::new(Scripted::new(vec![response(404)]));
    let client = Client::with_transport(Flaky(forbidden.clone()))
        .with_policy(RetryPolicy::bounded(3, Duration::ZERO))
        .without_sleep();
    assert_eq!(client.get(&request).unwrap().status, 404);
    assert_eq!(forbidden.calls(), 1);

    let always = std::sync::Arc::new(Scripted::new(vec![
        response(503),
        response(503),
        response(503),
    ]));
    let client = Client::with_transport(Flaky(always.clone()))
        .with_policy(RetryPolicy::bounded(3, Duration::ZERO))
        .without_sleep();
    assert_eq!(client.get(&request).unwrap().status, 503);
    assert_eq!(always.calls(), 3);
}

/// Lets an `Arc<Scripted>` be handed to `Client::with_transport`.
struct Flaky(std::sync::Arc<Scripted>);

impl Transport for Flaky {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        self.0.get(request)
    }
}

fn store_with_pages() -> (tempfile::TempDir, SourceStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    for (page, body) in [(1_u32, sse_body(1, 2, 3, 2)), (2, sse_body(2, 2, 3, 1))] {
        let request = ExchangeRequest::new(SourceId::SseListing, "https://example.invalid/list")
            .paged(PagingFormat::SsePageHelp, page);
        let mut facts = Registration::new(
            request.source_id.as_str(),
            StoreKind::HttpFetch,
            request.fingerprint(),
            "2026-10-30T02:00:00Z",
        );
        facts.parser_version = Some(SSE_PARSER_VERSION.to_string());
        facts.basis_status = Some(BasisStatus::ApprovedSource.as_str().to_string());
        facts.http_etag = Some(format!("\"etag-{page}\""));
        store.register(&body, &facts).unwrap();
    }
    (dir, store)
}

#[test]
fn rebuild_from_the_store_is_deterministic_and_keeps_etags() {
    let (_dir, store) = store_with_pages();
    let records: Vec<ManifestRecord> = store.records().unwrap();
    assert!(known_parser_version(SSE_PARSER_VERSION));
    let refs: Vec<&ManifestRecord> = records.iter().collect();
    let first = index::rebuild(&store, &refs).unwrap();
    assert!(first.problems.is_empty());
    assert_eq!(first.rows.len(), 2);
    assert_eq!(first.rows.iter().map(|row| row.row_count).sum::<usize>(), 3);
    assert!(
        first
            .rows
            .iter()
            .all(|row| row.as_of == "2026-10-30" && row.page_count == Some(2))
    );
    let etags: Vec<&str> = first
        .rows
        .iter()
        .filter_map(|row| row.http_etag.as_deref())
        .collect();
    assert_eq!(etags.len(), 2);
    assert!(etags.contains(&"\"etag-1\""));
    let second = index::rebuild(&store, &refs).unwrap();
    assert_eq!(first.summary(), second.summary());

    let sha = first.rows[0].snapshot_id.clone();
    std::fs::write(store.blob_path(&sha), b"tampered").unwrap();
    assert!(matches!(
        index::rebuild(&store, &refs),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn a_frozen_zero_row_page_is_indexed_as_a_zero_row_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let request = ExchangeRequest::new(SourceId::SseBulletin, "https://example.invalid/bulletin");
    let mut facts = Registration::new(
        request.source_id.as_str(),
        StoreKind::HttpFetch,
        request.fingerprint(),
        "2026-10-30T03:00:00Z",
    );
    facts.parser_version = Some(SSE_PARSER_VERSION.to_string());
    facts.basis_status = Some(BasisStatus::ApprovedSource.as_str().to_string());
    store
        .register(&sse_body(1, 1, 0, 0), &facts)
        .expect("the bytes are frozen before parsing");
    let records = store.records().unwrap();
    let refs: Vec<&ManifestRecord> = records.iter().collect();
    let rebuild = index::rebuild(&store, &refs).unwrap();
    // The index records the store, not a verdict: the entry refuses the window
    // (`unverified_zero`), and the frozen page is still reproducible.
    assert!(rebuild.problems.is_empty());
    assert_eq!(rebuild.rows.len(), 1);
    assert_eq!(rebuild.rows[0].row_count, 0);
    assert!(store.blob_exists(&records[0].sha256));
}

#[test]
fn a_frozen_page_without_a_usable_envelope_is_reported_not_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let request = ExchangeRequest::new(SourceId::SseListing, "https://example.invalid/list");
    let mut facts = Registration::new(
        request.source_id.as_str(),
        StoreKind::HttpFetch,
        request.fingerprint(),
        "2026-10-30T04:00:00Z",
    );
    facts.parser_version = Some(SSE_PARSER_VERSION.to_string());
    facts.basis_status = Some(BasisStatus::ApprovedSource.as_str().to_string());
    store
        .register(b"<html>maintenance</html>", &facts)
        .expect("the bytes are frozen before parsing");
    let records = store.records().unwrap();
    let refs: Vec<&ManifestRecord> = records.iter().collect();
    let rebuild = index::rebuild(&store, &refs).unwrap();
    assert!(rebuild.rows.is_empty());
    assert_eq!(rebuild.problems.len(), 1);
    assert_eq!(rebuild.problems[0].reason, "source_invalid");
    assert!(store.blob_exists(&records[0].sha256));
}

#[test]
fn binding_digest_is_stable_and_gates_on_basis_status() {
    let (_dir, store) = store_with_pages();
    let records = store.records().unwrap();
    let refs: Vec<&ManifestRecord> = records.iter().collect();
    let rows: Vec<SnapshotRow> = index::rebuild(&store, &refs).unwrap().rows;
    let binding =
        bind::bind_from_rows(&rows, SourceId::SseListing, BasisStatus::ApprovedSource).unwrap();
    assert_eq!(binding.snapshots.len(), 2);
    let again =
        bind::bind_from_rows(&rows, SourceId::SseListing, BasisStatus::ApprovedSource).unwrap();
    assert_eq!(binding.source_binding_digest, again.source_binding_digest);
    assert_eq!(
        bind::verify_bytes(&store, &binding).unwrap(),
        2,
        "both bound blobs stay readable"
    );

    let mut downgraded = rows.clone();
    downgraded[0].basis_status = BasisStatus::OwnerRiskAccepted.as_str().to_string();
    let error = bind::bind_from_rows(
        &downgraded,
        SourceId::SseListing,
        BasisStatus::ApprovedSource,
    )
    .unwrap_err();
    assert_eq!(error.code(), "source_not_admitted");
    assert!(
        bind::bind_from_rows(
            &downgraded,
            SourceId::SseListing,
            BasisStatus::OwnerRiskAccepted
        )
        .is_ok()
    );
    assert!(
        bind::bind_from_rows(&rows, SourceId::SzseListing, BasisStatus::ApprovedSource).is_err()
    );
}

#[test]
fn basis_status_ordering_is_the_admission_order() {
    assert!(BasisStatus::ApprovedSource.admits(BasisStatus::ApprovedSource));
    assert!(BasisStatus::OwnerRiskAccepted.admits(BasisStatus::OwnerRiskAccepted));
    assert!(!BasisStatus::NotAvailable.admits(BasisStatus::OwnerRiskAccepted));
    assert_eq!(SourceId::parse("sse_bulletin"), Some(SourceId::SseBulletin));
    assert_eq!(SourceId::parse("nope"), None);
    assert_eq!(
        PagingFormat::from_parser_version(SSE_PARSER_VERSION),
        Some(PagingFormat::SsePageHelp)
    );
}
