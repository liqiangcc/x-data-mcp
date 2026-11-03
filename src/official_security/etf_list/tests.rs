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

    let szse = parse(ListKind::Szse, SZSE).unwrap();
    assert_eq!(szse.len(), 770);
    assert!(szse.iter().all(|row| row.exchange == Exchange::Sz));
    assert!(szse.iter().all(|row| row.listing_date.is_none()));
    assert_eq!(szse[0].code, "159000");
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
    assert!(identity.iter().all(|row| row.effective_from == first));
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
    assert_eq!(evidence["sse_rows"], 947);
    assert_eq!(evidence["member_count"], 947);
}
