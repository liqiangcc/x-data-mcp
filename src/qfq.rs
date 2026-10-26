//! Pure forward-adjustment (前复权, QFQ) daily computation over the verified
//! #54 contract.
//!
//! Inputs are the existing structural decoders: `.day` bars
//! (`crate::day_file::RawDayRecord`) and decoded GBBQ events
//! (`crate::gbbq::Event`).  For each bar this module composes the affine
//! transforms of **all events with `event_date > bar_date`**, oldest→newest,
//! applies the chain to the raw price and rounds **once** at the target scale.
//!
//! Two entry points, one contract:
//! * [`forward_adjust_exact`] (`qfq-exact-v1`, issue-383 §4.3) is what the
//!   production path writes: the chain is composed over exact rationals
//!   (`gbbq::exact_compose_chain`) and rounded once, so a 6dp value depends
//!   only on (bar, events, scale, algorithm version) and never on intermediate
//!   precision.  [`forward_adjust_exact_reference`] is its per-bar oracle.
//! * [`forward_adjust`] / [`forward_adjust_reference`] are the **2dp baseline**
//!   over 28-significant-digit `Decimal` arithmetic.  They are no longer
//!   written to storage; they stay as the historical oracle AC-M1-3 measures
//!   the difference against (every differing field must sit exactly on a 2dp
//!   half-cent, see `tests/algorithms/qfq_exact.rs`).
//!
//! Boundaries (frozen contract):
//! * an event date with no corresponding bar needs no special handling — the
//!   event simply contributes to every earlier bar;
//! * a bar on the event date itself is not adjusted by that event
//!   (`event_date > bar_date` is strict);
//! * an empty event set yields the identity chain — adjusted prices equal the
//!   raw prices.
//!
//! `amount`/`volume` are **not** adjusted: the verified contract does not
//! cover them, so they are passed through as stored and documented as such.
//!
//! This is a pure computation layer.  `official_market` providers apply it
//! over each security's category==1 GBBQ events; the remaining
//! `SOURCE_NOT_VERIFIED` production blockers are unchanged.

use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};

use crate::day_file::RawDayRecord;
use crate::gbbq::{self, Event};

/// A forward-adjusted daily bar.  Prices are business decimals at the scale
/// the caller asked for: 2dp HALF_UP for the [`forward_adjust`] baseline, the
/// series' `price_scale` (6, `qfq-exact-v1`) for [`forward_adjust_exact`].
#[derive(Debug, Clone, PartialEq)]
pub struct QfqBar {
    pub date: NaiveDate,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    /// Number of events in this bar's chain (events strictly after `date`).
    pub chain_event_count: usize,
    /// Untouched raw fields — the contract does not cover adjustment of
    /// amount/volume, so the stored values are preserved verbatim.
    pub amount_float32_bits: u32,
    pub volume_raw: u32,
    pub r7_raw: u32,
}

/// Displayed 昨结 for one event day — a queryable output of the frozen
/// contract, not a bar field.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDayMark {
    /// Raw yyyymmdd event date.
    pub event_date: u32,
    pub category: u8,
    /// Date of the bar the displayed 昨结 was computed from.
    pub previous_bar_date: Option<NaiveDate>,
    /// `gbbq::displayed_previous_close` over the previous bar's raw close;
    /// `None` when no earlier bar exists.
    pub displayed_previous_close: Option<Decimal>,
}

fn ymd(date: NaiveDate) -> u32 {
    use chrono::Datelike;
    date.year() as u32 * 10000 + date.month() * 100 + date.day()
}

fn raw_price(cents: u32) -> Decimal {
    Decimal::from(cents) / Decimal::from(100)
}

