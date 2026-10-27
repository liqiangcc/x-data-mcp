//! `market_cap(entity, as_of, scope)` — the 市值 read model (issue-391 §5.3,
//! AC-M2-6).
//!
//! The share scope is explicit and never inferred:
//!
//! | scope | formula | state |
//! |---|---|---|
//! | `float_a` | `close_raw(T) × float_shares(T)` | reachable |
//! | `company_total` | `close_raw(T) × total_shares(T)` | reachable, **A+B+H** |
//! | `a_share_total` | `close_raw(T) × a_share_total_shares(T)` | `unknown` |
//!
//! `total_after` from `gbbq` is the **company** total (A+B+H), so it must never
//! be reported as an A-share total: for 000002 the difference is the H shares
//! (22.07 亿股). Historical A-share total shares are `NOT_AVAILABLE` (A-4: the
//! exchange interfaces only return the current period, and the date parameters
//! are ignored), so `a_share_total` returns `unknown` with an explicit reason
//! instead of borrowing `company_total` (AC-M2-6).
//!
//! Share state is §5.2 plus the two D-50 R-3 quality gates:
//!
//! - **G1 chain continuity**: when an event's "before" does not match the
//!   previous event's "after" (tolerance `max(110 股, 1e-6 × 值)`, the cat5/7/8
//!   evidence report §4), the entity's share history from that break on is
//!   `unknown`. Nothing is extrapolated across the break.
//! - **G2 end reconciliation**: when the latest shares differ from a current
//!   reference (exchange current value / `base.dbf`) by more than 1%, the
//!   float-share history is flagged `low_confidence`. A research read may drop
//!   such an entity ([`MarketCapOptions::exclude_low_confidence`]).
//!
//! G2 needs a current-share reference, and schema 24 has no snapshot column
//! for one (`security_master::CurrentShares` is parsed but not stored). The
//! gate is therefore a pure function here, fed by the caller; the stored
//! path reports `not_available` until a reference lands. That is a real gap,
//! not a default.
//!
//! The output value is an integer in **0.01 元 (分) × 股 = 分**; the price is
//! converted from the series' `price_scale` with HALF_UP only when the series
//! is finer than 分 (the raw daily series is 分, `price_scale=2`).

use chrono::NaiveDate;
use rusqlite::Connection;

use super::Error;
use super::temporal::{KnownAtCoverage, Temporal, TemporalReport};
use crate::market_data::{Adjustment, Period};
use crate::security_master::CapitalEvent;
use crate::sqlite_market::{security, series};

/// `derived_dataset_versions.dataset_kind` name of the share facts.
pub const DATASET_CAPITAL_EVENTS: &str = "capital_events";
/// Derivation label (§3.2: derived data carries one, and it is not part of any
/// `version_id`).
pub const ALGORITHM_VERSION: &str = "market-cap-v1";

/// Which share count the 市值 multiplies (§5.3). The name travels with the
/// number because the same close price times three different denominators
/// would otherwise be indistinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShareScope {
    /// 流通 A 股 (not B/H).
    FloatA,
    /// 公司总股本 (A+B+H) — the only total the source provides.
    CompanyTotal,
    /// A 股总股本 — `unknown` until a source is admitted.
    AShareTotal,
}

impl ShareScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FloatA => "float_a",
            Self::CompanyTotal => "company_total",
            Self::AShareTotal => "a_share_total",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "float_a" => Self::FloatA,
            "company_total" => Self::CompanyTotal,
            "a_share_total" => Self::AShareTotal,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketCapState {
    Known,
    Unknown,
}

/// Why a 市值 is `unknown`. Each variant is a distinct cause; none of them is
/// "assume the previous value".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MarketCapReason {
    /// `a_share_total` before a source admits historical A-share totals.
    AShareTotalNotAdmitted,
    /// No admitted capital event on or before `as_of`.
    NoShareEvent,
    /// D-50 G1: the event chain broke; no value from the break on.
    ChainBreak,
    /// Every event on or before `as_of` was gated as `suspicious`.
    SuspiciousOnly,
    /// D-50 G2: the float-share history failed the end reconciliation and the
    /// caller asked to exclude low-confidence entities.
    LowConfidence,
    /// The entity has no 不复权 daily series.
    NoSeries,
    /// The 不复权 series has no bar on `as_of`.
    NoBar,
}

