// Worker entry for serve() — runs inside a worker_threads Worker.
//
// Mirrors the Rust per-thread shape: each thread calls factory() to
// build its own handler (H need not be Send — it is built inside its
// thread), then either loops worker turns (role "background",
// server.rs:74-99) or serves frame dispatches (role "client",
// server.rs:127-150).
//
// workerData: {role, handlerModule, handlerExport, handlerArgs,
//              dispatchModule, dispatchExport, gateSab, lease,
//              pollIntervalMs}
// Handler module contract: exports createHandler(args) -> api, where
// api.call(name, args) is synchronous (better-sqlite3 shape) and may
// use withWorkerGate(gate). Dispatch module exports
// handleJson(api, input) -> string|null (src_node/mcp/transport.mjs
// once merged; a stub under scripts/ for probes).
//
// Error mapping: init-phase failures (module import, factory, lease
// attach, per-frame validate/decode) post done(ok:false, msg) — the
// Rust thread returning Err, which serve() reaps as a session/worker
// error. Dispatch-phase uncaught throws (api bugs) are NOT caught —
// they crash the worker, the panic equivalent that stops the service.
import { parentPort, workerData } from "node:worker_threads";
import { WorkerGate, SERVE_STOP_CELL } from "./gate.mjs";
import { LeaseRef } from "./lease_ref.mjs";

const {
  role, handlerModule, handlerExport = "createHandler", handlerArgs,
  dispatchModule, dispatchExport = "handleJson", gateSab, lease,
  pollIntervalMs,
} = workerData;

const gate = WorkerGate.fromShared(gateSab);
const serveCells = new Int32Array(gateSab);
let localStop = false;
const isStopping = () =>
  localStop || Atomics.load(serveCells, SERVE_STOP_CELL) !== 0;

const done = (ok, error = null) => {
  try { parentPort.postMessage({ type: "done", ok, error }); } catch { /**/ }
  setImmediate(() => process.exit(0)); // let the port message flush
};

let api = null, leaseRef = null, dispatch = null, utf8 = null, initErr = null;
let hmod = null;
try {
  // Module graph loads once per worker — conn-invariant (same service
  // root). The per-conn handler is built on 'assign' — Rust's
  // factory()-per-conn parity without a fresh isolate per conn.
  hmod = await import(handlerModule);
  dispatch = dispatchModule
    ? (await import(dispatchModule))[dispatchExport]
    : null;
  utf8 = new TextDecoder("utf-8", { fatal: true }); // from_utf8 parity
} catch (e) {
  initErr = e;
}
const initApi = async () => {
  api = await hmod[handlerExport](handlerArgs);
  api.withWorkerGate?.(gate);
  leaseRef = lease ? new LeaseRef(lease.path, lease.identity) : null;
  leaseRef?.attach();
};

if (initErr) {
  done(false, initErr.message);
} else if (role === "background") {
  try { await initApi(); } catch (e) {
    done(false, e.message);
    await new Promise(() => {}); // exit is scheduled; never fall through
  }
  // server.rs:74-99 — interruptible poll, validate, one worker turn.
  try {
    while (!isStopping()) {
      const until = Date.now() + pollIntervalMs;
      while (Date.now() < until && !isStopping()) {
        await new Promise((r) => setTimeout(r, 10));
      }
      if (isStopping()) break;
      leaseRef?.validate();
      try {
        await api.call("run_worker_once", {});
      } catch (e) {
        if (e.code === "worker_busy") {
          // tolerated — a manual call holds the turn
        } else if (e.code === "worker_stopping") {
          break;
        } else {
          throw new Error(
            `worker stopped without automatic retry: ${e.code ?? e.message}`);
        }
      }
    }
    done(true);
  } catch (e) {
    done(false, e.message);
  }
} else {
  // role "client": the session loop lives on the main thread (sockets
  // are not transferable); this worker is the per-connection handler
  // thread equivalent — one outstanding frame at a time, matching the
  // sequential read->validate->decode->handle->write order in
  // server.rs:132-148.
  // Per-conn lifecycle: 'assign' builds a fresh api (Rust factory()
  // per conn); 'release' drops it and returns the worker to the pool.
  parentPort.on("message", (msg) => {
    if (msg.type === "stop") { done(true); return; }
    if (msg.type === "assign") {
      initApi()
        .then(() => parentPort.postMessage({ type: "assigned" }))
        .catch((e) => done(false, e.message)); // init Err -> serveErr
      return;
    }
    if (msg.type === "release") {
      leaseRef?.close?.();
      api = null; leaseRef = null;
      parentPort.postMessage({ type: "idle" });
      return;
    }
    if (msg.type !== "frame" || api === null) return;
    let input;
    try {
      leaseRef?.validate();
      input = utf8.decode(msg.bytes);
    } catch (e) {
      done(false, e.message);
      return;
    }
    const line = dispatch(api, input); // uncaught -> panic equivalent
    parentPort.postMessage({ type: "response", id: msg.id, line });
  });
  parentPort.postMessage({ type: "ready" });
}
