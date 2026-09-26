//! Explicit schema lifecycle, never invoked by ordinary reads.
//! `init`/`upgrade` produce the legacy schema; `init20` creates a fresh
//! compact-storage database (schema 20), the production entry point for
//! #259 fresh-rebuild deployments.
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: task_sqlite <init|init20|upgrade> <database-path>".into());
    }
    let store = x_data_mcp::sqlite_tasks::SqliteTasks::new(&args[1]);
    let schema = match args[0].to_str() {
        Some("init") => {
            store.initialize()?;
            x_data_mcp::sqlite_tasks::SCHEMA_VERSION
        }
        Some("init20") => {
            store.initialize_compact()?;
            x_data_mcp::sqlite_tasks::COMPACT_SCHEMA_VERSION
        }
        Some("upgrade") => {
            store.upgrade()?;
            x_data_mcp::sqlite_tasks::SCHEMA_VERSION
        }
        _ => return Err("expected init, init20 or upgrade".into()),
    };
    println!("task schema {schema} ready");
    Ok(())
}