impl MarketCapReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AShareTotalNotAdmitted => "a_share_total_not_admitted",
            Self::NoShareEvent => "no_share_event",
            Self::ChainBreak => "chain_break",
            Self::SuspiciousOnly => "suspicious_only",
            Self::LowConfidence => "low_confidence",
            Self::NoSeries => "no_series",
            Self::NoBar => "no_bar",
        }
    }
}

/// Why a share count is unavailable (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShareReason {
    NoEvent,
    ChainBreak,
    SuspiciousOnly,
}

impl ShareReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoEvent => "no_event",
            Self::ChainBreak => "chain_break",
            Self::SuspiciousOnly => "suspicious_only",
        }
    }
}

/// D-50 G2 result for one share column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciliation {
    /// No current-share reference is available (schema 24 has no snapshot).
    NotAvailable,
    WithinTolerance,
    LowConfidence {
        reference: i64,
        latest: i64,
        /// `|latest − reference|` in basis points of the reference.
        diff_bp: i64,
    },
}

impl Reconciliation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAvailable => "not_available",
            Self::WithinTolerance => "within_tolerance",
            Self::LowConfidence { .. } => "low_confidence",
        }
    }
}

/// Knobs of the share state. `allow_suspicious` is the explicit relaxation of
/// §5.2's fail-closed default; the two references feed D-50 G2.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShareOptions {
    pub allow_suspicious: bool,
    pub current_float_reference: Option<i64>,
    pub current_total_reference: Option<i64>,
}

/// The share state on one day (§5.2 + G1/G2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareState {
    pub float_shares: Option<i64>,
    pub total_shares: Option<i64>,
    /// The event the two values come from (both are the same event, §5.2).
    pub event_day: Option<NaiveDate>,
    pub event_category: Option<u8>,
    pub reason: Option<ShareReason>,
    /// The first G1 break on or before `as_of`; `None` when the chain holds.
    pub chain_break_day: Option<NaiveDate>,
    /// Suspicious events skipped on or before `as_of` (fail-closed default).
    pub skipped_suspicious: usize,
    pub float_reconciliation: Reconciliation,
    pub total_reconciliation: Reconciliation,
    /// `true` when the float column failed G2 (R-3: the float history is what
    /// the gate flags). [`MarketCap::low_confidence`] is the same question for
    /// the column the requested scope uses.
    pub low_confidence: bool,
}

/// The 市值 answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketCap {
    pub entity: String,
    pub as_of: NaiveDate,
    pub known_at: Option<NaiveDate>,
    pub scope: ShareScope,
    pub state: MarketCapState,
    pub reason: Option<MarketCapReason>,
    /// `price_cents × shares`, in 分. `None` when the state is unknown.
    pub value_units: Option<i128>,
    /// The 不复权 close, in 分.
    pub price_cents: Option<i64>,
    pub price_day: Option<NaiveDate>,
    pub price_scale: Option<u32>,
    pub shares: Option<i64>,
    pub share_day: Option<NaiveDate>,
    pub share_category: Option<u8>,
    pub share_reason: Option<ShareReason>,
    pub chain_break_day: Option<NaiveDate>,
    pub float_reconciliation: Reconciliation,
    pub total_reconciliation: Reconciliation,
    /// The G2 verdict for the column `scope` uses; `false` for `a_share_total`,
    /// which has no value to reconcile.
    pub low_confidence: bool,
    pub algorithm_version: &'static str,
    pub known_at_coverage: KnownAtCoverage,
    pub temporal: TemporalReport,
}

/// Knobs of the 市值 answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarketCapOptions {
    pub shares: ShareOptions,
    /// Turn a G2 `low_confidence` float history into `unknown` (R-3: "研究读取
    /// 时可以选择剔除"). Off by default: the flag is reported either way.
    pub exclude_low_confidence: bool,
}

