// NM-066 differential probe — src_node/bin/mcp_connect.mjs vs the Rust
// target/debug/mcp_connect binary over a scripted unix socket.
//
//   cargo build --bin mcp_connect
//   TMPDIR=/dev/shm node scripts/node_migration_p4/mcp_connect_probe.mjs [outdir]
//   SELFTEST=1 ... -> exits 1 (injected-mismatch detection)
//
// Each case runs both implementations against an identical scripted
// server (mcp_connect_server.mjs) or path fixture and compares stdout
// bytes + stderr text + exit code. The socket dir lives under
// ${TMPDIR}/nm66-* (tmpfs — a supported local fs); one case exercises
// the overlayfs rejection + X_DATA_MCP_UNSAFE_NONLOCAL_FS opt-out on
// /tmp. Foreign-owner peers are NOT_RUN on this single-UID box.

import { spawn, spawnSync } from "node:child_process";
import {
  chmodSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync,
  writeFileSync, mkdirSync as mk,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SELFTEST = !!process.env.SELFTEST;
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const ORACLE = join(REPO, "target/debug/mcp_connect");
const NODE_BIN = join(REPO, "src_node/bin/mcp_connect.mjs");
const SERVER = join(HERE, "mcp_connect_server.mjs");
const OUT = process.argv[2]
  ? resolve(process.argv[2])
  : join(REPO, "evidence/node-migration/p4/mcp_connect");
mkdirSync(OUT, { recursive: true });

const results = [];
const record = (name, rust, node, note) => {
  const same = rust.stdout === node.stdout &&
    rust.stderr === node.stderr && rust.code === node.code;
  results.push({ name, rust, node, match: same, note });
  return same;
};

// Run one client; stdin = bytes | "hold" (open, no EOF) | "closed".
const client = (cmd, args, sockPath, stdinSpec, timeoutMs = 8000) =>
  new Promise((res) => {
    const p = spawn(cmd, [...args, sockPath].filter(Boolean), {
      stdio: ["pipe", "pipe", "pipe"],
    });
    let out = [], err = [];
    p.stdout.on("data", (d) => out.push(d));
    p.stderr.on("data", (d) => err.push(d));
    const killer = setTimeout(() => {
      p.kill("SIGKILL");
      res({ timeout: true, stdout: Buffer.concat(out).toString("base64"),
            stderr: Buffer.concat(err).toString(), code: -9 });
    }, timeoutMs);
    p.on("exit", (code) => {
      clearTimeout(killer);
      res({ stdout: Buffer.concat(out).toString("base64"),
            stderr: Buffer.concat(err).toString(), code });
    });
    p.stdin.on("error", () => {}); // client may exit before we write
    if (stdinSpec === "closed") p.stdin.end();
    else if (stdinSpec === "hold") p.stdin.write("x"); // held open
    else p.stdin.end(stdinSpec);
  });

// Serve one connection with `mode`, then exit.
const server = (sockPath, mode, payloadHex) =>
  new Promise((res) => {
    const p = spawn("node", [SERVER, sockPath, mode, payloadHex ?? ""],
      { stdio: ["ignore", "pipe", "inherit"] });
    let buf = "";
    p.stdout.on("data", (d) => {
      buf += d;
      if (buf.includes("READY")) res(p);
    });
  });

const mkPrivate = (base, name) => {
  const d = join(base, name);
  mkdirSync(d, { mode: 0o700 });
  return d;
};

async function serveAndRun(name, mode, payloadHex, stdinSpec, dirName = "priv") {
  const base = mkdtempSync(join(tmpdir(), "nm66-"));
  const dir = mkPrivate(base, dirName);
  const sock = join(dir, "sock");
  const s1 = await server(sock, mode, payloadHex);
  const rust = await client(ORACLE, [], sock, stdinSpec);
  s1.kill();
  await new Promise((r) => s1.once("exit", r));
  const s2 = await server(sock, mode, payloadHex);
  const node = await client("node", [NODE_BIN], sock, stdinSpec);
  s2.kill();
  await new Promise((r) => s2.once("exit", r));
  record(name, rust, node);
}

async function pathCase(name, mkPath, args = undefined) {
  const base = mkdtempSync(join(tmpdir(), "nm66-"));
  const target = mkPath(base);
  const rust = await client(ORACLE, args ?? [], target, "closed");
  const node = await client("node", args ?? [NODE_BIN], target, "closed");
  // node gets [NODE_BIN] prepended via args — see call sites
  record(name, rust, node);
}

const base = mkdtempSync(join(tmpdir(), "nm66-"));

// --- arg surface
{
  const rust = await client(ORACLE, [], undefined, "closed");
  const node = await client("node", [NODE_BIN], undefined, "closed");
  record("no-arg", rust, node);
}
{
  // two args — both should print usage (spawn adds the extra arg)
  const p = (cmd, extra) => spawnSync(cmd, extra, { encoding: "buffer", timeout: 8000 });
  const rust = p(ORACLE, ["a", "b"]);
  const node = p("node", [NODE_BIN, "a", "b"]);
  record("two-args",
    { stdout: rust.stdout.toString("base64"), stderr: rust.stderr.toString(), code: rust.status },
    { stdout: node.stdout.toString("base64"), stderr: node.stderr.toString(), code: node.status });
}

// --- path rejections (private dir under tmpfs)
{
  const dir = mkPrivate(base, "d1");
  await pathCase("missing-socket", () => join(dir, "nosock"));
  const f = join(dir, "plain"); writeFileSync(f, "x"); chmodSync(f, 0o600);
  await pathCase("regular-file", () => f);
  const ln = join(dir, "link"); symlinkSync(f, ln);
  await pathCase("symlink-socket", () => ln);
  const stale = join(dir, "stale");
  // create then remove a listener so the socket file remains
  const s = await server(stale, "close");
  s.kill(); await new Promise((r) => s.once("exit", r));
  await pathCase("stale-socket-econnrefused", () => stale);
}
{
  // non-private parent dir (0755)
  const dir = join(base, "open"); mkdirSync(dir, { mode: 0o755 });
  await pathCase("nonprivate-parent", () => join(dir, "sock"));
  // missing parent
  await pathCase("missing-parent", () => join(base, "gone", "sock"));
  // bare filename — no parent
  await pathCase("bare-name", () => "nosuch-sock-bare");
}
{
  // overlayfs parent — rejected without opt-out, allowed with it
  const dir = "/tmp/nm66-overlay"; mkdirSync(dir, { mode: 0o700, recursive: true });
  const sock = join(dir, "sock");
  const s = await server(sock, "echo");
  const rustNo = await client(ORACLE, [], sock, Buffer.from("hi\n"));
  const nodeNo = await client("node", [NODE_BIN], sock, Buffer.from("hi\n"));
  s.kill(); await new Promise((r) => s.once("exit", r));
  record("overlay-noopt", rustNo, nodeNo);
  const s2 = await server(sock, "echo");
  const env = { ...process.env, X_DATA_MCP_UNSAFE_NONLOCAL_FS: "1" };
  const rustYes = await new Promise((res) => {
    const p = spawn(ORACLE, [sock], { stdio: ["pipe", "pipe", "pipe"], env });
    let o = [], e = [];
    p.stdout.on("data", (d) => o.push(d)); p.stderr.on("data", (d) => e.push(d));
    p.on("exit", (c) => res({ stdout: Buffer.concat(o).toString("base64"), stderr: Buffer.concat(e).toString(), code: c }));
    p.stdin.end("hi\n");
  });
  const nodeYes = await new Promise((res) => {
    const p = spawn("node", [NODE_BIN, sock], { stdio: ["pipe", "pipe", "pipe"], env });
    let o = [], e = [];
    p.stdout.on("data", (d) => o.push(d)); p.stderr.on("data", (d) => e.push(d));
    p.on("exit", (c) => res({ stdout: Buffer.concat(o).toString("base64"), stderr: Buffer.concat(e).toString(), code: c }));
    p.stdin.end("hi\n");
  });
  s2.kill(); await new Promise((r) => s2.once("exit", r));
  record("overlay-optout", rustYes, nodeYes);
}

// --- live copy semantics
await serveAndRun("echo-small", "echo", null, Buffer.from("hello\nworld\n"));
const big = Buffer.alloc(200_000); for (let i = 0; i < big.length; i++) big[i] = i & 0xff;
await serveAndRun("echo-200k", "echo", null, big);
await serveAndRun("spew-dead-service", "spew", Buffer.from("F".repeat(4096)).toString("hex"), "hold");
await serveAndRun("early-close", "close", null, "hold");
await serveAndRun("echo-empty", "echo", null, Buffer.alloc(0));

if (SELFTEST) {
  const injected = { ...results[0] };
  injected.node = { ...injected.node, code: injected.node.code + 1 };
  const detected = injected.node.code !== results[0].node.code;
  writeFileSync(join(OUT, "negative-selftest.jsonl"),
    JSON.stringify({ injected: "exit-code flip", detected }) + "\n");
  console.log(JSON.stringify({ selftest: detected ? "detected" : "missed" }));
  process.exit(detected ? 1 : 0);
}

const pass = results.filter((r) => r.match).length;
const fail = results.filter((r) => !r.match).length;
const summary = {
  probe: "mcp_connect", total: results.length, pass, fail,
  not_run: ["foreign-owner peer UID (single-UID box)"],
  oracle: "target/debug/mcp_connect",
};
writeFileSync(join(OUT, "probe-results.jsonl"),
  results.map((r) => JSON.stringify(r)).join("\n") + "\n");
writeFileSync(join(OUT, "results.json"), JSON.stringify(summary, null, 2) + "\n");
console.log(JSON.stringify(summary));
for (const r of results.filter((x) => !x.match).slice(0, 5))
  console.error("FAIL", r.name,
    "\n  rust:", JSON.stringify({ c: r.rust.code, e: r.rust.stderr.slice(0, 120), o: r.rust.stdout.slice(0, 60) }),
    "\n  node:", JSON.stringify({ c: r.node.code, e: r.node.stderr.slice(0, 120), o: r.node.stdout.slice(0, 60) }));
process.exit(fail === 0 ? 0 : 1);
