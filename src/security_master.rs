//! Parsed security-master facts (M2 PR-2/PR-3, issue-391 §4/§5/§9.2).
//!
//! Layer position (separation point 1): this module knows the *meaning* of the
//! facts — an exchange-scoped code, a listing interval, a capital event — but
//! not where the bytes came from and not how they are stored. The TDX adapter
//! (`official_security`) parses `base.dbf`/`infoharbor_spec.cfg`/`gbbq` into
//! these types; [`assembly`] merges them with an admitted exchange-list
//! snapshot; `sqlite_market::security` writes them.
//!
//! Identity rule (same as `market_series`/`adjustment_events`, §3.10): the
//! capital-event digest is a pure function of the immutable source facts and
//! their order. The derived share counts and the `suspicious` gate are
//! excluded on purpose: both are recomputable from the stored f32 bits, so
//! folding them would let the digest disagree with itself when the gate
//! changes.
//!
//! Scope (issue-391 §16 PR-2/PR-3): PR-2 parses the TDX facts only. `SSDATE` is
//! a *subject-level* listing date (#399 A-5), `infoharbor_spec.cfg` gives the
//! delisting arrangement interval (#401 A-6), and `gbbq` categories
//! 2/3/5/7/8/9/10 are capital facts (§5.1, D-50 R-3 revision). For every
//! admitted category only the after value and the event day are state; the
//! before value is retained as audit evidence and never extrapolated across a
//! chain break (D-50). PR-3 assembles the main table from those facts plus an
//! exchange-list snapshot; see [`assembly`].

use std::collections::BTreeMap;

use chrono::NaiveDate;
use num_traits::ToPrimitive as _;
use rust_decimal::Decimal;

use crate::gbbq;

pub mod assembly;

/// One of the three mainland exchanges. M2 only consumes `Sh`/`Sz`; `Bj` is
/// part of the table vocabulary but out of the M2 scope (§1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Exchange {
    Sh,
    Sz,
    Bj,
}

impl Exchange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sh => "sh",
            Self::Sz => "sz",
            Self::Bj => "bj",
        }
    }

    /// The `infoharbor_spec.cfg` market digit (#401 A-6): `0`=SZ, `1`=SH,
    /// `2`=BJ. The file has no header and no official enum, so this mapping is
    /// an inference that the adapter cross-checks against the code segment.
    pub fn from_market(market: u8) -> Option<Self> {
        match market {
            0 => Some(Self::Sz),
            1 => Some(Self::Sh),
            2 => Some(Self::Bj),
            _ => None,
        }
    }

    /// Inverse of [`Self::as_str`], used when reading a stored row.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "sh" => Some(Self::Sh),
            "sz" => Some(Self::Sz),
            "bj" => Some(Self::Bj),
            _ => None,
        }
    }
}

/// A capital-event fact as stored: the immutable source bits plus the derived
/// 2dp share counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapitalEvent {
    /// The source's decode order after the category filter, contiguous from 0.
    pub ordinal: i64,
    /// The ex-rights/effective date. The source has no announcement date.
    pub effective_day: NaiveDate,
    /// `gbbq` category: 2 (分红), 3 (送配股), 5/7/8 (D-50 admitted),
    /// 9 (股本变动), 10 (其他).
    pub category: u8,
    /// `float_before` f32 bit pattern, verbatim.
    pub float_before_bits: u32,
    /// `total_before` f32 bit pattern, verbatim.
    pub total_before_bits: u32,
    /// `float_after` f32 bit pattern, verbatim.
    pub float_after_bits: u32,
    /// `total_after` f32 bit pattern, verbatim.
    pub total_after_bits: u32,
    /// `to_shares(float_before_bits)`.
    pub float_before: i64,
    /// `to_shares(total_before_bits)`.
    pub total_before: i64,
    /// `to_shares(float_after_bits)`.
    pub float_after: i64,
    /// `to_shares(total_after_bits)`.
    pub total_after: i64,
    /// The source's "before" is the previous record's state, which may already
    /// be stale (the owner's semantics declaration); always true in schema 24.
    pub prev_is_lagged: bool,
    /// The §5.2 quality gate. Stored, not a reason to drop the event.
    pub suspicious: bool,
    /// The 知晓时间. The source has only the effective date, so this is `None`
    /// in PR-2; it is never inferred from the effective day.
    pub known_at: Option<NaiveDate>,
}