/// The share state function (§5.2). Pure: same events, same day, same answer.
///
/// `events` must be the entity's admitted events in `ordinal` order (the
/// source's decode order), which is what the storage adapter returns.
pub fn shares_at(events: &[CapitalEvent], as_of: NaiveDate, options: &ShareOptions) -> ShareState {
    let latest = events
        .iter()
        .filter(|event| options.allow_suspicious || !event.suspicious)
        .max_by_key(|event| (event.effective_day, event.ordinal));
    let float_reconciliation = reconcile(
        latest.map(|event| event.float_after),
        options.current_float_reference,
    );
    let total_reconciliation = reconcile(
        latest.map(|event| event.total_after),
        options.current_total_reference,
    );
    let low_confidence = matches!(float_reconciliation, Reconciliation::LowConfidence { .. });

    let mut state = ShareState {
        float_shares: None,
        total_shares: None,
        event_day: None,
        event_category: None,
        reason: None,
        chain_break_day: None,
        skipped_suspicious: 0,
        float_reconciliation,
        total_reconciliation,
        low_confidence,
    };

    // G1: the chain is checked over the source order, not only over the events
    // on or before `as_of` — a break is a property of the stored chain.
    let mut break_day: Option<NaiveDate> = None;
    for pair in events.windows(2) {
        let [previous, current] = pair else {
            continue;
        };
        let holds = within_chain_tolerance(previous.float_after, current.float_before)
            && within_chain_tolerance(previous.total_after, current.total_before);
        if !holds && break_day.is_none() {
            break_day = Some(current.effective_day);
        }
    }
    if let Some(day) = break_day
        && day <= as_of
    {
        state.reason = Some(ShareReason::ChainBreak);
        state.chain_break_day = Some(day);
        return state;
    }

    let mut chosen: Option<&CapitalEvent> = None;
    let mut skipped = 0usize;
    for event in events.iter().filter(|event| event.effective_day <= as_of) {
        if event.suspicious && !options.allow_suspicious {
            skipped += 1;
            continue;
        }
        chosen = Some(event);
    }
    state.skipped_suspicious = skipped;
    match chosen {
        Some(event) => {
            state.float_shares = Some(event.float_after);
            state.total_shares = Some(event.total_after);
            state.event_day = Some(event.effective_day);
            state.event_category = Some(event.category);
        }
        None if skipped > 0 => state.reason = Some(ShareReason::SuspiciousOnly),
        None => state.reason = Some(ShareReason::NoEvent),
    }
    state
}

