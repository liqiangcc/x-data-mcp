//! `price_limits(entity, as_of)` — the `limit-v1` derivation (issue-391 §8,
//! AC-M2-9/AC-M2-10).
//!
//! Layer position (separation points 2/4/6):
//!
//! - The **rule rows** come from `price_limit_rules`/`min_tick_rules` through
//!   the storage adapter. No band, side, rounding or tick constant appears in
//!   this file; changing a rule is a data change (AC-M2-10).
//! - The **inputs** are parsed facts: the 不复权 last bar before `as_of`, the
//!   listing date and the risk status from the read model. The derived answer
//!   is never stored; it carries `algorithm_version='limit-v1'` and the rule's
//!   `basis_ref`.
//! - Every missing input propagates as `unknown` with a distinct reason. A
//!   missing rule is not "10%", a missing ST interval is not "normal", and an
//!   explicit `band_side='none'` is `unbounded` — a *known* answer that must
//!   not be confused with either (§8.5).
//!
//! The integer-domain algorithm (§8.3) is:
//!
//! ```text
//! up   = round(prev × (BP_SCALE + band_bp) / BP_SCALE)
//! down = round(prev × (BP_SCALE − band_bp) / BP_SCALE)
//! ```
//!
//! where `round` is the rule's `rounding` (HALF_UP for the admitted A-share
//! rules, R9) and the result is aligned to the rule's minimum tick. The
//! previous close is the last **bar** before `as_of`, not the previous
//! calendar day, so a suspension uses the pre-suspension close.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use rusqlite::Connection;

use super::Error;
use super::member_state::{Membership, NotMemberReason};
use super::temporal::{DatasetTemporal, Temporal, TemporalReport};
use super::universe::{UniverseRequest, member_state_report};
use crate::compact_market::canonical_entity;
use crate::market_data::{Adjustment, Period};
use crate::security_master::{Exchange, RiskStatus};
use crate::sqlite_market::{calendar, derived, rules, series};
use crate::trading_calendar::TradingCalendar;
use crate::trading_rules::{
    ALGORITHM_VERSION, BandSide, BasisStatus, DATASET_KEY, DATASET_KIND, ListingPhase, MinTickRule,
    PriceLimitRule, Rounding, RuleBoard, RuleBook, RuleLookup, RuleMarket, StatusClass,
};

/// `derived_dataset_versions.dataset_kind` this read model consumes.
pub const DATASET_PRICE_LIMIT_RULES: &str = DATASET_KIND;

/// The basis-point denominator. This is a scale, not an amplitude: every band
/// comes from the rule row's `band_bp`.
const BP_SCALE: i128 = 10_000;

/// The board a code belongs to. M2's universe is A shares only; a code that
/// does not fall into a known A-share segment is `board_unsupported` rather
/// than being guessed into `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Board {
    Main,
    Gem,
    Star,
}

impl Board {
    /// Segment classification by exchange + code prefix. BSE/B shares are out
    /// of scope (§1.2), so an unknown segment is not silently mapped to main.
    pub fn of_entity(entity: &str) -> Option<Self> {
        let (_exchange, code) = entity.split_once('.')?;
        let prefix = code.get(..3)?;
        Some(match prefix {
            "600" | "601" | "603" | "605" => Self::Main,
            "688" | "689" => Self::Star,
            "000" | "001" | "002" | "003" => Self::Main,
            "300" | "301" => Self::Gem,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Gem => "gem",
            Self::Star => "star",
        }
    }

    fn rule_board(self) -> RuleBoard {
        match self {
            Self::Main => RuleBoard::Main,
            Self::Gem => RuleBoard::Gem,
            Self::Star => RuleBoard::Star,
        }
    }
}

fn rule_market(exchange: Exchange) -> RuleMarket {
    match exchange {
        Exchange::Sh => RuleMarket::Sh,
        Exchange::Sz => RuleMarket::Sz,
        Exchange::Bj => RuleMarket::Bj,
    }
}

/// The answer's state. `Unbounded` is the rule saying "no price limit"; it is
/// not an unknown and not a huge number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceLimitState {
    Known,
    Unbounded,
    Unknown,
}

impl PriceLimitState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Unbounded => "unbounded",
            Self::Unknown => "unknown",
        }
    }
}

