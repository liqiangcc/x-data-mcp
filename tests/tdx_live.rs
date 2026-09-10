use x_data_mcp::market_data::{Adjustment, MarketDataSource, Period};
use x_data_mcp::storage::JsonFileStore;
use x_data_mcp::tdx::TdxSource;
#[test]
fn fetches_and_roundtrips_two_live_year_qfq_samples() {
    if std::env::var_os("X_DATA_MCP_TDX_LIVE").is_none() {
        return;
    }
    let python = std::env::var("X_DATA_MCP_TDX_PYTHON").expect("set X_DATA_MCP_TDX_PYTHON");
    let source = TdxSource::new(python, "scripts/tdx_bridge.py");
    let dir = tempfile::tempdir().unwrap();
    let store = JsonFileStore::new(dir.path());
    for symbol in ["000001", "600000"] {
        let batch = source
            .fetch(symbol, Period::Year, Adjustment::Forward)
            .unwrap();
        assert!(!batch.bars.is_empty());
        assert_eq!(batch.provenance.source, "tdx");
        assert_eq!(batch.provenance.source_period, "year");
        assert_eq!(batch.provenance.source_adjustment, "qfq");
        assert_eq!(batch.provenance.symbol, symbol);
        store.put(&batch).unwrap();
        assert_eq!(store.get(symbol).unwrap(), batch);
    }
}
