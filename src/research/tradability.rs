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
//! PR-4 reads `suspension_intervals`; deriving the `unresolved_gap` rows and
//! writing `confirmed` rows is PR-6 (§16). A missing calendar is `unknown`
//! rather than `false`: the calendar is derived data, and "we never derived it"
//! is not evidence that the day was not a trading day.

use chrono::NaiveDate;
use rusqlite::Connection;

use super::Error;
use super::member_state::{MemberState, Membership, NotMemberReason};
use super::temporal::{Temporal, TemporalReport};
use super::universe::{UniverseRequest, member_state_report};
use crate::security_master::{SuspensionCertainty, SuspensionInterval, SuspensionReason};
use crate::sqlite_market::{calendar, security, series};

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
    let request = UniverseRequest {
        temporal,
        strict: false,
    };
    let (member, mut report) = member_state_report(c, entity, &request)?;
    let Some(member) = member else {
        return Ok(TradabilityReport {
            entity: entity.into(),
            as_of: temporal.as_of,
            known_at: temporal.known_at,
            state: Tradability::NotTradable(TradabilityReason::NotListed),
            member: None,
            suspension: None,
            has_bar: false,
            temporal: report,
        });
    };
    let not_member = match member.membership {
        Membership::In(_) => None,
        Membership::Out(NotMemberReason::AfterDelisting) => {
            Some(Tradability::NotTradable(TradabilityReason::Delisted))
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
            temporal: report,
        });
    }

    // Step 2: the calendar. "No calendar" is unknown, not a non-trading day.
    let Some(calendar) = calendar::read_calendar(c).map_err(Error::from)? else {
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
    if !calendar.contains(temporal.as_of) {
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

    // Steps 3/4: the stored suspension rows. A `confirmed` row is an event; an
    // `unresolved_gap` row is derived and can only produce `unknown`.
    let intervals = security::read_suspension_intervals(c).map_err(Error::from)?;
    let mut confirmed: Option<&SuspensionInterval> = None;
    let mut gap: Option<&SuspensionInterval> = None;
    for interval in intervals
        .iter()
        .filter(|row| row.entity() == entity && row.covers(temporal.as_of))
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
        temporal: temporal_report,
    }
}
