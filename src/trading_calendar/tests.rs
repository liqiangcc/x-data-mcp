//! `cal-union-v1` domain tests (issue-383 §5, AC-M1-4/AC-M1-5): the union, the
//! one-way day semantics, the input registration, the digest and the coverage
//! threshold. The `.day` members themselves are exercised by the adapter test
//! (`tests/service/official_market.rs`) and the storage path by
//! `sqlite_market/calendar/tests.rs`.

use chrono::NaiveDate;

use super::*;
use crate::market_data::{CalendarBatch, CalendarInputDates};

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// A batch whose member digests are well-formed (the union validates them).
fn batch_owned(inputs: Vec<(&str, Vec<NaiveDate>)>) -> CalendarBatch {
    CalendarBatch {
        inputs: inputs
            .into_iter()
            .map(|(entity, days)| CalendarInputDates {
                entity: entity.to_string(),
                member: format!("{entity}.day"),
                day_member_sha256: crate::hash::sha256(entity.as_bytes()),
                days,
            })
            .collect(),
        security_event_sha256: crate::hash::sha256(&0u64.to_le_bytes()),
    }
}

fn batch(inputs: &[(&str, &[NaiveDate])]) -> CalendarBatch {
    batch_owned(
        inputs
            .iter()
            .map(|(entity, days)| (*entity, days.to_vec()))
            .collect(),
    )
}

fn coverage(total: i64, outside: i64) -> CalendarCoverage {
    CalendarCoverage {
        total_bars: total,
        outside_bars: outside,
        outside_days: Vec::new(),
        outside_samples: Vec::new(),
        days_truncated: false,
        samples_truncated: false,
        calendar_digest: "0".repeat(64),
    }
}

/// AC-M1-5: a date only one input printed is a trading day. The 1991 Shenzhen
/// Saturday sessions are the real case — sh.000001 has no bar on 1991-04-06
/// (a Saturday), sz.399001 does.
#[test]
fn a_date_only_one_index_traded_is_still_a_trading_day() {
    let saturday = day(1991, 4, 6);
    let calendar = TradingCalendar::union(&batch(&[
        ("sh.000001", &[day(1991, 4, 5), day(1991, 4, 8)]),
        ("sz.399001", &[saturday, day(1991, 4, 8)]),
    ]))
    .unwrap();
    assert_eq!(calendar.len(), 3);
    assert_eq!(calendar.sources_on(saturday), Some(1 << 1));
    // The whole point of the union: one index missing a bar must not turn a
    // trading day into a non-trading day.
    assert_eq!(calendar.day_status(saturday), Some(DayStatus::InCalendar));
    assert!(!calendar.is_non_trading_day(saturday));
    // A date both inputs printed carries both bits.
    assert_eq!(calendar.sources_on(day(1991, 4, 8)), Some(0b11));
}

/// The union is one-way (BR-M1-5): presence means "an index had a bar", not
/// "a security was tradable"; absence inside the span means non-trading; and
/// after the last input bar the answer is unknown, not non-trading.
#[test]
fn day_status_is_three_valued_and_never_claims_tradability() {
    let calendar = TradingCalendar::union(&batch(&[
        ("sh.000001", &[day(2026, 10, 22), day(2026, 10, 23)]),
        ("sz.399001", &[day(2026, 10, 22), day(2026, 10, 23)]),
    ]))
    .unwrap();
    assert_eq!(
        calendar.day_status(day(2026, 10, 22)),
        Some(DayStatus::InCalendar)
    );
    // Saturday 2026-10-24: inside the covered span, no row → non-trading.
    assert_eq!(
        calendar.day_status(day(2026, 10, 24)),
        Some(DayStatus::NonTrading)
    );
    assert!(calendar.is_non_trading_day(day(2026, 10, 24)));
    // A date before the first covered day is non-trading too.
    assert!(calendar.is_non_trading_day(day(1990, 1, 2)));
    // Beyond `last_day` the calendar predicts nothing (§3.8).
    assert_eq!(
        calendar.day_status(day(2026, 10, 26)),
        Some(DayStatus::BeyondCalendar)
    );
    assert!(!calendar.is_non_trading_day(day(2026, 10, 26)));
    assert!(!calendar.is_non_trading_day(day(2026, 10, 22)));
    // `last_day` is the last input bar, not "today".
    assert_eq!(calendar.first_day(), Some(day(2026, 10, 22)));
    assert_eq!(calendar.last_day(), Some(day(2026, 10, 23)));
}

#[test]
fn a_missing_input_fails_closed() {
    let error = TradingCalendar::union(&batch(&[("sh.000001", &[day(2026, 10, 22)])])).unwrap_err();
    assert!(error.contains("sz.399001"), "{error}");
    // An unregistered entity is not a calendar input, however plausible it is.
    let error = TradingCalendar::union(&batch(&[
        ("sh.000001", &[day(2026, 10, 22)]),
        ("sh.000300", &[day(2026, 10, 22)]),
    ]))
    .unwrap_err();
    assert!(error.contains("not a registered calendar input"), "{error}");
}

#[test]
fn an_input_without_dates_or_out_of_order_is_rejected() {
    for (label, inputs) in [
        (
            "empty",
            vec![
                ("sh.000001", vec![day(2026, 10, 22)]),
                ("sz.399001", vec![]),
            ],
        ),
        (
            "duplicate",
            vec![
                ("sh.000001", vec![day(2026, 10, 22), day(2026, 10, 22)]),
                ("sz.399001", vec![day(2026, 10, 22)]),
            ],
        ),
        (
            "descending",
            vec![
                ("sh.000001", vec![day(2026, 10, 23), day(2026, 10, 22)]),
                ("sz.399001", vec![day(2026, 10, 22)]),
            ],
        ),
    ] {
        let error = TradingCalendar::union(&batch_owned(inputs)).unwrap_err();
        assert!(
            error.contains("no bar dates") || error.contains("strictly increasing"),
            "{label}: {error}"
        );
    }
}

