use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Period {
    Year,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Adjustment {
    Forward,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub date: NaiveDate,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub amount: Decimal,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    pub source_period: String,
    pub source_adjustment: String,
    pub symbol: String,
    pub acquired_at: DateTime<Utc>,
    pub adapter_version: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketDataBatch {
    pub symbol: String,
    pub period: Period,
    pub adjustment: Adjustment,
    pub bars: Vec<Bar>,
    pub provenance: Provenance,
}
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("unsupported request: {0}")]
    Unsupported(String),
    #[error("source invocation failed: {0}")]
    Invocation(String),
    #[error("source invocation timed out after {timeout_ms} ms")]
    Timeout { timeout_ms: u64 },
    #[error("invalid source response: {0}")]
    InvalidResponse(String),
}
pub trait MarketDataSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError>;
}
