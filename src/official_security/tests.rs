use chrono::NaiveDate;

use super::*;
use crate::security_master::DelistingReason;

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

struct RowSpec {
    deleted: bool,
    market: &'static str,
    code: &'static str,
    ssdate: &'static str,
    zgb: &'static str,
    bg: &'static str,
    hg: &'static str,
    ltag: &'static str,
}

impl RowSpec {
    fn value(&self, index: usize) -> &'static str {
        match index {
            SC_INDEX => self.market,
            GPDM_INDEX => self.code,
            ZGB_INDEX => self.zgb,
            BG_INDEX => self.bg,
            HG_INDEX => self.hg,
            LTAG_INDEX => self.ltag,
            SSDATE_INDEX => self.ssdate,
            _ => "",
        }
    }
}

fn row(
    market: &'static str,
    code: &'static str,
    ssdate: &'static str,
    zgb: &'static str,
    ltag: &'static str,
) -> RowSpec {
    RowSpec {
        deleted: false,
        market,
        code,
        ssdate,
        zgb,
        bg: "0.00",
        hg: "0.00",
        ltag,
    }
}

/// Build a full 40-field DBF from the anchored schema, so the fixture exercises
/// the same header/record lengths as the real `base.dbf` (#399 A-5).
fn dbf(rows: &[RowSpec]) -> Vec<u8> {
    let header_length = usize::from(ANCHORED_HEADER_LENGTH);
    let record_length = usize::from(ANCHORED_RECORD_LENGTH);
    let mut out = vec![0u8; header_length];
    out[0] = 0x03;
    out[1..4].copy_from_slice(&[124, 1, 1]);
    out[4..8].copy_from_slice(&(rows.len() as u32).to_le_bytes());
    out[8..10].copy_from_slice(&ANCHORED_HEADER_LENGTH.to_le_bytes());
    out[10..12].copy_from_slice(&ANCHORED_RECORD_LENGTH.to_le_bytes());
    for (index, anchor) in ANCHORED_FIELDS.iter().enumerate() {
        let offset = 32 + index * 32;
        out[offset..offset + anchor.name.len()].copy_from_slice(anchor.name.as_bytes());
        out[offset + 11] = anchor.field_type;
        out[offset + 16] = anchor.length;
        out[offset + 17] = anchor.decimals;
    }
    out[header_length - 1] = 0x0d;
    for spec in rows {
        let mut record = vec![b' '; record_length];
        record[0] = if spec.deleted { b'*' } else { b' ' };
        let mut offset = 1;
        for (index, anchor) in ANCHORED_FIELDS.iter().enumerate() {
            let length = usize::from(anchor.length);
            let value = spec.value(index).as_bytes();
            assert!(
                value.len() <= length,
                "fixture value too long for {}",
                anchor.name
            );
            record[offset..offset + value.len()].copy_from_slice(value);
            offset += length;
        }
        out.extend_from_slice(&record);
    }
    out.push(0x1a);
    out
}

fn infoharbor(rows: &[(u8, &str, &str, &str)]) -> Vec<u8> {
    let mut out = String::new();
    for (market, code, start, end) in rows {
        out.push_str(&format!("{market}|{code}|0|{start}|{end}\r\n"));
    }
    out.into_bytes()
}

fn gbbq_event(market: u8, code: &str, date_raw: u32, category: u8, c_raw: [f32; 4]) -> gbbq::Event {
    gbbq::Event {
        market,
        code: code.to_string(),
        date_raw,
        category,
        c_raw,
    }
}

#[test]
fn anchored_offsets_match_the_a5_evidence_table() {
    let offset = |index: usize| {
        1 + ANCHORED_FIELDS[..index]
            .iter()
            .map(|field| usize::from(field.length))
            .sum::<usize>()
    };
    assert_eq!(offset(ZGB_INDEX), 16);
    assert_eq!(offset(BG_INDEX), 72);
    assert_eq!(offset(HG_INDEX), 86);
    assert_eq!(offset(LTAG_INDEX), 100);
    assert_eq!(offset(SSDATE_INDEX), 459);
    assert_eq!(ANCHORED_RECORD_LENGTH, 481);
}

