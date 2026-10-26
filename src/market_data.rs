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
    Forward,
}

impl Adjustment {
    /// Storage code (schema 22 `market_series.adjustment` and
    /// `market_events.adjustment`): 0 = 不复权, 1 = 前复权 (issue-383 §3.2).
    /// The enum has no raw variant yet — M1 PR-2 materializes it; until then
    /// the storage layer refuses code 0 instead of guessing a meaning.
    pub fn code(self) -> i64 {
        match self {
            Self::Forward => 1,
        }
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