fn cent(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

/// Forward-adjust a security's `.day` bar sequence against its GBBQ events,
/// rounded to 2dp HALF_UP.
///
/// **2dp baseline only** — production writes [`forward_adjust_exact`]
/// (`qfq-exact-v1`, issue-383 §4.3). This path keeps the pre-M1 numeric
/// behaviour so AC-M1-3 can enumerate exactly where the exact chain differs
/// from it, and so the frozen #54 client-display oracles stay checkable.
///
/// `bars` stay in input order; `events` are ordered by date internally, so
/// callers may pass them unsorted.  The verified event domain is the
/// security's `category == 1` records — the caller supplies that set; this
/// function deliberately does not filter by `category` because other
/// categories' semantics are unverified.  Fails closed on non-finite
/// factors.
pub fn forward_adjust(bars: &[RawDayRecord], events: &[Event]) -> Result<Vec<QfqBar>, gbbq::Error> {
    let mut dated: Vec<(u32, [Decimal; 4])> = Vec::with_capacity(events.len());
    for event in events {
        dated.push((event.date_raw, event.factors()?));
    }
    dated.sort_by_key(|(date, _)| *date);

    // Chains are memoized per distinct event suffix: every bar with the
    // same set of later events (`dated[k..]`) shares one fold, computed by
    // `compose_chain` itself so the arithmetic — and therefore every last
    // digit — is identical to folding per bar. Cost is O(bars) lookups plus
    // one fold per distinct boundary: at most events+1 folds total, and
    // typically far fewer than one per bar.
    let mut chains: Vec<Option<gbbq::Affine>> = vec![None; dated.len() + 1];
    let chain_at = |dated: &[(u32, [Decimal; 4])],
                    chains: &mut Vec<Option<gbbq::Affine>>,
                    k: usize|
     -> gbbq::Affine {
        if let Some(chain) = chains[k] {
            return chain;
        }
        let chain = gbbq::compose_chain(dated[k..].iter().map(|(_, factors)| *factors));
        chains[k] = Some(chain);
        chain
    };

    // First index whose event date is strictly after `bar_date`. `.day`
    // input is already ascending, so a forward-only pointer is amortized
    // O(1); an out-of-order input falls back to binary search, keeping the
    // result correct for arbitrary bar order.
    let mut k = 0usize;
    let mut last_bar_date = 0u32;
    Ok(bars
        .iter()
        .map(|bar| {
            let bar_date = ymd(bar.date);
            k = if bar_date >= last_bar_date {
                k + dated[k..].partition_point(|(date, _)| *date <= bar_date)
            } else {
                dated.partition_point(|(date, _)| *date <= bar_date)
            };
            last_bar_date = bar_date;
            let chain = chain_at(dated.as_slice(), &mut chains, k);
            QfqBar {
                date: bar.date,
                open: cent(chain.apply(raw_price(bar.ohlc_raw[0]))),
                high: cent(chain.apply(raw_price(bar.ohlc_raw[1]))),
                low: cent(chain.apply(raw_price(bar.ohlc_raw[2]))),
                close: cent(chain.apply(raw_price(bar.ohlc_raw[3]))),
                chain_event_count: dated.len() - k,
                amount_float32_bits: bar.amount_float32_bits,
                volume_raw: bar.volume_raw,
                r7_raw: bar.r7_raw,
            }
        })
        .collect())
}

/// Reference implementation retained as the equivalence oracle: the direct
/// per-bar filter+fold used before chain memoization. `forward_adjust`
/// calls `compose_chain` over the identical event suffix, so output is
/// bitwise identical by construction — this oracle guards the suffix-index
/// bookkeeping, and tests assert equality over the full retained corpus.
#[doc(hidden)]
pub fn forward_adjust_reference(
    bars: &[RawDayRecord],
    events: &[Event],
) -> Result<Vec<QfqBar>, gbbq::Error> {
    let mut dated: Vec<(u32, [Decimal; 4])> = Vec::with_capacity(events.len());
    for event in events {
        dated.push((event.date_raw, event.factors()?));
    }
    dated.sort_by_key(|(date, _)| *date);

    Ok(bars
        .iter()
        .map(|bar| {
            let bar_date = ymd(bar.date);
            let chain = gbbq::compose_chain(
                dated
                    .iter()
                    .filter(|(date, _)| *date > bar_date)
                    .map(|(_, factors)| *factors),
            );
            let count = dated.iter().filter(|(date, _)| *date > bar_date).count();
            QfqBar {
                date: bar.date,
                open: cent(chain.apply(raw_price(bar.ohlc_raw[0]))),
                high: cent(chain.apply(raw_price(bar.ohlc_raw[1]))),
                low: cent(chain.apply(raw_price(bar.ohlc_raw[2]))),
                close: cent(chain.apply(raw_price(bar.ohlc_raw[3]))),
                chain_event_count: count,
                amount_float32_bits: bar.amount_float32_bits,
                volume_raw: bar.volume_raw,
                r7_raw: bar.r7_raw,
            }
        })
        .collect())
}

/// `qfq-exact-v1` — the forward series' `algorithm_version` (issue-383 §4.4).
pub const ALGORITHM_VERSION: &str = "qfq-exact-v1";

/// Forward-adjust a security's `.day` bar sequence over the **exact** rational
/// chain, rounding once at `scale` (issue-383 §4.3, `qfq-exact-v1`).
///
/// Same frozen contract as [`forward_adjust`]: events with
/// `event_date > bar_date` only, oldest→newest, unsorted input accepted,
/// `amount`/`volume` untouched. The difference is arithmetic: the chain is
/// composed over exact integers (`gbbq::exact_compose_chain`) and each price is
/// rounded once HALF_UP away from zero at `scale`, so the last digit cannot
/// depend on intermediate precision. `scale` is the series' `price_scale`
/// (6 for the forward series, `market_series.price_scale` CHECK ≤ 6).
///
/// Chains are memoized per distinct event suffix exactly like
/// [`forward_adjust`]: one fold per boundary, not per bar.
pub fn forward_adjust_exact(
    bars: &[RawDayRecord],
    events: &[Event],
    scale: u32,
) -> Result<Vec<QfqBar>, gbbq::Error> {
    gbbq::check_price_scale(scale)?;
    let mut dated: Vec<(u32, [Decimal; 4])> = Vec::with_capacity(events.len());
    for event in events {
        dated.push((event.date_raw, event.factors()?));
    }
    dated.sort_by_key(|(date, _)| *date);

    let mut chains: Vec<Option<gbbq::ExactUnits>> = (0..=dated.len()).map(|_| None).collect();
    let mut k = 0usize;
    let mut last_bar_date = 0u32;
    let mut out = Vec::with_capacity(bars.len());
    for bar in bars {
        let bar_date = ymd(bar.date);
        k = if bar_date >= last_bar_date {
            k + dated[k..].partition_point(|(date, _)| *date <= bar_date)
        } else {
            dated.partition_point(|(date, _)| *date <= bar_date)
        };
        last_bar_date = bar_date;
        if chains[k].is_none() {
            let chain = gbbq::exact_compose_chain(dated[k..].iter().map(|(_, factors)| *factors))?;
            chains[k] = Some(chain.units(scale)?);
        }
        let chain = chains[k].as_ref().expect("chain filled above");
        out.push(QfqBar {
            date: bar.date,
            open: exact_price(chain, bar.ohlc_raw[0], scale)?,
            high: exact_price(chain, bar.ohlc_raw[1], scale)?,
            low: exact_price(chain, bar.ohlc_raw[2], scale)?,
            close: exact_price(chain, bar.ohlc_raw[3], scale)?,
            chain_event_count: dated.len() - k,
            amount_float32_bits: bar.amount_float32_bits,
            volume_raw: bar.volume_raw,
            r7_raw: bar.r7_raw,
        });
    }
    Ok(out)
}

/// Reference implementation of [`forward_adjust_exact`]: the direct per-bar
/// filter + exact fold, with no suffix memoization. `forward_adjust_exact`
/// composes the identical suffix with the identical exact arithmetic, so the
/// two are bitwise equal by construction — this oracle guards the
/// suffix-index bookkeeping, not the arithmetic.
#[doc(hidden)]
pub fn forward_adjust_exact_reference(
    bars: &[RawDayRecord],
    events: &[Event],
    scale: u32,
) -> Result<Vec<QfqBar>, gbbq::Error> {
    gbbq::check_price_scale(scale)?;
    let mut dated: Vec<(u32, [Decimal; 4])> = Vec::with_capacity(events.len());
    for event in events {
        dated.push((event.date_raw, event.factors()?));
    }
    dated.sort_by_key(|(date, _)| *date);

    let mut out = Vec::with_capacity(bars.len());
    for bar in bars {
        let bar_date = ymd(bar.date);
        let chain = gbbq::exact_compose_chain(
            dated
                .iter()
                .filter(|(date, _)| *date > bar_date)
                .map(|(_, factors)| *factors),
        )?
        .units(scale)?;
        out.push(QfqBar {
            date: bar.date,
            open: exact_price(&chain, bar.ohlc_raw[0], scale)?,
            high: exact_price(&chain, bar.ohlc_raw[1], scale)?,
            low: exact_price(&chain, bar.ohlc_raw[2], scale)?,
            close: exact_price(&chain, bar.ohlc_raw[3], scale)?,
            chain_event_count: dated.iter().filter(|(date, _)| *date > bar_date).count(),
            amount_float32_bits: bar.amount_float32_bits,
            volume_raw: bar.volume_raw,
            r7_raw: bar.r7_raw,
        });
    }
    Ok(out)
}

fn exact_price(chain: &gbbq::ExactUnits, cents: u32, scale: u32) -> Result<Decimal, gbbq::Error> {
    Ok(Decimal::new(chain.units(cents)?, scale))
}

/// Displayed event-day 昨结 marks for a security's events, queryable per
/// event: the mark is `displayed_previous_close` computed from the last bar
/// strictly before the event date.  Events without a preceding bar (or
/// whose date matches no bar — the event still only needs an earlier bar)
/// report `None` for the mark.
///
/// `bars` must be the security's `.day` records; they are searched by date.
/// Fails closed on non-finite factors.
pub fn event_day_marks(
    bars: &[RawDayRecord],
    events: &[Event],
) -> Result<Vec<EventDayMark>, gbbq::Error> {
    let mut sorted: Vec<&RawDayRecord> = bars.iter().collect();
    sorted.sort_by_key(|bar| bar.date);
    events
        .iter()
        .map(|event| {
            let factors = event.factors()?;
            let previous = sorted
                .iter()
                .rev()
                .find(|bar| ymd(bar.date) < event.date_raw);
            Ok(EventDayMark {
                event_date: event.date_raw,
                category: event.category,
                previous_bar_date: previous.map(|bar| bar.date),
                displayed_previous_close: previous.map(|bar| {
                    gbbq::displayed_previous_close(raw_price(bar.ohlc_raw[3]), &factors)
                }),
            })
        })
        .collect()
}
