use crate::market_data::MarketDataBatch;
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
        fs::write(self.path_for(&batch.symbol), payload)
    }
    pub fn get(&self, symbol: &str) -> Result<MarketDataBatch, io::Error> {
        let bytes = fs::read(self.path_for(symbol))?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
    fn path_for(&self, symbol: &str) -> PathBuf {
        self.root.join(format!("{symbol}.year.qfq.json"))
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}
