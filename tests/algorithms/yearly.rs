use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use std::{collections::BTreeSet, str::FromStr};
use x_data_mcp::{
    market_data::*,
    publish::{EffectiveVersion, ValidationSummary},
    yearly::*,
};
fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}
fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}
fn input() -> EffectiveVersion {
    let records = ["2024-12-31", "2025-01-02", "2025-12-30", "2026-01-05"]
        .iter()
        .enumerate()
        .map(|(i, s)| Bar {
            date: date(s),
            open: d("1.123456789"),
            high: d("3.987654321"),
            low: d("0.123456789"),
            close: Decimal::from(i + 1),
            volume: Some(d("1.25")),
            amount: Some(d("0.0000000000000000000000000001")),
            r7_raw: None,
        })
        .collect::<Vec<_>>();
    let mut input = EffectiveVersion {
        version_id: "published-1".into(),
        symbol: "sh.600006".into(),
        period: Period::Day,
        adjustment: Adjustment::Forward,
        source: "official-files:test-fixture".into(),
        records,
        published_at: Utc::now(),
        validation: ValidationSummary {
            record_count: 4,
            unique_record_count: 4,
            duplicate_count: 0,
        },
        unit_currency: UnitCurrencyEvidence::confirmed(
            "CNY",
            "yuan",
            "share",
            "CNY",
            "yuan",
            "test evidence",
        ),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "test evidence"),
        yearly_derivation: None,
    };
    input.records[3].high = d("4.5");
    input
}
#[test]
fn multiple_years_and_selected_historical_year_are_exact_and_immutable() {
    let daily = input();
    let original = daily.clone();
    let all = aggregate(&daily, None, date("2026-09-12")).unwrap();
    assert_eq!(all.records.len(), 3);
    let y = &all.records[1];
    assert_eq!(y.bar.open, d("1.123456789"));
    assert_eq!(y.bar.high, d("3.987654321"));
    assert_eq!(y.bar.low, d("0.123456789"));
    assert_eq!(y.bar.close, d("3"));
    assert_eq!(y.bar.volume, Some(d("2.50")));
    assert_eq!(y.bar.amount, Some(d("0.0000000000000000000000000002")));
    assert_eq!(y.lineage.first_trading_date, date("2025-01-02"));
    assert_eq!(y.lineage.last_trading_date, date("2025-12-30"));
    assert_eq!(y.lineage.base_daily_version_id, daily.version_id);
    assert!(!y.year_unfinished);
    assert!(all.records[2].year_unfinished);
    let selected = aggregate(&daily, Some(&BTreeSet::from([2025])), date("2026-09-12")).unwrap();
    assert_eq!(selected.records, vec![y.clone()]);
    assert_eq!(daily, original);
    assert_eq!(all, aggregate(&daily, None, date("2026-09-12")).unwrap());
}
#[test]
fn unknown_optional_fields_do_not_become_zero_or_affect_other_years() {
    let mut daily = input();
    daily.records[1].volume = None;
    daily.unit_currency.amount_unit = EvidenceValue::unknown("unknown");
    let output = aggregate(&daily, None, date("2026-09-12")).unwrap();
    assert_eq!(output.records[1].bar.volume, None);
    assert_eq!(output.records[0].bar.volume, Some(d("1.25")));
    assert!(output.records.iter().all(|r| r.bar.amount.is_none()));
}
#[test]
fn invalid_inputs_and_missing_years_reject() {
    let base = input();
    for mutate in [
        |d: &mut EffectiveVersion| d.period = Period::Year,
        |d: &mut EffectiveVersion| d.version_id.clear(),
        |d: &mut EffectiveVersion| d.source.clear(),
        |d: &mut EffectiveVersion| d.symbol.clear(),
        |d: &mut EffectiveVersion| d.adjustment_evidence = AdjustmentEvidence::unknown("missing"),
        |d: &mut EffectiveVersion| d.unit_currency.price_unit = EvidenceValue::unknown("missing"),
        |d: &mut EffectiveVersion| d.records.swap(0, 1),
        |d: &mut EffectiveVersion| d.records[1].date = d.records[0].date,
    ] {
        let mut daily = base.clone();
        mutate(&mut daily);
        assert!(aggregate(&daily, None, date("2026-09-12")).is_err());
    }
    assert_eq!(
        aggregate(&base, Some(&BTreeSet::from([2023])), date("2026-09-12")),
        Err(AggregationError::MissingYear(2023))
    );
    assert!(aggregate(&base, Some(&BTreeSet::new()), date("2026-09-12")).is_err());
    assert!(aggregate(&base, None, date("2025-12-31")).is_err());
}
#[test]
fn sum_overflow_or_loss_of_precision_rejects_but_unknown_remains_unknown() {
    let mut daily = input();
    daily.records[1].volume = Some(Decimal::MAX);
    daily.records[2].volume = Some(Decimal::ONE);
    assert_eq!(
        aggregate(&daily, None, date("2026-09-12")),
        Err(AggregationError::InexactSum)
    );
    daily.records[2].volume = Some(d("0.0000000000000000000000000001"));
    assert_eq!(
        aggregate(&daily, None, date("2026-09-12")),
        Err(AggregationError::InexactSum)
    );
    daily.records[2].volume = None;
    assert!(
        aggregate(&daily, None, date("2026-09-12")).unwrap().records[1]
            .bar
            .volume
            .is_none()
    );
}
#[test]
fn calendar_end_flag_does_not_fabricate_a_december_31_bar() {
    let daily = input();
    let output = aggregate(&daily, None, date("2026-12-31")).unwrap();
    assert!(output.records[2].year_unfinished);
    assert!(!aggregate(&daily, None, date("2027-01-01")).unwrap().records[2].year_unfinished);
    assert_eq!(output.records[2].bar.date, date("2026-01-05"));
}

