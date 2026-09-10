use chrono::Utc;
use rust_decimal::Decimal;
use tempfile::tempdir;
use x_data_mcp::market_data::{Adjustment, Bar, MarketDataBatch, Period, Provenance};
use x_data_mcp::storage::JsonFileStore;
#[test]
fn persists_and_reads_back_year_qfq_with_provenance() {
    let dir = tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    let batch = MarketDataBatch {
        symbol: "000001".into(),
        period: Period::Year,
        adjustment: Adjustment::Forward,
        bars: vec![Bar {
            date: chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
            open: Decimal::new(1106, 2),
            high: Decimal::new(1208, 2),
            low: Decimal::new(999, 2),
            close: Decimal::new(1185, 2),
            volume: Decimal::ONE,
            amount: Decimal::TWO,
        }],
        provenance: Provenance {
            source: "tdx".into(),
            source_period: "year".into(),
            source_adjustment: "qfq".into(),
            symbol: "000001".into(),
            acquired_at: Utc::now(),
            adapter_version: "test".into(),
        },
    };
    store.put(&batch).unwrap();
    assert_eq!(store.get("000001").unwrap(), batch);
}