/// Why a derivation did not produce both limit prices (§8.5). The codes are
/// the design's `rule_missing`/`rule_pending`/`rule_ambiguous`/`st_unknown`/
/// `listing_unknown`/`no_prev_close` plus the storage-level failures the
/// implementation must also distinguish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PriceLimitReason {
    RuleMissing,
    RulePending,
    RuleAmbiguous,
    MinTickMissing,
    MinTickPending,
    MinTickAmbiguous,
    StUnknown,
    ListingUnknown,
    ListingPhaseUnknown,
    CalendarMissing,
    NonTradingDay,
    NotListed,
    NotAShare,
    BoardUnsupported,
    NoSeries,
    NoPrevClose,
    InvalidRule,
}

impl PriceLimitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RuleMissing => "rule_missing",
            Self::RulePending => "rule_pending",
            Self::RuleAmbiguous => "rule_ambiguous",
            Self::MinTickMissing => "min_tick_missing",
            Self::MinTickPending => "min_tick_pending",
            Self::MinTickAmbiguous => "min_tick_ambiguous",
            Self::StUnknown => "st_unknown",
            Self::ListingUnknown => "listing_unknown",
            Self::ListingPhaseUnknown => "listing_phase_unknown",
            Self::CalendarMissing => "calendar_missing",
            Self::NonTradingDay => "non_trading_day",
            Self::NotListed => "not_listed",
            Self::NotAShare => "not_a_share",
            Self::BoardUnsupported => "board_unsupported",
            Self::NoSeries => "no_series",
            Self::NoPrevClose => "no_prev_close",
            Self::InvalidRule => "invalid_rule",
        }
    }
}

/// One diagnostic candidate when the status class (usually ST) is unknown.
/// The values come from the rule table, never from a fallback constant; the
/// date correction is therefore automatic (R2: 5% before 2026-07-06, 10%
/// after, because that is what the rows say).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleCandidate {
    pub status_class: StatusClass,
    pub band_bp: Option<i64>,
    pub band_side: BandSide,
    pub rounding: Rounding,
    pub rule_id: String,
    pub basis_ref: String,
}

/// The full answer with its evidence and the `temporal` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceLimitReport {
    pub entity: String,
    pub as_of: NaiveDate,
    pub known_at: Option<NaiveDate>,
    pub state: PriceLimitState,
    pub reason: Option<PriceLimitReason>,
    pub board: Option<Board>,
    pub listing_phase: Option<ListingPhase>,
    pub status_class: Option<StatusClass>,
    pub limit_up_units: Option<i64>,
    pub limit_down_units: Option<i64>,
    /// The last 不复权 bar before `as_of`, in the series' price scale.
    pub prev_close_units: Option<i64>,
    pub prev_close_day: Option<NaiveDate>,
    pub price_scale: Option<u32>,
    pub rule_id: Option<String>,
    pub basis_ref: Option<String>,
    pub basis_status: Option<BasisStatus>,
    pub band_bp: Option<i64>,
    pub band_side: Option<BandSide>,
    pub rounding: Option<Rounding>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    pub min_tick_rule_id: Option<String>,
    pub min_tick_units: Option<i64>,
    pub tick_price_scale: Option<u32>,
    pub candidates: Vec<RuleCandidate>,
    pub algorithm_version: &'static str,
    pub rules_version: Option<String>,
    pub temporal: TemporalReport,
}

impl PriceLimitReport {
    pub fn reason_code(&self) -> &'static str {
        match self.state {
            PriceLimitState::Known => "known",
            PriceLimitState::Unbounded => "unbounded",
            PriceLimitState::Unknown => self.reason.map_or("unknown", PriceLimitReason::as_str),
        }
    }

    fn unknown(
        entity: &str,
        temporal: Temporal,
        report: TemporalReport,
        reason: PriceLimitReason,
    ) -> Self {
        Self {
            entity: entity.into(),
            as_of: temporal.as_of,
            known_at: temporal.known_at,
            state: PriceLimitState::Unknown,
            reason: Some(reason),
            board: None,
            listing_phase: None,
            status_class: None,
            limit_up_units: None,
            limit_down_units: None,
            prev_close_units: None,
            prev_close_day: None,
            price_scale: None,
            rule_id: None,
            basis_ref: None,
            basis_status: None,
            band_bp: None,
            band_side: None,
            rounding: None,
            effective_from: None,
            effective_to: None,
            min_tick_rule_id: None,
            min_tick_units: None,
            tick_price_scale: None,
            candidates: Vec::new(),
            algorithm_version: ALGORITHM_VERSION,
            rules_version: None,
            temporal: report,
        }
    }
}

