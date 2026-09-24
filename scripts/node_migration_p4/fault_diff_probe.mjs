#!/usr/bin/env node
// NM-092 — fault-injection differential suite (Node vs Rust).
// Runs identical failure scenarios against both impls via public
// surfaces only (frame/fs internals are pub(super)/pub(crate), so
// those go through the real acquire_service daemons). Divergences are
// first-class findings — emitted as "DIVERGENCE", never auto-fixed.
// Usage: node fault_diff_probe.mjs <workdir>
//   env: ORACLE=<rust oracle bin> RUST_BIN=<rust acquire_service>
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import { createHash } from "node:crypto";
import { crc32 } from "node:zlib";
import { join } from "node:path";
import { decodeRecord } from "../../src_node/parsers/gbbq_decode.mjs";

const ROOT = process.argv[2] || "/dev/shm/fault-diff";
const ORACLE = process.env.ORACLE || "target/fault_oracle";
const RUST_BIN = process.env.RUST_BIN || "target/release/acquire_service";
const NODE_BIN = ["node", "src_node/bin/acquire_service.mjs"];
const HELPER = ["node", "scripts/node_migration_p4/fault_helpers.mjs"];
// canonical JSON — key order is an impl artifact, not a divergence.
const canon = (v) => JSON.stringify(v, (k, x) =>
  (x && typeof x === "object" && !Array.isArray(x))
    ? Object.fromEntries(Object.entries(x).sort()) : x);
const records = [];
const rec = (scenario, impl, outcome) =>
  records.push({ scenario, impl, ...outcome });
const verdict = (scenario, ok, rust, node, note = "") =>
  records.push({ scenario, impl: "diff",
    verdict: ok ? "MATCH" : "DIVERGENCE", rust, node, note });

// ---------- child drivers ----------
const runSync = (impl, sub, args) => {
  const cmd = impl === "rust" ? [ORACLE, sub, ...args] : [...HELPER, sub, ...args];
  const r = spawnSync(cmd[0], cmd.slice(1), { encoding: "utf8", timeout: 60000 });
  const line = (r.stdout || "").trim().split("\n").filter(Boolean).pop();
  return { exit: r.status, out: line ? JSON.parse(line) : null,
    stderr: (r.stderr || "").trim().slice(-200) };
};
const spawnHold = (impl, sub, args) => {
  const cmd = impl === "rust" ? [ORACLE, sub, ...args] : [...HELPER, sub, ...args];
  const p = spawn(cmd[0], cmd.slice(1), { stdio: ["pipe", "pipe", "pipe"] });
  return p;
};
const waitLine = (p, ms = 15000) => new Promise((res, rej) => {
  let buf = "";
  const to = setTimeout(() => rej(new Error("hold wait timeout")), ms);
  p.stdout.on("data", (d) => {
    buf += d;
    if (buf.includes("\n")) { clearTimeout(to); res(JSON.parse(buf)); }
  });
  p.on("exit", () => { clearTimeout(to); rej(new Error(`exited: ${buf} ${p.exitCode}`)); });
});

const mkDb = (p) => {
  // private 0600 regular file, nlink 1 — lease requirements
  fs.writeFileSync(p, "");
  fs.chmodSync(p, 0o600);
};

