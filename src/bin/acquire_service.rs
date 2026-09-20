//! x-data-acquire daemon: generation store + durable acquisition state +
//! owner-only MCP socket, with the service worker driving download tasks.
//! Same runtime contract as data_service (lease, endpoint, worker gate).
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "linux"))]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("acquire_service requires Linux owner-only IPC".into())
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
        acquire::{AcquireApi, AcquireService, Source},
        local_runtime::{Config, DatabaseLease, Endpoint, WorkerGate, serve},
    };
    static SIGNAL: AtomicBool = AtomicBool::new(false);
    extern "C" fn stop(_: libc::c_int) {
        SIGNAL.store(true, Ordering::SeqCst);
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--version" {
        println!(
            "x-data-acquire {} {} NOT_PRODUCTION_READY",
            env!("CARGO_PKG_VERSION"),
            env!("X_DATA_MCP_BUILD_SHA")
        );
        return Ok(());
    }
    let mut config = Config::default();
    let mut source = Source::Remote;
    let mut positional = vec![];
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].to_str().ok_or("arguments must be UTF-8")?;
        match arg {
            "--poll-ms" => {
                let ms: u64 = args
                    .get(i + 1)
                    .and_then(|v| v.to_str())
                    .ok_or("--poll-ms requires a value")?
                    .parse()?;
                if !(10..=60000).contains(&ms) {
                    return Err("poll interval outside 10..60000 ms".into());
                }
                config.poll_interval = Duration::from_millis(ms);
                i += 2;
            }
            "--mirror" => {
                let dir = args.get(i + 1).ok_or("--mirror requires a directory")?;
                source = Source::Mirror(PathBuf::from(dir));
                i += 2;
            }
            _ => {
                positional.push(PathBuf::from(arg));
                i += 1;
            }
        }
    }
    if positional.len() != 2 {
        return Err("usage: acquire_service <generation-root> <private-socket> \
             [--poll-ms <10..60000>] [--mirror <dir>]"
            .into());
    }
    // Open first: creates the private store layout and reconciles durable
    // state before the lease proves single-writer ownership.
    let service = AcquireService::open(&positional[0], source)?;
    let lease = Arc::new(DatabaseLease::acquire(&service.db_path())?);
    let endpoint = Endpoint::bind(&positional[1], &lease)?;
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
    let factory = move || Ok(AcquireApi::new(service.clone()));
    let result = serve(endpoint, lease, factory, gate, stopping.clone(), config);
    stopping.store(true, Ordering::SeqCst);
    let _ = monitor.join();
    result?;
    Ok(())
}
