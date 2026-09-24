// Node port of WorkerGate (src/local_runtime/mod.rs:14-22) —
// "one execution turn across the background worker and explicit manual
// calls".
//
// Rust: Mutex<()> try_lock + AtomicBool stopping, Ordering::SeqCst.
// Here: cells[LOCK] is the mutex (Atomics.compareExchange try_lock),
// cells[STOPPING] the stopping flag. The Int32Array sits on a
// SharedArrayBuffer so main thread and every worker share the SAME
// gate — pass gate.sharedBuffer through workerData. Atomics operations
// are sequentially consistent per the JS memory model.
//
// cells[SERVE_STOP] is reserved for serve()'s stopping flag so workers
// observe shutdown without waiting for a message turn.
const LOCK = 0, STOPPING = 1, SERVE_STOP = 2;

const gateErr = (code) => Object.assign(new Error(code), { code });

export class WorkerGate {
  constructor(cells) {
    this.cells = cells ?? new Int32Array(new SharedArrayBuffer(12));
  }
  static fromShared(sab) {
    return new WorkerGate(new Int32Array(sab));
  }
  get sharedBuffer() {
    return this.cells.buffer;
  }
  stop() {
    Atomics.store(this.cells, STOPPING, 1);
  }
  // Rust: acquire() -> Result<MutexGuard, &'static str>. Here: returns a
  // guard {release()} or throws Error with .code "worker_busy" /
  // "worker_stopping" — the verbatim Rust error strings.
  acquire() {
    if (Atomics.compareExchange(this.cells, LOCK, 0, 1) !== 0)
      throw gateErr("worker_busy");
    if (Atomics.load(this.cells, STOPPING) !== 0) {
      Atomics.store(this.cells, LOCK, 0);
      throw gateErr("worker_stopping");
    }
    let released = false;
    return {
      release: () => {
        if (!released) {
          released = true;
          Atomics.store(this.cells, LOCK, 0);
        }
      },
    };
  }
}
export const SERVE_STOP_CELL = SERVE_STOP;
