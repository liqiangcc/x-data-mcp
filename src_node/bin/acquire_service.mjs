// x-data-acquire daemon — Node port of src/bin/acquire_service.rs:
// generation store + durable acquisition state + owner-only MCP socket,
// with the service worker driving download tasks.
// Usage: acquire_service <generation-root> <private-socket>
//          [--poll-ms <10..60000>] [--mirror <dir>] | --version
import { execSync } from "node:child_process";
import { AcquireService, Source } from "../acquire/index.mjs";
import {
  DatabaseLease, Endpoint, WorkerGate, serve,
} from "../local_runtime/index.mjs";
import { die } from "./_shared.mjs";

const PKG_VERSION = "0.2.0-preview.24"; // env!("CARGO_PKG_VERSION")

// env!("X_DATA_MCP_BUILD_SHA") — build.rs runs `git rev-parse HEAD` at
// compile time; the source-tree runtime resolves it at startup with the
// same "unknown" fallback.
const buildSha = () => {
  try {
    return execSync("git rev-parse HEAD", { encoding: "utf8" }).trim();
  } catch { return "unknown"; }
};

async function run() {
  // Rust: #[cfg(not(target_os = "linux"))] -> Err(...).
  if (process.platform !== "linux") {
    throw new Error("acquire_service requires Linux owner-only IPC");
  }
  const args = process.argv.slice(2);
  if (args.length === 1 && args[0] === "--version") {
    process.stdout.write(
      `x-data-acquire ${PKG_VERSION} ${buildSha()} NOT_PRODUCTION_READY\n`);
    return;
  }
  const config = {};
  let source = Source.remote();
  const positional = [];
  for (let i = 0; i < args.length; ) {
    const arg = args[i];
    if (typeof arg !== "string" || !arg.isWellFormed()) {
      throw new Error("arguments must be UTF-8");
    }
    if (arg === "--poll-ms") {
      const v = args[i + 1];
      if (v === undefined) throw new Error("--poll-ms requires a value");
      // Rust .parse::<u64>() — verbatim error texts.
      if (v === "") throw new Error("cannot parse integer from empty string");
      if (!/^\d+$/.test(v)) throw new Error("invalid digit found in string");
      const ms = BigInt(v);
      if (ms > 0xFFFFFFFFFFFFFFFFn) {
        throw new Error("number too large to fit in target type");
      }
      if (ms < 10n || ms > 60000n) {
        throw new Error("poll interval outside 10..60000 ms");
      }
      config.poll_interval = Number(ms);
      i += 2;
    } else if (arg === "--mirror") {
      const dir = args[i + 1];
      if (dir === undefined) throw new Error("--mirror requires a directory");
      source = Source.mirror(dir);
      i += 2;
    } else {
      positional.push(arg);
      i += 1;
    }
  }
  if (positional.length !== 2) {
    throw new Error(
      "usage: acquire_service <generation-root> <private-socket> " +
      "[--poll-ms <10..60000>] [--mirror <dir>]");
  }
  // Open first: creates the private store layout and reconciles durable
  // state before the lease proves single-writer ownership.
  const service = AcquireService.open(positional[0], source);
  const lease = DatabaseLease.acquire(service.dbPath());
  const endpoint = await Endpoint.bind(positional[1], lease);
  const gate = new WorkerGate();
  const stopping = { value: false };
  // sigaction(SIGTERM/SIGINT -> SIGNAL=true) + 10ms monitor thread:
  // Node signal handlers already run on the event loop (NM-061 caveat:
  // queued while the main thread blocks), so the flag set is the whole
  // body — the monitor indirection collapses.
  process.on("SIGTERM", () => { stopping.value = true; });
  process.on("SIGINT", () => { stopping.value = true; });
  try {
    await serve({
      endpoint, db: lease,
      factory: {
        module: new URL("./acquire_handler.mjs", import.meta.url).pathname,
        args: { root: positional[0], source },
      },
      dispatch: {
        module: new URL("../mcp/transport.mjs", import.meta.url).pathname,
      },
      gate, stopping, config,
    });
  } finally {
    stopping.value = true;
  }
}

run().catch((e) => die(e));
