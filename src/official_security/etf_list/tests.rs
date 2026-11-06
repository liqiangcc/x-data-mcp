//! Unit tests for the ETF identity adapter (M3-2 PR-A).
//!
//! The fixtures are **synthetic**: they reproduce the real counts and shape
//! (947 SSE rows, 770 SZSE rows) without containing exchange bytes. The real
//! responses' sha256 are the provenance recorded in the design (§12):
//! SSE `FUND_LIST` 947 rows / 694,715 B; SZSE `1945` 770 rows / 35,959 B.

use std::collections::BTreeMap;

use chrono::NaiveDate;

use super::*;
use crate::exchange_source::select::Freshness;
use crate::security_master::assembly::{ExchangeSource, PendingSource, assemble_security_master};
use crate::security_master::{DelistingReason, DelistingSource, IssuerResolution};

const SSE: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-etf-list/sse_etf_list_synthetic.json");
const SSE_DUPLICATE: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-etf-list/sse_etf_list_duplicate.json");
const SSE_TOTAL_MISMATCH: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-etf-list/sse_etf_list_total_mismatch.json");
const SZSE: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-etf-list/szse_etf_list_synthetic.xlsx");
const SZSE_DUPLICATE: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-etf-list/szse_etf_list_duplicate.xlsx");
const SZSE_SMALL: &[u8] =
    include_bytes!("../../../tests/fixtures/exchange-etf-list/szse_etf_list_small.xlsx");

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

#[test]
fn the_two_lists_parse_to_the_real_member_counts() {
    let sse = parse(ListKind::Sse, SSE).unwrap();
    assert_eq!(sse.len(), 947);
    assert!(sse.iter().all(|row| row.exchange == Exchange::Sh));
    assert!(sse.iter().all(|row| row.listing_date.is_some()));
    assert_eq!(sse[0].code, "510000");
    assert_eq!(sse[0].listing_date, Some(day(2010, 1, 1)));
    assert_eq!(sse[0].name.as_deref(), Some("合成ETF0000"));

    let szse = parse(ListKind::Szse, SZSE).unwrap();
    assert_eq!(szse.len(), 770);
    assert!(szse.iter().all(|row| row.exchange == Exchange::Sz));
    assert!(szse.iter().all(|row| row.listing_date.is_none()));
    assert_eq!(szse[0].code, "159000");
    assert_eq!(szse[0].name.as_deref(), Some("合成ETF159000"));
    assert_eq!(szse[769].code, "159769");
}

#[test]
fn an_empty_list_is_not_evidence() {
    let empty = br#"{"pageHelp":{"total":0,"data":[]}}"#;
    let error = parse(ListKind::Sse, empty).unwrap_err();
    assert_eq!(error.code(), "etf_list_empty");
}

#[test]
fn duplicate_codes_fail_closed() {
    let error = parse(ListKind::Sse, SSE_DUPLICATE).unwrap_err();
    assert_eq!(error.code(), "etf_code_duplicate");
    let error = parse(ListKind::Szse, SZSE_DUPLICATE).unwrap_err();
    assert_eq!(error.code(), "etf_code_duplicate");
}

#[test]
fn an_sse_total_that_disagrees_with_the_rows_is_truncated_not_partial() {
    let error = parse(ListKind::Sse, SSE_TOTAL_MISMATCH).unwrap_err();
    assert_eq!(error.code(), "etf_total_mismatch");
}

#[test]
fn an_empty_or_truncated_szse_workbook_is_not_an_identity_set() {
    let rows = parse(ListKind::Szse, SZSE_SMALL).unwrap();
    let error = validate_set(&rows).unwrap_err();
    assert_eq!(error.code(), "etf_row_count_below_floor");
}

