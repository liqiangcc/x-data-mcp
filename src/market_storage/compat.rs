//! The legacy digest operates on the original Value, not a reconstructed struct.
use super::Error;
use crate::{
    market_data::{CoverageStatus, MarketDataBatch, Period},
    publish::EffectiveVersion,
};
use serde_json::Value;
pub const DIGEST_FORMAT: &str = "legacy-published-record-v1";
/// Explicit implementation budget, not a market coverage/performance claim.
pub const MAX_RECORDS: usize = 100_000;
#[derive(Debug, Clone)]
pub struct CheckedRecord {
    pub(crate) raw: Value,
    pub(crate) version: EffectiveVersion,
    pub(crate) batch: MarketDataBatch,
}
impl CheckedRecord {
    pub fn version(&self) -> &EffectiveVersion {
        &self.version
    }
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}
pub fn legacy_digest(raw: &Value) -> Result<String, Error> {
    let mut copy = raw.clone();
    copy.as_object_mut()
        .ok_or_else(|| Error::Integrity("record must be object".into()))?
        .remove("integrity_sha256");
    Ok(crate::hash::sha256(
        &serde_json::to_vec(&copy).map_err(|e| Error::Integrity(e.to_string()))?,
    ))
}
pub fn check(raw: &Value) -> Result<CheckedRecord, Error> {
    if raw.get("integrity_sha256").and_then(Value::as_str) != Some(legacy_digest(raw)?.as_str()) {
        return Err(Error::Integrity("missing/mismatched legacy digest".into()));
    }
    let version: EffectiveVersion =
        serde_json::from_value(raw.clone()).map_err(|e| Error::Integrity(e.to_string()))?;
    let batch: MarketDataBatch =
        serde_json::from_value(raw.get("batch").cloned().unwrap_or(Value::Null))
            .map_err(|e| Error::Integrity(format!("publication evidence missing: {e}")))?;
    if version.records.len() > MAX_RECORDS || batch.bars.len() > MAX_RECORDS {
        return Err(Error::Invalid(
            "version exceeds 100000 row verification budget".into(),
        ));
    }
    let (bars, validation) = crate::publish::validate_and_deduplicate(&batch)
        .map_err(|e| Error::Integrity(e.to_string()))?;
    crate::publish::validate_unit_currency_evidence(&batch)
        .map_err(|e| Error::Integrity(e.to_string()))?;
    crate::publish::validate_adjustment_evidence(&batch)
        .map_err(|e| Error::Integrity(e.to_string()))?;
    if version.version_id.trim().is_empty()
        || version.symbol.trim().is_empty()
        || version.source.trim().is_empty()
        || batch.coverage.status != CoverageStatus::Complete
        || version.symbol != batch.symbol
        || version.period != batch.period
        || version.adjustment != batch.adjustment
        || version.source != batch.provenance.source
        || batch.provenance.symbol != version.symbol
        || batch.provenance.source_adjustment != "qfq"
        || version.records != bars
        || version.validation != validation
        || version.unit_currency != batch.unit_currency
        || version.adjustment_evidence != batch.adjustment_evidence
        || version.yearly_derivation != batch.provenance.yearly_derivation
        || (version.period == Period::Day && version.yearly_derivation.is_some())
        || (version.period == Period::Year
            && (version.yearly_derivation.is_none() || batch.provenance.source_period != "day"))
    {
        return Err(Error::Integrity(
            "record/batch publication evidence mismatch".into(),
        ));
    }
    Ok(CheckedRecord {
        raw: raw.clone(),
        version,
        batch,
    })
}
