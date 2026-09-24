// NM-071 probe — acquire_service bin end-to-end: arg-surface
// byte-compare vs the Rust binary, then a live unix-socket MCP client
// drives both daemons through the same call sequence (normalized diff).
// JSONL out. Usage: node acquire_bin_probe.mjs <dir> [rust-bin]
import fs from "node:fs";
import net from "node:net";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { crc32 } from "node:zlib";
import { spawn, spawnSync } from "node:child_process";
import { decodeRecord } from "../../src_node/parsers/gbbq_decode.mjs";

const dir = process.argv[2];
// privateMetadata: runtime dirs must be owner-private — the probe dir
// tree lives entirely under `dir`, made 0700 like a deployment root.
fs.mkdirSync(dir, { recursive: true });
fs.chmodSync(dir, 0o700);
const RUST_BIN = process.argv[3] ??
  new URL("../../target/debug/acquire_service", import.meta.url).pathname;
const NODE_BIN = new URL("../../src_node/bin/acquire_service.mjs",
  import.meta.url).pathname;
const GOLDEN = new URL(
  "../../evidence/node-migration/p4/acquire_api/tools-golden.json",
  import.meta.url);

let failures = 0;
const emit = (id, status, detail, evidence = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ id, status, detail, evidence }));
};
const acheck = async (id, fn) => {
  try { emit(id, "PASS", null, await fn()); }
  catch (e) { emit(id, "FAIL", `${e.code ?? ""} ${e.message}`); }
};

// ---------- fixtures ----------

function writeZip(path, members) {
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
}
const dayRecord = (d) => {
  const r = Buffer.alloc(32);
  r.writeUInt32LE(d, 0); r.writeUInt32LE(100, 24); return r;
};
const dayMember = (ds) => Buffer.concat(ds.map(dayRecord));
function gbbqRecord() {
  for (let i = 0; ; i++) {
    const rec = createHash("sha256")
      .update(`gbbq-fixture-${i}`).digest().subarray(0, 29);
    try {
      if (typeof decodeRecord(0, new Uint8Array(rec)).code === "string")
        return new Uint8Array(rec);
    } catch { /* next */ }
    if (i > 200000) throw new Error("no decryptable record");
  }
}
const gbbqMember = (recs) => {
  const b = Buffer.alloc(4); b.writeUInt32LE(recs.length, 0);
  return Buffer.concat([b, ...recs.map((r) => Buffer.from(r))]);
};
const todayU32 = () => {
  const d = new Date(Date.now() + 8 * 3600_000);
  return d.getUTCFullYear() * 10000 + (d.getUTCMonth() + 1) * 100 + d.getUTCDate();
};
function writeMirror(dir2, lastBar) {
  fs.mkdirSync(dir2, { recursive: true });
  const tdxhy = Buffer.from(
    "1|600006|T090302|||X6102\r\n0|000001|T1001|||X500102\r\n", "latin1");
  const ex = Buffer.from("600006|n1|kw\r\n000001|n2|kw\r\n", "latin1");
  writeZip(join(dir2, "base.zip"),
    [["tdxhy.cfg", tdxhy], ["infoharbor_ex.code", ex]]);
  writeZip(join(dir2, "gbbq.zip"), [["gbbq", gbbqMember([gbbqRecord()])]]);
  writeZip(join(dir2, "hsjday.zip"), [
    ["sh\\lday\\sh600006.day", dayMember([lastBar])],
    ["sz\\lday\\sz000001.day", dayMember([lastBar])],
  ]);
}

// ---------- client ----------

function mcpClient(sock) {
  return new Promise((resolve, reject) => {
    const s = net.connect(sock);
    let buf = Buffer.alloc(0);
    const waiters = [];
    s.on("data", (d) => {
      buf = Buffer.concat([buf, d]);
      let i;
      while ((i = buf.indexOf(0x0a)) !== -1) {
        const line = buf.subarray(0, i).toString("utf8");
        buf = buf.subarray(i + 1);
        if (waiters.length) waiters.shift()(line);
      }
    });
    s.on("error", reject);
    s.on("connect", () => resolve({
      call: (method, params) => new Promise((res2, rej2) => {
        const t = setTimeout(() => rej2(new Error("resp timeout")), 20000);
        waiters.push((line) => { clearTimeout(t); res2(JSON.parse(line)); });
        s.write(JSON.stringify({
          jsonrpc: "2.0", id: waiters.length, method, params }) + "\n");
      }),
      close: () => s.destroy(),
    }));
  });
}
const waitFor = async (path, ms = 15000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    if (fs.existsSync(path)) return true;
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error(`timeout waiting for ${path}`);
};