#[test]
fn identity_rows_are_class_two_and_keep_the_two_listing_sources() {
    let mut rows = parse(ListKind::Sse, SSE).unwrap();
    rows.extend(parse(ListKind::Szse, SZSE).unwrap());
    let first = day(2026, 11, 3);
    let first_seen = rows
        .iter()
        .map(|row| ((row.exchange, row.code.clone()), first))
        .collect();
    let admitted = Admitted {
        rows,
        history: Vec::new(),
        effective_to: BTreeMap::new(),
        names: BTreeMap::new(),
        name_ends: BTreeMap::new(),
        first_seen,
        bindings: Vec::new(),
        snapshot_ids: BTreeMap::new(),
        freshness: Freshness {
            as_of: Some("2026-11-03".into()),
            verified_through: Some("2026-11-03".into()),
            staleness_days: Some(0),
            run_ids: Vec::new(),
            selected_by: "fetch",
        },
    };
    let tdx: BTreeMap<(u8, String), NaiveDate> = admitted
        .rows
        .iter()
        .filter(|row| row.exchange == Exchange::Sz)
        .map(|row| ((0, row.code.clone()), day(2015, 1, 5)))
        .collect();
    let identity = identity_rows(&admitted, &tdx).unwrap();
    assert_eq!(identity.len(), 1_717);
    assert!(identity.iter().all(|row| row.instrument_class == 2));
    assert!(identity.iter().all(|row| row.delisting_date.is_none()));
    // D-80: every row here has a listing day, so it opens the interval; the
    // first observation day stays in `known_at`.
    assert!(
        identity
            .iter()
            .all(|row| Some(row.effective_from) == row.listing_date)
    );
    assert!(identity.iter().all(|row| row.known_at == Some(first)));
    let sh = identity
        .iter()
        .find(|row| row.exchange == Exchange::Sh)
        .unwrap();
    assert_eq!(sh.listing_source, ListingSource::ExchangeList);
    let sz = identity
        .iter()
        .find(|row| row.exchange == Exchange::Sz)
        .unwrap();
    assert_eq!(sz.listing_source, ListingSource::TdxSsdate);
    assert_eq!(sz.listing_date, Some(day(2015, 1, 5)));
    assert_eq!(sz.listing_date_tdx, Some(day(2015, 1, 5)));
    // 128 `hsjday`-only candidates never enter class=2; 512390 is one of them.
    assert!(!identity.iter().any(|row| row.code == "512390"));
}

#[test]
fn a_szse_code_without_a_tdx_ssdate_keeps_an_unknown_listing_day() {
    let rows = parse(ListKind::Szse, SZSE_SMALL).unwrap();
    let first_seen = rows
        .iter()
        .map(|row| ((row.exchange, row.code.clone()), day(2026, 11, 3)))
        .collect();
    let admitted = Admitted {
        rows,
        history: Vec::new(),
        effective_to: BTreeMap::new(),
        names: BTreeMap::new(),
        name_ends: BTreeMap::new(),
        first_seen,
        bindings: Vec::new(),
        snapshot_ids: BTreeMap::new(),
        freshness: Freshness {
            as_of: Some("2026-11-03".into()),
            verified_through: Some("2026-11-03".into()),
            staleness_days: Some(0),
            run_ids: Vec::new(),
            selected_by: "fetch",
        },
    };
    let identity = identity_rows(&admitted, &BTreeMap::new()).unwrap();
    assert_eq!(identity.len(), 2);
    assert!(identity.iter().all(|row| row.listing_date.is_none()));
    // No listing day: the first observation day is the only dated evidence, so
    // the previous behaviour stays (D-80).
    assert!(identity.iter().all(
        |row| row.effective_from == day(2026, 11, 3) && row.known_at == Some(day(2026, 11, 3))
    ));
    assert!(
        identity
            .iter()
            .all(|row| row.listing_source == ListingSource::Unknown)
    );
}

