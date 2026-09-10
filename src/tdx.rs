use crate::market_data::{
    Adjustment, Bar, MarketDataBatch, MarketDataSource, Period, Provenance, SourceError,
};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::{
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct TdxSource {
    python: String,
    bridge_script: String,
    timeout: Duration,
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
        Self::with_timeout(python, bridge_script, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(
        python: impl Into<String>,
        bridge_script: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            python: python.into(),
            bridge_script: bridge_script.into(),
            timeout,
        }
    }

    fn invoke_bridge(&self, symbol: &str, source_period: &str) -> Result<Output, SourceError> {
        let mut child = Command::new(&self.python)
            .arg(&self.bridge_script)
            .arg(symbol)
            .arg(source_period)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| SourceError::Invocation(error.to_string()))?;
        let deadline = Instant::now() + self.timeout;

        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    return child
                        .wait_with_output()
                        .map_err(|error| SourceError::Invocation(error.to_string()));
                }
                Ok(None) => {
                    let now = Instant::now();
                    if now >= deadline {
                        let timeout_ms = self.timeout.as_millis().min(u128::from(u64::MAX)) as u64;
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(SourceError::Timeout { timeout_ms });
                    }
                    let remaining = deadline.saturating_duration_since(now);
                    thread::sleep(remaining.min(Duration::from_millis(10)));
                }
                Err(error) => {
                    return Err(SourceError::Invocation(error.to_string()));
                }
            }
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
        if adjustment != Adjustment::Forward {
            return Err(SourceError::Unsupported(
                "TDX vertical slice only supports day/year + forward".into(),
            ));
        }
        let source_period = match period {
            Period::Day => "day",
            Period::Year => "year",
        };
        if symbol != "000001" && symbol != "600000" {
            return Err(SourceError::Unsupported(format!(
                "TDX vertical slice only supports 000001 and 600000, got {symbol}"
            )));
        }
        let output = self.invoke_bridge(symbol, source_period)?;
        if !output.status.success() {
            return Err(SourceError::Invocation(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let response: BridgeResponse = serde_json::from_slice(&output.stdout)
            .map_err(|e| SourceError::InvalidResponse(e.to_string()))?;
        if response.source != "tdx"
            || response.source_period != source_period
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
                    volume: Some(b.volume),
                    amount: Some(b.amount),
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
