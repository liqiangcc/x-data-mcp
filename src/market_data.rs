use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Period {
    Day,
    Year,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Adjustment {
    /// 不复权: the `.day` price fact itself, no corporate-action transform
    /// (schema 22 code 0, issue-383 §3.2/§4.1).
    Raw,
    /// 前复权: derived from the raw series plus category-1 events
    /// (schema 22 code 1).
    Forward,
}

impl Adjustment {
    /// Storage code (schema 22 `market_series.adjustment` and
    /// `market_events.adjustment`): 0 = 不复权, 1 = 前复权 (issue-383 §3.2).
    pub fn code(self) -> i64 {
        match self {
            Self::Raw => 0,
            Self::Forward => 1,
        }
    }

    /// Decode a stored code; `None` for values the DDL does not define so
    /// callers fail closed instead of coercing (issue-383 §2.3 item 3).
    pub fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::Raw),
            1 => Some(Self::Forward),
            _ => None,
        }
    }

    /// Wire/evidence string: `Provenance.source_adjustment`,
    /// `AdjustmentEvidence.value`, `market.daily` metadata (issue-383 §2.2:
    /// the 9 hand-written `"qfq"` literals become this one definition).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Forward => "qfq",
        }
    }

    /// Parse the wire/evidence string; `None` for anything else. The exact
    /// inverse of [`Self::as_str`], used where an external string must be
    /// recognized without silently accepting a new 口径.
    pub fn from_str_exact(value: &str) -> Option<Self> {
        match value {
            "raw" => Some(Self::Raw),
            "qfq" => Some(Self::Forward),
            _ => None,
        }
    }

    /// True when the confirmed evidence value names this 口径.
    pub fn matches_evidence(self, evidence: &AdjustmentEvidence) -> bool {
        evidence.is_confirmed() && evidence.value.as_deref() == Some(self.as_str())
    }
}

