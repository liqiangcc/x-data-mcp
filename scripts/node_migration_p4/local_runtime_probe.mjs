// NM-062 probe — exercises the src_node/local_runtime port against the
// behaviors pinned in src/local_runtime/*:
//   Endpoint::bind guards, connect(), peer-UID authorize, frame byte-cap
//   + incomplete deadline, malformed input, max_clients, WorkerGate
//   busy/stopping, background worker turns, db-identity fail-closed,
//   shutdown drain + budget expiry, panic semantics.
// Usage: node scripts/node_migration_p4/local_runtime_probe.mjs <private-dir>
// <dir> must be a private (0700, same-UID) tmpfs dir.
import fs from "node:fs";
import net from "node:net";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import {
  DatabaseLease, Endpoint, connect, serve, defaultConfig, WorkerGate,
} from "../../src_node/local_runtime/index.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const dir = process.argv[2];
const HANDLER = join(ROOT, "scripts/node_migration_p4/stub_handler.mjs");
const DISPATCH = join(ROOT, "scripts/node_migration_p4/stub_dispatch.mjs");

let failures = 0;
const emit = (id, status, detail, evidence = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ id, status, detail, evidence }));
};
const check = (id, fn) => {
  try { const v = fn(); emit(id, "PASS", null, v); }
  catch (e) { emit(id, "FAIL", `${e.code || ""} ${e.message}`); }
};
const acheck = async (id, fn) => {
  try { const v = await fn(); emit(id, "PASS", null, v); }
  catch (e) { emit(id, "FAIL", `${e.code || ""} ${e.message}`); }
};
const expectRej = async (id, re, fn) => {
  try { await fn(); emit(id, "FAIL", "expected rejection but succeeded"); }
  catch (e) {
    emit(id, re.test(`${e.code || ""} ${e.message}`) ? "PASS" : "FAIL",
      `${e.code || ""} ${e.message}`);
  }
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const mkDb = (name) => {
  const p = join(dir, name);
  fs.writeFileSync(p, "x-data-mcp-probe", { mode: 0o600 });
  return DatabaseLease.acquire(p); // flock held for the probe duration
};

const serveCtx = (endpoint, db, config, args = {}) => {
  const ctx = {
    gate: new WorkerGate(),
    stopping: { value: false },
    promise: null,
  };
  ctx.promise = serve({
    endpoint, db,
    factory: { module: HANDLER, args },
    dispatch: { module: DISPATCH },
    gate: ctx.gate, stopping: ctx.stopping, config,
  }).then(() => ({ ok: true }))
    .catch((e) => ({ ok: false, error: e.message }));
  return ctx;
};

// ---- scripted client over the ported connect() ----
class Client {
  static async open(path) {
    const s = await connect(path);
    return new Client(s);
  }
  constructor(s) {
    this.s = s;
    this.buf = Buffer.alloc(0);
    this.lines = [];
    this.waiters = [];
    this.id = 0;
    this.closed = new Promise((res) => {
      s.on("close", () => res("closed"));
      s.on("error", () => res("error"));
    });
    s.on("data", (d) => {
      this.buf = Buffer.concat([this.buf, d]);
      let i;
      while ((i = this.buf.indexOf(0x0a)) >= 0) {
        this.lines.push(this.buf.subarray(0, i).toString());
        this.buf = this.buf.subarray(i + 1);
      }
      const w = this.waiters.splice(0);
      for (const f of w) f();
    });
  }
  nextLine(timeoutMs = 4000) {
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => reject(new Error("no response line")), timeoutMs);
      const poll = () => {
        if (this.lines.length) {
          clearTimeout(t);
          resolve(this.lines.shift());
        } else {
          this.waiters.push(poll);
        }
      };
      poll();
    });
  }
  async call(name, args = {}, timeoutMs = 4000) {
    const id = ++this.id;
    this.s.write(JSON.stringify({
      jsonrpc: "2.0", id, method: "tools/call",
      params: { name, arguments: args },
    }) + "\n");
    const line = await this.nextLine(timeoutMs);
    return JSON.parse(line);
  }
  writeRaw(text) { this.s.write(text); }
  close() { try { this.s.destroy(); } catch { /**/ } }
}

