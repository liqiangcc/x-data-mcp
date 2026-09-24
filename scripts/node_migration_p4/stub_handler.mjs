// Probe-local handler for the local_runtime serve port — the
// createHandler(args) -> api contract used by worker_entry.mjs.
// api.call is synchronous (better-sqlite3 shape); api.withWorkerGate
// wires the shared gate (DataApi::with_worker_gate equivalent).
// args.sharedSab (Int32Array SAB) lets the probe observe background
// worker turns across worker boundaries.

export function createHandler(args = {}) {
  const shared = args && args.sharedSab ? new Int32Array(args.sharedSab) : null;
  return {
    _gate: null,
    withWorkerGate(g) { this._gate = g; return this; },
    tools() {
      return ["echo", "block_ms", "gated", "run_worker_once",
              "turn_count", "panic_now"].map((name) => ({ name }));
    },
    call(name, a = {}) {
      switch (name) {
        case "echo":
          return { echo: a };
        case "block_ms": {
          Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0,
                       a.ms ?? 0);
          return { blocked: a.ms ?? 0 };
        }
        case "gated": {
          const g = this._gate.acquire(); // throws {code:"worker_busy"|"worker_stopping"}
          try {
            Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0,
                         a.ms ?? 300);
            return { held: a.ms ?? 300 };
          } finally { g.release(); }
        }
        case "run_worker_once": {
          const g = this._gate.acquire();
          g.release();
          if (shared) Atomics.add(shared, 0, 1);
          return { ran: true };
        }
        case "turn_count":
          return { turns: shared ? Atomics.load(shared, 0) : 0 };
        case "panic_now":
          throw new Error("deliberate panic (non-ApiError)");
        default:
          throw Object.assign(new Error(`unknown tool: ${name}`),
                              { code: "unknown_tool" });
      }
    },
  };
}
