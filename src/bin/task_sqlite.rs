//! Explicit schema lifecycle, never invoked by ordinary reads.
//! `init`/`upgrade` produce the legacy schema; `init20` creates a fresh
//! compact-storage database (schema 21), the production entry point for
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
        return Err("usage: task_sqlite <init|init20|upgrade|check> <database-path>".into());
    }
    let store = x_data_mcp::sqlite_tasks::SqliteTasks::new(&args[1]);
    if args[0].to_str() == Some("check") {
        // Read-only full registry sweep: safe while the service is running.
        let mut findings = store.registry_sweep_readonly()?;
        // Schema-21 latest-only series deep check: closure probes plus a
        // full digest recompute per series (SPEC-LATEST-ONLY §3.5/Q11);
        // empty on earlier schemas.
        for reason in store.series_check_readonly()? {
            findings.push(x_data_mcp::sqlite_tasks::SweepFinding {
                check: "series.deep",
                reason,
            });
        }
        if findings.is_empty() {
            println!("task registry check passed");
            return Ok(());
        }
        for finding in &findings {
            eprintln!("{}: {}", finding.check, finding.reason);
        }
        return Err(format!("task registry check failed: {} finding(s)", findings.len()).into());
    }
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
        _ => return Err("expected init, init20, upgrade or check".into()),
    };
    println!("task schema {schema} ready");
    Ok(())
}
