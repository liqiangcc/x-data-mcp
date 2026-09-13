use super::{DatabaseLease, Endpoint, WorkerGate};
use crate::mcp::{ToolHandler, handle_json};
use serde_json::json;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct Config {
    pub poll_interval: Duration,
    pub stop_budget: Duration,
    pub io_timeout: Duration,
    pub max_clients: usize,
    pub max_frame_bytes: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(100),
            stop_budget: Duration::from_secs(10),
            io_timeout: Duration::from_secs(30),
            max_clients: 16,
            max_frame_bytes: 4 * 1024 * 1024,
        }
    }
}
struct Client {
    thread: thread::JoinHandle<io::Result<()>>,
    socket: UnixStream,
}
struct Slot(Arc<AtomicUsize>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
/// Each session owns its protocol/query state; only the service execution gate
/// and run identity are shared. H need not be Send: it is built inside its thread.
pub fn serve<F, H>(
    endpoint: Endpoint,
    db: Arc<DatabaseLease>,
    factory: F,
    gate: Arc<WorkerGate>,
    stopping: Arc<AtomicBool>,
    config: Config,
) -> io::Result<()>
where
    F: Fn() -> io::Result<H> + Send + Sync + 'static,
    H: ToolHandler + 'static,
{
    if config.poll_interval.is_zero()
        || config.stop_budget.is_zero()
        || config.io_timeout.is_zero()
        || config.max_clients == 0
        || config.max_frame_bytes == 0
        || config.max_frame_bytes == usize::MAX
    {
        return Err(io::Error::other("invalid runtime budget"));
    }
    let factory = Arc::new(factory);
    let active = Arc::new(AtomicUsize::new(0));
    let worker = {
        let factory = factory.clone();
        let db = db.clone();
        let stop = stopping.clone();
        let cfg = config.clone();
        thread::spawn(move || -> io::Result<()> {
            let api = factory()?;
            while !stop.load(Ordering::SeqCst) {
                // Wait between turns, interruptibly; never a source retry loop.
                let until = Instant::now() + cfg.poll_interval;
                while Instant::now() < until && !stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(10));
                }
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                db.validate()?;
                match api.call("run_worker_once", json!({})) {
                    Ok(_) => {}
                    Err(e) if e.code == "worker_busy" => {}
                    Err(e) if e.code == "worker_stopping" => break,
                    Err(e) => {
                        return Err(io::Error::other(format!(
                            "worker stopped without automatic retry: {}",
                            e.code
                        )));
                    }
                }
            }
            Ok(())
        })
    };
    let mut clients = Vec::new();
    let result = (|| -> io::Result<()> {
        while !stopping.load(Ordering::SeqCst) {
            if worker.is_finished() {
                return Err(io::Error::other(
                    "background worker exited; task facts retained",
                ));
            }
            db.validate()?;
            match endpoint.listener.accept() {
                Ok((stream, _)) => {
                    if endpoint.authorize(&stream).is_err()
                        || active.load(Ordering::SeqCst) >= config.max_clients
                    {
                        drop(stream);
                        continue;
                    }
                    stream.set_read_timeout(Some(config.io_timeout))?;
                    stream.set_write_timeout(Some(config.io_timeout))?;
                    active.fetch_add(1, Ordering::SeqCst);
                    let slot = Slot(active.clone());
                    let factory = factory.clone();
                    let db = db.clone();
                    let stop = stopping.clone();
                    let cfg = config.clone();
                    let socket = stream.try_clone()?;
                    let thread = thread::spawn(move || -> io::Result<()> {
                        let _slot = slot;
                        let api = factory()?;
                        let mut writer = stream.try_clone()?;
                        let mut reader = BufReader::new(stream);
                        while !stop.load(Ordering::SeqCst) {
                            let mut frame = Vec::new();
                            let n = reader
                                .by_ref()
                                .take((cfg.max_frame_bytes + 1) as u64)
                                .read_until(b'\n', &mut frame)?;
                            if n == 0 {
                                break;
                            }
                            if n > cfg.max_frame_bytes || frame.last() != Some(&b'\n') {
                                return Err(io::Error::other(
                                    "IPC frame exceeds limit or is incomplete",
                                ));
                            }
                            db.validate()?;
                            let input = std::str::from_utf8(&frame).map_err(io::Error::other)?;
                            if let Some(response) = handle_json(&api, input) {
                                writeln!(writer, "{response}")?;
                                writer.flush()?;
                            }
                        }
                        Ok(())
                    });
                    clients.push(Client { thread, socket });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(e) => return Err(e),
            }
            let mut index = 0;
            while index < clients.len() {
                if clients[index].thread.is_finished() {
                    let _ = clients.swap_remove(index).thread.join();
                } else {
                    index += 1;
                }
            }
        }
        Ok(())
    })();
    stopping.store(true, Ordering::SeqCst);
    gate.stop();
    drop(endpoint); // Stops accepting. This never releases task occupancy.
    for client in &clients {
        let _ = client.socket.shutdown(Shutdown::Read);
    }
    let deadline = Instant::now() + config.stop_budget;
    while (!worker.is_finished() || clients.iter().any(|c| !c.thread.is_finished()))
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(10));
    }
    if !worker.is_finished() || clients.iter().any(|c| !c.thread.is_finished()) {
        return Err(io::Error::other(
            "shutdown budget expired; unfinished task state retained for verification",
        ));
    }
    for client in clients {
        let _ = client.thread.join();
    }
    let work = worker
        .join()
        .map_err(|_| io::Error::other("worker panicked; no automatic retry"))?;
    result.and(work)
}