/// Schema-22 `market_series.instrument_class` values (issue-383 §3.2).
/// M1 writes `Stock` and (from PR-5) `Index`; `Etf` is the reserved M3 value
/// and no M1 code writes it.
pub const INSTRUMENT_CLASS_STOCK: i64 = 0;
pub const INSTRUMENT_CLASS_INDEX: i64 = 1;
pub const INSTRUMENT_CLASS_ETF: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CoverageStatus {
    Complete,
    Incomplete,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageEvidence {
    pub status: CoverageStatus,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EvidenceStatus {
    Confirmed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceValue {
    pub status: EvidenceStatus,
    pub value: Option<String>,
    pub reason: String,
}

impl Default for EvidenceValue {
    fn default() -> Self {
        Self::unknown("unit/currency evidence was not supplied")
    }
}

impl EvidenceValue {
    pub fn confirmed(value: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            status: EvidenceStatus::Confirmed,
            value: Some(value.into()),
            reason: reason.into(),
        }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            status: EvidenceStatus::Unknown,
            value: None,
            reason: reason.into(),
        }
    }

    pub fn is_confirmed(&self) -> bool {
        self.status == EvidenceStatus::Confirmed
            && self.value.as_deref().is_some_and(|value| !value.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitCurrencyEvidence {
    pub price_currency: EvidenceValue,
    pub price_unit: EvidenceValue,
    pub volume_unit: EvidenceValue,
    pub amount_currency: EvidenceValue,
    pub amount_unit: EvidenceValue,
}

impl Default for UnitCurrencyEvidence {
    fn default() -> Self {
        Self::unknown("unit/currency evidence was not supplied")
    }
}

impl UnitCurrencyEvidence {
    pub fn confirmed(
        price_currency: impl Into<String>,
        price_unit: impl Into<String>,
        volume_unit: impl Into<String>,
        amount_currency: impl Into<String>,
        amount_unit: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        let reason = reason.into();
        Self {
            price_currency: EvidenceValue::confirmed(price_currency, reason.clone()),
            price_unit: EvidenceValue::confirmed(price_unit, reason.clone()),
            volume_unit: EvidenceValue::confirmed(volume_unit, reason.clone()),
            amount_currency: EvidenceValue::confirmed(amount_currency, reason.clone()),
            amount_unit: EvidenceValue::confirmed(amount_unit, reason),
        }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            price_currency: EvidenceValue::unknown(reason.clone()),
            price_unit: EvidenceValue::unknown(reason.clone()),
            volume_unit: EvidenceValue::unknown(reason.clone()),
            amount_currency: EvidenceValue::unknown(reason.clone()),
            amount_unit: EvidenceValue::unknown(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdjustmentEvidence {
    pub status: EvidenceStatus,
    pub value: Option<String>,
    pub reason: String,
}

impl Default for AdjustmentEvidence {
    fn default() -> Self {
        Self::unknown("adjustment evidence was not supplied")
    }
}

impl AdjustmentEvidence {
    pub fn confirmed(value: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            status: EvidenceStatus::Confirmed,
            value: Some(value.into()),
            reason: reason.into(),
        }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            status: EvidenceStatus::Unknown,
            value: None,
            reason: reason.into(),
        }
    }

    pub fn is_confirmed(&self) -> bool {
        self.status == EvidenceStatus::Confirmed
            && self.value.as_deref().is_some_and(|value| !value.is_empty())
    }
}

impl Default for CoverageEvidence {
    fn default() -> Self {
        Self::unknown("coverage evidence was not supplied")
    }
}

impl CoverageEvidence {
    pub fn complete(reason: impl Into<String>) -> Self {
        Self {
            status: CoverageStatus::Complete,
            reason: reason.into(),
        }
    }

    pub fn incomplete(reason: impl Into<String>) -> Self {
        Self {
            status: CoverageStatus::Incomplete,
            reason: reason.into(),
        }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            status: CoverageStatus::Unknown,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bar {
    pub date: NaiveDate,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Option<Decimal>,
    pub amount: Option<Decimal>,
    /// Eighth `.day` word preserved verbatim (internal evidence field, not a
    /// promoted contract value). A-share ex-basis semantics are anchored only
    /// for 1999–2010 (#240); it is not part of the query wire contract.
    #[serde(default)]
    pub r7_raw: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YearlyDerivation {
    pub base_daily_version_id: String,
    pub algorithm_version: String,
    pub first_trading_date: NaiveDate,
    pub last_trading_date: NaiveDate,
}

/// One byte-exact snapshot input consumed while materializing a batch: the
/// manifest-pinned archive digest plus the member actually read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotInputBinding {
    pub role: String,
    pub member: String,
    pub archive_sha256: String,
    pub member_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    pub source_period: String,
    pub source_adjustment: String,
    pub symbol: String,
    pub acquired_at: DateTime<Utc>,
    pub adapter_version: String,
    /// None for daily and legacy records; never infer lineage for old yearly data.
    #[serde(default)]
    pub yearly_derivation: Option<YearlyDerivation>,
    /// Snapshot archive/member digests this batch was materialized from;
    /// None for records that predate snapshot-bound provenance.
    #[serde(default)]
    pub snapshot_inputs: Option<Vec<SnapshotInputBinding>>,
}
/// One category-1 corporate-action event of one entity — a 解析事实
/// (separation point 2, issue-383 §3.6).
///
/// Two times are deliberately kept apart (separation point 3):
/// * [`Self::effective_day`] is the 生效日 (ex-rights day) the source carries;
/// * [`Self::known_at`] is the 知晓时间 (when the fact became knowable). The
///   source has no announcement date, so M1 always stores `None` = unknown
///   (BR-M1-9) instead of guessing or substituting a file timestamp. Any
///   point-in-time query therefore has to declare that the factor set has an
///   effective date but no known-at date.
///
/// `c_bits` is the immutable fact (the source's f32 bit patterns verbatim);
/// `factors` is the derived 2dp business decimal (`gbbq-cat1-2dp-v1`). The
/// relation `normalize(c_bits) == factors` is bound by a test, so "the factors
/// can be recomputed" is an executable assertion rather than a promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjustmentEvent {
    /// Decode order within the entity: a stable ordinal that keeps same-day
    /// events distinguishable (`adjustment_events` primary key).
    pub ordinal: i64,
    /// 生效日, as the same epoch-day integer `series_bars.trade_day` uses.
    pub effective_day: NaiveDate,
    /// Category as decoded; M1 stores only the verified value 1.
    pub category: u8,
    /// C1..C4 exactly as stored by the source (f32 bit patterns).
    pub c_bits: [u32; 4],
    /// C1..C4 as the derived 2dp business decimals (stored as `c1..c4`).
    pub factors: [Decimal; 4],
    /// 知晓时间; `None` = unknown, which is the only M1 value.
    pub known_at: Option<NaiveDate>,
}

/// The whole parsed factor set of one entity plus the records this build
/// refuses to interpret (issue-383 §3.6).
///
/// Non-category-1 records are never stored: their semantics are not verified.
/// They survive only as the `excluded` count in the evidence bundle, which is
/// the same accounting the per-security event binding already used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjustmentEventSet {
    pub entity: String,
    /// Ordered by `ordinal` (the source's decode order).
    pub events: Vec<AdjustmentEvent>,
    pub excluded: usize,
}

impl AdjustmentEventSet {
    /// The empty set: an entity with no verified event. It carries no version
    /// (a `derived_dataset_versions` row requires `record_count > 0`), and a
    /// commit carrying it clears any stored set.
    pub fn empty(entity: impl Into<String>) -> Self {
        Self {
            entity: entity.into(),
            events: Vec::new(),
            excluded: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// The 不复权 + 因子 side of one daily item's write set: the raw series is the
/// source price fact, the factor set is the parsed corporate-action fact, and
/// the forward series is derived from both, so all of them are committed in
/// one transaction (issue-383 §9.1).
///
/// The commit aligns `bars` to the dates the forward side actually writes:
/// the raw series always covers exactly the forward series' date range.
/// A forward rewrite caused by a corporate action therefore resolves to a
/// raw `no_change` by content digest, which is the intended asymmetry.
#[derive(Debug, Clone, PartialEq)]
pub struct RawBatch {
    /// Raw bars for the forward side's written dates: the tail for an
    /// append, the full history for a first publish/replace, empty for a
    /// no-change.
    pub bars: Vec<Bar>,
    /// Raw 口径 evidence (`value == "raw"`) the raw version evidence uses.
    pub adjustment_evidence: AdjustmentEvidence,
    /// The entity's current factor set, replaced wholesale in the same
    /// transaction. Every production daily path fills it from the same gbbq
    /// decode that produced the forward prices.
    pub factors: AdjustmentEventSet,
}

impl RawBatch {
    pub fn new(
        bars: Vec<Bar>,
        adjustment_evidence: AdjustmentEvidence,
        factors: AdjustmentEventSet,
    ) -> Self {
        Self {
            bars,
            adjustment_evidence,
            factors,
        }
    }

    pub fn no_change(adjustment_evidence: AdjustmentEvidence, factors: AdjustmentEventSet) -> Self {
        Self::new(Vec::new(), adjustment_evidence, factors)
    }
}

/// One index benchmark's materialized daily facts plus the member digests the
/// commit binds (issue-383 §6.3).
///
/// An index is an ordinary bar series (no separate bar table), but its facts
/// differ from a security's in three ways the adapter states here rather than
/// in code constants: the unit evidence is index-point/手/元, the 口径 is
/// 不复权 only (an index level has no ex-rights event), and the bound event
/// set is the **empty** set digest instead of a security's gbbq records.
/// `batch` stays the neutral fact set the domain layer understands.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexBatch {
    pub batch: MarketDataBatch,
    /// sha256 of the decompressed `.day` member actually read.
    pub day_member_sha256: String,
    /// sha256 of the empty corporate-action set — the index's
    /// `source_bindings.security_event_sha256` (§3.1).
    pub security_event_sha256: String,
}

/// One calendar input's neutral facts (issue-383 §5.1, M1 PR-6): the bar dates
/// of one index `.day` member and that member's digest.
///
/// Dates only — the calendar needs "an index printed a level that day", not
/// the prices — so the derived calendar depends on the narrowest possible fact
/// and stays recomputable from the layer below (§11 separation point 2). The
/// entity is the registry key (`trading_calendar::INPUTS`); the adapter never
/// decides which indices feed the calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarInputDates {
    /// The registered index entity this member belongs to.
    pub entity: String,
    /// Resolved member name inside the day archive, for the evidence bundle.
    pub member: String,
    /// sha256 of the decompressed `.day` member actually read.
    pub day_member_sha256: String,
    /// Strictly increasing bar dates as the member records them.
    pub days: Vec<NaiveDate>,
}

/// Every registered calendar input's dates for one generation (§5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarBatch {
    pub inputs: Vec<CalendarInputDates>,
    /// sha256 of the **empty** corporate-action set: a calendar is derived from
    /// index levels, which have no ex-rights event, so its
    /// `source_bindings.security_event_sha256` records "no events" rather than
    /// borrowing a security's gbbq set (the same value an index series binds,
    /// §3.1).
    pub security_event_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketDataBatch {
    pub symbol: String,
    pub period: Period,
    pub adjustment: Adjustment,
    pub bars: Vec<Bar>,
    pub provenance: Provenance,
    #[serde(default)]
    pub coverage: CoverageEvidence,
    #[serde(default)]
    pub unit_currency: UnitCurrencyEvidence,
    #[serde(default)]
    pub adjustment_evidence: AdjustmentEvidence,
}
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("SOURCE_NOT_VERIFIED: {0}")]
    NotVerified(String),
    #[error("unsupported request: {0}")]
    Unsupported(String),
    #[error("source invocation failed: {0}")]
    Invocation(String),
    #[error("source invocation timed out after {timeout_ms} ms")]
    Timeout { timeout_ms: u64 },
    #[error("invalid source response: {0}")]
    InvalidResponse(String),
}
/// Local processing port, Issue #55. No external period/year fetch contract.
/// Implementations materialize QFQ daily from official file snapshots; yearly
/// consumes exactly one already published QFQ daily version, never a source year.
pub trait MarketDataPipeline {
    fn materialize_daily(
        &self,
        symbol: &str,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError>;

    fn derive_yearly(
        &self,
        daily: &crate::publish::EffectiveVersion,
    ) -> Result<MarketDataBatch, SourceError>;
}

impl<S: MarketDataPipeline + ?Sized> MarketDataPipeline for &S {
    fn materialize_daily(
        &self,
        symbol: &str,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        (**self).materialize_daily(symbol, adjustment)
    }

    fn derive_yearly(
        &self,
        daily: &crate::publish::EffectiveVersion,
    ) -> Result<MarketDataBatch, SourceError> {
        (**self).derive_yearly(daily)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// issue-383 §2.3 item 3: the 口径 has one definition for its storage
    /// code, its evidence string and its inverse — the nine hand-written
    /// `"qfq"` comparisons now go through these.
    #[test]
    fn adjustment_code_and_evidence_string_round_trip() {
        for (adjustment, code, text) in
            [(Adjustment::Raw, 0, "raw"), (Adjustment::Forward, 1, "qfq")]
        {
            assert_eq!(adjustment.code(), code);
            assert_eq!(Adjustment::from_code(code), Some(adjustment));
            assert_eq!(adjustment.as_str(), text);
            assert_eq!(Adjustment::from_str_exact(text), Some(adjustment));
            let confirmed = AdjustmentEvidence::confirmed(text, "bound");
            assert!(adjustment.matches_evidence(&confirmed));
            let other = if adjustment == Adjustment::Raw {
                Adjustment::Forward
            } else {
                Adjustment::Raw
            };
            assert!(!other.matches_evidence(&confirmed));
            assert!(!adjustment.matches_evidence(&AdjustmentEvidence::unknown("no")));
        }
        assert_eq!(Adjustment::from_code(2), None);
        assert_eq!(Adjustment::from_str_exact("unadjusted"), None);
    }
}
