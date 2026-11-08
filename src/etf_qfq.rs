//! ETF forward-adjustment (前复权) over the exact rational chain — pure
//! computation layer (#607 PR-A, D-89 / D-91 / D-92).
//!
//! The gbbq date of an ETF event **is its 除权除息日 `E`**, exactly like the
//! A-share records (`adjustment_events.effective_day`); the earlier
//! "event day is the 权益登记日 `R`" reading came from a day-off decode and does
//! not hold. So the ETF path enters the same chain as an A-share series and
//! keeps the same strict rule: **an event adjusts only bars dated strictly
//! before `E`** — the `E` bar itself and everything after it are untouched, and
//! a weekend/holiday `E` changes nothing but the set of earlier bars.
//!
//! What differs from A-shares is exactly two things:
//!
//! * **event → transform**: category 11 (份额折算) is `p ← p / c3`, category 1
//!   (每 10 份派现) is `p ← p − c1/10`. The A-share `(10+C3+C4)/10` formula does
//!   not apply to funds;
//! * **factor precision**: ETF factors use the f32 shortest round-trip decimal
//!   (`gbbq::shortest_factor`), not the 2dp business normalization — `c3 =
//!   0.93584` must stay `0.93584`, never `0.94`.
//!
//! Events compose in `E` order. Two events that share one `E` are the only ones
//! whose internal order matters, and it is fixed: **cat11 first, then cat1**
//! (D-91 第 1 条). They are composed into one barrier, so a bar before that `E`
//! gets one adjustment, not two sequentially-rounded ones.
//!
//! Prices are `raw / 1000` (ETF `.day` integers are 厘) and the result is
//! rounded **once**, HALF_UP away from zero, at the caller's scale (6,
//! `PRICE_SCALE_QFQ`).
//!
//! Fail-closed boundaries (no guessing):
//! * a category outside {1, 11} is [`gbbq::Error::EtfUnsupportedCategory`];
//! * a non-finite / zero-denominator / oversized factor is
//!   `EtfDegenerateFactor` / `ExactChainTooLarge`.
//!
//! This module is not wired into any read path yet; PR-B..PR-F do storage, the
//! daily update, the read surface, the contract and the export.

use std::collections::BTreeMap;

use num_bigint::BigInt;
use num_traits::Zero as _;
use rust_decimal::Decimal;

use crate::day_file::RawDayRecord;
use crate::gbbq::{self, ExactAffine};
use crate::qfq::QfqBar;

/// ETF `.day` integers are 厘 (1/1000 元), not 分.
pub const ETF_PRICE_DIVISOR: u32 = 1000;

/// 份额折算: `p ← p / c3`.
pub const ETF_CATEGORY_SCALE: u8 = 11;
/// 每 10 份派现: `p ← p − c1/10`.
pub const ETF_CATEGORY_CASH: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EtfError {
    #[error(transparent)]
    Gbbq(#[from] gbbq::Error),
}

/// One ETF event: its 除权除息日 `E`, category and shortest-precision factors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EtfEvent {
    /// 除权除息日, raw yyyymmdd — the same `effective_day` the A-share chain uses.
    pub effective_day: u32,
    pub category: u8,
    /// C1..C4 at shortest round-trip precision (`Event::shortest_factors`).
    pub factors: [Decimal; 4],
}

impl EtfEvent {
    /// Build an event from a decoded gbbq record; the record's date is `E`.
    pub fn from_gbbq(event: &gbbq::Event) -> Result<Self, EtfError> {
        Ok(Self {
            effective_day: event.date_raw,
            category: event.category,
            factors: event.shortest_factors()?,
        })
    }
}

/// The barrier the shared exact chain consumes: one `E` plus the already
/// composed transform of **all** events on that day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EtfBarrier {
    pub effective_day: u32,
    pub transform: ExactAffine,
}

impl crate::qfq::ExactTransform for EtfBarrier {
    fn to_affine(&self) -> ExactAffine {
        self.transform.clone()
    }
}

// ---------------------------------------------------------------------------
// event → transform
// ---------------------------------------------------------------------------

/// Exact per-event transform for one ETF event.
///
/// * cat11 `p ← p / c3`;
/// * cat1 `p ← p − c1/10`.
///
/// Any other category is a hard error — the ETF contract has exactly two, and
/// guessing a formula for a third would silently corrupt a series.
///
/// The chain works in fen (`gbbq::ExactAffine`, `raw / 100`), so the cash offset
/// keeps its ×10 against the yuan-denominated `c1`; the cat11 ratio is
/// unit-free.
pub fn etf_event_transform(category: u8, factors: &[Decimal; 4]) -> Result<ExactAffine, EtfError> {
    match category {
        ETF_CATEGORY_SCALE => {
            let (num, den) = decimal_fraction(factors[2])?;
            if num.is_zero() {
                return Err(EtfError::Gbbq(gbbq::Error::EtfDegenerateFactor {
                    category,
                }));
            }
            // The affine triple is a ratio: reducing `den / num` keeps both
            // signs together, so a negative `c3` still divides.
            Ok(ExactAffine::scale_by(&den, &num)?)
        }
        ETF_CATEGORY_CASH => {
            // c1 元 per 10 份 → `c1 / 10` 元 per 份, the unit `ExactAffine` works in.
            let (num, den) = decimal_fraction(factors[0])?;
            Ok(ExactAffine::translate(
                &-&num,
                &(&den * BigInt::from(10u32)),
            )?)
        }
        other => Err(EtfError::Gbbq(gbbq::Error::EtfUnsupportedCategory {
            category: other,
        })),
    }
}

