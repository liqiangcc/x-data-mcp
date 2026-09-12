use chrono::Utc;
use rust_decimal::Decimal;
use tempfile::tempdir;
use x_data_mcp::market_data::{Adjustment, Bar, MarketDataBatch, Period, Provenance};
use x_data_mcp::storage::JsonFileStore;

fn sample_batch(symbol: &str, period: Period) -> MarketDataBatch {
    let source_period = match period {
        Period::Day => "day",
        Period::Year => "day",
    };
    MarketDataBatch {
        symbol: symbol.into(),
        period,
        adjustment: Adjustment::Forward,
        bars: vec![Bar {
            date: chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
            open: Decimal::new(1106, 2),
            high: Decimal::new(1208, 2),
            low: Decimal::new(999, 2),
            close: Decimal::new(1185, 2),
            volume: Some(Decimal::ONE),
            amount: Some(Decimal::TWO),
        }],
        provenance: Provenance {
            source: "tdx".into(),
            source_period: source_period.into(),
            source_adjustment: "qfq".into(),
            symbol: symbol.into(),
            acquired_at: Utc::now(),
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
    }
}

#[test]
fn persists_day_and_year_without_overwriting_provenance() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let day = sample_batch("000001", Period::Day);
    let year = sample_batch("000001", Period::Year);

    store.put(&day).unwrap();
    store.put(&year).unwrap();

    assert_eq!(
        store
            .get("000001", Period::Day, Adjustment::Forward)
            .unwrap(),
        day
    );
    assert_eq!(
        store
            .get("000001", Period::Year, Adjustment::Forward)
            .unwrap(),
        year
    );
    assert!(dir.path().join("000001.day.qfq.json").is_file());
    assert!(dir.path().join("000001.year.qfq.json").is_file());
}
