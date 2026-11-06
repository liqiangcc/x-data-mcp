//! Synthetic fixtures for PR-4 delisted ETF names (D-76 Q4/Q5).
//!
//! No PDF bytes, no network, no real CSV: every announcement index and
//! extraction batch here is built in memory and registered in a temporary
//! source store with the same `Registration` facts a real import writes.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;

use chrono::NaiveDate;
use serde_json::json;

use crate::official_security::etf_delisting as delist;
use crate::security_master::Exchange;
use crate::security_master::names::{self, EtfNameSet, NameCertainty, NameInterval};
use crate::source_store::{Registration, SourceStore, StoreKind};

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn announcement(
    exchange: Exchange,
    code: &str,
    date: NaiveDate,
    title: &str,
    url: &str,
    sec_name: Option<&str>,
    sha: &str,
) -> delist::Announcement {
    let (source_id, source_label) = match exchange {
        Exchange::Sz => ("szse_etf_notice_ann", "szse_etf_notice_ann"),
        _ => ("sse_fund_delist_ann", "sse_fund_delist_ann"),
    };
    delist::Announcement {
        exchange,
        code: code.into(),
        announcement_date: date,
        title: title.into(),
        url: url.into(),
        source_id,
        source_label,
        sec_name: sec_name.map(str::to_string),
        snapshot_sha256: sha.into(),
    }
}

fn szse_body(rows: &[serde_json::Value]) -> Vec<u8> {
    json!({"announceCount": rows.len(), "data": rows})
        .to_string()
        .into_bytes()
}

fn sse_body(rows: &[serde_json::Value]) -> Vec<u8> {
    json!({"pageHelp": {"pageNo": 1, "pageCount": null, "total": 0}, "result": rows})
        .to_string()
        .into_bytes()
}

/// One row of the synthetic D-72 extraction CSV. `short_name` is optional so
/// the "PDF extraction empty" case is a real CSV, not a special code path.
fn csv_row(
    entity: &str,
    source: &str,
    doc_sha: &str,
    short_name: &str,
    announcement: &str,
    delisting: &str,
) -> String {
    format!(
        "{entity},{delisting},{announcement},{source},https://example.invalid/{doc_sha}.pdf,\
         {doc_sha},1,终止上市日{delisting},etf-short-name-v1,2026-11-06T02:00:00Z,high,,,\
         {short_name},1,场内简称{short_name},etf-short-name-v1\n"
    )
}

const CSV_HEADER: &str = "entity,delisting_date,announcement_date,source_id,doc_url,doc_sha256,\
page_no,snippet,extractor,extracted_at,confidence,last_operating_day,registration_date,\
short_name,short_name_page_no,short_name_snippet,short_name_extractor\n";

fn extraction_csv(rows: &[String]) -> Vec<u8> {
    let mut text = CSV_HEADER.to_string();
    for row in rows {
        text.push_str(row);
    }
    text.into_bytes()
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: SourceStore,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let store = SourceStore::new(dir.path().join("source-store"));
        Self { _dir: dir, store }
    }

    fn register(
        &self,
        source_id: &str,
        kind: StoreKind,
        bytes: &[u8],
        acquired_at: &str,
        imported_at: Option<&str>,
        parser: Option<&str>,
    ) -> String {
        let mut reg = Registration::new(
            source_id,
            kind,
            format!("synthetic:{source_id}"),
            acquired_at,
        );
        reg.parser_version = parser.map(str::to_string);
        reg.imported_at = imported_at.map(str::to_string);
        if kind == StoreKind::ArchiveImport {
            reg.import_batch = Some("synthetic-batch".into());
            reg.basis_status = Some("approved_source".into());
            reg.evidence_grade = Some("A".into());
        }
        self.store.register(bytes, &reg).unwrap().sha256
    }
}