const cfg = (o) => ({ ...defaultConfig(), ...o });

// ================= bind guards (no serve needed) =================
const db0 = mkDb("db0.sqlite");

await acheck("bind_ok_socket_owner_mode", async () => {
  const d = join(dir, "a");
  fs.mkdirSync(d, { mode: 0o700 });
  const ep = await Endpoint.bind(join(d, "svc.sock"), db0);
  const st = fs.lstatSync(ep.path);
  if (!st.isSocket() || (st.mode & 0o777) !== 0o600)
    throw new Error(`bad socket mode ${(st.mode & 0o777).toString(8)}`);
  const owner = JSON.parse(fs.readFileSync(ep.path + ".owner.json", "utf8"));
  const [device, inode] = db0.identity.split(":").map(Number);
  if (owner.device !== device || owner.inode !== inode)
    throw new Error("owner identity mismatch");
  ep.close();
  if (fs.existsSync(ep.path)) throw new Error("socket not removed on close");
  return { owner };
});

await acheck("bind_unregistered_path_rejected", async () => {
  const d = join(dir, "b");
  fs.mkdirSync(d, { mode: 0o700 });
  fs.writeFileSync(join(d, "x.sock"), "x", { mode: 0o600 });
  try {
    await Endpoint.bind(join(d, "x.sock"), db0);
    throw new Error("not rejected");
  } catch (e) {
    if (!/unregistered existing endpoint; no clobber/.test(e.message)) throw e;
    return { err: e.message };
  }
});

await acheck("bind_foreign_owner_rejected", async () => {
  const d = join(dir, "c");
  fs.mkdirSync(d, { mode: 0o700 });
  fs.writeFileSync(join(d, "x.sock.owner.json"),
    JSON.stringify({ device: 9, inode: 9 }), { mode: 0o600 });
  try {
    await Endpoint.bind(join(d, "x.sock"), db0);
    throw new Error("not rejected");
  } catch (e) {
    if (!/endpoint belongs to another database/.test(e.message)) throw e;
    return { err: e.message };
  }
});

await acheck("bind_stale_socket_rebound", async () => {
  const d = join(dir, "d");
  fs.mkdirSync(d, { mode: 0o700 });
  const sp = join(d, "x.sock");
  // leave a dead socket file behind (no listener)
  const tmp = net.createServer();
  await new Promise((r) => tmp.listen(sp, r));
  await new Promise((r) => tmp.close(r));
  const [device, inode] = db0.identity.split(":").map(Number);
  fs.writeFileSync(sp + ".owner.json",
    JSON.stringify({ device, inode }), { mode: 0o600 });
  const ep = await Endpoint.bind(sp, db0);
  ep.close();
  return { rebound: true };
});

await acheck("bind_nonprivate_parent_rejected", async () => {
  const d = join(dir, "e");
  fs.mkdirSync(d, { mode: 0o755 }); // world-readable — parent() guard
  try {
    await Endpoint.bind(join(d, "x.sock"), db0);
    throw new Error("not rejected");
  } catch (e) {
    if (!/runtime path must be owned/.test(e.message)) throw e;
    return { err: e.message };
  } finally {
    fs.chmodSync(d, 0o700);
  }
});

await acheck("connect_non_socket_rejected", async () => {
  const d = join(dir, "a");
  const p = join(d, "regular.db");
  fs.writeFileSync(p, "x", { mode: 0o600 });
  try {
    await connect(p);
    throw new Error("not rejected");
  } catch (e) {
    if (!/endpoint is not a socket/.test(e.message)) throw e;
    return { err: e.message };
  }
});