// normalize for cross-impl compare: timestamps + 64-hex digests vary
// (wall clock inside manifest bytes → different generation ids).
const norm = (v) => JSON.stringify(v)
  .replace(/[0-9a-f]{64}/g, "<H>")
  .replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})/g, "<T>");

// ---------- process helpers ----------

const spawnBin = (bin, isNode, args) =>
  spawn(isNode ? "node" : bin, isNode ? [bin, ...args] : args,
    { stdio: ["ignore", "pipe", "pipe"] });
const runArgs = (bin, isNode, args) =>
  spawnSync(isNode ? "node" : bin, isNode ? [bin, ...args] : args,
    { encoding: "utf8", timeout: 30000 });

// ==================== arg surface (differential) ====================

const argCases = [
  ["--version"],
  [],
  ["--poll-ms"],
  ["--poll-ms", "abc", "r", "s"],
  ["--poll-ms", "5", "r", "s"],
  ["--poll-ms", "70000", "r", "s"],
  ["--poll-ms", "", "r", "s"],
  ["--mirror"],
  ["only-one-positional"],
];
for (const [i, args] of argCases.entries()) {
  await acheck(`args_${i}_${args[0] ?? "none"}`, async () => {
    const rust = runArgs(RUST_BIN, false, args);
    const node = runArgs(NODE_BIN, true, args);
    const rOut = (rust.stdout ?? "") + (rust.stderr ?? "");
    const nOut = (node.stdout ?? "") + (node.stderr ?? "");
    const shaNorm = (t) => t.replace(/[0-9a-f]{40}/g, "<SHA>");
    if (shaNorm(rOut.trim()) !== shaNorm(nOut.trim()))
      throw new Error(
        `rust=${JSON.stringify(rOut.trim())} node=${JSON.stringify(nOut.trim())}`);
    if ((rust.status ?? -1) !== (node.status ?? -1))
      throw new Error(`exit rust=${rust.status} node=${node.status}`);
    return { exit: node.status, out: shaNorm(nOut.trim()).slice(0, 80) };
  });
}

// ==================== shared client drive ====================

// One deterministic call sequence; returns {label: structuredContent}
// for cross-impl comparison. request_ids carry `tag` (same across impls
// so task_id sha stays identical).
async function driveService(client, tag) {
  const out = {};
  const call = (name, args) => client.call("tools/call",
    { name, arguments: args });
  out.tools = (await client.call("tools/list", {})).result.tools;
  out.check = (await call("check_official_updates",
    { contract_version: "v1" })).result;
  out.caps0 = (await call("get_acquisition_capabilities",
    { contract_version: "v1" })).result;
  out.accept = (await call("acquire_generation",
    { contract_version: "v1", request_id: `req-${tag}` })).result;
  for (let i = 0; i < 16; i++) {
    const t = (await call("run_worker_once", {})).result.structuredContent;
    if (t.idle) break;
  }
  out.status = (await call("sync_status",
    { contract_version: "v1" })).result.structuredContent;
  const gid = out.status.generations[0].generation_id;
  out.verify = (await call("verify_generation", {
    contract_version: "v1", request_id: `v-${tag}`, generation_id: gid,
  })).result.structuredContent;
  out.activate = (await call("activate_generation", {
    contract_version: "v1", request_id: `a-${tag}`, generation_id: gid,
  })).result.structuredContent;
  out.caps1 = (await call("get_acquisition_capabilities",
    { contract_version: "v1" })).result.structuredContent;
  // refusal outcome (normal result, not isError): the generation is
  // ACTIVE here — verbatim guard order refuses generation_active
  // before the token check (token-mismatch proven in the govern probe).
  out.refused = (await call("retire_generation", {
    contract_version: "v1", request_id: `r-${tag}`, generation_id: gid,
    confirm_token: "0".repeat(64) })).result.structuredContent;
  // mapped error (isError tool result)
  out.miss = (await call("get_acquisition_task",
    { contract_version: "v1", task_id: "acq-missing" })).result;
  out.unknown = (await call("bogus", {})).result;
  return out;
}

// ==================== e2e: Node service ====================

