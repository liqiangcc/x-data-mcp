//! Production source convergence stage 1, Issue #55.
//! This adapter performs no I/O and cannot yield a publishable candidate.
use crate::{
    market_data::{Adjustment, MarketDataBatch, MarketDataPipeline, SourceError},
    publish::EffectiveVersion,
};

/// BR-SOURCE-001: unverified official-file inputs must not enter production.
/// Each item remains unresolved; these are not runtime configuration switches.
///
/// All three original gates are resolved by Final Gate rulings:
/// GBBQ_QFQ (#54 PASS, 2026-09-16), FILE_SET_CONSISTENCY (#131 ACCEPT,
/// comment-5749856666, pinned-set scoped), FILE_ACCESS_USE (#71 ACCEPT,
/// comment-5749934485, declared scope: owner personal research, local-only,
/// no external exposure). The list stays as the gate registry so a future
/// blocker is added here, never as a runtime switch.
pub const PRODUCTION_BLOCKERS: &[&str] = &[];

/// The only production pipeline. No SDK, subprocess, remote source or fallback.
/// A future implementation requires evidence review and code changes, not flags.
pub struct OfficialFilePipeline;

impl OfficialFilePipeline {
    fn blocked() -> SourceError {
        let gates = PRODUCTION_BLOCKERS.join("; ");
        SourceError::NotVerified(format!(
            "NOT_PRODUCTION_READY; publish blocked; stub pipeline — the \
             governed path is the installed inspected-snapshot provider; \
             unresolved gates: {}",
            if gates.is_empty() { "none" } else { &gates }
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

/// Existing generic task refusal, shared by compatibility/application workers.
pub(crate) struct BlockedSource;
impl crate::task_control::Executor for BlockedSource {
    fn execute(
        &self,
        _key: &crate::task_control::CommandKey,
        _item: &crate::task_control::ItemSpec,
    ) -> crate::task_control::ItemOutcome {
        let gates = PRODUCTION_BLOCKERS.join("; ");
        crate::task_control::ItemOutcome {
            success: false,
            error: Some(format!(
                "SOURCE_NOT_VERIFIED: {}",
                if gates.is_empty() {
                    "no installed inspected official snapshot source"
                } else {
                    &gates
                }
            )),
            evidence: None,
        }
    }
}
