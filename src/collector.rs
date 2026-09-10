use crate::{
    market_data::{Adjustment, MarketDataSource, Period, SourceError},
    storage::JsonFileStore,
    task::FrozenScope,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionItemResult {
    pub symbol: String,
    pub period: Period,
    pub result: Result<(), String>,
}

pub struct Collector<S> {
    source: S,
    store: JsonFileStore,
}

impl<S: MarketDataSource> Collector<S> {
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
        let result = self
            .source
            .fetch(symbol, period, adjustment)
            .and_then(|batch| {
                validate_provenance(&batch, symbol, period, adjustment)?;
                self.store.put(&batch).map_err(|error| {
                    SourceError::Invocation(format!("storage write failed: {error}"))
                })
            })
            .map_err(|error| error.to_string());
        CollectionItemResult {
            symbol: symbol.to_owned(),
            period,
            result,
        }
    }
}

fn validate_provenance(
    batch: &crate::market_data::MarketDataBatch,
    symbol: &str,
    period: Period,
    adjustment: Adjustment,
) -> Result<(), SourceError> {
    let expected_period = match period {
        Period::Day => "day",
        Period::Year => "year",
    };
    let expected_adjustment = match adjustment {
        Adjustment::Forward => "qfq",
    };
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