impl CapitalEvent {
    /// Build one event from the four source values. Fails closed on a category
    /// outside 2/3/5/7/8/9/10 or a value that is not representable as integer
    /// shares (NaN/inf/overflow).
    pub fn from_raw(
        ordinal: i64,
        effective_day: NaiveDate,
        category: u8,
        c_bits: [u32; 4],
    ) -> Result<Self, Error> {
        if !is_admitted_category(category) {
            return Err(Error::Category(category));
        }
        let float_before = to_shares(c_bits[0]).ok_or(Error::Shares)?;
        let total_before = to_shares(c_bits[1]).ok_or(Error::Shares)?;
        let float_after = to_shares(c_bits[2]).ok_or(Error::Shares)?;
        let total_after = to_shares(c_bits[3]).ok_or(Error::Shares)?;
        Ok(Self {
            ordinal,
            effective_day,
            category,
            float_before_bits: c_bits[0],
            total_before_bits: c_bits[1],
            float_after_bits: c_bits[2],
            total_after_bits: c_bits[3],
            float_before,
            total_before,
            float_after,
            total_after,
            prev_is_lagged: true,
            suspicious: is_suspicious(float_before, total_before, float_after, total_after),
            known_at: None,
        })
    }

    /// The four derived share counts are exactly `to_shares` of the stored
    /// bits. Used by the writer to reject a caller that hands over an
    /// inconsistent fact.
    pub fn shares_match_bits(&self) -> bool {
        to_shares(self.float_before_bits) == Some(self.float_before)
            && to_shares(self.total_before_bits) == Some(self.total_before)
            && to_shares(self.float_after_bits) == Some(self.float_after)
            && to_shares(self.total_after_bits) == Some(self.total_after)
    }
}

/// `gbbq` categories admitted as capital facts. D-50 (R-3 revision) admits
/// 5/7/8 at the OWNER_RISK_ACCEPTED tier; 2/3/9/10 keep their earlier
/// admission. The order is the evidence vocabulary, not the event order.
pub const ADMITTED_CATEGORIES: [u8; 7] = [2, 3, 5, 7, 8, 9, 10];

/// Whether `gbbq` category `category` is stored as a capital fact.
pub fn is_admitted_category(category: u8) -> bool {
    ADMITTED_CATEGORIES.contains(&category)
}

/// Per-category count of records this build refuses to interpret. A type
/// alias keeps the field list readable and the pre-push arity heuristic from
/// splitting the generic's comma into a second field.
pub type ExcludedCategories = BTreeMap<u8, usize>;

/// One entity's complete capital-event set. `excluded` counts every `gbbq`
/// record for the entity that this build refuses to interpret (categories
/// outside [`ADMITTED_CATEGORIES`]); it is folded into the identity even though
/// it never reaches the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapitalEventSet {
    /// Canonical `sh.600000`/`sz.000001` entity.
    pub entity: String,
    /// Ordered by `ordinal` (the source's decode order after filtering).
    pub events: Vec<CapitalEvent>,
    pub excluded: usize,
    /// Which excluded categories were seen, for the evidence bundle only.
    pub excluded_by_category: ExcludedCategories,
}

impl CapitalEventSet {
    pub fn empty(entity: impl Into<String>) -> Self {
        Self {
            entity: entity.into(),
            events: Vec::new(),
            excluded: 0,
            excluded_by_category: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// `base.dbf`'s current share snapshot for one A-share row. The values are
/// integer shares converted from the file's 万股/2dp fields. PR-2 parses them
/// because the task requires the read; the stored tables have no snapshot
/// columns, so PR-4/PR-8 are the consumers (current market-cap reconciliation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentShares {
    pub zgb: Option<i64>,
    pub bg: Option<i64>,
    pub hg: Option<i64>,
    pub ltag: Option<i64>,
}

/// Where a listing date came from. Mirrors `security_master.listing_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListingSource {
    ExchangeList,
    TdxSsdate,
    Unknown,
}

impl ListingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExchangeList => "exchange_list",
            Self::TdxSsdate => "tdx_ssdate",
            Self::Unknown => "unknown",
        }
    }

    /// Inverse of [`Self::as_str`], used when reading a stored row. An
    /// unregistered value is rejected instead of guessed.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "exchange_list" => Some(Self::ExchangeList),
            "tdx_ssdate" => Some(Self::TdxSsdate),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// Where a delisting date came from. Mirrors `security_master.delisting_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelistingSource {
    ExchangeList,
    TdxArrangement,
    Unknown,
}

impl DelistingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExchangeList => "exchange_list",
            Self::TdxArrangement => "tdx_arrangement",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "exchange_list" => Some(Self::ExchangeList),
            "tdx_arrangement" => Some(Self::TdxArrangement),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// The delisting reason class. PR-2 only has the TDX arrangement interval; the
/// exchange-list/announcement source is PR-3, so everything else is `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelistingReason {
    Arrangement,
    Absorption,
    Other,
    Unknown,
}

