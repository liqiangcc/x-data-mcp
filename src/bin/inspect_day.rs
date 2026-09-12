//! Explicit local-file structural inspection; no network, normalization or publish.
use std::{env, fs, process::ExitCode};
use x_data_mcp::day_file;

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let path = args.next().ok_or("usage: inspect_day PATH_TO_DAY_FILE")?;
    if args.next().is_some() {
        return Err("expected exactly one local .day file".into());
    }
    let bytes = fs::read(path)?;
    let records = day_file::parse(&bytes)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema":"x-data-mcp.day-structural-inspection.v1",
            "sha256":day_file::sha256(&bytes),"bytes":bytes.len(),"record_count":records.len(),
            "first_date":records.first().map(|r|r.date),"last_date":records.last().map(|r|r.date),
            "business_semantics":"NOT_VERIFIED","production_ready":false,
            "evidence":"STRUCTURAL_ONLY","live_source":"NOT_RUN"
        }))?
    );
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
