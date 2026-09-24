#!/usr/bin/env node
// NM-103 — P5 concurrent service bench: acquire_service under
// 1/4/16/64 socket clients, Node vs Rust. Numbers only.
// Usage: node service_bench.mjs [workdir] [itersPerClient]
//   env: RUST_BIN=<rust acquire_service> NODE=<node binary>
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import { createHash } from "node:crypto";
import { crc32 } from "node:zlib";
import { join } from "node:path";
import { decodeRecord } from "../../src_node/parsers/gbbq_decode.mjs";

const ROOT = process.argv[2] || "/dev/shm/svc-bench";
const ITERS = Number(process.argv[3] || 200);
const WARMUP = 20;
const RUST_BIN = process.env.RUST_BIN || "target/release/acquire_service";
const CONCURRENCIES = [1, 4, 16, 64];
const TASK_ID = "acq-cf4cda70a0ab959ca07122fc"; // sha256("req-acq-1")[:24]
const REQ_ID = "req-acq-1";
const MIX = [
  ["sync_status", {}],
  ["get_acquisition_capabilities", {}],
  ["get_acquisition_task", { task_id: TASK_ID }],
  ["get_acquisition_request", { request_id: REQ_ID }],
  ["sync_status", {}],
  ["check_official_updates", {}],
];

// ---------- mirror fixture (same as fault probe) ----------
const dayRecord = (u) => {
  const r = Buffer.alloc(32);
  r.writeUInt32LE(u, 0); r.writeUInt32LE(100, 24); return r;
};
const gbbqRecord = () => {
  for (let i = 0; ; i++) {
    const b = createHash("sha256").update(`fault-fixture-${i}`).digest().subarray(0, 29);
    try {
      const ev = decodeRecord(0, new Uint8Array(b));
      if (ev && typeof ev.code === "string") return new Uint8Array(b);
    } catch { /* next */ }
    if (i > 200000) throw new Error("no record");
  }
};
const writeZip = (path, members) => {
  const chunks = [], cd = [];
  let off = 0;
  for (const [name, data0] of members) {
    const data = Buffer.isBuffer(data0) ? data0 : Buffer.from(data0);
    const nb = Buffer.from(name, "latin1");
    const crc = crc32(data);
    const lh = Buffer.alloc(30);
    lh.writeUInt32LE(0x04034b50, 0); lh.writeUInt16LE(20, 4);
    lh.writeUInt16LE(0, 6); lh.writeUInt16LE(0, 8);
    lh.writeUInt32LE(crc, 14); lh.writeUInt32LE(data.length, 18);
    lh.writeUInt32LE(data.length, 22); lh.writeUInt16LE(nb.length, 26);
    lh.writeUInt16LE(0, 28); chunks.push(lh, nb, data);
    const ce = Buffer.alloc(46);
    ce.writeUInt32LE(0x02014b50, 0); ce.writeUInt16LE(20, 4);
    ce.writeUInt16LE(20, 6); ce.writeUInt16LE(0, 8); ce.writeUInt16LE(0, 10);
    ce.writeUInt32LE(crc, 16); ce.writeUInt32LE(data.length, 20);
    ce.writeUInt32LE(data.length, 24); ce.writeUInt16LE(nb.length, 28);
    ce.writeUInt32LE(off, 42); cd.push(Buffer.concat([ce, nb]));
    off += 30 + nb.length + data.length;
  }
  const cdBuf = Buffer.concat(cd);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0); eocd.writeUInt16LE(members.length, 8);
  eocd.writeUInt16LE(members.length, 10); eocd.writeUInt32LE(cdBuf.length, 12);
  eocd.writeUInt32LE(off, 16);
  fs.writeFileSync(path, Buffer.concat([...chunks, cdBuf, eocd]));
};
const writeMirror = (dir) => {
  fs.mkdirSync(dir, { recursive: true });
  const d = new Date(Date.now() + 8 * 3600e3);
  const u32 = d.getUTCFullYear() * 1e4 + (d.getUTCMonth() + 1) * 100 + d.getUTCDate();
  writeZip(join(dir, "base.zip"), [
    ["tdxhy.cfg", Buffer.from("1|600006|T090302|||X6102\r\n0|000001|T1001|||X500102\r\n", "latin1")],
    ["infoharbor_ex.code", Buffer.from("600006|n1|kw\r\n000001|n2|kw\r\n", "latin1")],
  ]);
  writeZip(join(dir, "gbbq.zip"),
    [["gbbq", Buffer.concat([Buffer.from([1, 0, 0, 0]), Buffer.from(gbbqRecord())])]]);
  writeZip(join(dir, "hsjday.zip"), [
    ["sh\\lday\\sh600006.day", dayRecord(u32)],
    ["sz\\lday\\sz000001.day", dayRecord(u32)],
  ]);
};

// ---------- client ----------
// one call, await single line — resolves null on drop (daemon closes
// over-capacity conns: max_clients=16 on both impls).
const call = (s, id, method, params) => new Promise((res) => {
  let buf = "", done = false;
  const fin = (v) => {
    if (done) return; done = true;
    s.off("data", onData); s.off("close", onEnd); res(v);
  };
  const onEnd = () => fin(null);
  const onData = (d) => {
    buf += d;
    const i = buf.indexOf("\n");
    if (i >= 0) fin(JSON.parse(buf.slice(0, i)));
  };
  s.on("data", onData); s.once("close", onEnd);
  s.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
});
const tracked = (s) => {
  s.on("error", () => {});
  return s;
};
const connectTimed = (sock) => new Promise((res, rej) => {
  const t0 = performance.now();
  const s = net.createConnection(sock);
  s.once("connect", () => res({ s, ms: performance.now() - t0 }));
  s.once("error", rej);
});
const rssKb = (pid) => {
  try {
    const s = fs.readFileSync(`/proc/${pid}/status`, "utf8");
    return Number(/VmHWM:\s*(\d+)/.exec(s)[1]);
  } catch { return null; }
};