#[path = "../support/yearly_reference.rs"]
mod reference;

#[test]
fn optimized_aggregation_matches_reference() {
    let mut daily = input();
    for missing in [false, true] {
        if missing {
            daily.records[1].amount = None;
        }
        for years in [
            None,
            Some(BTreeSet::from([2024, 2026])),
            Some(BTreeSet::from([2023])),
        ] {
            assert_eq!(
                aggregate(&daily, years.as_ref(), date("2026-09-12")),
                reference::aggregate(&daily, years.as_ref(), date("2026-09-12"))
            );
        }
    }
}

#[test]
#[ignore = "manual release A/B timing; synthetic CPU-only aggregation, no storage"]
fn aggregation_ab_timing() {
    use std::{hint::black_box, time::Instant};
    let mut daily = input();
    let template = daily.records[0].clone();
    daily.records = (0..10000)
        .map(|i| {
            let mut bar = template.clone();
            bar.date = date("1990-01-01") + chrono::Duration::days(i);
            bar
        })
        .collect();
    let as_of = date("2026-09-12");
    assert_eq!(
        aggregate(&daily, None, as_of),
        reference::aggregate(&daily, None, as_of)
    );
    let mut old = Vec::new();
    let mut new = Vec::new();
    for round in 0..21 {
        for optimized in if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let start = Instant::now();
            for _ in 0..20 {
                black_box(if optimized {
                    aggregate(black_box(&daily), None, as_of)
                } else {
                    reference::aggregate(black_box(&daily), None, as_of)
                })
                .unwrap();
            }
            if optimized {
                new.push(start.elapsed().as_secs_f64() / 20.0);
            } else {
                old.push(start.elapsed().as_secs_f64() / 20.0);
            }
        }
    }
    old.sort_by(f64::total_cmp);
    new.sort_by(f64::total_cmp);
    eprintln!(
        "bars=10000 rounds=21 iterations=20 old_p50_us={:.1} new_p50_us={:.1} speedup={:.3}",
        old[10] * 1e6,
        new[10] * 1e6,
        old[10] / new[10]
    );
}

