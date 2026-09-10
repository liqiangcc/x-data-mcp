use crate::{
    market_data::{Adjustment, CoverageEvidence, MarketDataBatch, Period},
    publish::EffectiveVersion,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
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
        let path = self.path_for(symbol, period, adjustment);
        if path.exists() {
            let bytes = fs::read(path)?;
            return serde_json::from_slice(&bytes).map_err(io::Error::other);
        }
        let version = self.get_latest_effective_for_adjustment(symbol, period, adjustment)?;
        let version = version.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        Ok(MarketDataBatch {
            symbol: version.symbol.clone(),
            period: version.period,
            adjustment: version.adjustment,
            bars: version.records.clone(),
            provenance: version.provenance(),
            coverage: CoverageEvidence::complete("read from effective version"),
            unit_currency: version.unit_currency.clone(),
            adjustment_evidence: version.adjustment_evidence.clone(),
        })
    }

    pub fn publish_effective(&self, version: &EffectiveVersion) -> Result<(), io::Error> {
        fs::create_dir_all(self.effective_root())?;
        let path = self.effective_path(&version.symbol, version.period, version.adjustment);
        let mut versions: Vec<EffectiveVersion> = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?
        } else {
            Vec::new()
        };
        versions.push(version.clone());
        fs::write(
            path,
            serde_json::to_vec_pretty(&versions).map_err(io::Error::other)?,
        )
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
        let path = self.effective_path(symbol, period, adjustment);
        if !path.exists() {
            return Ok(None);
        }
        let versions: Vec<EffectiveVersion> =
            serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)?;
        Ok(versions.into_iter().last())
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