#[test]
fn parses_the_anchored_base_fields_as_integer_shares() {
    let base = BaseDbf::parse(&dbf(&[row(
        "0",
        "000001",
        "19910403",
        "1940591.88",
        "1940591.88",
    )]))
    .unwrap();
    assert_eq!(base.deleted, 0);
    assert_eq!(base.rows.len(), 1);
    let row = &base.rows[0];
    assert_eq!(row.market, 0);
    assert_eq!(row.code, "000001");
    assert_eq!(row.ssdate, Some(day(1991, 4, 3)));
    assert_eq!(row.zgb, Some(19_405_918_800));
    assert_eq!(row.bg, Some(0));
    assert_eq!(row.hg, Some(0));
    assert_eq!(row.ltag, Some(19_405_918_800));
}

#[test]
fn ssdate_zero_is_missing_not_a_delisting_date() {
    let base = BaseDbf::parse(&dbf(&[row("0", "002257", "0", "10308.00", "2600.00")])).unwrap();
    assert_eq!(base.rows[0].ssdate, None);
    assert_eq!(base.rows[0].zgb, Some(103_080_000));
    assert_eq!(base.rows[0].ltag, Some(26_000_000));
}

#[test]
fn deleted_records_are_counted_and_not_parsed() {
    let mut deleted = row("0", "000001", "19910403", "1.00", "1.00");
    deleted.deleted = true;
    let base = BaseDbf::parse(&dbf(&[
        deleted,
        row("0", "000002", "19910403", "1.00", "1.00"),
    ]))
    .unwrap();
    assert_eq!(base.deleted, 1);
    assert_eq!(base.rows.len(), 1);
    assert_eq!(base.rows[0].code, "000002");
}

#[test]
fn base_dbf_schema_drift_fails_closed() {
    let mut bytes = dbf(&[row("0", "000001", "19910403", "1.00", "1.00")]);
    // Shrink ZGB's declared length: the container still parses (the field
    // bytes fit), but the anchor must reject the different schema.
    bytes[32 + ZGB_INDEX * 32 + 16] = 13;
    let error = BaseDbf::parse(&bytes).unwrap_err();
    assert!(
        matches!(error, Error::SchemaDrift { index, .. } if index == ZGB_INDEX),
        "unexpected error: {error}"
    );
}

#[test]
fn duplicate_base_rows_fail_closed() {
    let error = BaseDbf::parse(&dbf(&[
        row("0", "000001", "19910403", "1.00", "1.00"),
        row("0", "000001", "19910403", "1.00", "1.00"),
    ]))
    .unwrap_err();
    assert!(matches!(error, Error::BaseDuplicate { .. }), "{error}");
}

#[test]
fn infoharbor_parses_crlf_rows_and_empty_bounds() {
    let rows = parse_infoharbor(&infoharbor(&[
        (1, "600001", "20130104", "20131231"),
        (0, "000001", "", ""),
        (0, "000002", "", "20200101"),
    ]))
    .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].market, 1);
    assert_eq!(rows[0].start, Some(day(2013, 1, 4)));
    assert_eq!(rows[0].end, Some(day(2013, 12, 31)));
    assert_eq!(rows[1].start, None);
    assert_eq!(rows[1].end, None);
    assert_eq!(rows[2].start, None);
    assert_eq!(rows[2].end, Some(day(2020, 1, 1)));
}