// ---------- fixture: minimal acquire mirror (store zips) ----------
const dayRecord = (dateU32) => {
  const r = Buffer.alloc(32);
  r.writeUInt32LE(dateU32, 0); r.writeUInt32LE(100, 24);
  return r;
};
const gbbqRecord = () => {
  for (let i = 0; ; i++) {
    const b = createHash("sha256").update(`fault-fixture-${i}`).digest().subarray(0, 29);
    try {
      const ev = decodeRecord(0, new Uint8Array(b));
      if (ev && typeof ev.code === "string") return new Uint8Array(b);
    } catch { /* next */ }
    if (i > 200000) throw new Error("no decryptable record");
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
    lh.writeUInt32LE(crc, 14);
    lh.writeUInt32LE(data.length, 18); lh.writeUInt32LE(data.length, 22);
    lh.writeUInt16LE(nb.length, 26); lh.writeUInt16LE(0, 28);
    chunks.push(lh, nb, data);
    const ce = Buffer.alloc(46);
    ce.writeUInt32LE(0x02014b50, 0);
    ce.writeUInt16LE(20, 4); ce.writeUInt16LE(20, 6);
    ce.writeUInt16LE(0, 8); ce.writeUInt16LE(0, 10);
    ce.writeUInt32LE(crc, 16);
    ce.writeUInt32LE(data.length, 20); ce.writeUInt32LE(data.length, 24);
    ce.writeUInt16LE(nb.length, 28); ce.writeUInt32LE(off, 42);
    cd.push(Buffer.concat([ce, nb]));
    off += 30 + nb.length + data.length;
  }
  const cdBuf = Buffer.concat(cd);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  eocd.writeUInt16LE(members.length, 8); eocd.writeUInt16LE(members.length, 10);
  eocd.writeUInt32LE(cdBuf.length, 12); eocd.writeUInt32LE(off, 16);
  fs.writeFileSync(path, Buffer.concat([...chunks, cdBuf, eocd]));
};
const writeMirror = (dir) => {
  fs.mkdirSync(dir, { recursive: true });
  const d = new Date(Date.now() + 8 * 3600_000);
  const u32 = d.getUTCFullYear() * 10000 + (d.getUTCMonth() + 1) * 100 + d.getUTCDate();
  fs.writeFileSync(join(dir, "base.zip"), "");
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

// ---------- scenario helpers ----------
const scenarioLeaseContention = async () => {
  const per = {};
  for (const impl of ["rust", "node"]) {
    const dir = join(ROOT, `lease-${impl}`);
    fs.mkdirSync(dir, { recursive: true });
    const db = join(dir, "l.db");
    mkDb(db);
    const holder = spawnHold(impl, "lease_hold", [db]);
    await waitLine(holder);
    const attempt = runSync(impl, "lease_try", [db]);
    holder.stdin.end(); // release
    await new Promise((r) => holder.on("exit", r));
    per[impl] = { held: true, try: attempt };
    rec("lease_contention", impl, attempt);
  }
  const r = per.rust.try, n = per.node.try;
  verdict("lease_contention",
    r.exit === n.exit && String(r.out?.error ?? "") === String(n.out?.error ?? ""),
    { exit: r.exit, error: r.out?.error }, { exit: n.exit, error: n.out?.error },
    "second writer while first holds");
};

const scenarioCommitKill = () => {
  const per = {};
  for (const impl of ["rust", "node"]) {
    const dir = join(ROOT, `ck-${impl}`);
    fs.mkdirSync(dir, { recursive: true });
    const db = join(dir, "k.db");
    mkDb(db);
    const killed = runSync(impl, "commit_kill", [db, "8", "4"]);
    const post = runSync(impl, "reopen_state", [db]);
    per[impl] = { killed: killed.exit, post: post.out };
    rec("commit_kill", impl, { kill_exit: killed.exit, ...post.out });
  }
  const same = canon(per.rust.post) === canon(per.node.post);
  verdict("commit_kill", same && per.rust.killed === per.node.killed,
    per.rust, per.node, "SIGKILL mid-BEGIN..COMMIT, reopen state");
};

const scenarioStaleSocket = async () => {
  const per = {};
  for (const impl of ["rust", "node"]) {
    const dir = join(ROOT, `sock-${impl}`);
    fs.mkdirSync(dir, { recursive: true });
    fs.chmodSync(dir, 0o700);
    const db = join(dir, "svc.db");
    mkDb(db);
    const holder = spawnHold(impl, "bind", [dir, "svc.db"]);
    await waitLine(holder);
    holder.kill("SIGKILL"); // owner dies; socket + owner marker remain
    await new Promise((r) => holder.on("exit", r));
    const marker = join(dir, "svc.sock.owner.json");
    const markerPre = fs.existsSync(marker)
      ? createHash("sha256").update(fs.readFileSync(marker)).digest("hex") : null;
    const rebind = spawnHold(impl, "bind", [dir, "svc.db"]);
    const b = await waitLine(rebind).catch((e) => ({ error: e.message }));
    rebind.stdin.end();
    await new Promise((r) => rebind.on("exit", r));
    const markerPost = fs.existsSync(marker)
      ? createHash("sha256").update(fs.readFileSync(marker)).digest("hex") : null;
    per[impl] = { rebind: b, markerSame: markerPre === markerPost };
    rec("stale_socket", impl, { rebind: b, marker_same: markerPre === markerPost });
  }
  verdict("stale_socket",
    Boolean(per.rust.rebind.bound) === Boolean(per.node.rebind.bound) &&
    per.rust.markerSame === per.node.markerSame,
    per.rust, per.node, "SIGKILL owner -> stale socket+marker -> rebind");
};

const scenarioSymlink = () => {
  const per = {};
  for (const impl of ["rust", "node"]) {
    const dir = join(ROOT, `sym-${impl}`);
    fs.mkdirSync(dir, { recursive: true });
    const real = join(dir, "real.db");
    mkDb(real);
    const link = join(dir, "link.db");
    try { fs.symlinkSync("real.db", link); } catch { /* exists */ }
    const r = runSync(impl, "lease_try", [link]);
    per[impl] = { exit: r.exit, error: r.out?.error ?? null };
    rec("symlink_path", impl, per[impl]);
  }
  verdict("symlink_path",
    per.rust.exit === per.node.exit &&
    per.rust.error === per.node.error,
    per.rust, per.node, "O_NOFOLLOW on symlinked db path");
};

const scenarioUnsupportedFs = () => {
  const per = {};
  for (const impl of ["rust", "node"]) {
    const dir = "/tmp"; // overlayfs — outside the whitelist
    const p = join(dir, `fault-ufs-${impl}-${process.pid}.db`);
    mkDb(p);
    const r = runSync(impl, "lease_try", [p]);
    per[impl] = { exit: r.exit, error: r.out?.error ?? null };
    rec("unsupported_fs", impl, per[impl]);
    fs.rmSync(p, { force: true });
  }
  verdict("unsupported_fs",
    per.rust.exit === per.node.exit && per.rust.error === per.node.error,
    per.rust, per.node, "lease on overlayfs /tmp (fail-closed)");
};

// real-daemon scenario: incomplete frame at EOF
const daemonCall = (sockPath, method, params, timeout = 15000) =>
  new Promise((res, rej) => {
    const s = net.createConnection(sockPath);
    let buf = "";
    const to = setTimeout(() => { s.destroy(); rej(new Error("timeout")); }, timeout);
    s.on("connect", () => s.write(JSON.stringify({
      jsonrpc: "2.0", id: 7, method, params }) + "\n"));
    s.on("data", (d) => {
      buf += d;
      const i = buf.indexOf("\n");
      if (i >= 0) { clearTimeout(to); s.destroy(); res(JSON.parse(buf.slice(0, i))); }
    });
    s.on("error", rej);
  });
const startDaemon = async (impl, dir) => {
  const root = join(dir, "root");
  const sock = join(dir, "svc.sock");
  const mirror = join(dir, "mirror");
  fs.mkdirSync(dir, { recursive: true });
  fs.chmodSync(dir, 0o700);
  writeMirror(mirror);
  const cmd = impl === "rust" ? [RUST_BIN] : NODE_BIN;
  const p = spawn(cmd[0], [...cmd.slice(1), root, sock, "--mirror", mirror],
    { stdio: ["ignore", "pipe", "pipe"] });
  for (let i = 0; i < 100; i++) {
    if (fs.existsSync(sock)) break;
    if (p.exitCode !== null) throw new Error(`daemon died: ${impl}`);
    await new Promise((r) => setTimeout(r, 100));
  }
  return { p, root, sock };
};
const scenarioFrameEof = async () => {
  const per = {};
  for (const impl of ["rust", "node"]) {
    const dir = join(ROOT, `frame-${impl}`);
    const { p, sock } = await startDaemon(impl, dir);
    // partial frame, then EOF — daemon must drop the conn and survive.
    await new Promise((res) => {
      const s = net.createConnection(sock);
      s.on("connect", () => { s.write(`{"jsonrpc":"2.0","id":1,"method":"tools/`); s.end(); });
      s.on("close", res); s.on("error", res);
    });
    let alive = null, after = null;
    try { after = await daemonCall(sock, "tools/list", {}); alive = true; }
    catch (e) { alive = false; after = e.message; }
    const exit = await new Promise((res) => {
      p.on("exit", (c, sig) => res({ code: c, signal: sig }));
      p.kill("SIGTERM");
      setTimeout(() => { p.kill("SIGKILL"); res({ code: null, signal: "timeout" }); }, 10000);
    });
    per[impl] = { alive, tools: Array.isArray(after?.result?.tools) ? after.result.tools.length : null, exit };
    rec("frame_eof", impl, per[impl]);
  }
  verdict("frame_eof",
    per.rust.alive === per.node.alive &&
    per.rust.tools === per.node.tools &&
    per.rust.exit.code === per.node.exit.code,
    per.rust, per.node, "partial frame + EOF; service survives, SIGTERM clean");
};

// acquire scenarios: corrupt generation reconcile + mid-verify interrupt
const scenarioCorruptGen = () => {
  const per = {};
  for (const impl of ["rust", "node"]) {
    const dir = join(ROOT, `gen-${impl}`);
    const root = join(dir, "root");
    const mirror = join(dir, "mirror");
    fs.mkdirSync(dir, { recursive: true });
    fs.chmodSync(dir, 0o700);
    writeMirror(mirror);
    const ran = runSync(impl, "acquire_run", [root, mirror]);
    rec("corrupt_gen_install", impl, { exit: ran.exit, tasks: ran.out?.tasks });
    // corrupt the installed generation's snapshot-manifest.json
    const gensDir = join(root, "generations");
    const gens = fs.existsSync(gensDir) ? fs.readdirSync(gensDir) : [];
    if (!gens.length) {
      per[impl] = { harness_error: `no generation installed: ${JSON.stringify(ran.out)}` };
      rec("corrupt_gen", impl, per[impl]);
      continue;
    }
    const man = join(gensDir, gens[0], "snapshot-manifest.json");
    const bytes = fs.readFileSync(man);
    bytes[10] ^= 0xff;
    fs.writeFileSync(man, bytes);
    // stray corrupt generation dir — reconcile adopts/records it.
    const stray = join(gensDir, "0".repeat(64));
    fs.mkdirSync(stray, { recursive: true });
    fs.writeFileSync(join(stray, "snapshot-manifest.json"), "{corrupt");
    const reopen = runSync(impl, "acquire_open", [root, mirror]);
    per[impl] = reopen.out;
    rec("corrupt_gen", impl, { exit: reopen.exit, generations: reopen.out?.generations });
  }
  // normalize: generation ids/embed timestamps legitimately differ
  // (manifest sha covers acquired_at) — compare state columns only.
  const normTasks = (ts) => (ts ?? []).map((t) => {
    const c = t.split("|");
    return [c[0], c[1], c[2], c[4], c[5]].join("|"); // id,kind,state,stage,error
  });
  const normGens = (gs) => (gs ?? []).map((g) => g.split("|").slice(1).join("|"));
  verdict("corrupt_gen",
    canon(normGens(per.rust?.generations)) === canon(normGens(per.node?.generations)) &&
    canon(normTasks(per.rust?.tasks)) === canon(normTasks(per.node?.tasks)),
    { generations: normGens(per.rust?.generations), tasks: normTasks(per.rust?.tasks) },
    { generations: normGens(per.node?.generations), tasks: normTasks(per.node?.tasks) },
    "corrupted manifest + stray gen dir -> reconcile state");
};

const scenarioMidVerify = () => {
  const per = {};
  for (const impl of ["rust", "node"]) {
    const dir = join(ROOT, `mv-${impl}`);
    const root = join(dir, "root");
    const mirror = join(dir, "mirror");
    fs.mkdirSync(dir, { recursive: true });
    fs.chmodSync(dir, 0o700);
    writeMirror(mirror);
    runSync(impl, "acquire_run", [root, mirror]);
    // simulate verify interrupted mid-flight: task row running +
    // generation stays installed (no decision persisted). The durable
    // residue is what reconcile must realign — identical on both impls.
    spawnSync("python3", ["-c",
      `import sqlite3; c=sqlite3.connect("${root}/acquire.sqlite");
c.execute("UPDATE acquisition_tasks SET state='running', stage='verify' WHERE state='succeeded'"); c.commit();`]);
    const reopen = runSync(impl, "acquire_open", [root, mirror]);
    per[impl] = reopen.out;
    // F-001 assertion: post-reconcile syncStatus must serialize and its
    // u64 columns must be JSON numbers (serde u64 parity).
    const bdt = reopen.out?.status?.inflight_tasks?.[0]?.bytes_done;
    rec("mid_verify", impl, { exit: reopen.exit, error: reopen.out?.error,
      stderr: reopen.stderr, tasks: reopen.out?.tasks,
      generations: reopen.out?.generations,
      sync_status_ok: reopen.out?.opened === true,
      bytes_done_type: bdt === undefined ? "absent" : typeof bdt });
  }
  const normT = (ts) => (ts ?? []).map((t) => {
    const c = t.split("|");
    return [c[0], c[1], c[2], c[4], c[5]].join("|");
  });
  const normG = (gs) => (gs ?? []).map((g) => g.split("|").slice(1).join("|"));
  const bdtType = (s) =>
    s?.status?.inflight_tasks?.[0]?.bytes_done === undefined ? "absent"
      : typeof s.status.inflight_tasks[0].bytes_done;
  verdict("mid_verify",
    canon(normT(per.rust?.tasks)) === canon(normT(per.node?.tasks)) &&
    canon(normG(per.rust?.generations)) === canon(normG(per.node?.generations)) &&
    canon(per.rust?.error ?? null) === canon(per.node?.error ?? null) &&
    bdtType(per.rust) === bdtType(per.node),
    { tasks: normT(per.rust?.tasks), generations: normG(per.rust?.generations),
      error: per.rust?.error ?? null,
      bytes_done_type: per.rust?.status?.inflight_tasks?.[0]?.bytes_done },
    { tasks: normT(per.node?.tasks), generations: normG(per.node?.generations),
      error: per.node?.error ?? null,
      bytes_done_type: per.node?.status?.inflight_tasks?.[0]?.bytes_done },
    "running task + installed gen, no decision -> reconcile realign");
};

// ---------- run ----------
const t0 = Date.now();
await scenarioLeaseContention();
scenarioCommitKill();
await scenarioStaleSocket();
scenarioSymlink();
scenarioUnsupportedFs();
await scenarioFrameEof();
scenarioCorruptGen();
scenarioMidVerify();

let divergences = 0;
for (const r of records) {
  if (r.verdict === "DIVERGENCE") divergences++;
  console.log(JSON.stringify(r));
}
console.log(JSON.stringify({ check: "summary", elapsed_ms: Date.now() - t0,
  scenarios: 8, divergences, records: records.length }));
process.exit(0); // divergences are findings, not failures
