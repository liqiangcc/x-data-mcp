//! Explicit opt-in task-only SQLite stdio. No production backend switch.
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err("usage: mcp_sqlite_dev <initialized-database-path>".into());
    }
    let run_id = format!(
        "run-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let api = x_data_mcp::sqlite_task_mcp::SqliteTaskApi::new(
        x_data_mcp::sqlite_tasks::SqliteTasks::new(&args[0]),
        run_id,
    )?;
    x_data_mcp::mcp::run_stdio(api)?;
    Ok(())
}
