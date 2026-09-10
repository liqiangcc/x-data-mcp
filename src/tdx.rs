use crate::market_data::{
    Adjustment, Bar, MarketDataBatch, MarketDataSource, Period, Provenance, SourceError,
};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct TdxSource {
    python: String,
    bridge_script: String,
}
#[derive(Debug, Deserialize)]
struct BridgeResponse {
    source: String,
    source_period: String,
    source_adjustment: String,
    symbol: String,
    acquired_at: DateTime<Utc>,
    adapter_version: String,
    bars: Vec<BridgeBar>,
}
#[derive(Debug, Deserialize)]
struct BridgeBar {
    date: NaiveDate,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: Decimal,
    amount: Decimal,
}
impl TdxSource {
    pub fn new(python: impl Into<String>, bridge_script: impl Into<String>) -> Self {
        Self {
            python: python.into(),
            bridge_script: bridge_script.into(),
        }
    }
}
impl MarketDataSource for TdxSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        if period != Period::Year || adjustment != Adjustment::Forward {
            return Err(SourceError::Unsupported(
                "TDX vertical slice only supports year + forward".into(),
            ));
        }
        if symbol != "000001" && symbol != "600000" {
            return Err(SourceError::Unsupported(format!(
                "TDX vertical slice only supports 000001 and 600000, got {symbol}"
            )));
        }
        let output = Command::new(&self.python)
            .arg(&self.bridge_script)
            .arg(symbol)
            .output()
            .map_err(|e| SourceError::Invocation(e.to_string()))?;
        if !output.status.success() {
            return Err(SourceError::Invocation(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let response: BridgeResponse = serde_json::from_slice(&output.stdout)
            .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
        if response.source != "tdx"
            || response.source_period != "year"
            || response.source_adjustment != "qfq"
            || response.symbol != symbol
            || response.bars.is_empty()
        {
            return Err(SourceError::InvalidResponse(
                "bridge provenance or bars do not match requested year + qfq data".into(),
            ));
        }
        Ok(MarketDataBatch {
            symbol: response.symbol.clone(),
            period,
            adjustment,
            bars: response
                .bars
                .into_iter()
                .map(|b| Bar {
                    date: b.date,
                    open: b.open,
                    high: b.high,
                    low: b.low,
                    close: b.close,
                    volume: b.volume,
                    amount: b.amount,
                })
                .collect(),
            provenance: Provenance {
                source: response.source,
                source_period: response.source_period,
                source_adjustment: response.source_adjustment,
                symbol: response.symbol,
                acquired_at: response.acquired_at,
                adapter_version: response.adapter_version,
            },
        })
    }
}
