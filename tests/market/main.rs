// Consolidated integration test target (#272 S2).
mod day_file {
    pub use x_data_mcp::hash::sha256;
}
mod market_update;
mod publish;
mod sqlite_market;
mod sqlite_range_update;
mod storage_roundtrip;
mod yearly_publication;