/// `price_limits(entity, as_of)` (§8.3).
pub fn price_limit(
    c: &Connection,
    entity: &str,
    temporal: Temporal,
) -> Result<PriceLimitReport, Error> {
    canonical_entity(entity)
        .map_err(|_| Error::Invalid(format!("non-canonical entity {entity}")))?;
    let request = UniverseRequest {
        temporal,
        strict: false,
    };
    let (member, mut temporal_report) = member_state_report(c, entity, &request)?;

    // Rules are loaded once per query so the price row and the tick row come
    // from the same dataset version. The dataset has no `known_at` column, so
    // it is reported with coverage `none` instead of pretending it was bounded
    // by the information-set filter.
    let book = rules::read_trading_rules(c)?;
    let rule_rows = book.price_limits.len() + book.min_ticks.len();
    temporal_report.datasets.insert(
        DATASET_PRICE_LIMIT_RULES,
        DatasetTemporal {
            rows: rule_rows,
            rows_with_known_at: 0,
            unknown_known_at_rows: rule_rows,
            excluded_known_at_rows: 0,
        },
    );
    let rules_version =
        derived::read_version(c, DATASET_KIND, DATASET_KEY)?.map(|row| row.version_id);

    let mut answer = PriceLimitReport::unknown(
        entity,
        temporal,
        temporal_report,
        PriceLimitReason::NotListed,
    );
    answer.rules_version = rules_version;

    let Some(member) = member else {
        return Ok(answer);
    };
    match member.membership {
        Membership::In(_) => {}
        Membership::Out(NotMemberReason::NotAShare) => {
            answer.reason = Some(PriceLimitReason::NotAShare);
            return Ok(answer);
        }
        Membership::Out(_) => return Ok(answer),
    }

    let Some(board) = Board::of_entity(entity) else {
        answer.reason = Some(PriceLimitReason::BoardUnsupported);
        return Ok(answer);
    };
    answer.board = Some(board);
    let market = rule_market(member.exchange);

    let Some(listing) = member.listing_date else {
        answer.reason = Some(PriceLimitReason::ListingUnknown);
        return Ok(answer);
    };

    let Some(calendar) = calendar::read_calendar(c)? else {
        answer.reason = Some(PriceLimitReason::CalendarMissing);
        return Ok(answer);
    };
    let phase_result = listing_phase(&calendar, listing, temporal.as_of);
    let phase = phase_result.ok();
    answer.listing_phase = phase;

    let status = match status_class(board, member.status) {
        Ok(status) => status,
        Err(reason) => {
            answer.reason = Some(reason);
            if reason == PriceLimitReason::StUnknown {
                answer.candidates =
                    candidates(&book, market, board.rule_board(), phase, temporal.as_of);
            }
            return Ok(answer);
        }
    };
    answer.status_class = Some(status);

    let phase = match phase_result {
        Ok(phase) => phase,
        Err(reason) => {
            answer.reason = Some(reason);
            return Ok(answer);
        }
    };
    answer.listing_phase = Some(phase);

    let rule = match book.price_rule(market, board.rule_board(), status, phase, temporal.as_of) {
        RuleLookup::Found(rule) => rule,
        RuleLookup::Missing => {
            answer.reason = Some(PriceLimitReason::RuleMissing);
            return Ok(answer);
        }
        RuleLookup::Pending => {
            answer.reason = Some(PriceLimitReason::RulePending);
            return Ok(answer);
        }
        RuleLookup::Ambiguous(_) => {
            answer.reason = Some(PriceLimitReason::RuleAmbiguous);
            return Ok(answer);
        }
    };
    answer.rule_id = Some(rule.rule_id.clone());
    answer.basis_ref = Some(rule.basis_ref.clone());
    answer.basis_status = Some(rule.basis_status);
    answer.band_bp = rule.band_bp;
    answer.band_side = Some(rule.band_side);
    answer.rounding = Some(rule.rounding);
    answer.effective_from = Some(rule.effective_from);
    answer.effective_to = rule.effective_to;

    if rule.band_side == BandSide::None {
        answer.state = PriceLimitState::Unbounded;
        answer.reason = None;
        return Ok(answer);
    }

    let tick = match book.min_tick(market, board.rule_board(), temporal.as_of) {
        RuleLookup::Found(tick) => tick,
        RuleLookup::Missing => {
            answer.reason = Some(PriceLimitReason::MinTickMissing);
            return Ok(answer);
        }
        RuleLookup::Pending => {
            answer.reason = Some(PriceLimitReason::MinTickPending);
            return Ok(answer);
        }
        RuleLookup::Ambiguous(_) => {
            answer.reason = Some(PriceLimitReason::MinTickAmbiguous);
            return Ok(answer);
        }
    };
    answer.min_tick_rule_id = Some(tick.rule_id.clone());
    answer.min_tick_units = Some(tick.tick_units);
    answer.tick_price_scale = Some(tick.price_scale);

    if series::read_series_adjusted(c, entity, Period::Day, Adjustment::Raw)?.is_none() {
        answer.reason = Some(PriceLimitReason::NoSeries);
        return Ok(answer);
    }
    let Some((prev_close, price_scale, prev_day)) =
        series::raw_daily_close_before(c, entity, temporal.as_of)?
    else {
        answer.reason = Some(PriceLimitReason::NoPrevClose);
        return Ok(answer);
    };
    answer.prev_close_units = Some(prev_close);
    answer.prev_close_day = Some(prev_day);
    answer.price_scale = Some(price_scale);

    let limits = match compute_limit(prev_close, price_scale, rule, tick) {
        Ok(limits) => limits,
        Err(()) => {
            answer.reason = Some(PriceLimitReason::InvalidRule);
            return Ok(answer);
        }
    };
    answer.limit_up_units = limits.up;
    answer.limit_down_units = limits.down;
    answer.state = PriceLimitState::Known;
    answer.reason = None;
    Ok(answer)
}

