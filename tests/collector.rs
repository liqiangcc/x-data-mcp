use rust_decimal::Decimal;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use x_data_mcp::collector::Collector;
use x_data_mcp::market_data::{
    Adjustment, Bar, MarketDataBatch, MarketDataPipeline, Period, Provenance, SourceError,
};
use x_data_mcp::storage::JsonFileStore;

#[derive(Clone)]
struct FakeSource {
    calls: Arc<Mutex<Vec<(String, Period)>>>,
    fail: Option<Period>,
    wrong_provenance: bool,
}

impl FakeSource {
    fn fetch(
        &self,
        symbol: &str,
        period: Period,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        self.calls.lock().unwrap().push((symbol.to_owned(), period));
        if self.fail == Some(period) {
            return Err(SourceError::Invocation("planned failure".into()));
        }
        let source_period = match period {
            Period::Day => "day",
            Period::Year => "day",
        };
        Ok(MarketDataBatch {
            symbol: symbol.into(),
            period,
            adjustment,
            bars: vec![Bar {
                date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                open: Decimal::ONE,
                high: Decimal::ONE,
                low: Decimal::ONE,
                close: Decimal::ONE,
                volume: Some(Decimal::ONE),
                amount: Some(Decimal::ONE),
            }],
            provenance: Provenance {
                source: "fake".into(),
                source_period: if self.wrong_provenance {
                    "year"
                } else {
                    source_period
                }
                .into(),
                source_adjustment: "qfq".into(),
                symbol: symbol.into(),
                acquired_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
                adapter_version: "test".into(),
                yearly_derivation: None,
            },
            coverage: x_data_mcp::market_data::CoverageEvidence::complete("test fixture"),
            unit_currency: x_data_mcp::market_data::UnitCurrencyEvidence::confirmed(
                "CNY",
                "share",
                "lot",
                "CNY",
                "yuan",
                "test fixture",
            ),
            adjustment_evidence: x_data_mcp::market_data::AdjustmentEvidence::confirmed(
                "qfq",
                "test fixture",
            ),
        })
    }
}

fn run(
    source: FakeSource,
) -> (
    Vec<x_data_mcp::collector::CollectionItemResult>,
    JsonFileStore,
) {
    let store = JsonFileStore::new(tempdir().unwrap().keep());
    let collector = Collector::new(source, store.clone());
    let results = collector.collect(
        &["000001"],
        &[Period::Day, Period::Year],
        Adjustment::Forward,
    );
    (results, store)
}

#[test]
fn calls_and_writes_day_and_year() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (results, store) = run(FakeSource {
        calls: calls.clone(),
        fail: None,
        wrong_provenance: false,
    });
    assert!(results.iter().all(|item| item.result.is_ok()));
    assert_eq!(calls.lock().unwrap().len(), 2);
    for period in [Period::Day, Period::Year] {
        assert_eq!(
            store
                .get("000001", period, Adjustment::Forward)
                .unwrap()
                .period,
            period
        );
    }
}

#[test]
fn one_period_failure_does_not_block_other() {
    let (results, store) = run(FakeSource {
        calls: Arc::new(Mutex::new(Vec::new())),
        fail: Some(Period::Year),
        wrong_provenance: false,
    });
    assert!(results[0].result.is_ok());
    assert!(results[1].result.is_err());
    assert!(
        store
            .get("000001", Period::Day, Adjustment::Forward)
            .is_ok()
    );
    assert!(
        store
            .get("000001", Period::Year, Adjustment::Forward)
            .is_err()
    );
}

#[test]
fn mismatched_provenance_is_not_published() {
    let (results, store) = run(FakeSource {
        calls: Arc::new(Mutex::new(Vec::new())),
        fail: None,
        wrong_provenance: true,
    });
    assert!(results[0].result.is_err());
    assert!(results[1].result.is_err());
    assert!(
        store
            .get("000001", Period::Day, Adjustment::Forward)
            .is_err()
    );
    assert!(
        store
            .get("000001", Period::Year, Adjustment::Forward)
            .is_err()
    );
}

#[test]
fn storage_roundtrip_matches_source_batch() {
    let store = JsonFileStore::new(tempdir().unwrap().keep());
    let source = FakeSource {
        calls: Arc::new(Mutex::new(Vec::new())),
        fail: None,
        wrong_provenance: false,
    };
    let expected = source
        .fetch("000001", Period::Day, Adjustment::Forward)
        .unwrap();
    let collector = Collector::new(source, store.clone());
    assert!(
        collector.collect(&["000001"], &[Period::Day], Adjustment::Forward)[0]
            .result
            .is_ok()
    );
    assert_eq!(
        store
            .get("000001", Period::Day, Adjustment::Forward)
            .unwrap(),
        expected
    );
}

impl MarketDataPipeline for FakeSource {
    fn materialize_daily(
        &self,
        symbol: &str,
        adjustment: Adjustment,
    ) -> Result<MarketDataBatch, SourceError> {
        self.fetch(symbol, Period::Day, adjustment)
    }

    fn derive_yearly(
        &self,
        daily: &x_data_mcp::publish::EffectiveVersion,
    ) -> Result<MarketDataBatch, SourceError> {
        let mut batch = self.fetch(&daily.symbol, Period::Year, daily.adjustment)?;
        batch.provenance.source_period = "day".into();
        batch.provenance.yearly_derivation = Some(x_data_mcp::market_data::YearlyDerivation {
            base_daily_version_id: daily.version_id.clone(),
            algorithm_version: "fake-yearly-fixture".into(),
            first_trading_date: daily.records.first().unwrap().date,
            last_trading_date: daily.records.last().unwrap().date,
        });
        Ok(batch)
    }
}