await acheck("connect_bare_path_rejected", async () => {
  try {
    await connect("bare.sock");
    throw new Error("not rejected");
  } catch (e) {
    if (!/explicit private parent/.test(e.message)) throw e;
    return { err: e.message };
  }
});

await acheck("serve_config_validation", async () => {
  try {
    await serve({ endpoint: null, db: db0,
      factory: { module: HANDLER }, dispatch: { module: DISPATCH },
      gate: new WorkerGate(), stopping: { value: false },
      config: { ...defaultConfig(), max_clients: 0 } });
    throw new Error("not rejected");
  } catch (e) {
    if (!/invalid runtime budget/.test(e.message)) throw e;
    return { err: e.message };
  }
});

// ================= serve A: roundtrip / protocol / turns / drain =====
const dA = join(dir, "sa");
fs.mkdirSync(dA, { mode: 0o700 });
const dbA = mkDb("dbA.sqlite");
const sharedSab = new SharedArrayBuffer(8);
const epA = await Endpoint.bind(join(dA, "svc.sock"), dbA);
const sA = serveCtx(epA, dbA, cfg({ poll_interval: 100 }),
                    { sharedSab });

await acheck("connect_roundtrip_echo", async () => {
  const c = await Client.open(epA.path);
  const r = await c.call("echo", { x: 7 });
  if (r.result?.structuredContent?.echo?.x !== 7)
    throw new Error(`bad echo ${JSON.stringify(r)}`);
  c.close();
  return { response: r.result.structuredContent };
});

await acheck("initialize_handshake", async () => {
  const c = await Client.open(epA.path);
  c.id++;
  c.s.write(JSON.stringify({ jsonrpc: "2.0", id: c.id,
    method: "initialize", params: {} }) + "\n");
  const r = JSON.parse(await c.nextLine());
  if (r.result?.protocolVersion !== "2025-06-18")
    throw new Error(`bad initialize ${JSON.stringify(r)}`);
  c.close();
  return { protocolVersion: r.result.protocolVersion };
});

await acheck("malformed_parse_error", async () => {
  const c = await Client.open(epA.path);
  c.writeRaw("this is not json\n");
  const r = JSON.parse(await c.nextLine());
  if (r.error?.code !== -32700 || r.error?.data?.code !== "parse_error")
    throw new Error(`bad envelope ${JSON.stringify(r)}`);
  c.close();
  return { code: r.error.code, data: r.error.data.code };
});

await acheck("notification_dropped", async () => {
  const c = await Client.open(epA.path);
  c.s.write(JSON.stringify({ jsonrpc: "2.0",
    method: "notifications/initialized" }) + "\n");
  const alive = await c.call("echo", { after: "notification" });
  if (alive.result?.structuredContent?.echo?.after !== "notification")
    throw new Error("session broken after notification");
  c.close();
  return { dropped: true, session_alive: true };
});

await acheck("background_worker_turns", async () => {
  // poll_interval=100ms — turns accumulate in the shared cell.
  await sleep(550);
  const c = await Client.open(epA.path);
  const r = await c.call("turn_count", {});
  const turns = r.result?.structuredContent?.turns ?? 0;
  c.close();
  if (turns < 1) throw new Error(`no worker turns (${turns})`);
  return { turns_observed: turns, poll_ms: 100 };
});

await acheck("shutdown_drain_within_budget", async () => {
  const c = await Client.open(epA.path);
  const inflight = c.call("block_ms", { ms: 400 }); // worker blocks 400ms
  await sleep(50);
  sA.stopping.value = true;                        // signal-equivalent
  const t0 = Date.now();
  const res = await sA.promise;                    // must wait the block out
  const drainMs = Date.now() - t0;
  const r = await inflight; // response still delivered mid-shutdown
  if (!res.ok) throw new Error(`serve failed: ${res.error}`);
  if (drainMs < 300) throw new Error(`did not drain (${drainMs}ms)`);
  if (fs.existsSync(epA.path))
    throw new Error("socket file not removed");
  if (r.result?.structuredContent?.blocked !== 400)
    throw new Error("in-flight call lost");
  c.close();
  return { drain_ms: drainMs, in_flight_answered: true };
});

