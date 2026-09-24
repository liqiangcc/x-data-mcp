// Node port of src/local_runtime/server.rs::serve (:46-201).
//
// Threading model adaptation (SPIKE-S13 — sockets are NOT transferable
// to workers on Node 20): the main event loop owns accept, frame
// assembly (readFrame over SocketSource) and writes — the Rust client
// thread's IO half; each accepted+authorized connection spawns a
// worker_threads Worker running factory()->api + dispatch — the Rust
// client thread's blocking-work half. One frame is outstanding per
// connection, matching the sequential read->validate->decode->handle->
// write order of server.rs:132-148. The background worker turn loop
// (:74-99) runs in its own worker. WorkerGate is shared via a
// SharedArrayBuffer; serve's stopping flag is shared via cell 2 plus a
// stop message (the shutdown(Shutdown::Read) equivalent).
//
// Rust: serve(endpoint, db, factory, gate, stopping, config)
//   factory: Fn()->io::Result<H>, H: ToolHandler — per-thread api.
// Node: serve({endpoint, db, factory, dispatch, gate, stopping, config})
//   factory  = {module, export?="createHandler", args?} — imported and
//              invoked inside each worker (functions can't cross threads)
//   dispatch = {module, export?="handleJson"} — handleJson(api, input)
//              -> string|null (src_node/mcp/transport.mjs; injected so
//              local_runtime does not depend on the mcp lane)
//   stopping = {value:boolean} — the Arc<AtomicBool> equivalent; the
//              bin's signal handler sets .value = true
import { Worker } from "node:worker_threads";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
import { SocketSource, readFrame, writeFrame } from "./frame.mjs";
import { SERVE_STOP_CELL } from "./gate.mjs";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const ioErr = (m) => new Error(m); // io::Error::other — Display = msg

// server.rs:16-32 Config + Default
export const defaultConfig = () => ({
  poll_interval: 100,
  stop_budget: 10000,
  io_timeout: 30000,
  max_clients: 16,
  max_frame_bytes: 4 * 1024 * 1024,
});

