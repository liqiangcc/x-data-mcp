#!/usr/bin/env node
// NM-108 — sustained soak: acquire_service under c16 load for a
// fixed wall, then idle-drain. Samples VmRSS/VmHWM/FD/conns/calls/
// errors. Usage: node soak.mjs [workdir] [loadSecs]
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import { createHash } from "node:crypto";
import { crc32 } from "node:zlib";
import { join } from "node:path";
import { decodeRecord } from "../../src_node/parsers/gbbq_decode.mjs";

const ROOT = process.argv[2] || "/dev/shm/soak";
const LOAD_SECS = Number(process.argv[3] || 240);
const RUST_BIN = process.env.RUST_BIN || "target/release/acquire_service";
const CONC = 16;
const SAMPLE_MS = 5000;
const MIX = [["sync_status", {}], ["get_acquisition_capabilities", {}],
  ["get_acquisition_task", { task_id: "acq-cf4cda70a0ab959ca07122fc" }],
  ["check_official_updates", {}]];

// mirror fixture (identical to service_bench)
const dayRecord = (u) => {
  const r = Buffer.alloc(32); r.writeUInt32LE(u, 0);
  r.writeUInt32LE(100, 24); return r;
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
  const chunks = [], cd = []; let off = 0;
  for (const [name, data0] of members) {
    const data = Buffer.isBuffer(data0) ? data0 : Buffer.from(data0);
    const nb = Buffer.from(name, "latin1"); const crc = crc32(data);
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
    ["infoharbor_ex.code", Buffer.from("600006|n1|kw\r\n000001|n2|kw\r\n", "latin1")]]);
  writeZip(join(dir, "gbbq.zip"),
    [["gbbq", Buffer.concat([Buffer.from([1, 0, 0, 0]), Buffer.from(gbbqRecord())])]]);
  writeZip(join(dir, "hsjday.zip"), [
    ["sh\\lday\\sh600006.day", dayRecord(u32)],
    ["sz\\lday\\sz000001.day", dayRecord(u32)]]);
};

const proc = (pid) => {
  const s = fs.readFileSync(`/proc/${pid}/status`, "utf8");
  const g = (k) => Number(new RegExp(`${k}:\\s*(\\d+)`).exec(s)[1]);
  let fds = null;
  try { fds = fs.readdirSync(`/proc/${pid}/fd`).length; } catch { /**/ }
  return { rss_kb: g("VmRSS"), hwm_kb: g("VmHWM"), threads: g("Threads"),
    fds };
};

const startDaemon = async (impl, dir) => {
  const root = join(dir, "root"), sock = join(dir, "svc.sock");
  const mirror = join(dir, "mirror");
  fs.mkdirSync(dir, { recursive: true });
  fs.chmodSync(dir, 0o700);
  writeMirror(mirror);
  const seed = impl === "rust"
    ? spawnSync("target/fault_oracle", ["acquire_run", root, mirror], { encoding: "utf8" })
    : spawnSync("node", ["scripts/node_migration_p4/fault_helpers.mjs",
        "acquire_run", root, mirror], { encoding: "utf8" });
  if (seed.status !== 0) throw new Error(`seed ${impl}: ${seed.stdout}`);
  const cmd = impl === "rust" ? [RUST_BIN] : ["node", "src_node/bin/acquire_service.mjs"];
  const p = spawn(cmd[0], [...cmd.slice(1), root, sock, "--mirror", mirror],
    { stdio: ["ignore", "pipe", "pipe"] });
  for (let i = 0; i < 200; i++) {
    if (fs.existsSync(sock)) return { p, sock };
    if (p.exitCode !== null) throw new Error(`${impl} daemon died`);
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error("no socket");
};

const call = (s, id, method, params) => new Promise((res) => {
  let buf = "", done = false;
  const fin = (v) => { if (!done) { done = true;
    s.off("data", onData); s.off("close", onEnd); res(v); } };
  const onEnd = () => fin({ drop: true });
  const onData = (d) => {
    buf += d;
    const i = buf.indexOf("\n");
    if (i >= 0) {
      let r; try { r = JSON.parse(buf.slice(0, i)); } catch { r = { bad: true }; }
      fin(r.error ? { err: r.error } : { ok: true });
    }
  };
  s.on("data", onData); s.once("close", onEnd);
  s.write(JSON.stringify({ jsonrpc: "2.0", id, method: "tools/call",
    params: { name: method, arguments: params } }) + "\n");
});

const soak = async (impl) => {
  const dir = join(ROOT, impl);
  const { p, sock } = await startDaemon(impl, dir);
  let calls = 0, errors = 0, drops = 0, reconnects = 0;
  const samples = [];
  const stopAt = Date.now() + LOAD_SECS * 1000;
  const clientLoop = async (ci) => {
    let s = null;
    const ensure = async () => {
      for (;;) {
        try {
          const c = await new Promise((res, rej) => {
            const c0 = net.createConnection(sock);
            c0.once("connect", () => res(c0));
            c0.once("error", rej);
          });
          c.on("error", () => {});
          return c;
        } catch { await new Promise((r) => setTimeout(r, 50)); }
      }
    };
    s = await ensure();
    for (let i = 0; Date.now() < stopAt; i++) {
      const [m, params] = MIX[(ci + i) % MIX.length];
      const r = await call(s, i + 1, m, params);
      calls++;
      if (r.drop) { drops++; s.destroy(); s = await ensure(); reconnects++; }
      else if (r.err) errors++;
    }
    s.destroy();
  };
  const sampler = setInterval(() => {
    samples.push({ t_s: Math.round((Date.now() - t0) / 1000),
      phase: "load", ...proc(p.pid), calls, errors, drops });
  }, SAMPLE_MS);
  const t0 = Date.now();
  samples.push({ t_s: 0, phase: "load", ...proc(p.pid), calls: 0,
    errors: 0, drops: 0 });
  await Promise.all(Array.from({ length: CONC }, (_, ci) => clientLoop(ci)));
  const loadEnd = Date.now();
  samples.push({ t_s: Math.round((loadEnd - t0) / 1000), phase: "load_end",
    ...proc(p.pid), calls, errors, drops });
  // idle-drain leg
  for (const wait of [5, 15, 30, 60]) {
    await new Promise((r) => setTimeout(r, wait === 5 ? 5000 : wait === 15 ? 10000 : wait === 30 ? 15000 : 30000));
    samples.push({ t_s: Math.round((Date.now() - t0) / 1000),
      phase: `idle_${wait}s`, ...proc(p.pid), calls, errors, drops });
  }
  clearInterval(sampler);
  p.kill("SIGTERM");
  const exit = await new Promise((res) => {
    p.on("exit", (c) => res(c));
    setTimeout(() => { p.kill("SIGKILL"); res(null); }, 15000);
  });
  return { impl, samples, calls, errors, drops, reconnects, exit,
    load_secs: LOAD_SECS, concurrency: CONC };
};

console.log(JSON.stringify({ check: "env",
  code_sha: spawnSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).stdout.trim(),
  node: process.version, load_secs: LOAD_SECS, concurrency: CONC }));
for (const impl of ["rust", "node"]) {
  const r = await soak(impl);
  for (const s of r.samples)
    console.log(JSON.stringify({ check: "sample", impl, ...s }));
  console.log(JSON.stringify({ check: "verdict", ...r, samples: undefined }));
}
