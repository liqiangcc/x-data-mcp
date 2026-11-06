//! `tradability(entity, as_of)` — could this security be traded that day?
//! (issue-391 §7.4, AC-M2-8).
//!
//! The decision order is fixed and fail-closed:
//!
//! ```text
//! 1. not in universe(as_of)          → false  (not_listed / delisted)
//! 2. as_of is not a trading day      → false  (non_trading_day)
//! 3. a confirmed suspension covers T → false  (suspended)
//! 4. an unresolved_gap covers T      → unknown (data_gap)
//! 5. no bar and no gap explains it   → unknown (no_bar)
//! 6. a bar exists                    → true
//! ```
//!
//! Only "a bar exists" is positive evidence. `false` needs a named event, and
//! everything else is `unknown` — in particular a run of missing bars is a
//! **data gap**, never a suspension (§7.1, BR-M2-8). That is the whole point of
//! AC-M2-8: `sz000631` between 2006-04-28 and 2009-06-04 has no bars and no
//! announcement, so the answer is `unknown` + `data_gap`, not `suspended`.
//!
//! Missing evidence about the *question* is `unknown` too, never a negative
//! (issue-433 G1–G4): a main-table row hidden by the `known_at` filter
//! (`known_at_unavailable`), a date after the last calendar day
//! (`beyond_calendar`), an ETF or index (`not_a_share`) and a code the main
//! table does not contain (`not_in_master`).
//!
//! PR-4 reads `suspension_intervals`; deriving the `unresolved_gap` rows and
//! writing `confirmed` rows is PR-6 (§16). A missing calendar is `unknown`
//! rather than `false`: the calendar is derived data, and "we never derived it"
//! is not evidence that the day was not a trading day.

use chrono::NaiveDate;
use rusqlite::Connection;

use super::Error;
use super::context::ResearchContext;
use super::member_state::{MemberState, Membership, NotMemberReason};
use super::temporal::{Temporal, TemporalReport};
use super::universe::{UniverseRequest, member_state_report_in};
use crate::security_master::{SuspensionCertainty, SuspensionInterval, SuspensionReason};
use crate::sqlite_market::series;
use crate::trading_calendar::DayStatus;

/// The algorithm label of this derivation (BR-M2-10). It changes whenever a
/// verdict for the same inputs could change.
pub const ALGORITHM_VERSION: &str = "tradability-v1";

/// `derived_dataset_versions.dataset_kind` name of the suspension facts.
pub const DATASET_SUSPENSIONS: &str = "suspensions";

/// The answer. `Tradable` is the only affirmative one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tradability {
    Tradable,
    NotTradable(TradabilityReason),
    Unknown(TradabilityReason),
}

impl Tradability {
    pub fn reason(self) -> TradabilityReason {
        match self {
            Self::Tradable => TradabilityReason::Tradable,
            Self::NotTradable(reason) | Self::Unknown(reason) => reason,
        }
    }

    pub fn is_tradable(self) -> bool {
        matches!(self, Self::Tradable)
    }
}

/// The machine-readable reason codes of §7.4 (plus `calendar_missing`, the
/// fail-closed answer when the derived calendar does not exist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TradabilityReason {
    Tradable,
    NotListed,
    Delisted,
    NonTradingDay,
    Suspended,
    DataGap,
    NoBar,
    CalendarMissing,
    /// The main-table row exists but the `known_at` filter hides it (G1).
    KnownAtUnavailable,
    /// `as_of` is after the last calendar day; the calendar does not predict
    /// (G2).
    BeyondCalendar,
    /// An ETF, index or other non-A-share instrument (G3).
    NotAShare,
    /// The code is not in the main table at all (G4).
    NotInMaster,
}

impl TradabilityReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tradable => "tradable",
            Self::NotListed => "not_listed",
            Self::Delisted => "delisted",
            Self::NonTradingDay => "non_trading_day",
            Self::Suspended => "suspended",
            Self::DataGap => "data_gap",
            Self::NoBar => "no_bar",
            Self::CalendarMissing => "calendar_missing",
            Self::KnownAtUnavailable => "known_at_unavailable",
            Self::BeyondCalendar => "beyond_calendar",
            Self::NotAShare => "not_a_share",
            Self::NotInMaster => "not_in_master",
        }
    }
}

