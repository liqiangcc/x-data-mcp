// Consolidated integration test target (#272 S2).
mod data_stdio;
mod data_supply;
mod data_supply_generation;
mod mcp;
mod mcp_stdio;
mod metrics_stdio;
mod metrics_storage;
mod plan_run;
mod query;
mod query_cache;
#[path = "../support/market.rs"]
mod support_market;
#[path = "../support/metrics.rs"]
mod support_metrics;
mod yearly_presence;