struct ComputedLimit {
    up: Option<i64>,
    down: Option<i64>,
}

/// The `limit-v1` integer core. It takes the rule and tick rows verbatim: the
/// only numbers in this function are the basis-point scale and the integer
/// rounding helpers.
fn compute_limit(
    prev_close_units: i64,
    price_scale: u32,
    rule: &PriceLimitRule,
    tick: &MinTickRule,
) -> Result<ComputedLimit, ()> {
    let band_bp = rule.band_bp.ok_or(())?;
    if band_bp < 0 || i128::from(band_bp) > BP_SCALE {
        return Err(());
    }
    if rule.band_side == BandSide::None {
        return Ok(ComputedLimit {
            up: None,
            down: None,
        });
    }
    if price_scale < tick.price_scale || tick.tick_units <= 0 || tick.price_scale > 6 {
        return Err(());
    }
    let factor = 10_i128
        .checked_pow(price_scale - tick.price_scale)
        .ok_or(())?;
    if factor <= 0 {
        return Err(());
    }
    let prev = i128::from(prev_close_units).checked_mul(factor).ok_or(())?;
    let scale = i128::from(tick.tick_units);

    let up = if matches!(rule.band_side, BandSide::Both | BandSide::UpOnly) {
        let target = prev.checked_mul(BP_SCALE + i128::from(band_bp)).ok_or(())?;
        Some(to_bar_units(target, rule.rounding, scale, factor)?)
    } else {
        None
    };
    let down = if rule.band_side == BandSide::Both {
        let target = prev.checked_mul(BP_SCALE - i128::from(band_bp)).ok_or(())?;
        Some(to_bar_units(target, rule.rounding, scale, factor)?)
    } else {
        None
    };
    Ok(ComputedLimit { up, down })
}

/// Round `numerator / BP_SCALE` with `rounding`, align to `tick_units`, then
/// convert the tick-scale result back to the series' price scale.
fn to_bar_units(
    numerator: i128,
    rounding: Rounding,
    tick_units: i128,
    factor: i128,
) -> Result<i64, ()> {
    if numerator < 0 {
        return Err(());
    }
    let rounded = divide_round(numerator, BP_SCALE, rounding);
    let aligned = divide_round(rounded, tick_units, rounding)
        .checked_mul(tick_units)
        .ok_or(())?;
    let bar_units = aligned.checked_div(factor).ok_or(())?;
    if bar_units * factor != aligned {
        return Err(());
    }
    i64::try_from(bar_units).map_err(|_| ())
}