/// `market_cap(entity, as_of, scope)` (§5.3, AC-M2-6).
pub fn market_cap(
    c: &Connection,
    entity: &str,
    temporal: Temporal,
    scope: ShareScope,
    options: &MarketCapOptions,
) -> Result<MarketCap, Error> {
    crate::compact_market::canonical_entity(entity)
        .map_err(|_| Error::Invalid(format!("non-canonical entity {entity}")))?;
    let mut report = TemporalReport::new(temporal);
    let events = security::read_capital_events(c, entity).map_err(Error::from)?;
    let admitted: Vec<CapitalEvent> = events
        .into_iter()
        .filter(|event| report.observe(DATASET_CAPITAL_EVENTS, event.known_at))
        .collect();
    let shares = shares_at(&admitted, temporal.as_of, &options.shares);
    let known_at_coverage = report.coverage(DATASET_CAPITAL_EVENTS);

    let mut answer = MarketCap {
        entity: entity.into(),
        as_of: temporal.as_of,
        known_at: temporal.known_at,
        scope,
        state: MarketCapState::Unknown,
        reason: None,
        value_units: None,
        price_cents: None,
        price_day: None,
        price_scale: None,
        shares: None,
        share_day: shares.event_day,
        share_category: shares.event_category,
        share_reason: shares.reason,
        chain_break_day: shares.chain_break_day,
        float_reconciliation: shares.float_reconciliation,
        total_reconciliation: shares.total_reconciliation,
        low_confidence: false,
        algorithm_version: ALGORITHM_VERSION,
        known_at_coverage,
        temporal: report,
    };

    if scope == ShareScope::AShareTotal {
        // A-4: historical A-share totals are NOT_AVAILABLE. Borrowing the
        // company total would overstate an A+H issuer by the H shares.
        answer.reason = Some(MarketCapReason::AShareTotalNotAdmitted);
        return Ok(answer);
    }

    let (share_value, reconciliation) = match scope {
        ShareScope::FloatA => (shares.float_shares, shares.float_reconciliation),
        ShareScope::CompanyTotal => (shares.total_shares, shares.total_reconciliation),
        ShareScope::AShareTotal => unreachable!("handled above"),
    };
    // The D-50 G2 gate is per column: the float history failing the
    // reconciliation says nothing about the company total (R-3 flags the float
    // column, the one the source's own current value can be compared with).
    answer.low_confidence = matches!(reconciliation, Reconciliation::LowConfidence { .. });
    let Some(shares_value) = share_value else {
        answer.reason = Some(match shares.reason {
            Some(ShareReason::ChainBreak) => MarketCapReason::ChainBreak,
            Some(ShareReason::SuspiciousOnly) => MarketCapReason::SuspiciousOnly,
            Some(ShareReason::NoEvent) | None => MarketCapReason::NoShareEvent,
        });
        return Ok(answer);
    };
    answer.shares = Some(shares_value);

    if options.exclude_low_confidence && answer.low_confidence {
        answer.reason = Some(MarketCapReason::LowConfidence);
        return Ok(answer);
    }

    if series::read_series_adjusted(c, entity, Period::Day, Adjustment::Raw)
        .map_err(Error::from)?
        .is_none()
    {
        answer.reason = Some(MarketCapReason::NoSeries);
        return Ok(answer);
    }
    let Some((close_units, price_scale)) =
        series::raw_daily_close_at(c, entity, temporal.as_of).map_err(Error::from)?
    else {
        answer.reason = Some(MarketCapReason::NoBar);
        return Ok(answer);
    };
    let price_cents = price_cents(close_units, price_scale)?;
    answer.price_cents = Some(price_cents);
    answer.price_day = Some(temporal.as_of);
    answer.price_scale = Some(price_scale);
    answer.value_units = Some(i128::from(price_cents) * i128::from(shares_value));
    answer.state = MarketCapState::Known;
    Ok(answer)
}

/// D-50 G1: `|previous.after − current.before| ≤ max(110 股, 1e-6 × 值)`
/// (the cat5/7/8 evidence report §4; 0.011 万股 = 110 股).
fn within_chain_tolerance(previous_after: i64, current_before: i64) -> bool {
    let diff = (i128::from(previous_after) - i128::from(current_before)).abs();
    let scale = i128::from(previous_after.max(current_before));
    diff <= (scale / 1_000_000).max(110)
}

/// D-50 G2: more than 1% away from the current reference is low confidence.
fn reconcile(latest: Option<i64>, reference: Option<i64>) -> Reconciliation {
    let (Some(latest), Some(reference)) = (latest, reference) else {
        return Reconciliation::NotAvailable;
    };
    if reference <= 0 {
        return Reconciliation::NotAvailable;
    }
    let diff = (i128::from(latest) - i128::from(reference)).abs();
    let diff_bp = i64::try_from(diff * 10_000 / i128::from(reference)).unwrap_or(i64::MAX);
    if diff_bp > 100 {
        Reconciliation::LowConfidence {
            reference,
            latest,
            diff_bp,
        }
    } else {
        Reconciliation::WithinTolerance
    }
}

/// `series_bars.close_units` → 分. The raw daily series is already 分
/// (`price_scale=2`); a finer series is rounded HALF_UP once, here, and a
/// coarser one is scaled exactly.
fn price_cents(units: i64, price_scale: u32) -> Result<i64, Error> {
    if price_scale > 6 {
        return Err(Error::Integrity(format!(
            "price_scale {price_scale} is above the DDL ceiling"
        )));
    }
    if price_scale <= 2 {
        let factor = 10i64
            .checked_pow(2 - price_scale)
            .ok_or_else(|| Error::Integrity("price scale factor overflow".into()))?;
        units
            .checked_mul(factor)
            .ok_or_else(|| Error::Integrity("price does not fit 分".into()))
    } else {
        let factor = 10i64
            .checked_pow(price_scale - 2)
            .ok_or_else(|| Error::Integrity("price scale factor overflow".into()))?;
        Ok((units + factor / 2) / factor)
    }
}
