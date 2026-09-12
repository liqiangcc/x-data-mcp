//! Explicit SQLite data-supply stdio. Does not initialize or import fixture data.
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--version" {
        println!(
            "x-data-mcp {} {} NOT_PRODUCTION_READY",
            env!("CARGO_PKG_VERSION"),
            env!("X_DATA_MCP_BUILD_SHA")
        );
        return Ok(());
    }
    if args.len() != 1 {
        return Err("usage: mcp_data <initialized-database-path>".into());
    }
    let run = format!(
        "data-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let api = x_data_mcp::data_supply::mcp::DataApi::new(
        x_data_mcp::sqlite_tasks::SqliteTasks::new(&args[0]),
        run,
        x_data_mcp::data_supply::Access::local(),
    )?;
    x_data_mcp::mcp::run_stdio(api)?;
    Ok(())
}
