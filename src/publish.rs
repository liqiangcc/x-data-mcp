use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    market_data::{Adjustment, Bar, MarketDataBatch, Period, Provenance},
    storage::JsonFileStore,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationSummary {
    pub record_count: usize,
    pub unique_record_count: usize,
    pub duplicate_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveVersion {
    pub version_id: String,
    pub symbol: String,
    pub period: Period,
    pub adjustment: Adjustment,
    pub source: String,
    pub records: Vec<Bar>,
    pub published_at: DateTime<Utc>,
    pub validation: ValidationSummary,
}

impl EffectiveVersion {
    pub fn provenance(&self) -> Provenance {
        Provenance {
            source: self.source.clone(),
            source_period: period_name(self.period).into(),
            source_adjustment: adjustment_name(self.adjustment).into(),
            symbol: self.symbol.clone(),
            acquired_at: self.published_at,
            adapter_version: "effective-publish".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    Published(EffectiveVersion),
    NoChange { version_id: String },
    NeedsVerification { version_id: String, reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("invalid market data record: {0}")]
    InvalidRecord(String),
    #[error("conflicting duplicate market data key: {0}")]
    DuplicateConflict(String),
    #[error("effective publish storage failed: {0}")]
    Storage(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct MarketDataPublisher {
    store: JsonFileStore,
}

impl MarketDataPublisher {
    pub fn new(store: JsonFileStore) -> Self {
        Self { store }
    }

    pub fn publish(&self, batch: &MarketDataBatch) -> Result<PublishOutcome, PublishError> {
        let (records, validation) = validate_and_deduplicate(batch)?;
        let latest = self.store.get_latest_effective_for_adjustment(
            &batch.symbol,
            batch.period,
            batch.adjustment,
        )?;
        if let Some(existing) = latest {
            if same_content(&existing, batch, &records) {
                return Ok(PublishOutcome::NoChange {
                    version_id: existing.version_id,
                });
            }
            return Ok(PublishOutcome::NeedsVerification {
                version_id: existing.version_id,
                reason: "candidate differs from effective version without revision evidence".into(),
            });
        }

        let version = EffectiveVersion {
            version_id: next_version_id(),
            symbol: batch.symbol.clone(),
            period: batch.period,
            adjustment: batch.adjustment,
            source: batch.provenance.source.clone(),
            records,
            published_at: Utc::now(),
            validation,
        };
        self.store.publish_effective(&version)?;
        self.store.put(batch)?;
        Ok(PublishOutcome::Published(version))
    }
}

fn validate_and_deduplicate(
    batch: &MarketDataBatch,
) -> Result<(Vec<Bar>, ValidationSummary), PublishError> {
    if batch.bars.is_empty() {
        return Err(PublishError::InvalidRecord("batch has no records".into()));
    }
    let mut records = Vec::new();
    let mut duplicate_count = 0;
    for bar in &batch.bars {
        if bar.date.year() <= 0 {
            return Err(PublishError::InvalidRecord(
                "record has no natural year/date".into(),
            ));
        }
        if bar.high < bar.low {
            return Err(PublishError::InvalidRecord("high is below low".into()));
        }
        if bar.open < bar.low || bar.open > bar.high {
            return Err(PublishError::InvalidRecord(
                "open is outside low/high".into(),
            ));
        }
        if bar.close < bar.low || bar.close > bar.high {
            return Err(PublishError::InvalidRecord(
                "close is outside low/high".into(),
            ));
        }
        if bar.volume.is_some_and(|value| value < 0.into()) {
            return Err(PublishError::InvalidRecord("volume is negative".into()));
        }
        if bar.amount.is_some_and(|value| value < 0.into()) {
            return Err(PublishError::InvalidRecord("amount is negative".into()));
        }
        let key = record_key(batch.period, bar);
        if let Some(existing) = records
            .iter()
            .find(|existing: &&Bar| record_key(batch.period, existing) == key)
        {
            if *existing != *bar {
                return Err(PublishError::DuplicateConflict(key));
            }
            duplicate_count += 1;
        } else {
            records.push(bar.clone());
        }
    }
    records.sort_by_key(|bar| record_key(batch.period, bar));
    let summary = ValidationSummary {
        record_count: batch.bars.len(),
        unique_record_count: records.len(),
        duplicate_count,
    };
    Ok((records, summary))
}

fn same_content(existing: &EffectiveVersion, batch: &MarketDataBatch, records: &[Bar]) -> bool {
    existing.symbol == batch.symbol
        && existing.period == batch.period
        && existing.adjustment == batch.adjustment
        && existing.source == batch.provenance.source
        && existing.records == records
}

fn record_key(period: Period, bar: &Bar) -> String {
    match period {
        Period::Day => bar.date.to_string(),
        Period::Year => bar.date.year().to_string(),
    }
}

fn period_name(period: Period) -> &'static str {
    match period {
        Period::Day => "day",
        Period::Year => "year",
    }
}

fn adjustment_name(adjustment: Adjustment) -> &'static str {
    match adjustment {
        Adjustment::Forward => "qfq",
    }
}

static VERSION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn next_version_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = VERSION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("version-{nanos:x}-{sequence:x}")
}