/// Build a CSV whose `doc_sha256` values are real registered PDF blob shas.
fn csv_with_blobs(fixture: &Fixture, rows: &[(String, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (entity, short_name) in rows {
        let pdf = b"%PDF-1.4 synthetic";
        let sha = fixture.register(
            delist::PDF_SOURCE_ID,
            StoreKind::ArchiveImport,
            pdf,
            "2019-01-01T00:00:00Z",
            Some("2026-11-06T02:00:00Z"),
            Some("pdf-v1"),
        );
        let source = if entity.starts_with("sh.") {
            "sse_fund_delist_ann"
        } else {
            "szse_etf_notice_ann"
        };
        out.push(csv_row(
            entity,
            source,
            &sha,
            short_name,
            "2026-05-25",
            "2026-06-01",
        ));
    }
    let bytes = extraction_csv(&out);
    fixture.register(
        delist::EXTRACT_SOURCE_ID,
        StoreKind::ArchiveImport,
        &bytes,
        "2019-01-01T00:00:00Z",
        Some("2026-11-06T02:00:00Z"),
        Some(delist::EXTRACT_PARSER_VERSION),
    );
    bytes
}

fn targets(rows: &[(Exchange, &str, NaiveDate)]) -> Vec<delist::DelistedEtfTarget> {
    rows.iter()
        .map(|(exchange, code, effective_to)| delist::DelistedEtfTarget {
            exchange: *exchange,
            code: (*code).into(),
            effective_to: *effective_to,
        })
        .collect()
}

fn arrivals(pairs: &[(&str, NaiveDate)]) -> BTreeMap<String, NaiveDate> {
    pairs
        .iter()
        .map(|(sha, day)| ((*sha).into(), *day))
        .collect()
}

fn announcements_set(rows: Vec<delist::Announcement>) -> delist::AnnouncementSet {
    let main = delist::main_announcements(&rows);
    let mut snapshot_ids = BTreeMap::new();
    for row in &rows {
        snapshot_ids
            .entry(row.source_label)
            .or_insert_with(Vec::new)
            .push(row.snapshot_sha256.clone());
    }
    delist::AnnouncementSet {
        main,
        rows,
        snapshot_ids,
    }
}

fn empty_extractions() -> delist::ExtractionSet {
    delist::ExtractionSet::default()
}

// ---------------------------------------------------------------------------
// 1. known_at: a 2019 announcement first ingested in 2026 is known from 2026.
// ---------------------------------------------------------------------------
#[test]
fn backfilled_name_known_at_is_the_first_ingestion_day_not_the_announcement_day() {
    let notice = announcement(
        Exchange::Sz,
        "159503",
        day(2019, 6, 1),
        "终止上市公告",
        "/szse/terminate.pdf",
        Some("财富管理ETF"),
        "ann-sz-1",
    );
    let set = announcements_set(vec![notice]);
    let found = delist::assemble_delisted_etf_names(
        &targets(&[(Exchange::Sz, "159503", day(2019, 6, 20))]),
        &set,
        &empty_extractions(),
        &arrivals(&[("ann-sz-1", day(2026, 11, 6))]),
    );
    assert_eq!(found.rows.len(), 1);
    let row = &found.rows[0];
    assert_eq!(row.effective_from, day(2019, 6, 1));
    assert_eq!(row.known_at, Some(day(2026, 11, 6)));
    assert_eq!(row.effective_to, Some(day(2019, 6, 20)));
    assert_eq!(row.source_kind, names::NameSourceKind::ExchangeSnapshot);
    assert_eq!(
        row.source_record_id.as_deref(),
        Some("szse_etf_notice_ann:/szse/terminate.pdf")
    );
}

// ---------------------------------------------------------------------------
// 2. SZSE notice selection: rename notices in the channel never win.
// ---------------------------------------------------------------------------
#[test]
fn szse_name_comes_from_the_termination_notice_not_a_later_rename() {
    let rows = [
        announcement(
            Exchange::Sz,
            "159915",
            day(2026, 3, 1),
            "终止上市公告",
            "/terminate.pdf",
            Some("创业板ETF易方达"),
            "sha-a",
        ),
        // A later rename notice in the same channel: `main_announcements`
        // (identity) picks this one, but its secName is NOT the termination
        // value.
        announcement(
            Exchange::Sz,
            "159915",
            day(2026, 4, 1),
            "关于变更基金简称的公告",
            "/rename.pdf",
            Some("改名后ETF"),
            "sha-b",
        ),
    ];
    let set = announcements_set(rows.to_vec());
    assert_eq!(
        set.main[&(Exchange::Sz, "159915".into())].url,
        "/rename.pdf",
        "the identity main-announcement can be a different notice"
    );
    let code_rows: Vec<&delist::Announcement> = rows.iter().collect();
    let chosen = delist::termination_notice(&code_rows).unwrap();
    assert_eq!(chosen.url, "/terminate.pdf");
    assert_eq!(chosen.sec_name.as_deref(), Some("创业板ETF易方达"));

    let found = delist::assemble_delisted_etf_names(
        &targets(&[(Exchange::Sz, "159915", day(2026, 6, 1))]),
        &set,
        &empty_extractions(),
        &arrivals(&[("sha-a", day(2026, 11, 6)), ("sha-b", day(2026, 11, 6))]),
    );
    assert_eq!(found.rows.len(), 1);
    assert_eq!(found.rows[0].name, "创业板ETF易方达");
    assert_eq!(found.rows[0].effective_from, day(2026, 3, 1));
    assert_eq!(found.evidence.identity_notice_mismatch, 1);
}

#[test]
fn a_formal_termination_notice_beats_a_prompt_one_for_the_name() {
    let rows = [
        announcement(
            Exchange::Sz,
            "159986",
            day(2026, 1, 2),
            "终止上市提示性公告",
            "/prompt.pdf",
            Some("提示名"),
            "sha-1",
        ),
        announcement(
            Exchange::Sz,
            "159986",
            day(2026, 1, 1),
            "终止上市公告",
            "/formal.pdf",
            Some("正式名"),
            "sha-2",
        ),
    ];
    let code_rows: Vec<&delist::Announcement> = rows.iter().collect();
    let chosen = delist::termination_notice(&code_rows).unwrap();
    assert_eq!(chosen.url, "/formal.pdf");
    assert_eq!(chosen.sec_name.as_deref(), Some("正式名"));
}

#[test]
fn no_termination_titled_notice_means_no_szse_name() {
    let rows = vec![announcement(
        Exchange::Sz,
        "159986",
        day(2026, 1, 2),
        "关于基金份额持有人大会的公告",
        "/other.pdf",
        Some("别的名字"),
        "sha-1",
    )];
    let set = announcements_set(rows);
    let found = delist::assemble_delisted_etf_names(
        &targets(&[(Exchange::Sz, "159986", day(2026, 6, 1))]),
        &set,
        &empty_extractions(),
        &arrivals(&[("sha-1", day(2026, 11, 6))]),
    );
    assert!(found.rows.is_empty());
    assert_eq!(found.evidence.missing_termination_notice, 1);
    assert_eq!(found.evidence.no_name, 0);
}

// ---------------------------------------------------------------------------
// 3. Empty interval: no row, counted, never back-filled to the listing day.
// ---------------------------------------------------------------------------
#[test]
fn an_announcement_day_not_before_effective_to_writes_no_row_and_is_counted() {
    let set = announcements_set(vec![announcement(
        Exchange::Sz,
        "159503",
        day(2026, 6, 1),
        "终止上市公告",
        "/terminate.pdf",
        Some("财富管理ETF"),
        "sha-a",
    )]);
    let found = delist::assemble_delisted_etf_names(
        &targets(&[(Exchange::Sz, "159503", day(2026, 6, 1))]),
        &set,
        &empty_extractions(),
        &arrivals(&[("sha-a", day(2026, 11, 6))]),
    );
    assert!(found.rows.is_empty());
    assert_eq!(found.evidence.empty_interval, 1);
    assert_eq!(found.evidence.no_name, 0);
}

#[test]
fn as_of_before_the_announcement_has_no_name_row_but_after_does() {
    // Listing 2005, announcement 2026-05, delisting 2026-06. The name must
    // not be visible as_of=2020 (D-80 would have put the master interval at
    // 2005), and is visible from 2026-05 until the exclusive effective_to.
    let set = announcements_set(vec![announcement(
        Exchange::Sh,
        "510070",
        day(2026, 5, 10),
        "关于XX交易型开放式指数证券投资基金终止上市的公告",
        "/sse/terminate.pdf",
        None,
        "sha-sse",
    )]);
    let fixture = Fixture::new();
    let csv = csv_with_blobs(&fixture, &[("sh.510070".into(), "民企ETF".into())]);
    let extractions = delist::ExtractionSet {
        records: BTreeMap::from([(
            (Exchange::Sh, "510070".into()),
            delist::ExtractionRecord {
                entity: "sh.510070".into(),
                delisting_date: day(2026, 6, 1),
                announcement_date: day(2026, 5, 10),
                source_id: "sse_fund_delist_ann".into(),
                doc_url: "https://example.invalid/a.pdf".into(),
                doc_sha256: "f".repeat(64),
                page_no: 1,
                snippet: "场内简称：民企 ETF".into(),
                extractor: "etf-short-name-v1".into(),
                extracted_at: "2026-11-06T02:00:00Z".into(),
                confidence: "high".into(),
                last_operating_day: None,
                registration_date: None,
                short_name: Some("民企ETF".into()),
            },
        )]),
        extraction_shas: vec!["csv-sha".into()],
    };
    // The CSV bytes must have been registered so the parse path is real.
    assert!(!csv.is_empty());
    let found = delist::assemble_delisted_etf_names(
        &targets(&[(Exchange::Sh, "510070", day(2026, 6, 1))]),
        &set,
        &extractions,
        &arrivals(&[("sha-sse", day(2026, 11, 6)), ("csv-sha", day(2026, 11, 6))]),
    );
    assert_eq!(found.rows.len(), 1);
    let row = &found.rows[0];
    assert_eq!(row.effective_from, day(2026, 5, 10));
    assert_eq!(row.effective_to, Some(day(2026, 6, 1)));
    assert_eq!(row.known_at, Some(day(2026, 11, 6)));
    // Name interval semantics: [effective_from, effective_to). No row covers
    // 2020, and the row covers 2026-05-10 but not 2026-06-01.
    assert!(!covers(row, day(2020, 1, 1)));
    assert!(covers(row, day(2026, 5, 10)));
    assert!(covers(row, day(2026, 5, 31)));
    assert!(!covers(row, day(2026, 6, 1)));
}

fn covers(row: &NameInterval, as_of: NaiveDate) -> bool {
    row.effective_from <= as_of && row.effective_to.is_none_or(|end| as_of < end)
}

// ---------------------------------------------------------------------------
// 4. Missing index field / empty PDF extraction → unavailable, not a guess.
// ---------------------------------------------------------------------------
#[test]
fn a_missing_secname_field_leaves_the_name_unavailable() {
    let fixture = Fixture::new();
    let body = szse_body(&[json!({
        "title": "终止上市公告",
        "publishTime": "2019-06-01 09:00:00",
        "secCode": ["159503"],
        "attachPath": "/terminate.pdf",
    })]);
    let sha = fixture.register(
        "szse_etf_notice_ann",
        StoreKind::HttpFetch,
        &body,
        "2026-11-06T02:00:00Z",
        None,
        Some("exchange-szse-announcement-v1"),
    );
    let parsed = delist::parse(delist::AnnouncementKind::Szse, &body).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].sec_name, None);
    let mut rows = parsed;
    for row in &mut rows {
        row.snapshot_sha256 = sha.clone();
    }
    let set = announcements_set(rows);
    let found = delist::assemble_delisted_etf_names(
        &targets(&[(Exchange::Sz, "159503", day(2019, 6, 20))]),
        &set,
        &empty_extractions(),
        &arrivals(&[(sha.as_str(), day(2026, 11, 6))]),
    );
    assert!(found.rows.is_empty());
    assert_eq!(found.evidence.no_name, 1);
}