/// The interval row that decided the answer, so a caller can re-check it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspensionRef {
    pub certainty: SuspensionCertainty,
    pub start_day: NaiveDate,
    pub end_day: NaiveDate,
    pub missing_days: i64,
    pub reason: SuspensionReason,
    pub known_at: Option<NaiveDate>,
}

impl From<&SuspensionInterval> for SuspensionRef {
    fn from(interval: &SuspensionInterval) -> Self {
        Self {
            certainty: interval.certainty,
            start_day: interval.start_day,
            end_day: interval.end_day,
            missing_days: interval.missing_days,
            reason: interval.reason,
            known_at: interval.known_at,
        }
    }
}

/// The full answer with its evidence and the `temporal` block (§12.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradabilityReport {
    pub entity: String,
    pub as_of: NaiveDate,
    pub known_at: Option<NaiveDate>,
    pub state: Tradability,
    /// The interval this verdict belongs to; `None` when the code is not in
    /// `security_master` at all.
    pub member: Option<MemberState>,
    pub suspension: Option<SuspensionRef>,
    /// Whether a 不复权 bar exists on `as_of`.
    pub has_bar: bool,
    /// [`ALGORITHM_VERSION`]; with the input dataset versions it reproduces
    /// the answer.
    pub algorithm_version: &'static str,
    pub temporal: TemporalReport,
}

impl TradabilityReport {
    pub fn reason_code(&self) -> &'static str {
        self.state.reason().as_str()
    }
}

/// `tradability(entity, as_of)` (§7.4).
///
/// `strict` is deliberately not a parameter: strictness is a research-sample
/// choice about ambiguous membership, not a property of whether the exchange
/// would have accepted an order. The membership test here is the default one.
pub fn tradability(
    c: &Connection,
    entity: &str,
    temporal: Temporal,
) -> Result<TradabilityReport, Error> {
    tradability_in(&ResearchContext::new(c), entity, temporal)
}