#[test]
fn infoharbor_rejects_lf_only_duplicates_and_a_bad_third_field() {
    let lf = parse_infoharbor(b"1|600001|0|20130104|20131231\n").unwrap_err();
    assert!(matches!(lf, Error::InfoharborLine { .. }), "{lf}");

    let duplicate =
        parse_infoharbor(&infoharbor(&[(1, "600001", "", ""), (1, "600001", "", "")])).unwrap_err();
    assert!(
        matches!(duplicate, Error::InfoharborDuplicate { .. }),
        "{duplicate}"
    );

    let third = parse_infoharbor(b"1|600001|1||\r\n").unwrap_err();
    assert!(matches!(third, Error::InfoharborLine { .. }), "{third}");
}

#[test]
fn assembles_lifecycle_capital_and_current_shares() {
    let base = BaseDbf::parse(&dbf(&[
        row("0", "000001", "19910403", "1940591.88", "1940591.88"),
        row("1", "600001", "19911101", "100000.00", "50000.00"),
        row("0", "200002", "19950101", "10000.00", "10000.00"),
    ]))
    .unwrap();
    let arrangements = parse_infoharbor(&infoharbor(&[
        (1, "600001", "20130104", "20131231"),
        (0, "000001", "", ""),
        (0, "200002", "20200101", "20200201"),
    ]))
    .unwrap();
    let events = vec![
        gbbq_event(0, "000001", 20240620, 2, [0.0, 1000.0, 500.0, 1500.0]),
        gbbq_event(0, "000001", 20250620, 3, [0.0, 1000.0, 0.0, 1000.0]),
        gbbq_event(0, "000001", 20260620, 9, [0.0, 1000.0, 1000.0, 1000.0]),
        gbbq_event(0, "000001", 20270620, 10, [0.0, 1000.0, 1000.0, 1000.0]),
        gbbq_event(0, "000001", 20280620, 5, [0.0, 0.0, 0.0, 0.0]),
        gbbq_event(0, "000001", 20290620, 8, [0.0, 0.0, 0.0, 0.0]),
    ];
    let facts = parse_security_facts(&base, &arrangements, &events).unwrap();

    assert_eq!(facts.lifecycle.len(), 2);
    assert_eq!(facts.capital.len(), 2);
    assert_eq!(facts.current_shares.len(), 2);
    assert_eq!(facts.skipped.non_a_share_base_rows, 1);
    assert_eq!(facts.skipped.non_a_share_arrangement_rows, 1);

    let delisted = facts
        .lifecycle
        .iter()
        .find(|fact| fact.entity() == "sh.600001")
        .unwrap();
    assert_eq!(delisted.effective_from, day(1991, 11, 1));
    assert_eq!(delisted.effective_to, Some(day(2013, 12, 31)));
    assert_eq!(delisted.listing_date, Some(day(1991, 11, 1)));
    assert_eq!(delisted.listing_date_tdx, Some(day(1991, 11, 1)));
    assert_eq!(delisted.listing_source, ListingSource::TdxSsdate);
    assert_eq!(delisted.delisting_date, Some(day(2013, 12, 31)));
    assert_eq!(delisted.delisting_date_tdx, Some(day(2013, 12, 31)));
    assert_eq!(delisted.delisting_source, DelistingSource::TdxArrangement);
    assert_eq!(delisted.delisting_reason_class, DelistingReason::Unknown);
    assert_eq!(delisted.arrangement_start, Some(day(2013, 1, 4)));
    assert_eq!(delisted.arrangement_end, Some(day(2013, 12, 31)));
    assert_eq!(delisted.issuer_resolution, IssuerResolution::CodeDate);

    let live = facts
        .lifecycle
        .iter()
        .find(|fact| fact.entity() == "sz.000001")
        .unwrap();
    assert_eq!(live.effective_to, None);
    assert_eq!(live.delisting_date, None);
    assert_eq!(live.delisting_source, DelistingSource::Unknown);
    assert_eq!(live.delisting_reason_class, DelistingReason::Unknown);
    assert_eq!(live.arrangement_start, None);
    assert_eq!(live.arrangement_end, None);

    let capital = facts
        .capital
        .iter()
        .find(|set| set.entity == "sz.000001")
        .unwrap();
    assert_eq!(capital.events.len(), 4);
    assert_eq!(
        capital
            .events
            .iter()
            .map(|event| event.category)
            .collect::<Vec<_>>(),
        vec![2, 3, 9, 10]
    );
    assert_eq!(capital.events[0].ordinal, 0);
    assert_eq!(capital.events[0].float_after, 5_000_000);
    assert_eq!(capital.events[0].total_after, 15_000_000);
    assert_eq!(capital.excluded, 2);
    assert_eq!(capital.excluded_by_category.get(&5), Some(&1));
    assert_eq!(capital.excluded_by_category.get(&8), Some(&1));

    let shares = facts
        .current_shares
        .iter()
        .find(|snapshot| snapshot.entity == "sz.000001")
        .unwrap();
    assert_eq!(shares.shares.zgb, Some(19_405_918_800));
    assert_eq!(shares.shares.ltag, Some(19_405_918_800));
}