#[test]
fn an_empty_pdf_short_name_leaves_the_name_unavailable() {
    let fixture = Fixture::new();
    let csv = csv_with_blobs(
        &fixture,
        &[
            ("sh.510070".into(), "民企ETF".into()),
            ("sh.512780".into(), "".into()),
        ],
    );
    let parsed = delist::parse_extraction_csv(&csv).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].short_name.as_deref(), Some("民企ETF"));
    assert_eq!(parsed[1].short_name, None);
}

// ---------------------------------------------------------------------------
// 5. Merging with the current ETF name chain: no overlaps.
// ---------------------------------------------------------------------------
#[test]
fn merging_a_delisted_name_closes_the_previous_snapshot_segment() {
    let current = EtfNameSet {
        rows: vec![NameInterval {
            exchange: Exchange::Sz,
            code: "159915".into(),
            effective_from: day(2026, 1, 5),
            effective_to: None,
            name: "创业板ETF".into(),
            certainty: NameCertainty::Unbounded,
            known_at: Some(day(2026, 1, 5)),
            source_kind: names::NameSourceKind::ExchangeSnapshot,
            source_record_id: Some("szse_etf_list:list-sha".into()),
        }],
        entities: BTreeSet::from([(Exchange::Sz, "159915".into())]),
    };
    let set = announcements_set(vec![announcement(
        Exchange::Sz,
        "159915",
        day(2026, 3, 1),
        "终止上市公告",
        "/terminate.pdf",
        Some("创业板ETF易方达"),
        "sha-a",
    )]);
    let mut delisted = delist::assemble_delisted_etf_names(
        &targets(&[(Exchange::Sz, "159915", day(2026, 6, 1))]),
        &set,
        &empty_extractions(),
        &arrivals(&[("sha-a", day(2026, 11, 6))]),
    );
    let merged = delist::merge_delisted_names(current, &mut delisted).unwrap();
    assert_eq!(merged.rows.len(), 2);
    assert_eq!(merged.rows[0].effective_from, day(2026, 1, 5));
    assert_eq!(merged.rows[0].effective_to, Some(day(2026, 3, 1)));
    assert_eq!(merged.rows[0].certainty, NameCertainty::Interval);
    assert_eq!(merged.rows[1].effective_from, day(2026, 3, 1));
    assert_eq!(merged.rows[1].effective_to, Some(day(2026, 6, 1)));
    assert_eq!(merged.rows[1].name, "创业板ETF易方达");
    names::validate_set_for(&merged.rows, &merged.entities).unwrap();
}

