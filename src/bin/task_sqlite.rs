//! Explicit development schema lifecycle, never invoked by ordinary reads.
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: task_sqlite <init|upgrade> <database-path>".into());
    }
    let store = x_data_mcp::sqlite_tasks::SqliteTasks::new(&args[1]);
    match args[0].to_str() {
        Some("init") => store.initialize()?,
        Some("upgrade") => store.upgrade()?,
        _ => return Err("expected init or upgrade".into()),
    };
    println!(
        "task schema {} ready (development only)",
        x_data_mcp::sqlite_tasks::SCHEMA_VERSION
    );
    Ok(())
}