#[test]
fn missing_listing_is_counted_and_not_written() {
    let base = BaseDbf::parse(&dbf(&[row("0", "002257", "0", "10308.00", "2600.00")])).unwrap();
    let arrangements =
        parse_infoharbor(&infoharbor(&[(0, "002257", "20200101", "20200201")])).unwrap();
    let facts = parse_security_facts(&base, &arrangements, &[]).unwrap();
    assert!(facts.lifecycle.is_empty());
    assert_eq!(facts.capital.len(), 1);
    assert_eq!(facts.skipped.missing_listing_base_rows, 1);
    assert_eq!(facts.skipped.missing_listing_with_arrangement_rows, 1);
    assert_eq!(facts.skipped.unmatched_arrangement_rows, 0);
}

#[test]
fn market_mismatch_and_unmatched_arrangements_are_counted() {
    let base = BaseDbf::parse(&dbf(&[
        row("1", "000001", "19910403", "1.00", "1.00"),
        row("0", "200002", "19950101", "1.00", "1.00"),
    ]))
    .unwrap();
    let arrangements = parse_infoharbor(&infoharbor(&[
        (0, "000002", "", "20200101"),
        (0, "600002", "", "20200101"),
        (0, "200002", "", "20200101"),
    ]))
    .unwrap();
    let facts = parse_security_facts(&base, &arrangements, &[]).unwrap();
    assert!(facts.lifecycle.is_empty());
    assert_eq!(facts.skipped.market_mismatch_base_rows, 1);
    assert_eq!(facts.skipped.non_a_share_base_rows, 1);
    assert_eq!(facts.skipped.unmatched_arrangement_rows, 1);
    assert_eq!(facts.skipped.market_mismatch_arrangement_rows, 1);
    assert_eq!(facts.skipped.non_a_share_arrangement_rows, 1);
}

#[test]
fn category_one_is_excluded_like_five_seven_and_eight() {
    let base = BaseDbf::parse(&dbf(&[row("0", "000001", "19910403", "1.00", "1.00")])).unwrap();
    let events = vec![gbbq_event(0, "000001", 20240620, 1, [0.0, 0.0, 0.0, 0.0])];
    let facts = parse_security_facts(&base, &[], &events).unwrap();
    let set = &facts.capital[0];
    assert!(set.events.is_empty());
    assert_eq!(set.excluded, 1);
    assert_eq!(set.excluded_by_category.get(&1), Some(&1));
}

#[test]
fn an_invalid_event_date_fails_closed() {
    let base = BaseDbf::parse(&dbf(&[row("0", "000001", "19910403", "1.00", "1.00")])).unwrap();
    let events = vec![gbbq_event(0, "000001", 20230230, 2, [0.0, 0.0, 0.0, 0.0])];
    let error = parse_security_facts(&base, &[], &events).unwrap_err();
    assert!(matches!(error, Error::EventDate { .. }), "{error}");
}
