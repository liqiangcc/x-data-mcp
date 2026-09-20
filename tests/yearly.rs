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
