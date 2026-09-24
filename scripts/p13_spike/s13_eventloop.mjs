// S13 proof: event-loop blocking pattern. A unix-socket echo responder
// on the MAIN thread is pinged serially by an external process.
// Scenario "main_inline": a 50ms blocking call on the main thread stalls
// every reply (control — shows the failure mode). Scenarios
// "worker_atomics"/"worker_sqlite": the same block inside a
// worker_threads Worker leaves the responder live. Models the Rust
// per-client-thread/serve split: blocking work (sqlite sync calls via
// better-sqlite3, or native addon calls) must run on workers, not the
// accept/respond loop.
// Usage: node scripts/p13_spike/s13_eventloop.mjs <private-dir>
import net from "node:net";
import { spawn } from "node:child_process";
import { Worker } from "node:worker_threads";
import { monitorEventLoopDelay } from "node:perf_hooks";
import { join } from "node:path";
import { emit, finish, dir } from "./common.mjs";

// Serial ping-pong: one outstanding 1-byte ping at a time; each reply
// completes an RTT sample. On socket close, print stats JSON and exit.
const PINGER = `
const net = require("net");
const s = net.connect(process.env.SOCK);
let t0 = null;
const rtts = [];
s.on("connect", () => { t0 = process.hrtime.bigint(); s.write("x"); });
s.on("error", () => {}); // ECONNRESET on teardown — 'close' still fires
s.on("data", () => {
  rtts.push(Number(process.hrtime.bigint() - t0) / 1e6);
  t0 = process.hrtime.bigint();
  s.write("x");
});
s.on("close", () => {
  rtts.sort((a, b) => a - b);
  const q = (p) => rtts.length
    ? rtts[Math.min(rtts.length - 1, Math.floor(p * rtts.length))]
    : null;
  console.log(JSON.stringify({ count: rtts.length,
    p50: q(0.5), p95: q(0.95), max: rtts[rtts.length - 1] ?? null }));
  process.exit(0);
});
setTimeout(() => {}, 60000);
`;

const BLOCK_MS = 50;

const blockMain = () => {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, BLOCK_MS);
};

const blockWorker = (src) => new Promise((resolve, reject) => {
  const w = new Worker(src, { eval: true });
  w.once("message", resolve);
  w.once("error", reject);
});

const WORKER_ATOMIC = `
const { parentPort } = require("worker_threads");
Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ${BLOCK_MS});
parentPort.postMessage({ blocked_ms: ${BLOCK_MS} });
`;

const WORKER_SQLITE = `
const { parentPort } = require("worker_threads");
const D = require("better-sqlite3");
const db = new D(":memory:");
db.exec("create table t(x)");
const ins = db.prepare("insert into t values (?)");
const t0 = Date.now(); let n = 0;
while (Date.now() - t0 < ${BLOCK_MS}) {
  for (let i = 0; i < 500; i++) ins.run(n++);
}
parentPort.postMessage({ blocked_ms: Date.now() - t0, rows: n });
`;

async function scenario(name, block) {
  const sockPath = join(dir, `echo-${name}.sock`);
  const conns = new Set();
  const server = net.createServer((s) => {
    conns.add(s);
    s.on("data", (d) => s.write(d));
    s.on("close", () => conns.delete(s));
  });
  await new Promise((r) => server.listen(sockPath, r));

  const child = spawn(process.execPath, ["-e", PINGER],
    { env: { ...process.env, SOCK: sockPath }, encoding: "utf8" });
  let out = "";
  child.stdout.on("data", (d) => (out += d));

  const eld = monitorEventLoopDelay();
  eld.enable();
  await new Promise((r) => setTimeout(r, 200)); // warmup
  const blockInfo = block ? await block() : { blocked_ms: 0 };
  await new Promise((r) => setTimeout(r, 400)); // post-block window
  eld.disable();

  for (const s of conns) s.destroy();
  server.close();
  const stats = await new Promise((resolve, reject) => {
    child.on("exit", () => {
      try { resolve(JSON.parse(out.trim().split("\n").pop())); }
      catch (e) { reject(new Error(`pinger output: ${out}`)); }
    });
    setTimeout(() => reject(new Error("pinger timeout")), 5000);
  });
  return { ...stats, block: blockInfo,
           event_loop_max_ms: +(eld.max / 1e6).toFixed(2) };
}

const idle = await scenario("idle", null);
emit("eventloop", "baseline_idle", "PASS", null, idle);

const main = await scenario("main_inline", async () => {
  const t0 = Date.now(); blockMain(); return { blocked_ms: Date.now() - t0 };
});
emit("eventloop", "main_thread_block_stalls_responder",
  main.max >= 40 ? "PASS" : "FAIL",
  `max RTT ${main.max?.toFixed(1)}ms during ${main.block.blocked_ms}ms inline block`,
  { ...main, note: "control case — the failure mode the service must avoid" });

const wa = await scenario("worker_atomics",
  () => blockWorker(WORKER_ATOMIC));
emit("eventloop", "worker_block_keeps_responder_live",
  wa.max < 40 ? "PASS" : "FAIL",
  `max RTT ${wa.max?.toFixed(1)}ms during ${wa.block.blocked_ms}ms worker block`,
  wa);

const ws = await scenario("worker_sqlite",
  () => blockWorker(WORKER_SQLITE));
emit("eventloop", "worker_sync_sqlite_keeps_responder_live",
  ws.max < 40 ? "PASS" : "FAIL",
  `max RTT ${ws.max?.toFixed(1)}ms during ${ws.block.blocked_ms}ms sqlite burst`,
  { ...ws, note: "better-sqlite3 is synchronous — the real service driver" });

finish();
