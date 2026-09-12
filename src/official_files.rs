//! Production source convergence stage 1, Issue #55.
//! This adapter performs no I/O and cannot yield a publishable candidate.
use crate::{
    market_data::{Adjustment, MarketDataBatch, MarketDataPipeline, SourceError},
    publish::EffectiveVersion,
};

/// BR-SOURCE-001: unverified official-file inputs must not enter production.
/// Each item remains unresolved; these are not runtime configuration switches.
pub const PRODUCTION_BLOCKERS: &[&str] = &[
    "CATALOG_INDUSTRY_NOT_VERIFIED (#6/#46: official-file Shanghai/Shenzhen A-share identity and TDX research industry level 2)",
    "GBBQ_QFQ_NOT_VERIFIED (#54: float32 business decimal, event and QFQ semantics)",
    "FILE_ACCESS_USE_NOT_VERIFIED (long-running server acquisition/use boundaries)",
    "FILE_SET_CONSISTENCY_NOT_VERIFIED (independent .day/gbbq hashes, update times and compatibility evidence)",
    "LOCAL_PIPELINE_NOT_IMPLEMENTED (validated parsers, QFQ and fixed-published-daily yearly)",
];

/// The only production pipeline. No SDK, subprocess, remote source or fallback.
/// A future implementation requires evidence review and code changes, not flags.
pub struct OfficialFilePipeline;

impl OfficialFilePipeline {
    fn blocked() -> SourceError {
        SourceError::NotVerified(format!(
            "NOT_PRODUCTION_READY; publish blocked; {}",
            PRODUCTION_BLOCKERS.join("; ")
        ))
    }
}

impl MarketDataPipeline for OfficialFilePipeline {
    fn materialize_daily(
        &self,
        _symbol: &str,
        _adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        Err(Self::blocked())
    }

    fn derive_yearly(&self, _daily: &EffectiveVersion) -> Result<MarketDataBatch, SourceError> {
        Err(Self::blocked())
    }
}
