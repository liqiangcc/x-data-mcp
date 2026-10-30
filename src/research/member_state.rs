//! Per-member state at `as_of` (issue-391 §11.1, separation point 4).
//!
//! One row of `security_master` describes an interval of a code. This module
//! turns that row into the *answer* a research caller needs: is the code a
//! member of the universe on that day, on what evidence, what is its risk
//! status, and which caveats travel with it.
//!
//! Two rules are load-bearing and must not be relaxed:
//!
//! - **Membership is not tradability.** A suspended security is still a
//!   member (§11.1); removing it would turn "could I buy that day" into a
//!   membership filter and bias a backtest by suspension, not just by
//!   survival.
//! - **No interval is not `normal`.** The risk status is `unknown` unless an
//!   admitted interval covers the day, because no source proves the closed
//!   world §6.3 requires.
//!
//! Selection itself (loading rows, `known_at` filtering, counting exclusions)
//! lives in [`super::universe`]; this module owns the vocabulary and the pure
//! per-row decision so `universe` and `member_state` cannot drift.

use chrono::NaiveDate;

use crate::security_master::{Exchange, IssuerResolution, RiskStatus};

/// Why a member is in the universe on the queried day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MembershipBasis {
    /// The listing date is known and `as_of >= listing_date`.
    ListingDate,
    /// The listing date is unknown, but a bar exists on or before `as_of`, so
    /// the code demonstrably traded (§11.1).
    BarEvidence,
}

impl MembershipBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ListingDate => "listing_date",
            Self::BarEvidence => "bar_evidence",
        }
    }
}

/// A caveat attached to an included member. None of these removes the member;
/// `strict` is the only switch that does, and only for the two ambiguity
/// flags (§11.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemberFlag {
    /// `listing_date` is unknown; membership rests on bar evidence.
    ListingUnknown,
    /// `delisting_date` is unknown; membership is assumed to continue.
    DelistingUnknown,
    /// The code was reused by another issuer and the boundary is not provable
    /// (§4.1, Q3). Included by default, excluded by `strict`.
    AmbiguousIssuer,
    /// A candidate code migration overlaps this interval: the new code's
    /// history is a copy of the old code's bars. Included by default, the new
    /// code is excluded by `strict` (§4.1).
    CandidateMigrationOverlap,
}

impl MemberFlag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ListingUnknown => "listing_unknown",
            Self::DelistingUnknown => "delisting_unknown",
            Self::AmbiguousIssuer => "ambiguous_issuer",
            Self::CandidateMigrationOverlap => "candidate_migration_overlap",
        }
    }
}

/// Why a row is **not** a member on the queried day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NotMemberReason {
    /// `as_of` is before the interval starts.
    BeforeInterval,
    /// `as_of` is at or after the interval ends.
    AfterInterval,
    /// `as_of` is before the (known) listing date.
    BeforeListing,
    /// `as_of` is at or after the (known) delisting date.
    AfterDelisting,
    /// The listing date is unknown and no bar exists yet.
    BeforeFirstBar,
    /// The row's `known_at` is NULL (or later than the filter), so a
    /// point-in-time query cannot use it (§12.2).
    ExcludedByKnownAt,
    /// The row is not an A share (the universe is A-share only in M2).
    NotAShare,
    /// A confirmed code migration decides this code's identity: the old code
    /// is gone from `effective_from`, and the new code does not exist before
    /// it.
    Migrated,
    /// `strict`: the interval's issuer is ambiguous (code reuse).
    StrictAmbiguousIssuer,
    /// `strict`: a candidate migration's overlapping copy of the history.
    StrictCandidateOverlap,
}

impl NotMemberReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeforeInterval => "before_interval",
            Self::AfterInterval => "after_interval",
            Self::BeforeListing => "before_listing",
            Self::AfterDelisting => "after_delisting",
            Self::BeforeFirstBar => "before_first_bar",
            Self::ExcludedByKnownAt => "excluded_by_known_at",
            Self::NotAShare => "not_a_share",
            Self::Migrated => "migrated",
            Self::StrictAmbiguousIssuer => "strict_ambiguous_issuer",
            Self::StrictCandidateOverlap => "strict_candidate_overlap",
        }
    }
}

/// The membership verdict for one interval on one day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    In(MembershipBasis),
    Out(NotMemberReason),
}

impl Membership {
    pub fn is_member(self) -> bool {
        matches!(self, Self::In(_))
    }

    pub fn basis(self) -> Option<MembershipBasis> {
        match self {
            Self::In(basis) => Some(basis),
            Self::Out(_) => None,
        }
    }

    pub fn reason(self) -> Option<NotMemberReason> {
        match self {
            Self::In(_) => None,
            Self::Out(reason) => Some(reason),
        }
    }
}

/// One member's state on the queried day (§11.3 `Member`, plus the interval
/// identity needed to answer a single-code lookup).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberState {
    pub entity: String,
    pub exchange: Exchange,
    pub code: String,
    /// The interval this verdict belongs to. For a single-code lookup on a
    /// reused code it is what separates the old subject from the current one.
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub membership: Membership,
    pub listing_date: Option<NaiveDate>,
    pub delisting_date: Option<NaiveDate>,
    pub issuer_resolution: IssuerResolution,
    /// `unknown` unless an admitted interval covers the day (§6.3).
    pub status: RiskStatus,
    pub flags: Vec<MemberFlag>,
    /// Whether a 不复权 daily series exists at all. `false` is a legal state
    /// (M2 has 335 delisted codes without stored bars, §11.2) and never
    /// removes a member.
    pub has_series: bool,
    /// The interval's 知晓时间 (NULL for every source admitted so far).
    pub known_at: Option<NaiveDate>,
}

impl MemberState {
    pub fn is_member(&self) -> bool {
        self.membership.is_member()
    }
}