const startDaemon = async (impl, dir) => {
  const root = join(dir, "root"), sock = join(dir, "svc.sock");
  const mirror = join(dir, "mirror");
  fs.mkdirSync(dir, { recursive: true });
  fs.chmodSync(dir, 0o700);
  writeMirror(mirror);
  // seed a completed task via the oracle/helper so task reads hit data
  const seed = impl === "rust"
    ? spawnSync("target/fault_oracle", ["acquire_run", root, mirror], { encoding: "utf8" })
    : spawnSync("node", ["scripts/node_migration_p4/fault_helpers.mjs",
        "acquire_run", root, mirror], { encoding: "utf8" });
  if (seed.status !== 0) throw new Error(`seed ${impl}: ${seed.stdout} ${seed.stderr}`);
  const cmd = impl === "rust" ? [RUST_BIN] : ["node", "src_node/bin/acquire_service.mjs"];
  const p = spawn(cmd[0], [...cmd.slice(1), root, sock, "--mirror", mirror],
    { stdio: ["ignore", "pipe", "pipe"] });
  for (let i = 0; i < 200; i++) {
    if (fs.existsSync(sock)) return { p, sock };
    if (p.exitCode !== null) throw new Error(`${impl} daemon died`);
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error(`${impl} socket never appeared`);
};

const benchC = async (impl, conc) => {
  const dir = join(ROOT, `${impl}-c${conc}`);
  const { p, sock } = await startDaemon(impl, dir);
  const rss0 = rssKb(p.pid);
  const lat = [];
  // each virtual client owns a slot; over max_clients the daemon drops
  // conns — client reconnects with jitter and retries. drops/retries
  // are themselves evidence of the capacity contract.
  let drops = 0, reconnects = 0;
  const connMs = [];
  const clientLoop = async (ci) => {
    let s = null;
    const ensure = async () => {
      while (true) {
        try {
          const c = await connectTimed(sock);
          connMs.push(c.ms);
          return tracked(c.s);
        } catch {
          await new Promise((r) => setTimeout(r, 20 + Math.random() * 60));
        }
      }
    };
    s = await ensure();
    // warmup on first conn
    for (let i = 0; i < WARMUP; i++) {
      const r = await call(s, 0, "sync_status", {});
      if (r === null) { drops++; s.destroy(); s = await ensure(); reconnects++; }
    }
    for (let i = 0; i < ITERS; i++) {
      const [m, params] = MIX[(ci + i) % MIX.length];
      const a = performance.now();
      const r = await call(s, i + 1, m, params);
      if (r === null) { // dropped mid-call — not a latency sample
        drops++; s.destroy(); s = await ensure(); reconnects++; i--;
        continue;
      }
      lat.push(performance.now() - a);
    }
    s.destroy();
  };
  const t0 = performance.now();
  await Promise.all(Array.from({ length: conc }, (_, ci) => clientLoop(ci)));
  const wall = performance.now() - t0;
  const peakLoad = rssKb(p.pid);
  lat.sort((a, b) => a - b);
  const pct = (q) => lat[Math.min(lat.length - 1, Math.floor(q * lat.length))];
  // drain under (now-idle) connections still open
  const dt0 = performance.now();
  const drainP = new Promise((res) => p.on("exit", (c) => res(c)));
  p.kill("SIGTERM");
  const code = await Promise.race([drainP,
    new Promise((r) => setTimeout(() => { p.kill("SIGKILL"); r(null); }, 15000))]);
  const drainMs = performance.now() - dt0;
  return {
    impl, concurrency: conc, iters: ITERS, warmup: WARMUP,
    requests: conc * ITERS, wall_ms: +wall.toFixed(1),
    req_per_s: +(conc * ITERS / wall * 1000).toFixed(0),
    p50_us: +(pct(0.5) * 1000).toFixed(0), p95_us: +(pct(0.95) * 1000).toFixed(0),
    max_us: +(lat[lat.length - 1] * 1000).toFixed(0),
    mean_us: +(lat.reduce((a, b) => a + b) / lat.length * 1000).toFixed(0),
    conn_ms_mean: +(connMs.reduce((a, b) => a + b) / connMs.length).toFixed(2),
    conn_ms_max: +Math.max(...connMs).toFixed(2),
    conn_total: connMs.length, drops, reconnects,
    rss_kb_idle: rss0, rss_kb_peak: peakLoad,
    sigterm_exit: code, drain_ms: +drainMs.toFixed(0),
  };
};

console.log(JSON.stringify({ check: "env",
  code_sha: spawnSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).stdout.trim(),
  node: process.version,
  rustc: spawnSync("rustc", ["--version"], { encoding: "utf8", env: { ...process.env, PATH: `${process.env.HOME}/.cargo/bin:${process.env.PATH}` } }).stdout.trim(),
  uname: spawnSync("uname", ["-srvm"], { encoding: "utf8" }).stdout.trim(),
  iters: ITERS, warmup: WARMUP, concurrencies: CONCURRENCIES }));
for (const impl of ["rust", "node"]) {
  for (const c of CONCURRENCIES) {
    const row = await benchC(impl, c);
    console.log(JSON.stringify({ check: "bench", ...row }));
  }
}