#[test]
fn etf_rows_do_not_change_the_a_share_security_master_rows() {
    let tdx = vec![SecurityLifecycleFact {
        exchange: Exchange::Sh,
        code: "600000".into(),
        effective_from: day(1999, 11, 10),
        effective_to: None,
        instrument_class: 0,
        issuer_key: None,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_date: Some(day(1999, 11, 10)),
        listing_date_tdx: Some(day(1999, 11, 10)),
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
    }];
    let base = assemble_security_master(
        &tdx,
        &[],
        ExchangeSource::Pending(PendingSource {
            dataset: crate::security_master::assembly::DATASET_KIND,
            source: "exchange_list",
            reason: "fixture",
        }),
        &[],
    )
    .unwrap();

    let rows = parse(ListKind::Szse, SZSE_SMALL).unwrap();
    let first_seen = rows
        .iter()
        .map(|row| ((row.exchange, row.code.clone()), day(2026, 11, 3)))
        .collect();
    let admitted = Admitted {
        rows,
        history: Vec::new(),
        effective_to: BTreeMap::new(),
        names: BTreeMap::new(),
        name_ends: BTreeMap::new(),
        first_seen,
        bindings: Vec::new(),
        snapshot_ids: BTreeMap::new(),
        freshness: Freshness {
            as_of: Some("2026-11-03".into()),
            verified_through: Some("2026-11-03".into()),
            staleness_days: Some(0),
            run_ids: Vec::new(),
            selected_by: "fetch",
        },
    };
    let mut etf = identity_rows(&admitted, &BTreeMap::new()).unwrap();
    let mut with_etf = base.clone();
    with_etf.rows.append(&mut etf);
    with_etf.rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
    let before: Vec<_> = base
        .rows
        .iter()
        .filter(|row| row.instrument_class == 0)
        .collect();
    let after: Vec<_> = with_etf
        .rows
        .iter()
        .filter(|row| row.instrument_class == 0)
        .collect();
    assert_eq!(before, after);
    assert_eq!(
        with_etf
            .rows
            .iter()
            .filter(|row| row.instrument_class == 2)
            .count(),
        2
    );
}

#[test]
fn the_adapter_evidence_pins_the_business_parser_version() {
    let admitted = Admitted {
        rows: parse(ListKind::Sse, SSE).unwrap(),
        history: Vec::new(),
        effective_to: BTreeMap::new(),
        names: BTreeMap::new(),
        name_ends: BTreeMap::new(),
        first_seen: BTreeMap::new(),
        bindings: Vec::new(),
        snapshot_ids: BTreeMap::new(),
        freshness: Freshness {
            as_of: Some("2026-11-03".into()),
            verified_through: Some("2026-11-03".into()),
            staleness_days: Some(0),
            run_ids: Vec::new(),
            selected_by: "fetch",
        },
    };
    let evidence = admitted.evidence();
    assert_eq!(evidence["parser_version"], PARSER_VERSION);
    assert_eq!(PARSER_VERSION, "etf-list-v2");
    // Names are parsed, not published: the item evidence keeps its v1 shape.
    let mut keys: Vec<&str> = evidence
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "adapter",
            "as_of",
            "first_seen_newest",
            "first_seen_oldest",
            "freshness",
            "history_member_count",
            "member_count",
            "parser_version",
            "removed_member_count",
            "snapshot_ids",
            "source_bindings",
            "sse_rows",
            "szse_rows",
        ]
    );
    assert_eq!(evidence["sse_rows"], 947);
    assert_eq!(evidence["member_count"], 947);
}

// ------------------------------------------------------ names (etf-list-v2) ---

fn sse_body(rows: &[serde_json::Value]) -> Vec<u8> {
    serde_json::json!({
        "pageHelp": {"pageNo": 1, "pageCount": 1, "total": rows.len(), "data": rows}
    })
    .to_string()
    .into_bytes()
}

fn sse_row(code: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut row = serde_json::json!({"fundCode": code, "listingDate": "20100101"});
    for (key, value) in extra.as_object().unwrap() {
        row[key] = value.clone();
    }
    row
}

fn name_of(rows: &[EtfListRow], code: &str) -> Option<String> {
    rows.iter()
        .find(|row| row.code == code)
        .unwrap_or_else(|| panic!("{code} is not in the parsed rows"))
        .name
        .clone()
}

