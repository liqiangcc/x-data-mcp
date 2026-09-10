use crate::market_data::{Adjustment, MarketDataBatch, Period};
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
        let bytes = fs::read(self.path_for(symbol, period, adjustment))?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
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
    pub fn root(&self) -> &Path {
        &self.root
    }
}
