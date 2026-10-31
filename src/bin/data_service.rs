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
use x_data_mcp::{source_store::Policy, sqlite_market::backend::MarketBackend};

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
        local_runtime::{Config, DatabaseLease, Endpoint, WorkerGate, serve_many},
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
    // v8 §3.4 + phase 2 §2.1(a): one binary, one process, one entry per socket.
    // The default stays `ops` so an unchanged old unit file keeps serving the
    // daily-sync surface. Extra sockets are positional too, and `--entry` binds
    // to the socket before it: `<db> <sock> [--entry k] [<sock> [--entry k]]...`.
    let mut config = Config::default();
    let mut positional: Vec<std::ffi::OsString> = Vec::new();
    let mut entries: Vec<EntryKind> = Vec::new();
    let mut pending = EntryKind::Ops;
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--entry") => {
                index += 1;
                let value = args
                    .get(index)
                    .and_then(|value| value.to_str())
                    .ok_or("--entry requires research|ops|maint")?;
                let kind =
                    EntryKind::parse(value).ok_or("unknown --entry value (research|ops|maint)")?;
                match entries.last_mut() {
                    Some(entry) => *entry = kind,
                    None => pending = kind,
                }
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
            _ => {
                // The first positional is the database; every later one is a
                // socket served by this process.
                if !positional.is_empty() {
                    entries.push(pending);
                }
                positional.push(args[index].clone());
            }
        }
        index += 1;
    }
    if positional.len() < 2 {
        return Err(
            "usage: data_service <initialized-db> <private-socket> [--entry research|ops|maint] [<private-socket> [--entry research|ops|maint]]... [--poll-ms <10..60000>]".into(),
        );
    }
    // Fail-closed startup check: an allowlist naming an unregistered tool is a
    // wiring error, not a silently narrower surface (v8 §3.3 item 4). Every
    // distinct entry is checked once, before any socket is bound.
    let mut checked: Vec<EntryKind> = Vec::new();
    for entry in entries.iter().copied() {
        if !checked.contains(&entry) {
            entry.validate()?;
            checked.push(entry);
        }
    }
    // text grading switch (v8 §5.4): an unknown value is a wiring error too.
    if let Ok(mode) = std::env::var("X_DATA_MCP_TEXT_MODE")
        && x_data_mcp::mcp::TextMode::parse(&mode).is_none()
    {
        return Err("X_DATA_MCP_TEXT_MODE must be full, summary or off".into());
    }
    // XDM-64: a bad preparation-pool override refuses to start. `1` is the
    // operator rollback to the sequential run path.
    x_data_mcp::market_update::runner::prepare_workers_from_env()?;
    // v8 §3.4: only the ops entry runs the background worker; research and
    // maint serve reads/forensics without writing. One process holds the single
    // write lease, so at most one worker exists even with several ops sockets.
    let has_ops = entries.contains(&EntryKind::Ops);
    config.run_worker = has_ops;
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
    // Issue #446, silent-path (2a, owner 2026-10-31): the ops entry on a
    // compact backend is the production form, so its four same-source derived
    // items must fail (not silently write zero rows) when
    // `X_DATA_MCP_SOURCE_STORE` is missing or unreadable. Every other
    // constructor keeps `Optional`: stdio/tests and the legacy backend stay
    // `pending_source`. This is not a startup gate; the first run reports it.
    let source_store_policy = source_store_policy(has_ops, &backend);
    // Plan-run tasks left non-terminal by a previous process close as
    // `interrupted` here: their chunk-committed work is durable and the
    // unfinished set is re-planned. Explicit update_data and all other
    // contracts are untouched — their needs_verification marking stays a
    // visible-state decision, not a startup write. Only the ops entry owns
    // that write (v8 §3.4); research/maint start without touching task state.
    if has_ops {
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
    // Bind every socket after the one-time startup checks. Any failure is
    // fail-closed: already-bound endpoints drop (removing their socket files)
    // and the process exits without serving a partial surface.
    let mut endpoints = Vec::new();
    for (offset, entry) in entries.iter().copied().enumerate() {
        let endpoint = Endpoint::bind(std::path::Path::new(&positional[offset + 1]), &lease)?;
        endpoints.push((endpoint, entry));
    }
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
        move |entry: EntryKind| {
            DataApi::new_with_backend(
                tasks.clone(),
                backend.clone(),
                run_id.clone(),
                Access::local(),
                generation.clone(),
            )
            .map(|api| {
                api.with_entry(entry)
                    .with_source_store_policy(source_store_policy)
                    .with_worker_gate(gate.clone())
            })
            .map_err(std::io::Error::other)
        }
    };
    // The worker's session is the ops entry; it never uses a research/maint
    // surface even when those sockets are bound in the same process.
    let result = serve_many(
        endpoints,
        EntryKind::Ops,
        lease,
        factory,
        gate,
        stopping.clone(),
        config,
    );
    stopping.store(true, Ordering::SeqCst);
    let _ = monitor.join();
    result?;
    Ok(())
}

/// Strict trigger surface (owner decision 2a, `silent-path #446` §14.5): only
/// the ops entry of the service on a compact backend is the production form.
/// Stdio, tests, `research`/`maint` and the legacy backend keep `Optional`, so
/// an unconfigured store stays `pending_source` there. Not a startup gate; the
/// first run reports the missing store through the four item failures.
#[cfg(target_os = "linux")]
fn source_store_policy(has_ops: bool, backend: &MarketBackend) -> Policy {
    match backend {
        MarketBackend::Compact(_) if has_ops => Policy::Required,
        _ => Policy::Optional,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn only_the_ops_entry_on_a_compact_backend_is_strict() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = x_data_mcp::sqlite_tasks::SqliteTasks::new(dir.path().join("m.sqlite"));
        tasks.initialize_compact().unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(tasks.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let compact = MarketBackend::open(&tasks).unwrap();
        assert!(matches!(compact, MarketBackend::Compact(_)));
        assert_eq!(source_store_policy(true, &compact), Policy::Required);
        assert_eq!(source_store_policy(false, &compact), Policy::Optional);
        let legacy = MarketBackend::Legacy(x_data_mcp::sqlite_market::SqliteMarket::new(tasks));
        assert_eq!(source_store_policy(true, &legacy), Policy::Optional);
        assert_eq!(source_store_policy(false, &legacy), Policy::Optional);
    }
}
