use crate::{
    market_data::{Adjustment, MarketDataPipeline, Period, SourceError},
    publish::{MarketDataPublisher, PublishOutcome},
    storage::JsonFileStore,
    task::FrozenScope,
};

/// Per-item collection outcome; the alias keeps the struct's field list
/// readable for the prepush heuristic, which does not parse `A<B, C>` types.
pub type CollectionOutcome = Result<(), String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionItemResult {
    pub symbol: String,
    pub period: Period,
    pub result: CollectionOutcome,
}

pub struct Collector<S> {
    source: S,
    store: JsonFileStore,
}

impl<S: MarketDataPipeline> Collector<S> {
    pub fn new(source: S, store: JsonFileStore) -> Self {
        Self { source, store }
    }

    pub fn collect(
        &self,
        symbols: &[&str],
        periods: &[Period],
        adjustment: Adjustment,
    ) -> Vec<CollectionItemResult> {
        symbols
            .iter()
            .flat_map(|symbol| periods.iter().map(move |period| (*symbol, *period)))
            .map(|(symbol, period)| self.collect_one(symbol, period, adjustment))
            .collect()
    }

    pub fn collect_frozen(
        &self,
        frozen_range: &[FrozenScope],
        adjustment: Adjustment,
    ) -> Vec<CollectionItemResult> {
        frozen_range
            .iter()
            .map(|scope| self.collect_one(&scope.symbol, scope.period, adjustment))
            .collect()
    }

    fn collect_one(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> CollectionItemResult {
        let candidate = match period {
            Period::Day => self.source.materialize_daily(symbol, adjustment),
            Period::Year => self.derive_yearly(symbol, adjustment),
        };
        let result = candidate
            .and_then(|batch| {
                validate_provenance(&batch, symbol, period, adjustment)?;
                match MarketDataPublisher::new(self.store.clone()).publish(&batch) {
                    Ok(PublishOutcome::Published(_)) | Ok(PublishOutcome::NoChange { .. }) => {
                        Ok(())
                    }
                    Ok(PublishOutcome::NeedsVerification { reason, .. }) => {
                        Err(SourceError::InvalidResponse(format!(
                            "effective publish needs verification: {reason}"
                        )))
                    }
                    Ok(PublishOutcome::CoverageRejected { status, reason }) => Err(
                        SourceError::InvalidResponse(format!("coverage {status:?}: {reason}")),
                    ),
                    Err(error) => Err(SourceError::InvalidResponse(error.to_string())),
                }
            })
            .map_err(|error| error.to_string());
        CollectionItemResult {
            symbol: symbol.to_owned(),
            period,
            result,
        }
    }

    fn derive_yearly(
        &self,
        symbol: &str,
        adjustment: Adjustment,
    ) -> Result<crate::market_data::MarketDataBatch, SourceError> {
        let daily = self.store.get_latest_effective_for_adjustment(symbol, Period::Day, adjustment)
            .map_err(|error| SourceError::Invocation(error.to_string()))?
            .ok_or_else(|| SourceError::NotVerified(
                "PUBLISHED_QFQ_DAILY_REQUIRED: yearly only derives from one fixed published QFQ daily version".into()
            ))?;
        let batch = self.source.derive_yearly(&daily)?;
        if batch
            .provenance
            .yearly_derivation
            .as_ref()
            .map(|lineage| lineage.base_daily_version_id.as_str())
            != Some(daily.version_id.as_str())
        {
            return Err(SourceError::InvalidResponse(
                "yearly does not reference the supplied published daily version".into(),
            ));
        }
        Ok(batch)
    }
}

fn validate_provenance(
    batch: &crate::market_data::MarketDataBatch,
    symbol: &str,
    period: Period,
    adjustment: Adjustment,
) -> Result<(), SourceError> {
    // Both local QFQ daily and derived yearly are based on daily inputs.
    let expected_period = "day";
    let expected_adjustment = adjustment.as_str();
    if batch.symbol != symbol
        || batch.period != period
        || batch.adjustment != adjustment
        || batch.provenance.symbol != symbol
        || batch.provenance.source_period != expected_period
        || batch.provenance.source_adjustment != expected_adjustment
    {
        return Err(SourceError::InvalidResponse(format!(
            "provenance does not match requested {symbol} {expected_period} {expected_adjustment}"
        )));
    }
    Ok(())
}