#[test]
fn a_snapshot_segment_at_or_after_the_announcement_is_skipped_not_overlapped() {
    let current = EtfNameSet {
        rows: vec![NameInterval {
            exchange: Exchange::Sz,
            code: "159915".into(),
            effective_from: day(2026, 5, 1),
            effective_to: None,
            name: "创业板ETF".into(),
            certainty: NameCertainty::Unbounded,
            known_at: Some(day(2026, 5, 1)),
            source_kind: names::NameSourceKind::ExchangeSnapshot,
            source_record_id: Some("szse_etf_list:list-sha".into()),
        }],
        entities: BTreeSet::from([(Exchange::Sz, "159915".into())]),
    };
    let set = announcements_set(vec![announcement(
        Exchange::Sz,
        "159915",
        day(2026, 4, 1),
        "终止上市公告",
        "/terminate.pdf",
        Some("创业板ETF易方达"),
        "sha-a",
    )]);
    let mut delisted = delist::assemble_delisted_etf_names(
        &targets(&[(Exchange::Sz, "159915", day(2026, 6, 1))]),
        &set,
        &empty_extractions(),
        &arrivals(&[("sha-a", day(2026, 11, 6))]),
    );
    let merged = delist::merge_delisted_names(current, &mut delisted).unwrap();
    assert_eq!(merged.rows.len(), 1);
    assert_eq!(merged.rows[0].name, "创业板ETF");
    assert_eq!(delisted.evidence.overlap_skipped, 1);
    assert_eq!(delisted.evidence.named, 0);
}