fn divide_round(value: i128, scale: i128, rounding: Rounding) -> i128 {
    debug_assert!(value >= 0);
    debug_assert!(scale > 0);
    match rounding {
        Rounding::HalfUp => (value + scale / 2).div_euclid(scale),
        Rounding::Floor => value.div_euclid(scale),
        Rounding::Ceil => (value + scale - 1).div_euclid(scale),
    }
}

fn status_class(board: Board, status: RiskStatus) -> Result<StatusClass, PriceLimitReason> {
    match status {
        RiskStatus::Normal => Ok(StatusClass::Normal),
        RiskStatus::OtherRiskWarning | RiskStatus::DelistingRiskWarning => {
            if board == Board::Star {
                Ok(StatusClass::StarSt)
            } else {
                Ok(StatusClass::St)
            }
        }
        RiskStatus::DelistingArrangement => Ok(StatusClass::DelistingArrangement),
        RiskStatus::TerminatedOrDelisted => Err(PriceLimitReason::NotListed),
        RiskStatus::Suspended | RiskStatus::MarkerOnlyUnresolved | RiskStatus::Unknown => {
            Err(PriceLimitReason::StUnknown)
        }
    }
}

fn listing_phase(
    calendar: &TradingCalendar,
    listing: NaiveDate,
    as_of: NaiveDate,
) -> Result<ListingPhase, PriceLimitReason> {
    if as_of < listing {
        return Err(PriceLimitReason::NotListed);
    }
    if as_of == listing {
        return Ok(ListingPhase::FirstDay);
    }
    let days = calendar.days();
    let Some(as_of_index) = index_of(days, as_of) else {
        return Err(PriceLimitReason::NonTradingDay);
    };
    match index_of(days, listing) {
        Some(listing_index) => {
            if as_of_index < listing_index {
                Err(PriceLimitReason::NotListed)
            } else if as_of_index - listing_index < 5 {
                Ok(ListingPhase::First5Days)
            } else {
                Ok(ListingPhase::Seasoned)
            }
        }
        // The listing date predates the derived calendar. We still know that
        // `as_of_index + 1` trading days have elapsed since the first calendar
        // day, and the listing is earlier, so five or more means seasoned.
        None if days.first().is_some_and(|first| listing < first.day) => {
            if as_of_index + 1 >= 5 {
                Ok(ListingPhase::Seasoned)
            } else {
                Err(PriceLimitReason::ListingPhaseUnknown)
            }
        }
        None if days.last().is_some_and(|last| listing > last.day) => {
            Err(PriceLimitReason::NotListed)
        }
        None => Err(PriceLimitReason::ListingPhaseUnknown),
    }
}

fn index_of(days: &[crate::trading_calendar::TradingDay], day: NaiveDate) -> Option<usize> {
    days.binary_search_by_key(&day, |row| row.day).ok()
}

/// The diagnostic candidates for `st_unknown`. Each one is a real rule row
/// that applies to the same market/board/phase on that day; the list is empty
/// when the phase itself is unknown, rather than guessing a phase.
fn candidates(
    book: &RuleBook,
    market: RuleMarket,
    board: RuleBoard,
    phase: Option<ListingPhase>,
    as_of: NaiveDate,
) -> Vec<RuleCandidate> {
    let Some(phase) = phase else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for status in [
        StatusClass::Normal,
        StatusClass::St,
        StatusClass::StarSt,
        StatusClass::DelistingArrangement,
    ] {
        if let RuleLookup::Found(rule) = book.price_rule(market, board, status, phase, as_of) {
            let key = (rule.band_side, rule.band_bp, rule.rounding);
            if seen.insert(key) {
                candidates.push(RuleCandidate {
                    status_class: status,
                    band_bp: rule.band_bp,
                    band_side: rule.band_side,
                    rounding: rule.rounding,
                    rule_id: rule.rule_id.clone(),
                    basis_ref: rule.basis_ref.clone(),
                });
            }
        }
    }
    candidates
}

#[cfg(test)]
mod tests;
