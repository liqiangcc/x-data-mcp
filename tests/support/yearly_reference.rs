// Frozen pre-optimization algorithm (434aa00), for differential checks only.
use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};
use x_data_mcp::{
    market_data::{Bar, Period, YearlyDerivation},
    publish::EffectiveVersion,
    yearly::{ALGORITHM_VERSION, AggregationError, AnnualRecord, AnnualSeries},
};
pub fn aggregate(
    daily: &EffectiveVersion,
    years: Option<&BTreeSet<i32>>,
    as_of: NaiveDate,
) -> Result<AnnualSeries, AggregationError> {
    if daily.period != Period::Day
        || daily.yearly_derivation.is_some()
        || daily.version_id.trim().is_empty()
        || daily.symbol.trim().is_empty()
        || daily.source.trim().is_empty()
        || !daily.adjustment_evidence.is_confirmed()
        || daily.adjustment_evidence.value.as_deref() != Some("qfq")
        || !daily.unit_currency.price_currency.is_confirmed()
        || !daily.unit_currency.price_unit.is_confirmed()
    {
        return Err(AggregationError::InvalidInput(
            "identity, period or evidence",
        ));
    }
    if daily.records.is_empty() || years.is_some_and(BTreeSet::is_empty) {
        return Err(AggregationError::InvalidInput("empty input or selection"));
    }
    let mut groups: BTreeMap<i32, Vec<&Bar>> = BTreeMap::new();
    let mut previous = None;
    for bar in &daily.records {
        if bar.date.year() <= 0
            || bar.date > as_of
            || previous.is_some_and(|date| date >= bar.date)
            || bar.high < bar.low
            || bar.open < bar.low
            || bar.open > bar.high
            || bar.close < bar.low
            || bar.close > bar.high
            || bar.volume.is_some_and(|v| v.is_sign_negative())
            || bar.amount.is_some_and(|v| v.is_sign_negative())
        {
            return Err(AggregationError::InvalidInput("invalid bar or date order"));
        }
        previous = Some(bar.date);
        groups.entry(bar.date.year()).or_default().push(bar);
    }
    if let Some(years) = years {
        for year in years {
            if !groups.contains_key(year) {
                return Err(AggregationError::MissingYear(*year));
            }
        }
    }
    let mut records = Vec::new();
    for (year, bars) in groups {
        if years.is_some_and(|years| !years.contains(&year)) {
            continue;
        }
        let first = bars[0];
        let last = bars[bars.len() - 1];
        let volume = sum_known(
            bars.iter().map(|bar| bar.volume),
            daily.unit_currency.volume_unit.is_confirmed(),
        )?;
        let amount = sum_known(
            bars.iter().map(|bar| bar.amount),
            daily.unit_currency.amount_unit.is_confirmed()
                && daily.unit_currency.amount_currency.is_confirmed(),
        )?;
        records.push(AnnualRecord {
            bar: Bar {
                date: last.date,
                open: first.open,
                high: bars.iter().map(|bar| bar.high).max().unwrap(),
                low: bars.iter().map(|bar| bar.low).min().unwrap(),
                close: last.close,
                volume,
                amount,
                r7_raw: None,
            },
            lineage: YearlyDerivation {
                base_daily_version_id: daily.version_id.clone(),
                algorithm_version: ALGORITHM_VERSION.into(),
                first_trading_date: first.date,
                last_trading_date: last.date,
            },
            year_unfinished: year == as_of.year(),
        });
    }
    Ok(AnnualSeries {
        symbol: daily.symbol.clone(),
        source: daily.source.clone(),
        base_daily_version_id: daily.version_id.clone(),
        as_of,
        unit_currency: daily.unit_currency.clone(),
        records,
    })
}

fn sum_known(
    values: impl Iterator<Item = Option<Decimal>>,
    unit_confirmed: bool,
) -> Result<Option<Decimal>, AggregationError> {
    if !unit_confirmed {
        return Ok(None);
    }
    // Missing data makes the whole year's field unknown, even if a partial sum
    // would overflow. Do not allow iteration order to change that classification.
    let Some(values) = values.collect::<Option<Vec<_>>>() else {
        return Ok(None);
    };
    let mut sum = Decimal::ZERO;
    for value in values {
        let next = sum.checked_add(value).ok_or(AggregationError::InexactSum)?;
        // checked_add can reduce scale to fit the 96-bit mantissa; detect loss.
        if next.checked_sub(sum) != Some(value) || next.checked_sub(value) != Some(sum) {
            return Err(AggregationError::InexactSum);
        }
        sum = next;
    }
    Ok(Some(sum))
}