// ---------------------------------------------------------------------------
// 6. Live/replay equivalence: both use `read_names_source` on the same store.
// ---------------------------------------------------------------------------
#[test]
fn read_names_source_is_a_pure_function_of_the_manifest_and_repeats_identically() {
    let fixture = Fixture::new();
    let body = szse_body(&[json!({
        "title": "终止上市公告",
        "publishTime": "2019-06-01 09:00:00",
        "secCode": ["159503"],
        "secName": ["财富管理ETF"],
        "attachPath": "/terminate.pdf",
    })]);
    fixture.register(
        "szse_etf_notice_ann",
        StoreKind::HttpFetch,
        &body,
        "2026-11-06T02:00:00Z",
        None,
        None,
    );
    let sse = sse_body(&[json!({
        "TITLE": "关于XX交易型开放式指数证券投资基金终止上市的公告",
        "SECURITY_CODE": "510070",
        "SSEDATE": "2019-06-01",
        "URL": "/sse/terminate.pdf",
    })]);
    fixture.register(
        "sse_fund_delist_ann",
        StoreKind::HttpFetch,
        &sse,
        "2026-11-06T02:00:00Z",
        None,
        None,
    );
    let csv = csv_with_blobs(&fixture, &[("sh.510070".into(), "民企ETF".into())]);
    let _ = csv;
    let first = delist::read_names_source(&fixture.store).unwrap();
    let second = delist::read_names_source(&fixture.store).unwrap();
    assert_eq!(
        first, second,
        "the same store must rebuild identical source facts"
    );
    let (announcements, extractions, arrival_days) = first;
    assert_eq!(announcements.rows.len(), 2);
    assert_eq!(extractions.records.len(), 1);
    assert!(
        arrival_days
            .values()
            .all(|stored| *stored == day(2026, 11, 6))
    );
    let found = delist::assemble_delisted_etf_names(
        &targets(&[
            (Exchange::Sz, "159503", day(2019, 6, 20)),
            (Exchange::Sh, "510070", day(2019, 6, 20)),
        ]),
        &announcements,
        &extractions,
        &arrival_days,
    );
    assert_eq!(found.rows.len(), 2);
    assert!(
        found
            .rows
            .iter()
            .all(|row| row.known_at == Some(day(2026, 11, 6)))
    );
    assert_eq!(found.rows[0].name, "民企ETF"); // sh sorts first
    assert_eq!(found.rows[1].name, "财富管理ETF");
}

// ---------------------------------------------------------------------------
// 7. arrival_days: imported_at (2026) beats acquired_at (2019).
// ---------------------------------------------------------------------------
#[test]
fn arrival_days_uses_the_import_day_for_backfilled_bytes() {
    let fixture = Fixture::new();
    let sha = fixture.register(
        delist::EXTRACT_SOURCE_ID,
        StoreKind::ArchiveImport,
        b"csv",
        "2019-01-01T00:00:00Z",
        Some("2026-11-06T02:00:00Z"),
        Some(delist::EXTRACT_PARSER_VERSION),
    );
    let days = delist::arrival_days(&fixture.store).unwrap();
    assert_eq!(days[&sha], day(2026, 11, 6));
}
