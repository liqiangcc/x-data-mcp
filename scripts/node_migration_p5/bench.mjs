// NM-072 P5 bench runner — Node side. Emits one JSONL stats line per
// surface (same shape as rust_bench.rs) plus an env meta line.
//
// Usage: node bench.mjs <workdir> [rust_bench_bin]
//   workdir must be on a supported local fs (/dev/shm).
//   If rust_bench_bin is given, its JSONL output is interleaved.
import fs from "node:fs";
import { join } from "node:path";
import { execFileSync, execSync } from "node:child_process";
import { parse as parseDay } from "../../src_node/parsers/day_file.mjs";
import { parseJsonBytes, serValue } from "../../src_node/mcp/serde_json.mjs";
import { handleJson } from "../../src_node/mcp/transport.mjs";
import { Db as AcquireDb } from "../../src_node/acquire/store.mjs";
import {
  compactBarFromBar, compactBarToBar,
} from "../../src_node/market/compact_market.mjs";
import { check as recordCheck } from "../../src_node/market/record_codec.mjs";
import { TaskAdmission } from "../../src_node/storage/task_repository.mjs";

const dir = process.argv[2];
const RUST_BENCH = process.argv[3] ?? null;
const ITERS = Number(process.env.BENCH_ITERS ?? 200);
const WARMUP = Number(process.env.BENCH_WARMUP ?? 20);
fs.mkdirSync(dir, { recursive: true });

const vmhwm = () => {
  try {
    const l = fs.readFileSync("/proc/self/status", "utf8")
      .split("\n").find((x) => x.startsWith("VmHWM"));
    return l ? Number(l.trim().split(/\s+/)[1]) : 0;
  } catch { return 0; }
};
const stats = (surface, samples, inputBytes, extra = {}) => {
  samples.sort((a, b) => a - b);
  const n = samples.length;
  const mean = Math.round(samples.reduce((a, b) => a + b, 0) / Math.max(n, 1));
  return JSON.stringify({ surface, impl: "node", iters: n,
    p50_ns: samples[Math.floor(n * 0.5)], p95_ns: samples[Math.floor(n * 0.95)],
    max_ns: samples[n - 1], mean_ns: mean, input_bytes: inputBytes,
    peak_rss_kb: vmhwm(), ...extra });
};
const timed = (fn) => {
  for (let i = 0; i < WARMUP; i++) fn();
  const s = [];
  for (let i = 0; i < ITERS; i++) {
    const t0 = process.hrtime.bigint();
    fn();
    s.push(Number(process.hrtime.bigint() - t0));
  }
  return s;
};
const dbBytes = (p) => ({
  db_bytes: fs.existsSync(p) ? fs.statSync(p).size : 0,
  wal_bytes: fs.existsSync(`${p}-wal`) ? fs.statSync(`${p}-wal`).size : 0,
});

// ---------- input fixtures ----------

// Real retained day member (gen-2, sha-pinned) — ~279KB / 8725 records.
const DAY_ZIP = "/home/box/retained-inputs/official-hsjday.zip";
const dayMemberPath = join(dir, "sh000001.day");
if (!fs.existsSync(dayMemberPath)) {
  // 549MB zip — exact-name member via python zipfile (unzip glob-
  // escapes the backslash member names).
  const out = execFileSync("python3", ["-c",
    "import zipfile,sys;sys.stdout.buffer.write(" +
    "zipfile.ZipFile(sys.argv[1]).read(sys.argv[2]))",
    DAY_ZIP, "sh\\lday\\sh000001.day"], { maxBuffer: 32 * 1024 * 1024 });
  fs.writeFileSync(dayMemberPath, out);
}
const dayBytes = fs.readFileSync(dayMemberPath);

// serde doc: realistic ~64KB JSON — a status doc with 500 generations.
const serdePath = join(dir, "doc.json");
if (!fs.existsSync(serdePath)) {
  const doc = { contract_version: "x-data-contract-v1",
    generations: Array.from({ length: 500 }, (_, i) => ({
      generation_id: "a".repeat(40) + String(i).padStart(4, "0"),
      state: "installed", installed_at: "2026-09-24T00:00:00Z",
      evidence: { sha256: "b".repeat(64), window: "2024-01-01/2024-12-31" },
      sources: ["t1", "t2", "t3"] })) };
  fs.writeFileSync(serdePath, JSON.stringify(doc));
}
const serdeBytes = fs.readFileSync(serdePath);

// record_check input: valid legacy records from the issue79 fixture,
// filtered to those that pass check() — emitted as JSONL.
const recSrc = JSON.parse(fs.readFileSync(
  new URL("../../tests/fixtures/issue79/legacy-records.json",
    import.meta.url), "utf8"));
const recPath = join(dir, "records.jsonl");
if (!fs.existsSync(recPath)) {
  const ok = recSrc.map((r) => r.record).filter((r) => {
    try { recordCheck(r); return true; } catch { return false; }
  });
  if (ok.length === 0) throw new Error("no valid fixture records");
  fs.writeFileSync(recPath, ok.map((r) => JSON.stringify(r)).join("\n"));
}
const recLines = fs.readFileSync(recPath, "utf8").trim().split("\n")
  .map((l) => JSON.parse(l));

// ---------- env meta ----------

const sha = execSync("git rev-parse HEAD", { encoding: "utf8" }).trim();
const meta = {
  type: "env", code_sha: sha,
  node: process.version,
  rustc: (() => { try {
    return execSync("rustc --version", { encoding: "utf8" }).trim();
  } catch { return null; } })(),
  uname: execSync("uname -srvmo", { encoding: "utf8" }).trim(),
  concurrency: 1,
  bench_iters: ITERS, bench_warmup: WARMUP,
  day_input_bytes: dayBytes.length,
  serde_input_bytes: serdeBytes.length,
  record_count: recLines.length,
};
console.log(JSON.stringify(meta));

