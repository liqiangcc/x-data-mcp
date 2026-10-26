//! Explicit Linux long-lived runtime; no initialization, migration or source override.
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "linux"))]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("data_service requires Linux owner-only IPC".into())
}
#[cfg(target_os = "linux")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi, registry::EntryKind},
        local_runtime::{Config, DatabaseLease, Endpoint, WorkerGate, serve},
        sqlite_tasks::SqliteTasks,
        task_control::TaskRepository,
    };
    static SIGNAL: AtomicBool = AtomicBool::new(false);
    extern "C" fn stop(_: libc::c_int) {
        SIGNAL.store(true, Ordering::SeqCst);
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--version" {
        println!(
            "x-data-mcp {} {} NOT_PRODUCTION_READY",
            env!("CARGO_PKG_VERSION"),
            env!("X_DATA_MCP_BUILD_SHA")
        );
        return Ok(());
    }
    // v8 §3.4: one binary, one entry per socket. The default stays `ops` so
    // an unchanged old unit file keeps serving the daily-sync surface.
    let mut config = Config::default();
    let mut entry = EntryKind::Ops;
    let mut positional: Vec<std::ffi::OsString> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--entry") => {
                index += 1;
                let value = args
                    .get(index)
                    .and_then(|value| value.to_str())
                    .ok_or("--entry requires research|ops|maint")?;
                entry =
                    EntryKind::parse(value).ok_or("unknown --entry value (research|ops|maint)")?;
            }
            Some("--poll-ms") => {
                index += 1;
                let ms: u64 = args
                    .get(index)
                    .and_then(|value| value.to_str())
                    .ok_or("poll interval must be UTF-8")?
                    .parse()?;
                if !(10..=60000).contains(&ms) {
                    return Err("poll interval outside 10..60000 ms".into());
                }
                config.poll_interval = Duration::from_millis(ms);
            }
            _ => positional.push(args[index].clone()),
        }
        index += 1;
    }
    if positional.len() != 2 {
        return Err(
            "usage: data_service <initialized-db> <private-socket> [--entry research|ops|maint] [--poll-ms <10..60000>]".into(),
        );
    }
    // Fail-closed startup check: an allowlist naming an unregistered tool is a
    // wiring error, not a silently narrower surface (v8 §3.3 item 4).
    entry.validate()?;
    // v8 §3.4: only the ops entry runs the background worker; research and
    // maint serve reads/forensics without writing.
    config.run_worker = entry == EntryKind::Ops;
    let path = PathBuf::from(&positional[0]);
    let lease = Arc::new(DatabaseLease::acquire(&path)?);
    let run_id = format!(
        "service-{}-{}",
        std::process::id(),
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .ok_or("run clock outside range")?
    );
    // Uses the existing read-only schema check. Never creates/migrates a database
    // or substitutes a new store on failure.
    // The backend is assembled once and shared across connections: on a
    // a compact database the repository reuses this process's lease
    // instead of acquiring a second flock.
    let tasks = SqliteTasks::new(&path);
    let backend =
        x_data_mcp::sqlite_market::backend::MarketBackend::open_shared(&tasks, lease.clone())?;
    backend.tasks().candidates(&run_id)?;
    // Plan-run tasks left non-terminal by a previous process close as
    // `interrupted` here: their chunk-committed work is durable and the
    // unfinished set is re-planned. Explicit update_data and all other
    // contracts are untouched — their needs_verification marking stays a
    // visible-state decision, not a startup write. Only the ops entry owns
    // that write (v8 §3.4); research/maint start without touching task state.
    if entry == EntryKind::Ops {
        backend.tasks().reconcile_plan_run(&run_id)?;
    }
    // The accept path only checks in-flight tasks; terminal history is
    // verified here, once per start. Any finding refuses to start.
    let findings = backend.tasks().registry_sweep()?;
    if !findings.is_empty() {
        for finding in &findings {
            eprintln!(
                "registry check failed: {}: {}",
                finding.check, finding.reason
            );
        }
        return Err("task registry history is inconsistent; refusing to start".into());
    }
    // Committed-state closure checks for latest-only market storage
    // (SPEC-LATEST-ONLY §3.5; no-op on schemas <21).
    backend.series_startup_check()?;
    let endpoint = Endpoint::bind(std::path::Path::new(&positional[1]), &lease)?;
    let gate = Arc::new(WorkerGate::default());
    let stopping = Arc::new(AtomicBool::new(false));
    // SAFETY: handler only stores a lock-free atomic; no allocation/I/O/locks.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) != 0
            || libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let monitor = {
        let stopping = stopping.clone();
        std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                if SIGNAL.load(Ordering::SeqCst) {
                    stopping.store(true, Ordering::SeqCst);
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    // One generation source for the whole service: its verified-session
    // cache is process-scoped, so every connection's accept path and the
    // worker share the same byte-verification verdicts.
    let generation = x_data_mcp::data_supply::generation::GenerationSource::from_env();
    let factory = {
        let gate = gate.clone();
        move || {
            DataApi::new_with_backend(
                tasks.clone(),
                backend.clone(),
                run_id.clone(),
                Access::local(),
                generation.clone(),
            )
            .map(|api| api.with_entry(entry).with_worker_gate(gate.clone()))
            .map_err(std::io::Error::other)
        }
    };
    let result = serve(endpoint, lease, factory, gate, stopping.clone(), config);
    stopping.store(true, Ordering::SeqCst);
    let _ = monitor.join();
    result?;
    Ok(())
}
