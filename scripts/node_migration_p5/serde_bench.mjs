// NM-090 — serde_json_encode surface, NM-072 bench.mjs verbatim slice.
// Same fixture (~64KB doc with 500 generations), same timed()/stats()
// shape, same ITERS/WARMUP env, same JSONL output line. Added:
//   --impl=old  measures the pre-NM-090 recursive serValue (vendored
//               below, bound to this module's F64/serF64)
//   --impl=new  measures src_node/mcp/serde_json.mjs serValue
// Run both in the same process pair for an apples-to-apples p50.
import fs from "node:fs";
import { join } from "node:path";
import { execFileSync, execSync } from "node:child_process";
import { parseJsonBytes, serValue, serF64, F64 }
  from "../../src_node/mcp/serde_json.mjs";

const IMPL = process.argv.slice(2).find((a) => a.startsWith("--impl="))
  ?.split("=")[1] ?? "new";
const dir = process.argv[2];
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
  return JSON.stringify({ surface, impl: `node-${IMPL}`, iters: n,
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

// serde doc: realistic ~64KB JSON — a status doc with 500 generations
// (NM-072 bench.mjs verbatim fixture).
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

// pre-NM-090 implementation (git-show baseline copy; F64/serF64 bound
// to the current module so instanceof works).
// pre-NM-091 parser: baseline serde_json.mjs written to /dev/shm lazily
// by the decode surface below.
const OLD_PARSE_SRC = execSync(
  "git show 558cb98:src_node/mcp/serde_json.mjs",
  { encoding: "utf8" });
const keyCmpOld = (a, b) =>
  Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8"));
const serValueOld = (v) => {
  if (v === null || v === undefined) return "null";
  if (v instanceof F64) return serF64(v.v);
  if (typeof v === "bigint") return v.toString();
  if (typeof v === "number") return Number.isInteger(v) ? String(v) : serF64(v);
  if (typeof v === "string") return JSON.stringify(v);
  if (typeof v === "boolean") return v ? "true" : "false";
  if (typeof v.toJSON === "function") return serValueOld(v.toJSON());
  if (Array.isArray(v)) return `[${v.map(serValueOld).join(",")}]`;
  const keys = Object.keys(v).sort(keyCmpOld);
  return `{${keys.map((k) => `${JSON.stringify(k)}:${serValueOld(v[k])}`).join(",")}}`;
};
const ser = IMPL === "old" ? serValueOld : serValue;

const meta = {
  type: "env", code_sha: execSync("git rev-parse HEAD", { encoding: "utf8" }).trim(),
  node: process.version,
  uname: execSync("uname -srvmo", { encoding: "utf8" }).trim(),
  concurrency: 1, bench_iters: ITERS, bench_warmup: WARMUP,
  serde_input_bytes: serdeBytes.length,
};
console.log(JSON.stringify(meta));

const v = parseJsonBytes(serdeBytes).value;
console.log(stats("serde_json_encode", timed(() => ser(v)), serdeBytes.length));

// NM-091 decode surface — same harness shape; --impl selects old/new
// parser via an extracted baseline module.
if (IMPL === "old") {
  fs.writeFileSync("/dev/shm/_serde_bench_old.mjs", OLD_PARSE_SRC);
}
const oldParse = IMPL === "old"
  ? (await import("/dev/shm/_serde_bench_old.mjs")).parseJsonBytes
  : null;
const dec = IMPL === "old" ? oldParse : parseJsonBytes;
console.log(stats("serde_json_decode",
  timed(() => { const r = dec(serdeBytes);
    if (!r.ok) throw new Error("decode"); return r; }), serdeBytes.length));