impl DelistingReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Arrangement => "arrangement",
            Self::Absorption => "absorption",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "arrangement" => Some(Self::Arrangement),
            "absorption" => Some(Self::Absorption),
            "other" => Some(Self::Other),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// How the issuer identity was resolved. PR-2's TDX rows are always
/// `CodeDate`: a `(exchange, code)` listing subject, no issuer key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssuerResolution {
    IssuerKey,
    CodeDate,
    Ambiguous,
}

impl IssuerResolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IssuerKey => "issuer_key",
            Self::CodeDate => "code_date",
            Self::Ambiguous => "ambiguous",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "issuer_key" => Some(Self::IssuerKey),
            "code_date" => Some(Self::CodeDate),
            "ambiguous" => Some(Self::Ambiguous),
            _ => None,
        }
    }
}

/// One `security_master` interval. `effective_from`/`effective_to` are the
/// identity interval (§4.1); the listing/delisting/arrangement dates are the
/// facts that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityLifecycleFact {
    pub exchange: Exchange,
    pub code: String,
    pub effective_from: NaiveDate,
    /// Exclusive upper bound; `None` = still current.
    pub effective_to: Option<NaiveDate>,
    /// 0 = A share, 1/2 reserved by the schema for later security types. M2
    /// writes A shares only; the field exists so the assembly and the writer
    /// never hardcode the class (§15 scenario 1).
    pub instrument_class: u8,
    /// The source's issuer key (company name / orgId). `None` when the source
    /// did not provide one; a code reused by a new issuer keeps the key on the
    /// current interval only (#396).
    pub issuer_key: Option<String>,
    /// How the row was attributed to an issuer (Q3).
    pub issuer_resolution: IssuerResolution,
    /// The chosen listing date. The exchange list wins when both sources
    /// describe the same interval (§4.2).
    pub listing_date: Option<NaiveDate>,
    /// The TDX `SSDATE` verbatim, kept even when another source wins.
    pub listing_date_tdx: Option<NaiveDate>,
    pub listing_source: ListingSource,
    /// The B-share listing date of the same company, when the exchange list
    /// carries one. Never used as the A-share listing date (§4.2).
    pub b_listing_date: Option<NaiveDate>,
    /// The chosen delisting date (the exchange list wins over the TDX
    /// arrangement end, §4.3).
    pub delisting_date: Option<NaiveDate>,
    /// The TDX `infoharbor` end verbatim.
    pub delisting_date_tdx: Option<NaiveDate>,
    pub delisting_source: DelistingSource,
    pub delisting_reason_class: DelistingReason,
    pub arrangement_start: Option<NaiveDate>,
    pub arrangement_end: Option<NaiveDate>,
    pub listing_conflict: bool,
    pub delisting_conflict: bool,
    /// The 知晓时间 of this interval. None of the sources admitted so far
    /// carries an announcement day, so the writer stores NULL (§4.2: the
    /// exchange snapshot's date is evidence, never a `known_at`). The column
    /// exists because the row-level filter is part of the read model (§12.2).
    pub known_at: Option<NaiveDate>,
}

impl SecurityLifecycleFact {
    pub fn entity(&self) -> String {
        format!("{}.{}", self.exchange.as_str(), self.code)
    }
}

/// The risk-warning vocabulary of §6.3 (the #308 Stage C list, not a second
/// one). `Suspended` is part of the vocabulary for #308 alignment; M2 never
/// writes a suspension row here (suspensions live in `suspension_intervals`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskStatus {
    Normal,
    OtherRiskWarning,
    DelistingRiskWarning,
    DelistingArrangement,
    TerminatedOrDelisted,
    Suspended,
    MarkerOnlyUnresolved,
    Unknown,
}

impl RiskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::OtherRiskWarning => "other_risk_warning",
            Self::DelistingRiskWarning => "delisting_risk_warning",
            Self::DelistingArrangement => "delisting_arrangement",
            Self::TerminatedOrDelisted => "terminated_or_delisted",
            Self::Suspended => "suspended",
            Self::MarkerOnlyUnresolved => "marker_only_unresolved",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "normal" => Self::Normal,
            "other_risk_warning" => Self::OtherRiskWarning,
            "delisting_risk_warning" => Self::DelistingRiskWarning,
            "delisting_arrangement" => Self::DelistingArrangement,
            "terminated_or_delisted" => Self::TerminatedOrDelisted,
            "suspended" => Self::Suspended,
            "marker_only_unresolved" => Self::MarkerOnlyUnresolved,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// One `risk_warning_intervals` row (§6.3). `announced_at` is the publisher's