#[test]
fn sse_name_is_sec_name_full_and_never_fund_abbr() {
    let body = sse_body(&[
        sse_row(
            "510300",
            serde_json::json!({"secNameFull": "沪深300ETF华泰柏瑞", "fundAbbr": "300ETF"}),
        ),
        sse_row(
            "588000",
            serde_json::json!({"secNameFull": "科创50ETF华夏", "fundAbbr": "科创50"}),
        ),
        // Only the short name is published: there is no name, not `300ETF`.
        sse_row("510310", serde_json::json!({"fundAbbr": "HS300ETF"})),
    ]);
    let rows = parse(ListKind::Sse, &body).unwrap();
    assert_eq!(
        name_of(&rows, "510300").as_deref(),
        Some("沪深300ETF华泰柏瑞")
    );
    assert_eq!(name_of(&rows, "588000").as_deref(), Some("科创50ETF华夏"));
    assert_eq!(name_of(&rows, "510310"), None);
}

#[test]
fn sse_names_keep_published_width_and_case_and_normalise_only_whitespace() {
    let body = sse_body(&[
        sse_row(
            "510001",
            serde_json::json!({"secNameFull": "  沪深300ETF\u{3000}华泰柏瑞\t "}),
        ),
        sse_row(
            "510002",
            serde_json::json!({"secNameFull": "沪深 300  ETF\u{a0}易方达"}),
        ),
        // Full-width letters and digits and lower case are the published text;
        // folding them is the search side's job, not the parser's.
        sse_row(
            "510003",
            serde_json::json!({"secNameFull": "沪深３００ｅｔｆ南方"}),
        ),
        sse_row(
            "510004",
            serde_json::json!({"secNameFull": "红利etf\u{200b}国泰"}),
        ),
    ]);
    let rows = parse(ListKind::Sse, &body).unwrap();
    assert_eq!(
        name_of(&rows, "510001").as_deref(),
        Some("沪深300ETF 华泰柏瑞")
    );
    assert_eq!(
        name_of(&rows, "510002").as_deref(),
        Some("沪深 300 ETF 易方达")
    );
    assert_eq!(
        name_of(&rows, "510003").as_deref(),
        Some("沪深３００ｅｔｆ南方")
    );
    assert_eq!(name_of(&rows, "510004").as_deref(), Some("红利etf国泰"));
}

#[test]
fn an_sse_name_that_is_missing_empty_blank_or_not_text_is_none_and_keeps_the_row() {
    let body = sse_body(&[
        sse_row("510010", serde_json::json!({"fundAbbr": "A"})),
        sse_row("510011", serde_json::json!({"secNameFull": ""})),
        sse_row("510012", serde_json::json!({"secNameFull": " \u{3000}\t "})),
        sse_row("510013", serde_json::json!({"secNameFull": null})),
        sse_row("510014", serde_json::json!({"secNameFull": 510014})),
        sse_row(
            "510015",
            serde_json::json!({"secNameFull": "\u{200b}\u{feff}"}),
        ),
        sse_row(
            "510016",
            serde_json::json!({"secNameFull": "红利ETF华泰柏瑞"}),
        ),
    ]);
    let rows = parse(ListKind::Sse, &body).unwrap();
    assert_eq!(rows.len(), 7, "a missing name never drops or fails the row");
    for code in ["510010", "510011", "510012", "510013", "510014", "510015"] {
        assert_eq!(name_of(&rows, code), None, "{code}");
    }
    assert_eq!(name_of(&rows, "510016").as_deref(), Some("红利ETF华泰柏瑞"));
    assert!(rows.iter().all(|row| row.listing_date.is_some()));
}

fn szse_book(header: &[&str], rows: &[&[&str]]) -> Vec<u8> {
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|cell| cell.to_string()).collect())
        .collect();
    crate::official_security::test_szse_xlsx(header, &rows, "2026-11-05T03:18:10Z")
}

const SZSE_HEADER: [&str; 5] = [
    "证券代码",
    "证券简称",
    "拟合指数",
    "当前规模(份)",
    "基金管理人",
];

#[test]
fn szse_name_is_the_short_name_column_wherever_it_sits() {
    let reordered = szse_book(
        &["基金管理人", "证券代码", "拟合指数", "证券简称"],
        &[
            &["合成基金", "159915", "399006 创业板指", "创业板ETF易方达"],
            &[
                "合成基金",
                "159919",
                "399300 沪深300",
                "  沪深300ETF\u{3000}嘉实 ",
            ],
        ],
    );
    let rows = parse(ListKind::Szse, &reordered).unwrap();
    assert_eq!(name_of(&rows, "159915").as_deref(), Some("创业板ETF易方达"));
    assert_eq!(name_of(&rows, "159919").as_deref(), Some("沪深300ETF 嘉实"));
    assert!(rows.iter().all(|row| row.exchange == Exchange::Sz));
}