/// `value` as an exact fraction `(mantissa, 10^scale)` — the affine triple
/// reduces it, so sign and common factors need no handling here.
fn decimal_fraction(value: Decimal) -> Result<(BigInt, BigInt), EtfError> {
    Ok((
        BigInt::from(value.mantissa()),
        BigInt::from(10u32).pow(value.scale()),
    ))
}

// ---------------------------------------------------------------------------
// same-E merge
// ---------------------------------------------------------------------------

/// Group events by `E` and compose each group into one transform.
///
/// Within a day the order is fixed: **every cat11 event first, then every cat1
/// event**, each group in input order. Days come back sorted, which is what the
/// shared chain wants.
pub fn etf_barriers(events: &[EtfEvent]) -> Result<Vec<EtfBarrier>, EtfError> {
    let mut days: BTreeMap<u32, Vec<&EtfEvent>> = BTreeMap::new();
    for event in events {
        days.entry(event.effective_day).or_default().push(event);
    }
    let mut barriers = Vec::with_capacity(days.len());
    for (day, mut group) in days {
        group.sort_by_key(|event| category_rank(event.category));
        let mut chain = ExactAffine::identity();
        for event in group {
            chain = chain.then(&etf_event_transform(event.category, &event.factors)?)?;
        }
        barriers.push(EtfBarrier {
            effective_day: day,
            transform: chain,
        });
    }
    Ok(barriers)
}

/// 0 for the cat11 group, 1 for the cat1 group — the merge order within one `E`.
fn category_rank(category: u8) -> u8 {
    if category == ETF_CATEGORY_SCALE { 0 } else { 1 }
}

// ---------------------------------------------------------------------------
// price adjustment
// ---------------------------------------------------------------------------

/// Forward-adjust an ETF's `.day` bars over `events`, rounded once HALF_UP at
/// `scale`.
///
/// The arithmetic is [`crate::qfq::forward_adjust_chained`] with the ETF barrier
/// adapter, so the bar→chain mapping, the strict `effective_day > bar_date` test
/// and the single rounding are the same code the A-share series uses.
pub fn etf_forward_adjust_exact(
    bars: &[RawDayRecord],
    events: &[EtfEvent],
    scale: u32,
) -> Result<Vec<QfqBar>, EtfError> {
    let barriers = etf_barriers(events)?;
    Ok(crate::qfq::forward_adjust_chained(
        bars,
        &barriers,
        scale,
        ETF_PRICE_DIVISOR,
        |barrier| barrier.effective_day,
        |barrier| barrier.transform.clone(),
    )?)
}

/// Reference implementation of [`etf_forward_adjust_exact`]: the direct per-bar
/// exact fold with no suffix memoization. Equal by construction; the tests
/// assert it bit for bit.
pub fn etf_forward_adjust_exact_reference(
    bars: &[RawDayRecord],
    events: &[EtfEvent],
    scale: u32,
) -> Result<Vec<QfqBar>, EtfError> {
    let barriers = etf_barriers(events)?;
    Ok(crate::qfq::forward_adjust_chained_reference(
        bars,
        &barriers,
        scale,
        ETF_PRICE_DIVISOR,
        |barrier| barrier.effective_day,
        |barrier| barrier.transform.clone(),
    )?)
}

/// Terse event constructor for fixtures and callers that resolve the fields
/// themselves.
pub fn etf_event(effective_day: u32, category: u8, factors: [Decimal; 4]) -> EtfEvent {
    EtfEvent {
        effective_day,
        category,
        factors,
    }
}

/// Parse the terse factor form used by fixtures — `"0.661"`, `"2.5"` — at
/// shortest round-trip precision, without the 2dp normalization
/// [`gbbq::Event::factors`] applies.
pub fn etf_factors(c1: &str, c2: &str, c3: &str, c4: &str) -> Result<[Decimal; 4], EtfError> {
    use std::str::FromStr;
    let mut out = [Decimal::ZERO; 4];
    for (index, (slot, text)) in out.iter_mut().zip([c1, c2, c3, c4]).enumerate() {
        *slot = Decimal::from_str(text)
            .map_err(|_| EtfError::Gbbq(gbbq::Error::NonFiniteFactor { index }))?;
    }
    Ok(out)
}