/// announcement time, `known_at` the earliest time the consumer could have
/// known; both are kept apart because only the latter bounds a point-in-time
/// query. Production rows require #308.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskWarningInterval {
    pub exchange: Exchange,
    pub code: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub raw_status_code: String,
    pub status: RiskStatus,
    pub announced_at: Option<String>,
    pub known_at: Option<NaiveDate>,
    pub source_kind: String,
}

impl RiskWarningInterval {
    pub fn entity(&self) -> String {
        format!("{}.{}", self.exchange.as_str(), self.code)
    }
}

/// `suspension_intervals.certainty` (§7.3): `confirmed` needs an admitted
/// announcement or GP29 event; `unresolved_gap` is derived and only says "no
/// bar, reason unknown" — it never means `suspended` (AC-M2-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SuspensionCertainty {
    Confirmed,
    UnresolvedGap,
}

impl SuspensionCertainty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::UnresolvedGap => "unresolved_gap",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "confirmed" => Some(Self::Confirmed),
            "unresolved_gap" => Some(Self::UnresolvedGap),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SuspensionReason {
    ExchangeAnnouncement,
    Gp29Resume,
    MissingBarNoEvent,
}

impl SuspensionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExchangeAnnouncement => "exchange_announcement",
            Self::Gp29Resume => "gp29_resume",
            Self::MissingBarNoEvent => "missing_bar_no_event",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "exchange_announcement" => Some(Self::ExchangeAnnouncement),
            "gp29_resume" => Some(Self::Gp29Resume),
            "missing_bar_no_event" => Some(Self::MissingBarNoEvent),
            _ => None,
        }
    }
}

/// One `suspension_intervals` row (§7.3). `end_day` is inclusive, so the
/// interval covers `[start_day, end_day]` (the DDL requires
/// `end_day >= start_day`); `missing_days` counts the missing trading days.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspensionInterval {
    pub exchange: Exchange,
    pub code: String,
    pub start_day: NaiveDate,
    pub end_day: NaiveDate,
    pub certainty: SuspensionCertainty,
    pub missing_days: i64,
    pub reason: SuspensionReason,
    pub known_at: Option<NaiveDate>,
    pub source_kind: String,
    pub source_record_id: Option<String>,
}

impl SuspensionInterval {
    pub fn entity(&self) -> String {
        format!("{}.{}", self.exchange.as_str(), self.code)
    }

    pub fn covers(&self, day: NaiveDate) -> bool {
        self.start_day <= day && day <= self.end_day
    }
}

/// One TDX `infoharbor_spec.cfg` row for an A-share code (#401 A-6). PR-2's
/// lifecycle facts embed the bounds for codes with an `SSDATE`; this list also
/// carries the rows whose `SSDATE` is the `'0'` sentinel, so the assembly can
/// attach the 退市整理期 to an exchange-only row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrangementFact {
    pub exchange: Exchange,
    pub code: String,
    /// 退市整理期首日; `None` = the source's empty field (no trading in the
    /// arrangement period), never "still in the period".
    pub start: Option<NaiveDate>,
    /// Open upper bound = delisting-effective day; `None` = unknown.
    pub end: Option<NaiveDate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("capital category {0} is not storable (only 2,3,5,7,8,9,10)")]
    Category(u8),
    #[error("capital value cannot be represented as integer shares")]
    Shares,
}

/// `f32` bits → integer shares. The source unit is 万股 at 2dp (#399 A-5), so
/// the frozen `gbbq` normalization (shortest round-trip decimal, then 2dp
/// HALF_UP) is applied first and the result is multiplied by 10,000. A value
/// that is not a finite 2dp amount returns `None`.
pub fn to_shares(bits: u32) -> Option<i64> {
    let value = gbbq::normalize_factor(f32::from_bits(bits))?;
    let shares = value.checked_mul(Decimal::from(10_000u32))?;
    let shares = shares.to_i64()?;
    (shares >= 0).then_some(shares)
}

/// §5.2 quality gate, literal:
/// - total shares changed by more than 50% in one record, or
/// - float shares decreased while total shares stayed flat.
///
/// `total_before == 0` skips the ratio test: the first record from zero is not
/// a percentage change. The flag is stored; the read model decides whether to
/// use a suspicious event as the state.
pub fn is_suspicious(
    float_before: i64,
    total_before: i64,
    float_after: i64,
    total_after: i64,
) -> bool {
    let delta = (i128::from(total_after) - i128::from(total_before)).abs();
    let total_jump = total_before > 0 && delta * 2 > i128::from(total_before);
    let float_drop_with_flat_total = float_after < float_before && total_after == total_before;
    total_jump || float_drop_with_flat_total
}

#[cfg(test)]
mod tests;