emit("peer_cred_foreign_uid", "NOT_RUN",
  "single UID on Box — reject branch is uid!==euid on real ucred; " +
  "same limitation as NM-061 spike");

// ================= serve B: frame budget + max_clients =============
const dB = join(dir, "sb");
fs.mkdirSync(dB, { mode: 0o700 });
const dbB = mkDb("dbB.sqlite");
const epB = await Endpoint.bind(join(dB, "svc.sock"), dbB);
const sB = serveCtx(epB, dbB, cfg({
  poll_interval: 60000, io_timeout: 600, max_clients: 1,
  max_frame_bytes: 256, stop_budget: 2000,
}));

await acheck("frame_over_cap_drops_session", async () => {
  const c = await Client.open(epB.path);
  c.writeRaw("x".repeat(300) + "\n"); // 301 bytes > 256 cap
  const closed = await Promise.race([c.closed, sleep(3000).then(() => "timeout")]);
  if (closed === "timeout") throw new Error("session not dropped");
  // service still alive — session error is not a service failure
  const c2 = await Client.open(epB.path);
  const r = await c2.call("echo", { ok: 1 });
  c.close(); c2.close();
  if (r.result?.structuredContent?.echo?.ok !== 1)
    throw new Error("service dead after cap drop");
  await sleep(200); // let c2's worker exit + reap free the single slot
  return { cap: 256, sent: 301, service_alive: true };
});

await acheck("incomplete_frame_deadline", async () => {
  await sleep(200); // single-slot endpoint: previous session reaped first
  const c = await Client.open(epB.path);
  const t0 = Date.now();
  c.writeRaw("partial-no-newline"); // < cap, never completes
  const closed = await Promise.race([c.closed, sleep(3000).then(() => "timeout")]);
  const elapsed = Date.now() - t0;
  c.close();
  if (closed === "timeout") throw new Error("session not dropped");
  if (elapsed < 500) throw new Error(`dropped too early ${elapsed}ms`);
  return { io_timeout_ms: 600, elapsed };
});

await acheck("max_clients_gate", async () => {
  await sleep(200); // single-slot endpoint: previous session reaped first
  const c1 = await Client.open(epB.path);
  await sleep(150); // c1 occupies the single slot
  const c2s = net.connect(epB.path); // raw connect — authorized, then dropped
  const closed = await new Promise((res) => {
    c2s.on("close", () => res("closed"));
    setTimeout(() => res("timeout"), 2000);
  });
  if (closed !== "closed") throw new Error("overflow client not dropped");
  const r = await c1.call("echo", { still: 1 });
  c1.close();
  if (r.result?.structuredContent?.echo?.still !== 1)
    throw new Error("slot-0 client broken");
  return { max_clients: 1, overflow_dropped: true };
});

sB.stopping.value = true;
{
  const res = await sB.promise;
  emit("serveB_clean_stop", res.ok ? "PASS" : "FAIL", res.error ?? null);
}

// ================= serve C: gate busy + budget expiry ===============
const dC = join(dir, "sc");
fs.mkdirSync(dC, { mode: 0o700 });
const dbC = mkDb("dbC.sqlite");
const epC = await Endpoint.bind(join(dC, "svc.sock"), dbC);
const sC = serveCtx(epC, dbC, cfg({
  poll_interval: 60000, io_timeout: 3000, max_clients: 4,
  stop_budget: 250,
}));