#[test]
fn an_empty_or_short_szse_name_cell_is_none_and_never_borrowed_from_other_columns() {
    let book = szse_book(
        &SZSE_HEADER,
        &[
            &["159901", "", "399001 深证100", "1", "甲"],
            &["159902", " \u{3000} ", "399001 深证100", "1", "甲"],
            // Row shorter than the header: the short-name cell is absent.
            &["159903"],
            &["159904", "中小100ETF华夏", "", "1", "乙"],
        ],
    );
    let rows = parse(ListKind::Szse, &book).unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(name_of(&rows, "159901"), None);
    assert_eq!(name_of(&rows, "159902"), None);
    assert_eq!(name_of(&rows, "159903"), None);
    assert_eq!(name_of(&rows, "159904").as_deref(), Some("中小100ETF华夏"));
}

#[test]
fn a_szse_workbook_without_the_name_column_still_gives_identity_with_no_names() {
    let book = szse_book(
        &["证券代码", "拟合指数", "基金管理人"],
        &[&["159901", "399001 深证100", "甲"], &["159902", "", "乙"]],
    );
    let rows = parse(ListKind::Szse, &book).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.code.as_str()).collect::<Vec<_>>(),
        ["159901", "159902"]
    );
    assert!(rows.iter().all(|row| row.name.is_none()));
}

#[test]
fn adding_names_changes_no_identity_row() {
    let mut rows = parse(ListKind::Sse, SSE).unwrap();
    rows.extend(parse(ListKind::Szse, SZSE).unwrap());
    let without_names: Vec<EtfListRow> = rows
        .iter()
        .cloned()
        .map(|mut row| {
            row.name = None;
            row
        })
        .collect();
    let admitted = |rows: Vec<EtfListRow>| {
        let first_seen = rows
            .iter()
            .map(|row| ((row.exchange, row.code.clone()), day(2026, 11, 3)))
            .collect();
        Admitted {
            rows,
            history: Vec::new(),
            effective_to: BTreeMap::new(),
            names: BTreeMap::new(),
            name_ends: BTreeMap::new(),
            first_seen,
            bindings: Vec::new(),
            snapshot_ids: BTreeMap::new(),
            freshness: Freshness::fetched(day(2026, 11, 3)),
        }
    };
    let tdx = BTreeMap::new();
    assert_eq!(
        identity_rows(&admitted(rows), &tdx).unwrap(),
        identity_rows(&admitted(without_names), &tdx).unwrap()
    );
}

fn obs(y: i32, m: u32, d: u32, name: &str) -> NameObservation {
    NameObservation {
        observed_from: day(y, m, d),
        known_at: day(y, m, d),
        name: name.into(),
        snapshot_sha256: String::new(),
    }
}

