use crate::{
    market_data::{Adjustment, CoverageEvidence, MarketDataBatch, Period},
    publish::EffectiveVersion,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PublishedRecord {
    #[serde(flatten)]
    version: EffectiveVersion,
    // Old arrays of EffectiveVersion remain readable without migration.
    #[serde(default)]
    batch: Option<MarketDataBatch>,
}

#[derive(Debug, thiserror::Error)]
pub enum VerifiedReadError {
    #[error("published version was not found in the requested scope")]
    NotFound,
    #[error("published version is unavailable: {0}")]
    Unavailable(String),
    #[error("invalid storage scope")]
    InvalidScope,
    #[error("published storage read failed: {0}")]
    Storage(#[from] io::Error),
}

#[derive(Debug, Clone)]
pub struct JsonFileStore {
    root: PathBuf,
}
impl JsonFileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn put(&self, batch: &MarketDataBatch) -> Result<(), io::Error> {
        fs::create_dir_all(&self.root)?;
        let payload = serde_json::to_vec_pretty(batch).map_err(io::Error::other)?;
        fs::write(
            self.path_for(&batch.symbol, batch.period, batch.adjustment),
            payload,
        )
    }
    pub fn get(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, io::Error> {
        if let Some(record) = self.latest_record(symbol, period, adjustment)? {
            if let Some(batch) = record.batch {
                return Ok(batch);
            }
            let version = record.version;
            return Ok(MarketDataBatch {
                symbol: version.symbol.clone(),
                period: version.period,
                adjustment: version.adjustment,
                bars: version.records.clone(),
                provenance: version.provenance(),
                coverage: CoverageEvidence::complete("read from effective version"),
                unit_currency: version.unit_currency.clone(),
                adjustment_evidence: version.adjustment_evidence.clone(),
            });
        }
        let bytes = fs::read(self.path_for(symbol, period, adjustment))?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    pub fn publish_effective(&self, version: &EffectiveVersion) -> Result<(), io::Error> {
        self.commit_record(version, None)
    }

    /// Batch and effective version share one atomic commit point. Legacy raw
    /// batch files are never a second commit and cannot override published data.
    pub(crate) fn publish_batch(
        &self,
        version: &EffectiveVersion,
        batch: &MarketDataBatch,
    ) -> Result<(), io::Error> {
        self.commit_record(version, Some(batch.clone()))
    }

    fn commit_record(
        &self,
        version: &EffectiveVersion,
        batch: Option<MarketDataBatch>,
    ) -> Result<(), io::Error> {
        fs::create_dir_all(self.effective_root())?;
        let path = self.effective_path(&version.symbol, version.period, version.adjustment);
        // Preserve old JSON values byte-semantically instead of reserializing
        // historical structs and potentially dropping unknown fields.
        let mut records = self.read_values(&path)?;
        let mut record = serde_json::to_value(PublishedRecord {
            version: version.clone(),
            batch,
        })
        .map_err(io::Error::other)?;
        let digest = record_digest(&record)?;
        record
            .as_object_mut()
            .unwrap()
            .insert("integrity_sha256".into(), Value::String(digest));
        records.push(record);
        let payload = serde_json::to_vec_pretty(&records).map_err(io::Error::other)?;
        atomic_replace(&path, |file| file.write_all(&payload))
    }

    fn read_records(&self, path: &Path) -> Result<Vec<PublishedRecord>, io::Error> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    fn read_values(&self, path: &Path) -> io::Result<Vec<Value>> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    /// Read one integrity-checked storage record, never a candidate.
    /// The checksum detects accidental changes; it is NOT source authentication.
    /// None selects the last committed record and never falls back from damage.
    pub fn get_verified_version(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
        version_id: Option<&str>,
    ) -> Result<Option<EffectiveVersion>, VerifiedReadError> {
        check_scope(symbol)?;
        let values = self.read_values(&self.effective_path(symbol, period, adjustment))?;
        let value = if let Some(id) = version_id {
            let matches: Vec<_> = values
                .iter()
                .filter(|value| value.get("version_id").and_then(Value::as_str) == Some(id))
                .collect();
            match matches.as_slice() {
                [] => return Err(VerifiedReadError::NotFound),
                [one] => Some(*one),
                _ => {
                    return Err(VerifiedReadError::Unavailable(
                        "duplicate version identity".into(),
                    ));
                }
            }
        } else {
            values.last()
        };
        value
            .map(|value| verified_record(value, symbol, period, adjustment))
            .transpose()
    }

    /// All records must be verifiable for this full-list operation. Explicit
    /// version reads can still select an intact record beside a damaged one.
    pub fn list_verified_versions(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<Vec<EffectiveVersion>, VerifiedReadError> {
        check_scope(symbol)?;
        let values = self.read_values(&self.effective_path(symbol, period, adjustment))?;
        let mut versions = Vec::new();
        for value in &values {
            let version = verified_record(value, symbol, period, adjustment)?;
            if versions
                .iter()
                .any(|v: &EffectiveVersion| v.version_id == version.version_id)
            {
                return Err(VerifiedReadError::Unavailable(
                    "duplicate version identity".into(),
                ));
            }
            versions.push(version);
        }
        Ok(versions)
    }

    fn latest_record(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<Option<PublishedRecord>, io::Error> {
        Ok(self
            .read_records(&self.effective_path(symbol, period, adjustment))?
            .pop())
    }

    pub fn get_latest_effective(
        &self,
        symbol: &str,
        period: Period,
    ) -> Result<Option<EffectiveVersion>, io::Error> {
        self.get_latest_effective_for_adjustment(symbol, period, Adjustment::Forward)
    }

    pub fn get_latest_effective_for_adjustment(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<Option<EffectiveVersion>, io::Error> {
        Ok(self
            .latest_record(symbol, period, adjustment)?
            .map(|record| record.version))
    }

    fn path_for(&self, symbol: &str, period: Period, adjustment: Adjustment) -> PathBuf {
        let period = match period {
            Period::Day => "day",
            Period::Year => "year",
        };
        let adjustment = adjustment.as_str();
        self.root
            .join(format!("{symbol}.{period}.{adjustment}.json"))
    }

    fn effective_root(&self) -> PathBuf {
        self.root.join("effective")
    }

    fn effective_path(&self, symbol: &str, period: Period, adjustment: Adjustment) -> PathBuf {
        let period = match period {
            Period::Day => "day",
            Period::Year => "year",
        };
        let adjustment = adjustment.as_str();
        self.effective_root()
            .join(format!("{symbol}.{period}.{adjustment}.json"))
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn check_scope(symbol: &str) -> Result<(), VerifiedReadError> {
    if symbol.is_empty()
        || !symbol
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    {
        Err(VerifiedReadError::InvalidScope)
    } else {
        Ok(())
    }
}

fn record_digest(value: &Value) -> io::Result<String> {
    let mut payload = value.clone();
    let object = payload
        .as_object_mut()
        .ok_or_else(|| io::Error::other("record must be an object"))?;
    object.remove("integrity_sha256");
    Ok(crate::day_file::sha256(
        &serde_json::to_vec(&payload).map_err(io::Error::other)?,
    ))
}

fn verified_record(
    value: &Value,
    symbol: &str,
    period: Period,
    adjustment: Adjustment,
) -> Result<EffectiveVersion, VerifiedReadError> {
    let digest = value
        .get("integrity_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| VerifiedReadError::Unavailable("integrity evidence missing".into()))?;
    if digest != record_digest(value)? {
        return Err(VerifiedReadError::Unavailable(
            "integrity digest mismatch".into(),
        ));
    }
    let record: PublishedRecord = serde_json::from_value(value.clone())
        .map_err(|e| VerifiedReadError::Unavailable(e.to_string()))?;
    if record.version.version_id.is_empty()
        || record.version.symbol != symbol
        || record.version.period != period
        || record.version.adjustment != adjustment
    {
        return Err(VerifiedReadError::Unavailable(
            "version identity/scope mismatch".into(),
        ));
    }
    Ok(record.version)
}

// Same-directory rename is the visibility commit point on the supported Linux
// filesystem. Errors before persist leave the existing destination unchanged.
// This promises process-crash atomic visibility, not power-loss durability.
pub(crate) fn atomic_replace(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent"))?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    write(staged.as_file_mut())?;
    staged.as_file_mut().sync_all()?;
    staged.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod atomic_tests {
    use super::*;
    #[test]
    fn partial_stage_failure_preserves_old_bytes_or_absence() {
        let dir = tempfile::tempdir().unwrap();
        for old in [None, Some(b"old-complete-version".as_slice())] {
            let path = dir.path().join("published.json");
            if let Some(bytes) = old {
                fs::write(&path, bytes).unwrap();
            }
            let result = atomic_replace(&path, |file| {
                file.write_all(b"partial-new-version")?;
                Err(io::Error::other("injected write failure"))
            });
            assert!(result.is_err());
            assert_eq!(fs::read(&path).ok().as_deref(), old);
            assert_eq!(
                fs::read_dir(dir.path()).unwrap().count(),
                usize::from(old.is_some())
            );
        }
    }
}
