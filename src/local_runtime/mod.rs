//! Linux owner-only IPC and process lifetime. Business decisions remain in DataApi.
mod paths;
pub use paths::{DatabaseLease, Endpoint, connect};
mod server;
pub use server::{Config, serve};

use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};
/// One execution turn across the background worker and explicit manual calls.
#[derive(Default)]
pub struct WorkerGate {
    lock: Mutex<()>,
    stopping: AtomicBool,
}
impl WorkerGate {
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
    pub fn acquire(&self) -> Result<MutexGuard<'_, ()>, &'static str> {
        let guard = self.lock.try_lock().map_err(|_| "worker_busy")?;
        if self.stopping.load(Ordering::SeqCst) {
            return Err("worker_stopping");
        }
        Ok(guard)
    }
}