/// §3.10: the digest is a function of the values and their order only. The
/// input order, the member digests and the coverage never enter it.
#[test]
fn digest_covers_values_only() {
    let sh = [day(2026, 10, 22), day(2026, 10, 23)];
    let sz = [day(2026, 10, 22), day(2026, 10, 26)];
    let forward =
        TradingCalendar::union(&batch(&[("sh.000001", &sh), ("sz.399001", &sz)])).unwrap();
    let reversed =
        TradingCalendar::union(&batch(&[("sz.399001", &sz), ("sh.000001", &sh)])).unwrap();
    assert_eq!(forward.days(), reversed.days());
    assert_eq!(forward.digest_hex(), reversed.digest_hex());
    // The mask is part of the value: losing one input's contribution to a date
    // is a different calendar even with the same day list.
    let narrower = TradingCalendar::union(&batch(&[
        ("sh.000001", &sh),
        ("sz.399001", &[day(2026, 10, 26)]),
    ]))
    .unwrap();
    assert_eq!(narrower.len(), forward.len());
    assert_ne!(narrower.digest_hex(), forward.digest_hex());
    // So is the date itself.
    let shifted = TradingCalendar::union(&batch(&[
        ("sh.000001", &sh),
        ("sz.399001", &[day(2026, 10, 22), day(2026, 10, 27)]),
    ]))
    .unwrap();
    assert_ne!(shifted.digest_hex(), forward.digest_hex());
    // The input members are folded separately: same days, different members.
    let mut reordered = batch(&[("sh.000001", &sh), ("sz.399001", &sz)]);
    reordered.inputs[1].day_member_sha256 = crate::hash::sha256(b"another member");
    let remeasured = TradingCalendar::union(&reordered).unwrap();
    assert_eq!(remeasured.digest_hex(), forward.digest_hex());
    assert_ne!(
        remeasured.source_member_digest_hex().unwrap(),
        forward.source_member_digest_hex().unwrap()
    );
}

/// AC-M1-4's threshold: at most 1 bar in 10,000 may fall outside the calendar.
#[test]
fn coverage_limit_is_the_exact_fraction() {
    assert!(coverage(10_000, 1).within_limit());
    assert!(!coverage(10_000, 2).within_limit());
    // 0.01% of the production bar count is 1,674.906 bars: 1,674 pass, 1,675
    // do not. The comparison is integer arithmetic, never a float.
    assert!(coverage(16_749_060, 1_674).within_limit());
    assert!(!coverage(16_749_060, 1_675).within_limit());
    // A run that checked nothing proved nothing.
    assert!(!coverage(0, 0).within_limit());
    assert!(
        coverage(0, 0)
            .require_within_limit()
            .unwrap_err()
            .contains("no 不复权 bar")
    );
    let over = coverage(1_000, 11);
    let error = over.require_within_limit().unwrap_err();
    assert!(error.contains("0.01%"), "{error}");
    assert!(error.contains("11/1000"), "{error}");
    assert_eq!(
        coverage(16_749_060, 261).ratio_text(),
        "261/16749060 = 0.00156%"
    );
}

#[test]
fn a_stored_calendar_answers_but_cannot_be_re_registered() {
    let stored = TradingCalendar::from_stored(vec![
        TradingDay {
            day: day(2026, 10, 22),
            sources: 1,
        },
        TradingDay {
            day: day(2026, 10, 23),
            sources: 3,
        },
    ])
    .unwrap();
    assert!(stored.inputs().is_empty());
    assert_eq!(
        stored.day_status(day(2026, 10, 24)),
        Some(DayStatus::NonTrading)
    );
    assert!(
        stored
            .source_member_digest_hex()
            .unwrap_err()
            .contains("no input members")
    );
    // An empty table is not a calendar, and a row without a source is invalid.
    assert!(TradingCalendar::from_stored(Vec::new()).is_err());
    assert!(
        TradingCalendar::from_stored(vec![TradingDay {
            day: day(2026, 10, 22),
            sources: 0,
        }])
        .is_err()
    );
}

/// §3.8 fixes the mask bits (bit0 = sh.000001, bit1 = sz.399001) and the
/// calendar inputs must be registered index benchmarks — a calendar fed from an
/// unregistered or non-index series would be evidence without a source.
#[test]
fn registration_matches_the_ddl_mask_and_the_index_registry() {
    assert_eq!(INPUTS[0].entity, "sh.000001");
    assert_eq!(INPUTS[0].bit, 1);
    assert_eq!(INPUTS[1].entity, "sz.399001");
    assert_eq!(INPUTS[1].bit, 2);
    for input in INPUTS {
        assert!(
            crate::index_registry::lookup(input.entity).is_some(),
            "{}",
            input.entity
        );
        assert!(!input.evidence_note.is_empty());
        assert_eq!(lookup(input.entity).map(|row| row.bit), Some(input.bit));
    }
    assert_eq!(ALGORITHM_VERSION, "cal-union-v1");
    assert_eq!(DIGEST_FORMAT, "trading-calendar-v1");
    assert_eq!(DATASET_KIND, "trading_calendar");
    assert_eq!(DATASET_KEY, "exchange_union");
}
