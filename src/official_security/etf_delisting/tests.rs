use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;

use super::*;

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

#[test]
fn the_etf_segment_filter_is_discovery_only() {
    assert!(is_etf_candidate_code(Exchange::Sh, "510050"));
    assert!(is_etf_candidate_code(Exchange::Sz, "159919"));
    assert!(!is_etf_candidate_code(Exchange::Sh, "519001"));
    assert!(!is_etf_candidate_code(Exchange::Sh, "550001"));
    assert!(!is_etf_candidate_code(Exchange::Sz, "161001"));
    assert!(!is_etf_candidate_code(Exchange::Sz, "15991"));
}

#[test]
fn the_main_announcement_prefers_a_formal_notice_over_a_prompt_one() {
    let rows = vec![
        Announcement {
            exchange: Exchange::Sz,
            code: "159986".into(),
            announcement_date: day(2026, 1, 2),
            title: "终止上市提示性公告".into(),
            url: "prompt.pdf".into(),
            source_id: "szse_etf_notice_ann",
            source_label: "szse_etf_notice_ann",
        },
        Announcement {
            exchange: Exchange::Sz,
            code: "159986".into(),
            announcement_date: day(2026, 1, 1),
            title: "终止上市公告".into(),
            url: "formal.pdf".into(),
            source_id: "szse_etf_notice_ann",
            source_label: "szse_etf_notice_ann",
        },
    ];
    let main = main_announcements(&rows);
    let chosen = &main[&(Exchange::Sz, "159986".to_string())];
    assert_eq!(chosen.url, "formal.pdf");
    assert!(!chosen.only_prompt);
}

#[test]
fn only_prompt_notices_are_used_and_marked() {
    let rows = vec![Announcement {
        exchange: Exchange::Sz,
        code: "159986".into(),
        announcement_date: day(2026, 1, 2),
        title: "终止上市提示性公告".into(),
        url: "prompt.pdf".into(),
        source_id: "szse_etf_notice_ann",
        source_label: "szse_etf_notice_ann",
    }];
    let main = main_announcements(&rows);
    assert!(main[&(Exchange::Sz, "159986".to_string())].only_prompt);
}

#[test]
fn extraction_dates_must_fit_the_announcement_window_and_the_last_bar() {
    let good = ExtractionRecord {
        entity: "sz.159986".into(),
        delisting_date: day(2026, 1, 10),
        announcement_date: day(2026, 1, 2),
        source_id: "szse_etf_notice_ann".into(),
        doc_url: "https://example.invalid/a.pdf".into(),
        doc_sha256: "ab".repeat(32),
        page_no: 1,
        snippet: "本基金将于2026年1月10日终止上市".into(),
        extractor: "synthetic-v1".into(),
        extracted_at: "2026-01-11T00:00:00Z".into(),
        confidence: "high".into(),
        last_operating_day: Some(day(2026, 1, 9)),
        registration_date: None,
    };
    good.validate(Some(day(2026, 1, 9))).unwrap();

    let mut too_early = good.clone();
    too_early.delisting_date = day(2026, 1, 1);
    assert_eq!(
        too_early.validate(None).unwrap_err().code(),
        "etf_delisting_invalid"
    );

    let mut after_bar = good.clone();
    after_bar.delisting_date = day(2026, 1, 9);
    assert_eq!(
        after_bar
            .validate(Some(day(2026, 1, 9)))
            .unwrap_err()
            .code(),
        "etf_delisting_invalid"
    );

    let mut too_late = good.clone();
    too_late.delisting_date = day(2026, 1, 12);
    assert_eq!(
        too_late.validate(None).unwrap_err().code(),
        "etf_delisting_invalid"
    );
}

#[test]
fn only_a_candidate_that_hits_an_announcement_becomes_a_member_fact() {
    let candidates: BTreeSet<String> = ["sz.159986", "sz.159999"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let announcements = AnnouncementSet {
        main: BTreeMap::from([(
            (Exchange::Sz, "159986".into()),
            MainAnnouncement {
                exchange: Exchange::Sz,
                code: "159986".into(),
                announcement_date: day(2026, 1, 2),
                title: "终止上市公告".into(),
                url: "formal.pdf".into(),
                source_id: "szse_etf_notice_ann",
                source_label: "szse_etf_notice_ann",
                only_prompt: false,
            },
        )]),
        rows: Vec::new(),
        snapshot_ids: BTreeMap::new(),
    };
    let facts = member_facts(
        &candidates,
        &announcements,
        &ExtractionSet::default(),
        &BTreeMap::from([((0, "159986".to_string()), day(2015, 1, 5))]),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].code, "159986");
    assert_eq!(facts[0].delisting_date, None);
    assert_eq!(facts[0].delisting_source, DelistingSource::Unknown);
    assert_eq!(facts[0].announcement_date, Some(day(2026, 1, 2)));
    assert_eq!(facts[0].effective_to, Some(day(2026, 1, 3)));
    assert_eq!(facts[0].listing_source, ListingSource::TdxSsdate);
}

