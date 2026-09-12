use crate::{
    market_data::{Adjustment, CoverageEvidence, MarketDataBatch, Period},
    publish::EffectiveVersion,
};
use serde::{Deserialize, Serialize};
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
        let mut records = self.read_records(&path)?;
        records.push(PublishedRecord {
            version: version.clone(),
            batch,
        });
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
        let adjustment = match adjustment {
            Adjustment::Forward => "qfq",
        };
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
        let adjustment = match adjustment {
            Adjustment::Forward => "qfq",
        };
        self.effective_root()
            .join(format!("{symbol}.{period}.{adjustment}.json"))
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}

// Same-directory rename is the visibility commit point on the supported Linux
// filesystem. Errors before persist leave the existing destination unchanged.
// This promises process-crash atomic visibility, not power-loss durability.
fn atomic_replace(
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