/// [`tradability`] over a shared [`ResearchContext`]: the same answer for the
/// same entity, with the whole-table reads done once for all entities.
pub fn tradability_in(
    ctx: &ResearchContext<'_>,
    entity: &str,
    temporal: Temporal,
) -> Result<TradabilityReport, Error> {
    let c = ctx.connection();
    let request = UniverseRequest {
        temporal,
        strict: false,
    };
    let (member, mut report) = member_state_report_in(ctx, entity, &request)?;
    let Some(member) = member else {
        return Ok(TradabilityReport {
            entity: entity.into(),
            as_of: temporal.as_of,
            known_at: temporal.known_at,
            // The main table is not a closed world: a new listing is absent
            // until the next derivation, so absence is not "not listed".
            state: Tradability::Unknown(TradabilityReason::NotInMaster),
            member: None,
            suspension: None,
            has_bar: false,
            algorithm_version: ALGORITHM_VERSION,
            temporal: report,
        });
    };
    let not_member = match member.membership {
        Membership::In(_) => None,
        Membership::Out(NotMemberReason::AfterDelisting) => {
            Some(Tradability::NotTradable(TradabilityReason::Delisted))
        }
        // The row exists but this information set cannot see it. That is a
        // missing input, not an unlisted security.
        Membership::Out(NotMemberReason::ExcludedByKnownAt) => {
            Some(Tradability::Unknown(TradabilityReason::KnownAtUnavailable))
        }
        // This derivation covers A shares only; an ETF or index is listed but
        // outside its scope.
        Membership::Out(NotMemberReason::NotAShare) => {
            Some(Tradability::Unknown(TradabilityReason::NotAShare))
        }
        // A code whose interval ended for another reason (a migration, a code
        // reuse boundary) is not listed *as this code* on that day; that is
        // `not_listed`, not `delisted`.
        Membership::Out(_) => Some(Tradability::NotTradable(TradabilityReason::NotListed)),
    };
    if let Some(state) = not_member {
        return Ok(TradabilityReport {
            entity: entity.into(),
            as_of: temporal.as_of,
            known_at: temporal.known_at,
            state,
            member: Some(member),
            suspension: None,
            has_bar: false,
            algorithm_version: ALGORITHM_VERSION,
            temporal: report,
        });
    }

    // Step 2: the calendar. "No calendar" is unknown, not a non-trading day.
    let Some(calendar) = ctx.calendar()? else {
        return Ok(finish(
            entity,
            temporal,
            member,
            Tradability::Unknown(TradabilityReason::CalendarMissing),
            None,
            false,
            report,
        ));
    };
    match calendar.day_status(temporal.as_of) {
        Some(DayStatus::InCalendar) => {}
        // Provably a non-trading day: inside the covered span, absent.
        Some(DayStatus::NonTrading) => {
            return Ok(finish(
                entity,
                temporal,
                member,
                Tradability::NotTradable(TradabilityReason::NonTradingDay),
                None,
                false,
                report,
            ));
        }
        // After the last input bar the calendar says nothing (G2).
        Some(DayStatus::BeyondCalendar) => {
            return Ok(finish(
                entity,
                temporal,
                member,
                Tradability::Unknown(TradabilityReason::BeyondCalendar),
                None,
                false,
                report,
            ));
        }
        // An empty calendar is never stored; treat it as no calendar.
        None => {
            return Ok(finish(
                entity,
                temporal,
                member,
                Tradability::Unknown(TradabilityReason::CalendarMissing),
                None,
                false,
                report,
            ));
        }
    }

    // Steps 3/4: the stored suspension rows. A `confirmed` row is an event; an
    // `unresolved_gap` row is derived and can only produce `unknown`.
    let mut confirmed: Option<&SuspensionInterval> = None;
    let mut gap: Option<&SuspensionInterval> = None;
    for interval in ctx
        .suspensions()?
        .of(entity)
        .filter(|row| row.covers(temporal.as_of))
    {
        if !report.observe(DATASET_SUSPENSIONS, interval.known_at) {
            continue;
        }
        match interval.certainty {
            SuspensionCertainty::Confirmed => {
                if confirmed.is_some() {
                    return Err(Error::Integrity(format!(
                        "confirmed suspension intervals overlap for {entity} on {}",
                        temporal.as_of
                    )));
                }
                confirmed = Some(interval);
            }
            SuspensionCertainty::UnresolvedGap => gap = Some(interval),
        }
    }
    if let Some(interval) = confirmed {
        return Ok(finish(
            entity,
            temporal,
            member,
            Tradability::NotTradable(TradabilityReason::Suspended),
            Some(SuspensionRef::from(interval)),
            false,
            report,
        ));
    }
    if let Some(interval) = gap {
        return Ok(finish(
            entity,
            temporal,
            member,
            Tradability::Unknown(TradabilityReason::DataGap),
            Some(SuspensionRef::from(interval)),
            false,
            report,
        ));
    }

    // Steps 5/6: the only affirmative evidence is a bar.
    let has_bar = series::raw_daily_close_at(c, entity, temporal.as_of)
        .map_err(Error::from)?
        .is_some();
    let state = if has_bar {
        Tradability::Tradable
    } else {
        Tradability::Unknown(TradabilityReason::NoBar)
    };
    Ok(finish(
        entity, temporal, member, state, None, has_bar, report,
    ))
}

fn finish(
    entity: &str,
    temporal: Temporal,
    member: MemberState,
    state: Tradability,
    suspension: Option<SuspensionRef>,
    has_bar: bool,
    temporal_report: TemporalReport,
) -> TradabilityReport {
    TradabilityReport {
        entity: entity.into(),
        as_of: temporal.as_of,
        known_at: temporal.known_at,
        state,
        member: Some(member),
        suspension,
        has_bar,
        algorithm_version: ALGORITHM_VERSION,
        temporal: temporal_report,
    }
}