const mirror = join(dir, "mirror"); writeMirror(mirror, todayU32());
const procs = [];
const startService = async (bin, isNode, root, sock, extra = []) => {
  const p = spawnBin(bin, isNode, [root, sock, ...extra]);
  p.stderr.on("data", () => {});
  procs.push(p);
  await waitFor(sock);
  return p;
};
const stopService = async (p) => {
  const exited = new Promise((r) => p.on("exit", (c, s) => r({ c, s })));
  p.kill("SIGTERM");
  return Promise.race([
    exited, new Promise((r) => setTimeout(() => r(null), 20000))]);
};

await acheck("e2e_node_full_flow", async () => {
  const proc = await startService(NODE_BIN, true,
    join(dir, "root-node"), join(dir, "svc-node.sock"),
    ["--mirror", mirror, "--poll-ms", "50"]);
  const client = await mcpClient(join(dir, "svc-node.sock"));
  const out = await driveService(client, "x");
  if (out.tools.length !== 10) throw new Error(`tools ${out.tools.length}`);
  if (out.caps0.structuredContent.runtime_readiness !== "blocked")
    throw new Error("fresh caps not blocked");
  if (out.accept.structuredContent.task_id == null) throw new Error("accept");
  if (out.status.generations.length !== 1) throw new Error("no gen");
  if (out.verify.state !== "consistency_verified")
    throw new Error(`verify ${out.verify.state}`);
  if (out.activate.state !== "active")
    throw new Error(`activate ${out.activate.state}`);
  if (out.caps1.runtime_readiness !== "ready") throw new Error("caps1");
  if (out.refused.state !== "refused" ||
      out.refused.reason_code !== "generation_active")
    throw new Error(`refused ${JSON.stringify(out.refused)}`);
  if (out.miss.isError !== true) throw new Error("miss not isError");
  if (out.unknown.isError !== true) throw new Error("unknown not isError");
  const res = await stopService(proc);
  if (res === null || res.c !== 0)
    throw new Error(`shutdown ${JSON.stringify(res)}`);
  await new Promise((r) => setTimeout(r, 200));
  if (fs.existsSync(join(dir, "svc-node.sock")))
    throw new Error("socket not cleaned");
  return { tools: 10, shutdown: res.c };
});

// ==================== differential: Rust vs Node ====================

await acheck("e2e_differential_node_vs_rust", async () => {
  const procN = await startService(NODE_BIN, true,
    join(dir, "root-dn"), join(dir, "svc-dn.sock"), ["--mirror", mirror]);
  const procR = await startService(RUST_BIN, false,
    join(dir, "root-dr"), join(dir, "svc-dr.sock"), ["--mirror", mirror]);
  const outN = await driveService(await mcpClient(join(dir, "svc-dn.sock")), "d");
  const outR = await driveService(await mcpClient(join(dir, "svc-dr.sock")), "d");
  // tools/list deep-equals the captured golden on both impls
  const golden = JSON.parse(fs.readFileSync(GOLDEN, "utf8"));
  const sortDeep = (v) => Array.isArray(v) ? v.map(sortDeep)
    : v && typeof v === "object"
      ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sortDeep(v[k])]))
      : v;
  for (const [tag, tools] of [["node", outN.tools], ["rust", outR.tools]]) {
    if (JSON.stringify(sortDeep(tools)) !== JSON.stringify(sortDeep(golden)))
      throw new Error(`${tag} tools/list diverged from golden`);
  }
  // normalized response parity for identical call sequences
  const keys = ["check", "caps0", "accept", "status", "verify",
                "activate", "caps1", "refused"];
  for (const k of keys) {
    const ns = norm(outN[k].structuredContent ?? outN[k]);
    const rs = norm(outR[k].structuredContent ?? outR[k]);
    if (ns !== rs) {
      throw new Error(
        `${k} diverged:\nN=${ns.slice(0, 400)}\nR=${rs.slice(0, 400)}`);
    }
  }
  const missN = norm(outN.miss), missR = norm(outR.miss);
  if (missN !== missR) throw new Error(`miss: ${missN} vs ${missR}`);
  const unkN = norm(outN.unknown), unkR = norm(outR.unknown);
  if (unkN !== unkR) throw new Error(`unknown: ${unkN} vs ${unkR}`);
  const [resN, resR] = [await stopService(procN), await stopService(procR)];
  if (resN?.c !== 0 || resR?.c !== 0)
    throw new Error(`exits ${resN?.c} ${resR?.c}`);
  return { compared: keys.length + 3, rust_exit: resR.c };
});

emit("summary", failures === 0 ? "PASS" : "FAIL", `${failures} failures`);
for (const p of procs) try { p.kill("SIGKILL"); } catch { /**/ }
process.exit(failures ? 1 : 0);
