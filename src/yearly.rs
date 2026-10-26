//! Issue #60 / Issue #3: exact local aggregation of one published daily version.
//! No I/O, clock, normalization or source approval occurs in this module.
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    market_data::{Bar, Period, UnitCurrencyEvidence, YearlyDerivation},
    publish::EffectiveVersion,
};

pub const ALGORITHM_VERSION: &str = "local-natural-year-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnualRecord {
    /// Date is the actual last participating Shanghai trading date, not Dec 31.
    pub bar: Bar,
    pub lineage: YearlyDerivation,
    pub year_unfinished: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnualSeries {
    pub symbol: String,
    pub source: String,
    pub base_daily_version_id: String,
    pub as_of: NaiveDate,
    pub unit_currency: UnitCurrencyEvidence,
    pub records: Vec<AnnualRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AggregationError {
    #[error("invalid fixed QFQ daily input: {0}")]
    InvalidInput(&'static str),
    #[error("requested natural year is absent: {0}")]
    MissingYear(i32),
    #[error("exact decimal sum is not representable")]
    InexactSum,
}

/// `daily` must be retrieved from published storage by the calling use case.
/// This pure computation validates its contents, but cannot authenticate storage.
/// `years=None` selects all years; explicit empty/missing selections reject.
/// `as_of` is an explicit Shanghai date; it must not precede the fixed input.
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
        || !Adjustment::Forward.matches_evidence(&daily.adjustment_evidence)
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
    // Daily records are strictly ordered by date, so each natural year is a
    // contiguous range. Keep only index bounds instead of allocating a Vec of
    // references for every bar; the aggregation pass below retains the exact
    // existing Decimal summation semantics.
    let mut groups: Vec<(i32, usize, usize)> = Vec::with_capacity(years.map_or(4, BTreeSet::len));
    let mut previous = None;
    for (index, bar) in daily.records.iter().enumerate() {
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
        let year = bar.date.year();
        if let Some((last_year, _, end)) = groups.last_mut()
            && *last_year == year
        {
            *end = index + 1;
        } else {
            groups.push((year, index, index + 1));
        }
    }
    if let Some(years) = years {
        for year in years {
            if groups.binary_search_by_key(year, |group| group.0).is_err() {
                return Err(AggregationError::MissingYear(*year));
            }
        }
    }
    let mut records = Vec::with_capacity(years.map_or(groups.len(), BTreeSet::len));
    for (year, start, end) in groups {
        if years.is_some_and(|years| !years.contains(&year)) {
            continue;
        }
        let bars = &daily.records[start..end];
        let first = &bars[0];
        let last = &bars[bars.len() - 1];
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

thread_local! {
    /// Memoized aggregates. The key binds the published base version's
    /// stored `integrity_digest` — which itself covers bar content
    /// (projection), evidence, source binding, and identity — together
    /// with the selected years and the `as_of` YEAR, so identical keys always
    /// yield identical `records` (see `aggregate_key`). Lets commit-time
    /// lineage verification reuse the derivation's computation instead of
    /// re-decoding the base window.
    static AGGREGATE_MEMO: RefCell<HashMap<[u8; 32], Rc<AnnualSeries>>> =
        RefCell::new(HashMap::new());
}

/// Memo key for `aggregate_memoized`. `base_digest_hex` is the stored
/// `integrity_sha256`/`integrity_digest` of the authenticated base
/// version the caller verified the `EffectiveVersion` against.
///
/// `as_of` enters the key only as its calendar YEAR (XP-031 R1). In
/// `aggregate`, `as_of` has exactly three effects: (a) rejecting any bar
/// dated after it, (b) `year_unfinished = (year == as_of.year())`, and
/// (c) the echoed `AnnualSeries.as_of` field. For one base and one year set,
/// every `as_of` that is not before the last bar and lies in the same year
/// therefore yields byte-identical `records`; only the echo differs and
/// `aggregate_memoized` rewrites it per caller. Different years never share a
/// key because `year_unfinished` may differ.
pub fn aggregate_key(base_digest_hex: &str, years: &BTreeSet<i32>, as_of: NaiveDate) -> [u8; 32] {
    let mut payload = Vec::with_capacity(128);
    payload.extend_from_slice(b"x-data-aggregate-v2\0");
    payload.extend_from_slice(base_digest_hex.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&as_of.year().to_be_bytes());
    for year in years {
        payload.extend_from_slice(&year.to_be_bytes());
    }
    Sha256::digest(&payload).into()
}

/// Diagnostic: the key ingredients at one call site (stage trace only).
pub fn trace_key(
    site: &'static str,
    base_digest_hex: &str,
    years: &BTreeSet<i32>,
    as_of: NaiveDate,
) {
    if !crate::stage_trace::enabled() {
        return;
    }
    let key = aggregate_key(base_digest_hex, years, as_of);
    let base8 = u64::from_str_radix(base_digest_hex.get(..16).unwrap_or("0"), 16).unwrap_or(0);
    crate::stage_trace::note(
        site,
        &[
            ("key8", u64::from_be_bytes(key[..8].try_into().unwrap())),
            ("base8", base8),
            ("n_years", years.len() as u64),
            ("year_first", years.first().map_or(0, |y| *y as u64)),
            ("year_last", years.last().map_or(0, |y| *y as u64)),
            ("as_of_days", as_of.num_days_from_ce() as u64),
        ],
    );
}

/// Test hook: drops this thread's memoized series so a re-verification
/// recomputes from the stored base.
#[cfg(test)]
#[allow(dead_code)] // memo reset used by the S3 re-derivation write path
pub(crate) fn clear_aggregate_memo() {
    AGGREGATE_MEMO.with(|memo| memo.borrow_mut().clear());
}

/// Runs `compute` (a `aggregate` call and any reads it needs) only when
/// the memo lacks `key`; otherwise returns the prior result with its
/// `as_of` echo set to the caller's `as_of`.
///
/// `window_last` is the date of the last bar of the window this caller
/// would aggregate. A memoized series is reused only when
/// `window_last <= as_of` — the very condition `aggregate` enforces by
/// rejecting later bars — otherwise (or when unknown) the call falls
/// through to `compute`, which keeps the original rejection and error.
pub fn aggregate_memoized<E>(
    key: [u8; 32],
    as_of: NaiveDate,
    window_last: Option<NaiveDate>,
    compute: impl FnOnce() -> Result<AnnualSeries, E>,
) -> Result<Rc<AnnualSeries>, E> {
    let hit = AGGREGATE_MEMO.with(|memo| memo.borrow().get(&key).cloned());
    if crate::stage_trace::enabled() {
        crate::stage_trace::note(
            "memo.lookup",
            &[
                ("hit", hit.is_some() as u64),
                ("key8", u64::from_be_bytes(key[..8].try_into().unwrap())),
            ],
        );
    }
    if let Some(hit) = hit.filter(|_| window_last.is_some_and(|last| last <= as_of)) {
        if hit.as_of == as_of {
            return Ok(hit);
        }
        return Ok(Rc::new(AnnualSeries {
            as_of,
            ..(*hit).clone()
        }));
    }
    let series = Rc::new(compute()?);
    AGGREGATE_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        if memo.len() >= 16_384 {
            memo.clear();
        }
        memo.insert(key, series.clone());
    });
    Ok(series)
}

fn sum_known(
    values: impl Iterator<Item = Option<Decimal>> + Clone,
    unit_confirmed: bool,
) -> Result<Option<Decimal>, AggregationError> {
    if !unit_confirmed {
        return Ok(None);
    }
    // Missing data makes the whole year's field unknown, even if a partial sum
    // would overflow. Do not allow iteration order to change that classification.
    // The callers iterate immutable slices. Cloning their iterator is O(1),
    // and a presence pass avoids materializing an annual Decimal buffer.
    if values.clone().any(|value| value.is_none()) {
        return Ok(None);
    }
    let mut sum = Decimal::ZERO;
    for value in values.flatten() {
        let next = sum.checked_add(value).ok_or(AggregationError::InexactSum)?;
        // checked_add can reduce scale to fit the 96-bit mantissa; detect loss.
        if next.checked_sub(sum) != Some(value) || next.checked_sub(value) != Some(sum) {
            return Err(AggregationError::InexactSum);
        }
        sum = next;
    }
    Ok(Some(sum))
}
