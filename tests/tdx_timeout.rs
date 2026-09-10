use std::{
    fs,
    time::{Duration, Instant},
};

use x_data_mcp::{
    market_data::{Adjustment, MarketDataSource, Period, SourceError},
    tdx::TdxSource,
};

#[test]
fn maps_a_slow_bridge_to_a_bounded_timeout_error() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("slow-bridge.sh");
    fs::write(&script, "#!/bin/sh\nexec sleep 2\n").unwrap();

    let source = TdxSource::with_timeout(
        "sh",
        script.to_string_lossy().into_owned(),
        Duration::from_millis(50),
    );
    let started = Instant::now();
    let result = source.fetch("000001", Period::Year, Adjustment::Forward);

    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        result,
        Err(SourceError::Timeout { timeout_ms: 50 })
    ));
}
