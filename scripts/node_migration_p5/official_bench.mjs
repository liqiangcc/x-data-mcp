// NM-110 P5 bench — Node side, official materialize/zip-inspect lane.
// Mirrors official_bench.rs subcommands; emits one JSONL stats line per
// surface plus an env meta line.
//
// Usage: node official_bench.mjs <surface> <args...>
//   zip_index <zip>
//   zip_member_read <zip> <member>
//   gbbq_decode <gbbq-zip>
//   official_inspect <root> <manifest>
//   materialize_warm <root> <manifest> <symbol>
//   materialize_cold <root> <manifest> <symbol>
import fs from "node:fs";
import { execSync } from "node:child_process";
import {
  ArchiveIndex, MaterializeSession, TdxMarketSource, sha256hex,
} from "../../src_node/official/official_market.mjs";

const ITERS = Number(process.env.BENCH_ITERS ?? 200);
const WARMUP = Number(process.env.BENCH_WARMUP ?? 20);
const NOW = new Date("2026-09-24T00:00:00Z");

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

const [surface, ...args] = process.argv.slice(2);
let nbytes = 0;
if (surface === "env") {
  const meta = {
    type: "env", code_sha: execSync("git rev-parse HEAD",
      { encoding: "utf8" }).trim(),
    node: process.version,
    rustc: (() => { try {
      return execSync("rustc --version", { encoding: "utf8" }).trim();
    } catch { return null; } })(),
    uname: execSync("uname -srvmo", { encoding: "utf8" }).trim(),
    concurrency: 1, bench_iters: ITERS, bench_warmup: WARMUP,
  };
  console.log(JSON.stringify(meta));
  process.exit(0);
}
switch (surface) {
  case "zip_index": {
    nbytes = fs.statSync(args[0]).size;
    // ArchiveIndex::open — File::open + get_metadata equivalent.
    console.log(stats(surface, timed(() => ArchiveIndex.open(args[0])),
      nbytes));
    break;
  }
  case "zip_member_read": {
    // member_bytes composition: fresh index + member read + CRC +
    // sha256 — same as the Rust ZipArchive::new arm.
    const [zip, member] = args;
    console.log(stats(surface, timed(() => {
      const idx = ArchiveIndex.open(zip);
      const i = idx.members.get(member)[0];
      const bytes = idx.memberRead(idx.memberOpen(i));
      nbytes = bytes.length;
      return sha256hex(bytes);
    }), nbytes));
    break;
  }
  case "gbbq_decode": {
    // decoded_gbbq body verbatim via a real inspected source; a fresh
    // session per iter pays member read + parse + decode + sha256
    // (the OnceCell equivalent is per-session, which is what the Rust
    // oracle arm measures).
    const [root, manifestPath] = args;
    const source = TdxMarketSource.inspect(
      root, fs.readFileSync(manifestPath), NOW);
    const [, gbytes] = new MaterializeSession(source)
      .memberBytes(source.gbbq, ["gbbq"]);
    nbytes = gbytes.length;
    console.log(stats(surface, timed(() => {
      const session = new MaterializeSession(source);
      return session.decodedGbbq().bySecurity.size;
    }), nbytes));
    break;
  }
  case "official_inspect": {
    const [root, manifestPath] = args;
    const manifest = fs.readFileSync(manifestPath);
    nbytes = manifest.length;
    console.log(stats(surface, timed(() =>
      TdxMarketSource.inspect(root, manifest, NOW)), nbytes));
    break;
  }
  case "materialize_warm": {
    const [root, manifestPath, sym] = args;
    const source = TdxMarketSource.inspect(
      root, fs.readFileSync(manifestPath), NOW);
    const session = source.materializeSession();
    nbytes = session.materializeDaily(sym).bars.length;
    console.log(stats(surface, timed(() =>
      session.materializeDaily(sym).bars.length), nbytes));
    break;
  }
  case "materialize_cold": {
    const [root, manifestPath, sym] = args;
    const source = TdxMarketSource.inspect(
      root, fs.readFileSync(manifestPath), NOW);
    console.log(stats(surface, timed(() => {
      const b = source.materializeDaily(sym);
      nbytes = b.bars.length;
      return b.bars.length;
    }), nbytes));
    break;
  }
  default:
    throw new Error(`unknown surface ${surface}`);
}
