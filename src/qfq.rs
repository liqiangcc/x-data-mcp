//! Pure forward-adjustment (前复权, QFQ) daily computation over the verified
//! #54 contract.
//!
//! Inputs are the existing structural decoders: `.day` bars
//! (`crate::day_file::RawDayRecord`) and decoded GBBQ events
//! (`crate::gbbq::Event`).  For each bar this module composes the affine
//! transforms of **all events with `event_date > bar_date`**, oldest→newest
//! with no intermediate rounding, applies the chain to the raw price, and
//! rounds the business output to 2dp HALF_UP.
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
//! This is a pure computation layer.  It is not wired into `official_files`
//! or any production source path, and `SOURCE_NOT_VERIFIED` is unchanged.

use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};

use crate::day_file::RawDayRecord;
use crate::gbbq::{self, Event};

/// A forward-adjusted daily bar.  Prices are business-decimal 2dp HALF_UP.
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

/// Forward-adjust a security's `.day` bar sequence against its GBBQ events.
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
