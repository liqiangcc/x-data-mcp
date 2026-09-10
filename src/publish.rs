use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    market_data::{
        Adjustment, AdjustmentEvidence, Bar, CoverageStatus, MarketDataBatch, Period, Provenance,
        UnitCurrencyEvidence,
    },
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
    #[serde(default)]
    pub unit_currency: UnitCurrencyEvidence,
    #[serde(default)]
    pub adjustment_evidence: AdjustmentEvidence,
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

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    Published(EffectiveVersion),
    NoChange {
        version_id: String,
    },
    NeedsVerification {
        version_id: String,
        reason: String,
    },
    CoverageRejected {
        status: CoverageStatus,
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("unit/currency evidence is not confirmed: {0}")]
    UnitEvidence(String),
    #[error("adjustment evidence is not confirmed: {0}")]
    AdjustmentEvidence(String),
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
        if batch.coverage.status != CoverageStatus::Complete {
            return Ok(PublishOutcome::CoverageRejected {
                status: batch.coverage.status,
                reason: batch.coverage.reason.clone(),
            });
        }
        let (records, validation) = validate_and_deduplicate(batch)?;
        validate_unit_currency_evidence(batch)?;
        validate_adjustment_evidence(batch)?;
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
            unit_currency: batch.unit_currency.clone(),
            adjustment_evidence: batch.adjustment_evidence.clone(),
        };
        self.store.publish_effective(&version)?;
        self.store.put(batch)?;
        Ok(PublishOutcome::Published(version))
    }
}

fn validate_unit_currency_evidence(batch: &MarketDataBatch) -> Result<(), PublishError> {
    require_confirmed("price currency", &batch.unit_currency.price_currency)?;
    require_confirmed("price unit", &batch.unit_currency.price_unit)?;

    if batch.bars.iter().any(|bar| bar.volume.is_some()) {
        require_confirmed("volume unit", &batch.unit_currency.volume_unit)?;
    }
    if batch.bars.iter().any(|bar| bar.amount.is_some()) {
        require_confirmed("amount currency", &batch.unit_currency.amount_currency)?;
        require_confirmed("amount unit", &batch.unit_currency.amount_unit)?;
    }
    Ok(())
}

fn validate_adjustment_evidence(batch: &MarketDataBatch) -> Result<(), PublishError> {
    let evidence = &batch.adjustment_evidence;
    if !evidence.is_confirmed() {
        return Err(PublishError::AdjustmentEvidence(evidence.reason.clone()));
    }
    let expected = match batch.adjustment {
        Adjustment::Forward => "qfq",
    };
    if evidence.value.as_deref() != Some(expected) {
        return Err(PublishError::AdjustmentEvidence(format!(
            "confirmed value must be {expected}"
        )));
    }
    Ok(())
}

fn require_confirmed(
    field: &str,
    evidence: &crate::market_data::EvidenceValue,
) -> Result<(), PublishError> {
    if evidence.is_confirmed() {
        Ok(())
    } else {
        Err(PublishError::UnitEvidence(format!(
            "{field}: {}",
            evidence.reason
        )))
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
        && existing.unit_currency == batch.unit_currency
        && existing.adjustment_evidence == batch.adjustment_evidence
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