await acheck("worker_gate_busy", async () => {
  const c1 = await Client.open(epC.path);
  const c2 = await Client.open(epC.path);
  const slow = c1.call("gated", { ms: 800 }); // holds the gate
  await sleep(150);                           // ensure c1 acquired
  const r2 = await c2.call("gated", { ms: 10 });
  const r1 = await slow;
  c1.close(); c2.close();
  const code = r2.result?.structuredContent?.code;
  if (code !== "worker_busy")
    throw new Error(`expected worker_busy, got ${JSON.stringify(r2)}`);
  if (r1.result?.structuredContent?.held !== 800)
    throw new Error("holder did not complete");
  return { contender: "worker_busy", holder_completed: true };
});

await acheck("shutdown_budget_expired", async () => {
  const c = await Client.open(epC.path);
  const inflight = c.call("block_ms", { ms: 3000 }); // outlives budget
  await sleep(100);
  sC.stopping.value = true;
  const res = await sC.promise;
  if (res.ok) throw new Error("serve returned Ok despite busy worker");
  if (!/shutdown budget expired/.test(res.error))
    throw new Error(`wrong error: ${res.error}`);
  c.close();
  await inflight.catch(() => {});
  return { stop_budget_ms: 250, err: res.error };
});

// ================= serve D: db identity fail-closed =================
const dD = join(dir, "sd");
fs.mkdirSync(dD, { mode: 0o700 });
const dbD = mkDb("dbD.sqlite");
const epD = await Endpoint.bind(join(dD, "svc.sock"), dbD);
const sD = serveCtx(epD, dbD, cfg({
  poll_interval: 60000, io_timeout: 2000, max_clients: 4,
  stop_budget: 2000,
}));

await acheck("db_swap_stops_service", async () => {
  await sleep(50); // let the serve loop settle
  const fresh = join(dD, "swapped.sqlite");
  fs.writeFileSync(fresh, "different-bytes", { mode: 0o600 });
  fs.renameSync(fresh, dbD.path); // same path, new inode
  const res = await sD.promise;
  if (res.ok) throw new Error("serve survived a db swap");
  if (!/database identity changed; stop rather than switch databases/
        .test(res.error))
    throw new Error(`wrong error: ${res.error}`);
  return { err: res.error };
});

// ================= serve E: panic semantics =========================
const dE = join(dir, "se");
fs.mkdirSync(dE, { mode: 0o700 });
const dbE = mkDb("dbE.sqlite");
const epE = await Endpoint.bind(join(dE, "svc.sock"), dbE);
const sE = serveCtx(epE, dbE, cfg({
  poll_interval: 60000, io_timeout: 2000, max_clients: 4,
  stop_budget: 2000,
}));

await acheck("dispatch_panic_stops_service", async () => {
  const c = await Client.open(epE.path);
  c.call("panic_now", {}).catch(() => {});
  const res = await sE.promise;
  if (res.ok) throw new Error("serve survived a handler panic");
  if (!/MCP session panicked/.test(res.error))
    throw new Error(`wrong error: ${res.error}`);
  c.close();
  return { err: res.error };
});

// ---- WorkerGate unit checks (mod.rs:14-22 verbatim) ----
check("worker_gate_try_lock_stopping", () => {
  const g = new WorkerGate();
  const a = g.acquire();
  let busy = null;
  try { g.acquire(); } catch (e) { busy = e.code; }
  if (busy !== "worker_busy") throw new Error(`contend -> ${busy}`);
  a.release();
  const b = g.acquire();
  b.release();
  g.stop();
  let stop = null;
  try { g.acquire(); } catch (e) { stop = e.code; }
  if (stop !== "worker_stopping") throw new Error(`stopping -> ${stop}`);
  return { busy, stop };
});

// cleanup leases
for (const l of [db0, dbA, dbB, dbC, dbD, dbE]) {
  try { l.close(); } catch { /**/ }
}

emit("summary", failures === 0 ? "PASS" : "FAIL",
  `${failures} failures`);
process.exit(failures ? 1 : 0);