export async function serve({
  endpoint, db, factory, dispatch, gate, stopping, config = {},
}) {
  const cfg = { ...defaultConfig(), ...config };
  // :58-66 — all budgets nonzero, max_clients>0, max_frame_bytes in (0, MAX)
  if (!(cfg.poll_interval > 0) || !(cfg.stop_budget > 0) ||
      !(cfg.io_timeout > 0) || !(cfg.max_clients > 0) ||
      !(cfg.max_frame_bytes > 0) ||
      !Number.isSafeInteger(cfg.max_frame_bytes)) {
    throw ioErr("invalid runtime budget");
  }
  const gateCells = gate.cells;

  const mkWorker = (role) => {
    const w = new Worker(
      new URL("./worker_entry.mjs", import.meta.url), {
        workerData: {
          role,
          handlerModule: pathToFileURL(resolve(factory.module)).href,
          handlerExport: factory.export ?? "createHandler",
          handlerArgs: factory.args ?? null,
          dispatchModule: pathToFileURL(resolve(dispatch.module)).href,
          dispatchExport: dispatch.export ?? "handleJson",
          gateSab: gate.sharedBuffer,
          lease: { path: db.path, identity: db.identity },
          pollIntervalMs: cfg.poll_interval,
        },
      });
    const rec = { worker: w, finished: false, panicked: false,
                  done: null };
    w.on("message", (m) => {
      if (m.type === "done") { rec.done = m; rec.finished = true; }
    });
    w.on("error", () => { rec.panicked = true; rec.finished = true; });
    w.on("exit", () => {
      // 'exit' can be delivered before the worker's queued 'done'
      // message — defer the verdict one tick so an in-flight done lands
      // first. A worker that exits without posting done() broke the
      // protocol — the panic equivalent (a Rust thread always returns a
      // value or panics; there is no silent exit).
      setImmediate(() => {
        if (!rec.done) rec.panicked = true;
        rec.finished = true;
      });
    });
    return rec;
  };

  // :69-100 — background worker turn thread.
  const bg = mkWorker("background");

  // Per-connection state: {rec(worker), sock, localStop}.
  const clients = new Set();
  const active = () => [...clients].filter((c) => !c.rec.finished).length;

  let nextReq = 1;
  const requestReply = (client, frame) => new Promise((res, rej) => {
    const id = nextReq++;
    const cleanup = () => {
      client.rec.worker.off("message", onMsg);
      client.rec.worker.off("error", onErr);
    };
    const onMsg = (m) => {
      if (m.type === "response" && m.id === id) {
        cleanup(); res(m.line);
      } else if (m.type === "done") {
        cleanup();
        rej(Object.assign(new Error(m.error ?? "worker exited"),
                          { session: true }));
      }
    };
    const onErr = (e) => { cleanup(); rej(e); };
    client.rec.worker.on("message", onMsg);
    client.rec.worker.once("error", onErr);
    client.rec.worker.postMessage({ type: "frame", id, bytes: frame });
  });

  // The Rust client-thread loop (server.rs:127-150): read_frame ->
  // (worker: db.validate -> utf8 -> handle_json) -> writeln+flush.
  // Session errors end the thread and are reaped silently; only a panic
  // (uncaught dispatch throw -> worker 'error'/exit-without-done)
  // stops the service.
  const runSession = async (client) => {
    const src = new SocketSource(
      client.sock, Math.min(cfg.io_timeout, 1000)); // :118 read tick
    try {
      for (;;) {
        const frame = await readFrame(
          src, () => stopping.value || client.localStop,
          cfg.max_frame_bytes, cfg.io_timeout);
        if (frame === null) break;
        const line = await requestReply(client, frame);
        if (line !== null) {
          await writeFrame(client.sock, line, cfg.io_timeout); // :145-146
        }
      }
    } catch (e) {
      client.rec.sessionError = e.message; // thread Err — reaped, not panic
    }
    try { client.rec.worker.postMessage({ type: "stop" }); } catch { /**/ }
  };

  let serveErr = null;
  let shuttingDown = false;

  // :110-152 — accept handling per connection event.
  endpoint.listener.on("connection", (sock) => {
    if (shuttingDown || stopping.value || serveErr) {
      sock.destroy();
      return;
    }
    try {
      db.validate(); // :109 — lease check before each accepted stream
    } catch (e) {
      serveErr = e;
      sock.destroy();
      return;
    }
    try {
      endpoint.authorize(sock); // :112 — SO_PEERCRED uid gate
    } catch {
      sock.destroy();
      return;
    }
    if (active() >= cfg.max_clients) { // :113-117
      sock.destroy();
      return;
    }
    let rec;
    try {
      rec = mkWorker("client");
    } catch (e) {
      serveErr = e;
      sock.destroy();
      return;
    }
    const client = { rec, sock, localStop: false };
    clients.add(client);
    runSession(client).catch(() => { /* recorded on rec */ });
  });

  // :103-170 — the serve loop's periodic checks (stopping, worker
  // liveness, lease validity, client reap). The WouldBlock sleep is the
  // 10ms housekeeping cadence.
  while (!serveErr && !stopping.value) {
    await sleep(10);
    if (stopping.value) break;
    if (bg.finished) {
      serveErr = ioErr("background worker exited; task facts retained");
      break;
    }
    try {
      db.validate(); // :109
    } catch (e) {
      serveErr = e;
      break;
    }
    for (const c of [...clients]) {
      if (c.rec.finished) {
        clients.delete(c);
        try { c.sock.destroy(); } catch { /**/ }
        if (c.rec.panicked && !serveErr) {
          // :161-164
          serveErr = ioErr(
            "MCP session panicked; stop this run without automatic retry");
        }
      }
    }
  }

  // --- shutdown (:173-200), verbatim order ---
  shuttingDown = true;
  stopping.value = true;                          // :173
  Atomics.store(gateCells, SERVE_STOP_CELL, 1);   //   visible to workers
  gate.stop();                                    // :174
  endpoint.close();                               // :175 drop(endpoint)
  for (const c of clients) {                      // :176-178
    c.localStop = true;
    c.sock.pause();                               // shutdown(Read) equivalent
    try { c.rec.worker.postMessage({ type: "stop" }); } catch { /**/ }
  }

  const deadline = Date.now() + cfg.stop_budget;  // :179-184
  while ((!bg.finished || [...clients].some((c) => !c.rec.finished)) &&
         Date.now() < deadline) {
    await sleep(10);
  }
  if (!bg.finished || [...clients].some((c) => !c.rec.finished)) {
    // :185-189 — budget expired. Rust detaches the threads and lets
    // process teardown reap them; here we terminate so the workers
    // cannot pin the event loop after serve() reports the error.
    for (const c of clients) { try { await c.rec.worker.terminate(); } catch { /**/ } }
    try { await bg.worker.terminate(); } catch { /**/ }
    throw ioErr(
      "shutdown budget expired; unfinished task state retained for verification");
  }
  // :190-196 — client panic join (covers panics during the drain too)
  if ([...clients].some((c) => c.rec.panicked)) {
    throw ioErr("MCP session panicked; no automatic retry");
  }
  // :197-199 — worker panicked
  if (bg.panicked) {
    throw ioErr("worker panicked; no automatic retry");
  }
  // :200 — work.and(result): the worker's own error wins over the
  // serve-loop error.
  if (bg.done && !bg.done.ok) {
    throw ioErr(bg.done.error);
  }
  if (serveErr) throw serveErr;
}
