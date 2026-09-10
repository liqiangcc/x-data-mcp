use x_data_mcp::collector::Collector;
use x_data_mcp::market_data::{Adjustment, Period};
use x_data_mcp::storage::JsonFileStore;
use x_data_mcp::tdx::TdxSource;

#[test]
fn fetches_and_roundtrips_two_live_day_and_year_qfq_samples() {
    if std::env::var_os("X_DATA_MCP_TDX_LIVE").is_none() {
        return;
    }
    let python = std::env::var("X_DATA_MCP_TDX_PYTHON").expect("set X_DATA_MCP_TDX_PYTHON");
    let source = TdxSource::new(python, "scripts/tdx_bridge.py");
    let dir = tempfile::tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());

    let collector = Collector::new(source, store.clone());
    let results = collector.collect(
        &["000001", "600000"],
        &[Period::Day, Period::Year],
        Adjustment::Forward,
    );
    assert_eq!(results.len(), 4);
    for item in results {
        assert!(
            item.result.is_ok(),
            "{} {:?}: {:?}",
            item.symbol,
            item.period,
            item.result
        );
        let batch = store
            .get(&item.symbol, item.period, Adjustment::Forward)
            .unwrap();
        assert!(!batch.bars.is_empty());
        assert_eq!(batch.provenance.source, "tdx");
        assert_eq!(batch.provenance.symbol, item.symbol);
    }
}
