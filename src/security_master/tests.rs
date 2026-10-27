use chrono::NaiveDate;

use super::*;

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn bits(value: f32) -> u32 {
    value.to_bits()
}

#[test]
fn to_shares_applies_the_frozen_2dp_rule_then_multiplies_by_ten_thousand() {
    assert_eq!(to_shares(bits(0.0)), Some(0));
    assert_eq!(to_shares(bits(1.0)), Some(10_000));
    assert_eq!(to_shares(bits(0.01)), Some(100));
    assert_eq!(to_shares(bits(12_345.67)), Some(123_456_700));
    // 2dp HALF_UP on the shortest round-trip decimal: the f32 nearest to
    // 12,345.676 is 12,345.67578125 (rounds up), 12,345.674 is
    // 12,345.673828125 (rounds down).
    assert_eq!(to_shares(bits(12_345.676)), Some(123_456_800));
    assert_eq!(to_shares(bits(12_345.674)), Some(123_456_700));
    // Not a finite 2dp amount: refused, never silently zeroed.
    assert_eq!(to_shares(f32::NAN.to_bits()), None);
    assert_eq!(to_shares(f32::INFINITY.to_bits()), None);
    assert_eq!(to_shares(bits(-1.0)), None);
}

#[test]
fn suspicious_gate_is_strictly_more_than_fifty_percent_or_float_down_on_flat_total() {
    // +60% total: suspicious.
    assert!(is_suspicious(100, 1_000, 100, 1_600));
    // Exactly +50% is not "more than 50%".
    assert!(!is_suspicious(100, 1_000, 100, 1_500));
    // -60% total: suspicious.
    assert!(is_suspicious(100, 1_000, 100, 400));
    // Float down while total is flat: suspicious even without a total jump.
    assert!(is_suspicious(100, 1_000, 90, 1_000));
    // Float up with a flat total is not a loss.
    assert!(!is_suspicious(100, 1_000, 110, 1_000));
    // A first record from zero is not a percentage change.
    assert!(!is_suspicious(0, 0, 100, 1_000));
}

#[test]
fn capital_event_keeps_the_bits_and_recomputes_the_shares() {
    let event = CapitalEvent::from_raw(
        3,
        day(2024, 6, 20),
        2,
        [bits(0.0), bits(1_000.0), bits(500.0), bits(1_500.0)],
    )
    .unwrap();
    assert_eq!(event.ordinal, 3);
    assert_eq!(event.effective_day, day(2024, 6, 20));
    assert_eq!(event.category, 2);
    assert_eq!(event.float_before_bits, bits(0.0));
    assert_eq!(event.total_after, 15_000_000);
    assert!(event.prev_is_lagged);
    // Exactly +50% is not "more than 50%".
    assert!(!event.suspicious);
    assert!(event.shares_match_bits());
}

#[test]
fn capital_event_admits_d50_categories_and_refuses_the_rest() {
    let day = day(2024, 6, 20);
    for category in ADMITTED_CATEGORIES {
        assert!(is_admitted_category(category));
        assert_eq!(
            CapitalEvent::from_raw(0, day, category, [0; 4])
                .unwrap()
                .category,
            category
        );
    }
    for category in [0, 1, 4, 6, 11] {
        assert!(!is_admitted_category(category));
        assert_eq!(
            CapitalEvent::from_raw(0, day, category, [0; 4]),
            Err(Error::Category(category))
        );
    }
    assert_eq!(
        CapitalEvent::from_raw(0, day, 2, [f32::NAN.to_bits(), 0, 0, 0]),
        Err(Error::Shares)
    );
}