#[test]
fn extraction_turns_an_announcement_identity_into_an_exact_delisting_date() {
    let candidates: BTreeSet<String> = ["sh.512390".to_string()].into_iter().collect();
    let announcements = AnnouncementSet {
        main: BTreeMap::from([(
            (Exchange::Sh, "512390".into()),
            MainAnnouncement {
                exchange: Exchange::Sh,
                code: "512390".into(),
                announcement_date: day(2026, 8, 1),
                title: "终止上市公告".into(),
                url: "sse.pdf".into(),
                source_id: "sse_fund_delist_ann",
                source_label: "sse_fund_delist_ann",
                only_prompt: false,
            },
        )]),
        rows: Vec::new(),
        snapshot_ids: BTreeMap::new(),
    };
    let record = ExtractionRecord {
        entity: "sh.512390".into(),
        delisting_date: day(2026, 8, 5),
        announcement_date: day(2026, 8, 1),
        source_id: "sse_fund_delist_ann".into(),
        doc_url: "sse.pdf".into(),
        doc_sha256: "cd".repeat(32),
        page_no: 1,
        snippet: "终止上市日：2026年8月5日".into(),
        extractor: "synthetic-v1".into(),
        extracted_at: "2026-08-06T00:00:00Z".into(),
        confidence: "high".into(),
        last_operating_day: Some(day(2026, 8, 4)),
        registration_date: None,
    };
    let extractions = ExtractionSet {
        records: BTreeMap::from([((Exchange::Sh, "512390".into()), record)]),
        extraction_shas: vec!["ef".repeat(32)],
    };
    let facts = member_facts(
        &candidates,
        &announcements,
        &extractions,
        &BTreeMap::from([((1, "512390".to_string()), day(2010, 1, 1))]),
        &BTreeMap::from([("sh.512390".to_string(), day(2026, 8, 4))]),
    )
    .unwrap();
    assert_eq!(facts[0].delisting_date, Some(day(2026, 8, 5)));
    assert_eq!(facts[0].delisting_source, DelistingSource::ExchangeList);
    assert_eq!(
        facts[0].delisting_reason_class,
        crate::security_master::DelistingReason::Other
    );
    assert_eq!(facts[0].effective_to, Some(day(2026, 8, 5)));
    assert_eq!(facts[0].extraction, Some("cd".repeat(32)));
}

#[test]
fn csv_reader_keeps_a_quoted_comma_in_the_snippet() {
    let csv = b"entity,delisting_date,announcement_date,source_id,doc_url,doc_sha256,page_no,snippet,extractor,extracted_at,confidence,last_operating_day,registration_date\nsz.159986,2026-01-10,2026-01-02,szse_etf_notice_ann,u,aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,2,\"a,b\",x,2026-01-11T00:00:00Z,high,2026-01-09,\n";
    let rows = parse_extraction_csv(csv).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].snippet, "a,b");
    assert_eq!(rows[0].last_operating_day, Some(day(2026, 1, 9)));
}

#[test]
fn the_identity_candidates_exclude_current_members_and_non_etf_segments() {
    let current: BTreeSet<String> = ["sh.510050".to_string()].into_iter().collect();
    let members = [
        "sh.510050".to_string(),
        "sh.512390".to_string(),
        "sh.519001".to_string(),
        "sz.159986".to_string(),
    ];
    let candidates = candidates(members.iter(), &current);
    assert_eq!(
        candidates,
        BTreeSet::from(["sh.512390".to_string(), "sz.159986".to_string()])
    );
}