// ---- XP-031 R1: memo key normalizes `as_of` to its year -------------------
fn years_2025_2026() -> BTreeSet<i32> {
    [2025, 2026].into_iter().collect()
}

#[test]
fn as_of_only_changes_the_echo_within_one_year() {
    let daily = input();
    let years = years_2025_2026();
    let a = aggregate(&daily, Some(&years), date("2026-09-12")).unwrap();
    let b = aggregate(&daily, Some(&years), date("2026-12-31")).unwrap();
    assert_eq!(
        a.records, b.records,
        "same year, both >= last bar: identical records"
    );
    assert_ne!(a.as_of, b.as_of, "only the echoed as_of differs");
    assert_eq!(
        aggregate_key("ab", &years, date("2026-09-12")),
        aggregate_key("ab", &years, date("2026-12-31"))
    );
}

#[test]
fn as_of_in_another_year_is_never_merged() {
    let daily = input();
    let years = years_2025_2026();
    let a = aggregate(&daily, Some(&years), date("2026-09-12")).unwrap();
    let b = aggregate(&daily, Some(&years), date("2027-01-02")).unwrap();
    let flags = |s: &AnnualSeries| {
        s.records
            .iter()
            .map(|r| r.year_unfinished)
            .collect::<Vec<_>>()
    };
    assert_ne!(
        flags(&a),
        flags(&b),
        "2026 is unfinished at 2026-09-12 but finished at 2027-01-02"
    );
    assert_ne!(
        aggregate_key("ab", &years, date("2026-09-12")),
        aggregate_key("ab", &years, date("2027-01-02"))
    );
}

#[test]
fn memo_reuses_across_as_of_in_one_year_and_rewrites_the_echo() {
    let daily = input();
    let years = years_2025_2026();
    let last = daily.records.last().map(|b| b.date);
    // A unique digest keeps this test independent of the thread-local memo's other users.
    let digest = "r1-memo-reuse";
    let mut runs = 0;
    let first_as_of = date("2026-09-12");
    let second_as_of = date("2026-11-30");
    let first = aggregate_memoized(
        aggregate_key(digest, &years, first_as_of),
        first_as_of,
        last,
        || {
            runs += 1;
            aggregate(&daily, Some(&years), first_as_of)
        },
    )
    .unwrap();
    let second = aggregate_memoized(
        aggregate_key(digest, &years, second_as_of),
        second_as_of,
        last,
        || {
            runs += 1;
            aggregate(&daily, Some(&years), second_as_of)
        },
    )
    .unwrap();
    assert_eq!(runs, 1, "second caller reuses the memoized records");
    assert_eq!(first.as_of, first_as_of);
    assert_eq!(second.as_of, second_as_of, "echo is rewritten per caller");
    assert_eq!(first.records, second.records);
    // Equivalent to computing directly for the second caller.
    assert_eq!(
        *second,
        aggregate(&daily, Some(&years), second_as_of).unwrap()
    );
}

#[test]
fn memo_hit_never_bypasses_the_bar_after_as_of_rejection() {
    let daily = input();
    let years = years_2025_2026();
    let last = daily.records.last().map(|b| b.date); // 2026-01-05
    let digest = "r1-memo-reject";
    let ok_as_of = date("2026-09-12");
    aggregate_memoized(
        aggregate_key(digest, &years, ok_as_of),
        ok_as_of,
        last,
        || aggregate(&daily, Some(&years), ok_as_of),
    )
    .unwrap();
    // Same year but BEFORE the last bar: must not be served from the memo.
    let early = date("2026-01-04");
    let mut ran = false;
    let result = aggregate_memoized(aggregate_key(digest, &years, early), early, last, || {
        ran = true;
        aggregate(&daily, Some(&years), early)
    });
    assert!(ran, "validation falls through to compute");
    assert!(
        result.is_err(),
        "the original InvalidInput rejection is preserved"
    );
}