/// `(day, acquired_at, sha, name)`, the shape the old per-row samples had.
type Sample = (u32, &'static str, &'static str, &'static str);

/// A chain built the way the scan builds it: snapshots in
/// `(day, acquired_at, sha)` order, one push per snapshot. The result is
/// compared on `(observed_from, name)` only, the part PR-1 pinned.
fn name_chain(mut samples: Vec<Sample>) -> Vec<NameObservation> {
    samples.sort();
    let key = (Exchange::Sh, "510300".to_string());
    let mut chains = NameChains::default();
    for (d, acquired, sha, name) in samples {
        let snapshot = Snapshot {
            kind: ListKind::Sse,
            day: day(2026, 11, d),
            acquired_at: acquired.into(),
            sha256: sha.into(),
        };
        chains.push(&key, &snapshot, day(2026, 11, d), name);
    }
    chains
        .finish()
        .remove(&key)
        .unwrap_or_default()
        .into_iter()
        .map(|mut step| {
            step.known_at = step.observed_from;
            step.snapshot_sha256 = String::new();
            step
        })
        .collect()
}

fn sample(d: u32, acquired: &'static str, sha: &'static str, name: &'static str) -> Sample {
    (d, acquired, sha, name)
}

#[test]
fn a_chain_merges_adjacent_repeats_and_starts_a_segment_on_each_change() {
    let chain = name_chain(vec![
        sample(5, "2026-11-05T01:00:00Z", "a", "甲ETF"),
        sample(6, "2026-11-06T01:00:00Z", "b", "甲ETF"),
        sample(7, "2026-11-07T01:00:00Z", "c", "乙ETF"),
        sample(8, "2026-11-08T01:00:00Z", "d", "乙ETF"),
    ]);
    assert_eq!(
        chain,
        vec![obs(2026, 11, 5, "甲ETF"), obs(2026, 11, 7, "乙ETF")]
    );
}

#[test]
fn a_chain_keeps_a_to_b_to_a_when_all_three_snapshots_are_in_the_manifest() {
    let chain = name_chain(vec![
        sample(7, "2026-11-07T01:00:00Z", "c", "甲ETF"),
        sample(5, "2026-11-05T01:00:00Z", "a", "甲ETF"),
        sample(6, "2026-11-06T01:00:00Z", "b", "乙ETF"),
    ]);
    assert_eq!(
        chain,
        vec![
            obs(2026, 11, 5, "甲ETF"),
            obs(2026, 11, 6, "乙ETF"),
            obs(2026, 11, 7, "甲ETF"),
        ]
    );
}

#[test]
fn several_snapshots_on_one_day_give_that_day_the_name_of_the_last_one() {
    let chain = name_chain(vec![
        sample(5, "2026-11-05T09:00:00Z", "z", "乙ETF"),
        sample(5, "2026-11-05T01:00:00Z", "y", "甲ETF"),
    ]);
    assert_eq!(chain, vec![obs(2026, 11, 5, "乙ETF")]);
    // Same second: the sha breaks the tie, so the answer never depends on
    // manifest order.
    let a = name_chain(vec![
        sample(5, "2026-11-05T01:00:00Z", "a", "甲ETF"),
        sample(5, "2026-11-05T01:00:00Z", "b", "乙ETF"),
    ]);
    let b = name_chain(vec![
        sample(5, "2026-11-05T01:00:00Z", "b", "乙ETF"),
        sample(5, "2026-11-05T01:00:00Z", "a", "甲ETF"),
    ]);
    assert_eq!(a, b);
    assert_eq!(a, vec![obs(2026, 11, 5, "乙ETF")]);
}

#[test]
fn names_behind_current_lists_codes_where_the_selected_list_disagrees_with_the_chain() {
    let rows = vec![
        named_row("510300", Some("甲ETF")),
        named_row("510310", Some("乙ETF")),
        named_row("510320", None),
        named_row("510330", Some("丙ETF")),
    ];
    let key = |code: &str| (Exchange::Sh, code.to_string());
    let admitted = Admitted {
        rows,
        history: Vec::new(),
        effective_to: BTreeMap::new(),
        names: BTreeMap::from([
            (key("510300"), vec![obs(2026, 11, 5, "甲ETF")]),
            // Chain ends at B while the selected list says A.
            (
                key("510310"),
                vec![obs(2026, 11, 5, "乙ETF"), obs(2026, 11, 6, "丁ETF")],
            ),
            (key("510320"), vec![obs(2026, 11, 5, "旧ETF")]),
        ]),
        name_ends: BTreeMap::new(),
        first_seen: BTreeMap::new(),
        bindings: Vec::new(),
        snapshot_ids: BTreeMap::new(),
        freshness: Freshness::fetched(day(2026, 11, 6)),
    };
    // 510330 has no chain at all but a current name: also unplaced in time.
    assert_eq!(
        admitted.names_behind_current(),
        vec![key("510310"), key("510330")]
    );
}

fn named_row(code: &str, name: Option<&str>) -> EtfListRow {
    EtfListRow {
        exchange: Exchange::Sh,
        code: code.into(),
        listing_date: Some(day(2010, 1, 1)),
        sub_class: None,
        index_code: None,
        name: name.map(str::to_string),
    }
}