#[test]
fn synthetic_128_candidate_hits_produce_exactly_128_class_two_identity_rows() {
    let mut candidates = BTreeSet::new();
    let mut main = BTreeMap::new();
    for index in 0..82 {
        let code = format!("510{:03}", index);
        candidates.insert(format!("sh.{code}"));
        main.insert(
            (Exchange::Sh, code.clone()),
            MainAnnouncement {
                exchange: Exchange::Sh,
                code,
                announcement_date: day(2026, 1, 2),
                title: "终止上市公告".into(),
                url: format!("sse-{index}.pdf"),
                source_id: "sse_fund_delist_ann",
                source_label: "sse_fund_delist_ann",
                only_prompt: false,
            },
        );
    }
    for index in 0..46 {
        let code = format!("159{index:03}");
        candidates.insert(format!("sz.{code}"));
        main.insert(
            (Exchange::Sz, code.clone()),
            MainAnnouncement {
                exchange: Exchange::Sz,
                code,
                announcement_date: day(2026, 1, 2),
                title: "终止上市公告".into(),
                url: format!("szse-{index}.pdf"),
                source_id: "szse_etf_notice_ann",
                source_label: "szse_etf_notice_ann",
                only_prompt: false,
            },
        );
    }
    let announcements = AnnouncementSet {
        main,
        rows: Vec::new(),
        snapshot_ids: BTreeMap::new(),
    };
    let facts = member_facts(
        &candidates,
        &announcements,
        &ExtractionSet::default(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(facts.len(), 128);
    let rows = crate::security_master::assembly::member_rows(&facts).unwrap();
    assert_eq!(rows.len(), 128);
    assert!(rows.iter().all(|row| row.instrument_class == 2));
    assert!(rows.iter().all(|row| row.delisting_date.is_none()));
    assert_eq!(
        rows.iter()
            .filter(|row| row.exchange == Exchange::Sh)
            .count(),
        82
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.exchange == Exchange::Sz)
            .count(),
        46
    );
}

#[test]
fn sse_announcement_shape_groups_the_title_filter_and_dedupes_by_url() {
    let body = serde_json::json!({
        "pageHelp": {"pageNo": 1, "pageCount": null, "total": 0},
        "result": [
            {"TITLE": "关于XX交易型开放式指数证券投资基金终止上市的公告",
             "SECURITY_CODE": "510070", "SSEDATE": "2018-01-02", "URL": "/a.pdf"},
            {"TITLE": "关于XX交易型开放式指数证券投资基金终止上市的公告",
             "SECURITY_CODE": "510070", "SSEDATE": "2018-01-02", "URL": "/a.pdf"},
            {"TITLE": "关于XXLOF基金终止上市的公告",
             "SECURITY_CODE": "501001", "SSEDATE": "2018-01-03", "URL": "/b.pdf"},
            {"TITLE": "关于XX交易型开放式指数证券投资基金终止上市的提示性公告",
             "SECURITY_CODE": "510071", "SSEDATE": "2018-01-04", "URL": "/c.pdf"}
        ]
    });
    let rows = parse(AnnouncementKind::Sse, body.to_string().as_bytes()).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.exchange == Exchange::Sh));
    assert_eq!(rows[0].code, "510070");
    assert_eq!(rows[0].announcement_date, day(2018, 1, 2));
    assert!(rows.iter().any(|row| row.code == "510071"));
    assert!(!rows.iter().any(|row| row.code == "501001"));
}

#[test]
fn szse_announcement_shape_reads_sec_code_arrays_and_publish_time() {
    let body = serde_json::json!({
        "announceCount": 3,
        "pageNum": 1,
        "data": [
            {"title": "终止上市公告", "publishTime": "2026-01-02 09:00:00",
             "secCode": ["159986", "159987"], "secName": "合成ETF",
             "attachPath": "/disclosure/2026/01/a.pdf"},
            {"title": "终止上市公告", "publishTime": "2026-01-02 09:00:00",
             "secCode": ["159986", "159987"], "secName": "合成ETF",
             "attachPath": "/disclosure/2026/01/a.pdf"}
        ]
    });
    let rows = parse(AnnouncementKind::Szse, body.to_string().as_bytes()).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].exchange, Exchange::Sz);
    assert_eq!(rows[0].announcement_date, day(2026, 1, 2));
    assert_eq!(rows[0].url, "/disclosure/2026/01/a.pdf");
}

