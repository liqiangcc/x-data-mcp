#!/usr/bin/env node
// NM-107 — per-worker heap composition diagnosis.
// Spawns real worker_entry workers and reports memoryUsage + V8 heap
// stats at each lifecycle stage: spawn -> module load -> factory ->
// N dispatches -> idle.
import { Worker } from "node:worker_threads";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
import { spawnSync, execSync } from "node:child_process";
import fs from "node:fs";

const ROOT = process.argv[2] || "/dev/shm/heap-diag";
fs.rmSync(ROOT, { recursive: true, force: true });
fs.mkdirSync(ROOT, { recursive: true });
fs.chmodSync(ROOT, 0o700);

// seed a store via the helper (task content irrelevant — heap only)
function join0(n) { return resolve(ROOT, n); }
fs.mkdirSync(join0("mirror"), { recursive: true });
const seeded = spawnSync("node", ["scripts/node_migration_p4/fault_helpers.mjs",
  "acquire_open", join0("root"), join0("mirror")], { encoding: "utf8" });
if (seeded.status !== 0)
  throw new Error(`seed failed: ${seeded.stdout} ${seeded.stderr}`);

const mem = (pid) => {
  const s = fs.readFileSync(`/proc/${pid}/status`, "utf8");
  const g = (k) => Number(new RegExp(`${k}:\\s*(\\d+)`).exec(s)[1]);
  return { vmRss: g("VmRSS"), vmHwm: g("VmHWM") };
};

const SAB = new SharedArrayBuffer(64);
const spawn = () => new Worker(
  new URL("../../src_node/local_runtime/worker_entry.mjs", import.meta.url), {
    workerData: {
      role: "client",
      handlerModule: pathToFileURL(resolve("src_node/bin/acquire_handler.mjs")).href,
      handlerExport: "createHandler",
      handlerArgs: { root: join0("root"),
        source: { type: "mirror", dir: join0("mirror") } },
      dispatchModule: pathToFileURL(resolve("src_node/mcp/transport.mjs")).href,
      dispatchExport: "handleJson",
      gateSab: SAB, lease: null, pollIntervalMs: 1000 },
  });

const stage = (w, name) => new Promise((res) => {
  const id = Math.random();
  const on = (m) => {
    if (m.type === "mem" && m.id === id) { w.off("message", on); res(m.mem); }
  };
  w.on("message", on);
  w.postMessage({ type: "mem_probe", id });
  setTimeout(() => res(null), 3000);
});

const w = spawn();
w.on("error", (e) => { console.error("worker error:", e.message); });
w.on("exit", (c) => console.error("worker exit", c));
const first = await new Promise((r) => w.once("message", r));
console.log("first msg:", JSON.stringify(first));
const readyMem = await stage(w, "ready");
w.postMessage({ type: "assign" });
await new Promise((res) => {
  const on = (m) => { if (m.type === "assigned") { w.off("message", on); res(); } };
  w.on("message", on);
});
const assignMem = await stage(w, "assigned");
// N dispatches
const call = (i) => new Promise((res) => {
  const on = (m) => { if (m.type === "response" && m.id === i) { w.off("message", on); res(); } };
  w.on("message", on);
  w.postMessage({ type: "frame", id: i,
    bytes: new TextEncoder().encode(JSON.stringify(
      { jsonrpc: "2.0", id: i, method: "tools/call",
        params: { name: "sync_status", arguments: {} } }) + "\n") });
});
for (let i = 0; i < 200; i++) await call(i);
const loadMem = await stage(w, "post-200-calls");
w.postMessage({ type: "release" });
await new Promise((res) => {
  const on = (m) => { if (m.type === "idle") { w.off("message", on); res(); } };
  w.on("message", on);
});
const idleMem = await stage(w, "released");
console.log(JSON.stringify({ ready: readyMem, assigned: assignMem,
  post_200_calls: loadMem, released_idle: idleMem }, null, 1));
await w.terminate();