// ---------- surfaces ----------

console.log(stats("day_parse", timed(() => parseDay(dayBytes)),
  dayBytes.length));

console.log(stats("serde_json_decode",
  timed(() => { const v = parseJsonBytes(serdeBytes);
    if (!v.ok) throw new Error("decode"); return v; }), serdeBytes.length));
{
  const v = parseJsonBytes(serdeBytes).value;
  console.log(stats("serde_json_encode",
    timed(() => serValue(v)), serdeBytes.length));
}

{
  const stub = { tools: () => [], call: () => ({ ok: true }) };
  const line = JSON.stringify({ jsonrpc: "2.0", id: 7,
    method: "tools/call", params: { name: "x", arguments: {} } });
  console.log(stats("mcp_roundtrip",
    timed(() => handleJson(stub, line)), line.length));
}

{
  const dbp = join(dir, "acq-bench.sqlite");
  try { fs.rmSync(dbp); fs.rmSync(`${dbp}-wal`); fs.rmSync(`${dbp}-shm`); }
  catch { /**/ }
  const db = new AcquireDb(dbp);
  db.open();
  const now = new Date(); // acceptRequest takes a Date (toRfc3339)
  let i = 0;
  console.log(stats("acquire_store_accept", timed(() =>
    db.acceptRequest(`bench-${i++}`, "acquire_generation", "{}", now)),
    0, dbBytes(dbp)));
  console.log(stats("acquire_store_read", timed(() =>
    db.request(`bench-${i - 1}`)), 0, dbBytes(dbp)));
}

{
  const bars = Array.from({ length: 1000 }, (_, i) => ({
    date: new Date(Date.UTC(2024, 0, 2 + i)).toISOString().slice(0, 10),
    open: `${(10 + i / 100).toFixed(2)}`,
    high: `${(11 + i / 100).toFixed(2)}`,
    low: `${(9 + i / 100).toFixed(2)}`,
    close: `${(10.5 + i / 100).toFixed(2)}`,
    volume: "100000", amount: "1050000.00", r7_raw: 7 }));
  console.log(stats("envelope_pack", timed(() =>
    bars.map((b) => compactBarFromBar(b, "Day"))), bars.length * 32));
  const cs = bars.map((b) => compactBarFromBar(b, "Day"));
  console.log(stats("envelope_expand", timed(() =>
    cs.map((c) => compactBarToBar(c, "Day"))), cs.length * 32));
}

console.log(stats("record_check",
  timed(() => recLines.forEach((r) => recordCheck(r))),
  fs.statSync(recPath).size, { records: recLines.length }));

// task admission + claim — per-iter plan bytes emitted verbatim by the
// Rust side (emit_plan admits real tasks via TaskControl then dumps
// stored row bytes) so Node inserts identical rows. Node claims the
// tasks it admitted; Rust claims the emit-seeded set.
const planPath = join(dir, "plans.json");
let planDoc = null;
if (RUST_BENCH) {
  const dbp = join(dir, "tasks-bench.db");
  const emit = execFileSync(RUST_BENCH,
    ["emit_plan", dbp, String(ITERS + WARMUP)], { encoding: "utf8" });
  fs.writeFileSync(planPath, emit);
  planDoc = JSON.parse(emit);
}
if (planDoc) {
  // Node admits into its own empty schema-18 db (identical row bytes;
  // the seeded db's scope occupancy would range-conflict by design).
  const dbp = planDoc.node_db;
  const adm = TaskAdmission.open(dbp, { mode: "readwrite" });
  const fixed = "2026-01-01T00:00:00Z";
  const plans = planDoc.plans; // ITERS+WARMUP distinct plans — no wrap
  const nodeIds = [];
  let i = 0;
  console.log(stats("task_admit", timed(() => {
    const p = plans[i]; i++;
    const r = adm.admit({
      requestId: `node-${i}`,
      commandJson: p.command_json,
      executionKeyJson: p.execution_key_json,
      workJson: p.work_json,
      items: p.items, scopes: p.scopes ?? [],
      runId: "bench", createdAt: fixed });
    nodeIds.push(r.task_id);
  }), 0, dbBytes(dbp)));
  const cTimed = (fn, n) => {
    const s = [];
    for (let k = 0; k < n; k++) {
      const t0 = process.hrtime.bigint(); fn(k);
      s.push(Number(process.hrtime.bigint() - t0));
    }
    return s;
  };
  console.log(stats("task_claim",
    cTimed((k) => adm.claim(nodeIds[k], 0n, "bench", fixed),
      nodeIds.length), 0, dbBytes(dbp)));
  adm.close();
}

// ---------- rust oracle (same surfaces, same inputs) ----------

if (RUST_BENCH) {
  const run = (s, ...a) => process.stdout.write(
    execFileSync(RUST_BENCH, [s, ...a, String(ITERS), String(WARMUP)],
      { encoding: "utf8" }));
  run("day_parse", dayMemberPath);
  run("serde_dec", serdePath);
  run("serde_enc", serdePath);
  run("mcp_roundtrip", "");
  run("acquire_accept", join(dir, "acq-rust"));
  run("acquire_read", join(dir, "acq-rust"));
  run("envelope_pack", "");
  run("envelope_expand", "");
  run("record_check", recPath);
  if (planDoc) {
    run("task_admit", planPath);
    run("task_claim", planPath);
  }
}