#[test]
fn szse_page_size_is_capped_at_fifty_and_announce_count_is_the_total() {
    use crate::exchange_source::validate::parse_page;
    let body = serde_json::json!({
        "announceCount": 101,
        "pageNum": 2,
        "data": (0..50).map(|index| serde_json::json!({
            "title": "终止上市公告",
            "publishTime": "2026-01-02 09:00:00",
            "secCode": [format!("159{:03}", index)],
            "attachPath": format!("/{index}.pdf"),
        })).collect::<Vec<_>>(),
    });
    let envelope = parse_page(
        PagingFormat::SzseAnnouncement,
        None,
        body.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(envelope.page, 2);
    assert_eq!(envelope.page_count, 3);
    assert_eq!(envelope.rows, 50);
    assert_eq!(envelope.total, Some(101));
}

#[test]
fn szse_plan_posts_iso_se_date_bounds() {
    // Live-verified 2026-11-05: `seDate` must be `YYYY-MM-DD`; the compact
    // `YYYYMMDD` shape answers HTTP 200 with `{}`, which fails closed.
    let window = Segment::parse("2026-01-01", "2026-03-31").unwrap();
    let (_, plan) = plans()
        .into_iter()
        .find(|(kind, _)| *kind == AnnouncementKind::Szse)
        .unwrap();
    let request = plan.request(2, Some(window), "seDateStart", "seDateEnd");
    assert_eq!(request.method.as_deref(), Some("POST"));
    let body: serde_json::Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
    assert_eq!(
        body["seDate"],
        serde_json::json!(["2026-01-01", "2026-03-31"])
    );
    assert_eq!(body["channelCode"], serde_json::json!(["etfNotice_disc"]));
    assert_eq!(body["pageSize"], 50);
    assert_eq!(body["pageNum"], 2);
}

#[test]
fn sse_plan_keeps_compact_window_dates() {
    // `COMMON_PL_JJXX_JJGG_L` is verified live with `START_DATE=20260101`;
    // only the SZSE announcement channel needs the dashed shape.
    let window = Segment::parse("2026-01-01", "2026-03-31").unwrap();
    let (_, plan) = plans()
        .into_iter()
        .find(|(kind, _)| *kind == AnnouncementKind::Sse)
        .unwrap();
    let request = plan.request(1, Some(window), "START_DATE", "END_DATE");
    assert_eq!(request.params["START_DATE"], "20260101");
    assert_eq!(request.params["END_DATE"], "20260331");
}

#[test]
fn szse_empty_object_response_is_a_format_failure_not_zero_rows() {
    use crate::exchange_source::validate::parse_page;
    let error = parse_page(PagingFormat::SzseAnnouncement, None, b"{}").unwrap_err();
    assert_eq!(error.code(), "source_invalid");
}

#[test]
fn an_sse_unpaged_quiet_window_is_one_complete_empty_page() {
    use crate::exchange_source::validate::parse_page;
    let body = br#"{"pageHelp":{"pageNo":1,"pageCount":null,"total":0},"result":[]}"#;
    let envelope = parse_page(PagingFormat::SsePageHelpUnpaged, None, body).unwrap();
    assert_eq!(envelope.page, 1);
    assert_eq!(envelope.page_count, 1);
    assert_eq!(envelope.rows, 0);
    assert_eq!(envelope.total, None);
}

#[test]
fn zero_lead_extraction_moves_the_start_not_the_delisting_upper_bound() {
    let values = delisted_row_values(day(2026, 1, 10), day(2026, 1, 10));
    assert_eq!(values.effective_from, day(2026, 1, 9));
    assert_eq!(values.effective_to, day(2026, 1, 10));
    assert_eq!(values.delisting_date, day(2026, 1, 10));
    assert_eq!(values.delisting_source, DelistingSource::ExchangeList);
    assert_eq!(
        values.delisting_reason_class,
        crate::security_master::DelistingReason::Other
    );

    let normal = delisted_row_values(day(2026, 1, 2), day(2026, 1, 10));
    assert_eq!(normal.effective_from, day(2026, 1, 2));
    assert_eq!(normal.effective_to, day(2026, 1, 10));
}

#[test]
fn announcement_windows_stop_at_yesterday_so_a_new_quarter_needs_no_todays_tail() {
    let quarter_start = day(2026, 10, 1);
    for kind in [AnnouncementKind::Sse, AnnouncementKind::Szse] {
        let windows = windows(kind, quarter_start).unwrap();
        assert!(windows.iter().all(|window| window.end() < quarter_start));
        assert_eq!(windows.last().unwrap().end(), day(2026, 9, 30));
    }
}
