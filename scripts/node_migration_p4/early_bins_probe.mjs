// NM-060 golden-vector probe: early CLI bins.
//
// Runs each Rust binary (target/debug/<bin>) and its Node port
// (src_node/bin/<bin>.mjs) on the SAME inputs and byte-compares
// stdout + stderr + exit code. Fixture generation commands:
//
//   cargo build --bin inspect_day --bin inspect_snapshot --bin consistency_check
//   (fixtures: committed under scripts/node_migration_p4/fixtures/early_bins/;
//    .day binaries, manifest mutations and a >1MiB manifest are materialized
//    into $TMPDIR by this probe at run time)
//
//   node scripts/node_migration_p4/early_bins_probe.mjs            -> exit 0/1
//   node scripts/node_migration_p4/early_bins_probe.mjs --selftest -> exit 1
//   node scripts/node_migration_p4/early_bins_probe.mjs --out DIR  -> evidence
//
// stderr modes: "exact" byte-compare (default); "category" asserts both
// sides emit the same error-class prefix — used for malformed-manifest
// cases where the ported snapshot parser intentionally keeps Node-side
// diagnostic text (documented delta from the snapshot slice).
// "divergence" cases assert a documented Node-side boundary instead of
// parity (currently: bytes > 2^53 hits isSafeInteger in checkFact).

import { spawnSync } from "node:child_process";
import {
  cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const RUST_BIN = join(REPO, "target/debug");
const FIX = join(REPO, "scripts/node_migration_p4/fixtures/early_bins");
const RETAINED = "/home/box/retained-inputs";
const args = process.argv.slice(2);
const SELFTEST = args.includes("--selftest");
const OUT = args.includes("--out") ? args[args.indexOf("--out") + 1] : null;

// ---------------------------------------------------------------- fixtures
const GEN = mkdtempSync(join(tmpdir(), "nm060-"));
const dayFile = (dates, tail = 0) => {
  const b = Buffer.alloc(dates.length * 32 + tail);
  dates.forEach((d, i) => {
    b.writeUInt32LE(d, i * 32);              // word0 = YYYYMMDD
    b.writeUInt32LE(1000 + i, i * 32 + 4);   // ohlc_raw[0]
    b.writeUInt32LE(2000 + i, i * 32 + 8);
    b.writeUInt32LE(1500 + i, i * 32 + 12);
    b.writeUInt32LE(1800 + i, i * 32 + 16);
    b.writeUInt32LE(0x3f800000, i * 32 + 20); // amount f32 bits
    b.writeUInt32LE(99999, i * 32 + 24);     // volume
    b.writeUInt32LE(7, i * 32 + 28);         // r7
  });
  return b;
};
const gen = (name, bytes) => {
  const p = join(GEN, name);
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, bytes);
  return p;
};
const P = {
  dayReal: join(REPO, "evidence/issue-166-windows-official-behavior/raw/files/sh600004.day"),
  dayValid: gen("valid.day", dayFile([20240102, 20240103, 20240105])),
  dayOne: gen("one.day", dayFile([20240102])),
  dayEmpty: gen("empty.day", Buffer.alloc(0)),
  dayTrunc5: gen("trunc5.day", dayFile([20240102]).subarray(0, 5)),
  dayTrunc33: gen("trunc33.day", Buffer.concat([dayFile([20240102]), Buffer.from([0])])),
  dayBadDate: gen("bad-date.day", dayFile([20240230])),
  dayBadYear: gen("bad-year.day", dayFile([1000000000])),
  dayNonIncr: gen("nonincr.day", dayFile([20240103, 20240102])),
  dayEqual: gen("equal.day", dayFile([20240102, 20240102])),
  dir: join(GEN, "somedir"),
  snapManifest: join(FIX, "snapshot/manifest.json"),
  snapInput: join(FIX, "snapshot/input"),
  snapRealManifest: join(REPO, "evidence/issue-4-update-gen3-acceptance/snapshot-manifest-gen3.json"),
  snapRealDir: join(RETAINED, "gen3"),
  bigManifest: gen("big-manifest.json", Buffer.alloc(1024 * 1024 + 1, 0x20)),
  ccReal: join(REPO, "evidence/issue-131-file-set-consistency/input.json"),
};
mkdirSync(P.dir, { recursive: true });
// snapshot mutations: copy fixture dir, remove/corrupt one artifact
const snapMissing = join(GEN, "snap-missing");
cpSync(P.snapInput, snapMissing, { recursive: true });
rmSync(join(snapMissing, "hsjday.zip"));
const snapCorrupt = join(GEN, "snap-corrupt");
cpSync(P.snapInput, snapCorrupt, { recursive: true });
writeFileSync(join(snapCorrupt, "gbbq.zip"), Buffer.concat([readFileSync(join(snapCorrupt, "gbbq.zip")), Buffer.from([1])]));
// manifest variants (contract-level "invalid manifest: ..." texts)
const manifest = JSON.parse(readFileSync(P.snapManifest, "utf8"));
const manifestVariant = (name, patch) => {
  const m = JSON.parse(JSON.stringify(manifest));
  patch(m);
  return gen(`manifest-${name}.json`, Buffer.from(JSON.stringify(m)));
};
const snapVariants = [
  ["wrong-schema", (m) => { m.schema = "x"; }],
  ["two-files", (m) => { m.files = m.files.slice(0, 2); }],
  ["dup-role", (m) => { m.files[2].role = "day"; }],
  ["dup-path", (m) => { m.files[2].relative_path = "hsjday.zip"; m.files[1].relative_path = "hsjday.zip"; }],
  ["abs-path", (m) => { m.files[0].relative_path = "/etc/passwd"; }],
  ["dotdot-path", (m) => { m.files[0].relative_path = "../base.zip"; }],
  ["bad-name", (m) => { m.files[1].relative_path = "day.zip"; }],
  ["bad-url", (m) => { m.files[0].source_url = "https://evil.example.com/products/data/data/dbf/base.zip"; }],
  ["inverted-interval", (m) => { m.files[0].acquisition_started_at = "2026-09-21T00:00:00Z"; }],
  ["future-interval", (m) => { m.files[0].acquisition_completed_at = "2999-01-01T00:00:00Z"; }],
  ["bytes-zero", (m) => { m.files[0].bytes = 0; }],
  ["sha-noncanonical", (m) => { m.files[0].sha256 = "XYZ"; }],
].map(([name, patch]) => ({
  name: `snapshot-${name}`, bin: "inspect_snapshot",
  args: [manifestVariant(name, patch), "{snapInput}"],
}));

// ---------------------------------------------------------------- scenarios
const ccFixtures = readdirSync(join(FIX, "cc"))
  .filter((f) => f.endsWith(".json") && !f.startsWith("divergence"))
  .sort()
  .map((f) => ({ name: `cc-${f.replace(/\.json$/, "")}`, bin: "consistency_check", args: [join(FIX, "cc", f)] }));

const SCENARIOS = [
  // inspect_day
  { name: "day-real-sh600004", bin: "inspect_day", args: ["{dayReal}"] },
  { name: "day-valid-3", bin: "inspect_day", args: ["{dayValid}"] },
  { name: "day-one-record", bin: "inspect_day", args: ["{dayOne}"] },
  { name: "day-empty", bin: "inspect_day", args: ["{dayEmpty}"] },
  { name: "day-trunc5", bin: "inspect_day", args: ["{dayTrunc5}"] },
  { name: "day-trunc33", bin: "inspect_day", args: ["{dayTrunc33}"] },
  { name: "day-bad-date", bin: "inspect_day", args: ["{dayBadDate}"] },
  { name: "day-bad-year", bin: "inspect_day", args: ["{dayBadYear}"] },
  { name: "day-non-increasing", bin: "inspect_day", args: ["{dayNonIncr}"] },
  { name: "day-equal-dates", bin: "inspect_day", args: ["{dayEqual}"] },
  { name: "day-missing-file", bin: "inspect_day", args: [join(GEN, "missing.day")] },
  { name: "day-dir-as-file", bin: "inspect_day", args: ["{dir}"] },
  { name: "day-no-args", bin: "inspect_day", args: [] },
  { name: "day-two-args", bin: "inspect_day", args: ["{dayValid}", "{dayOne}"] },
  // inspect_snapshot
  {
    name: "snapshot-real-gen3", bin: "inspect_snapshot",
    args: ["{snapRealManifest}", "{snapRealDir}"],
    requires: existsSync(join(P.snapRealDir, "hsjday.zip")) ? null : "retained-inputs/gen3 absent",
  },
  { name: "snapshot-synthetic", bin: "inspect_snapshot", args: ["{snapManifest}", "{snapInput}"] },
  { name: "snapshot-missing-manifest", bin: "inspect_snapshot", args: [join(GEN, "missing.json"), "{snapInput}"] },
  { name: "snapshot-dir-as-manifest", bin: "inspect_snapshot", args: ["{dir}", "{snapInput}"] },
  { name: "snapshot-missing-root", bin: "inspect_snapshot", args: ["{snapManifest}", join(GEN, "no-such-dir")] },
  { name: "snapshot-missing-artifact", bin: "inspect_snapshot", args: ["{snapManifest}", snapMissing] },
  { name: "snapshot-corrupt-artifact", bin: "inspect_snapshot", args: ["{snapManifest}", snapCorrupt] },
  { name: "snapshot-oversized-manifest", bin: "inspect_snapshot", args: ["{bigManifest}", "{snapInput}"] },
  { name: "snapshot-malformed-json", bin: "inspect_snapshot", args: [gen("bad-manifest.json", Buffer.from('{"schema":')), "{snapInput}"], stderr: "category", stderrPrefix: "manifest JSON invalid:" },
  { name: "snapshot-malformed-json2", bin: "inspect_snapshot", args: [gen("bad-manifest2.json", Buffer.from("x\ny")), "{snapInput}"], stderr: "category", stderrPrefix: "manifest JSON invalid:" },
  { name: "snapshot-not-json-array", bin: "inspect_snapshot", args: [gen("arr-manifest.json", Buffer.from("[]")), "{snapInput}"], stderr: "category", stderrPrefix: "manifest JSON invalid:" },
  ...snapVariants,
  { name: "snapshot-no-args", bin: "inspect_snapshot", args: [] },
  { name: "snapshot-one-arg", bin: "inspect_snapshot", args: ["{snapManifest}"] },
  { name: "snapshot-three-args", bin: "inspect_snapshot", args: ["a", "b", "c"] },
  // consistency_check
  { name: "cc-real-131", bin: "consistency_check", args: ["{ccReal}"] },
  ...ccFixtures,
  {
    name: "cc-divergence-u64max", bin: "consistency_check",
    args: [join(FIX, "cc/divergence-u64max.json")],
    divergence: {
      rust: { exit: 0 },
      node: { exit: 1, stderr: "files[0].bytes invalid\n" },
      note: "ported checkFact uses Number.isSafeInteger — u64 > 2^53 rejected at the governance layer (out of lane)",
    },
  },
  { name: "cc-no-args", bin: "consistency_check", args: [] },
  { name: "cc-two-args", bin: "consistency_check", args: ["{ccReal}", "{ccReal}"] },
  { name: "cc-missing-file", bin: "consistency_check", args: [join(GEN, "missing.json")] },
  { name: "cc-dir-as-file", bin: "consistency_check", args: ["{dir}"] },
];

// ------------------------------------------------------------------- runner
const resolveArgs = (xs) => xs.map((a) => a.replace(/\{(\w+)\}/g, (_, k) => P[k] ?? a));
function runBin(bin, argv, side) {
  const cmd = side === "rust"
    ? [join(RUST_BIN, bin), argv]
    : [process.execPath, [join(REPO, "src_node/bin", `${bin}.mjs`), ...argv]];
  const r = spawnSync(cmd[0], cmd[1], { encoding: "buffer" });
  return { stdout: r.stdout ?? Buffer.alloc(0), stderr: r.stderr ?? Buffer.alloc(0), exit: r.status ?? -1 };
}
const sha = (b) => createHash("sha256").update(b).digest("hex").slice(0, 16);

let pass = 0, fail = 0, notRun = 0, divergent = 0;
const results = [];
for (const c of SELFTEST ? [] : SCENARIOS) {
  const argv = resolveArgs(c.args);
  if (c.requires) {
    notRun++;
    results.push({ ...c, status: "not_run", reason: c.requires });
    continue;
  }
  const rust = runBin(c.bin, argv, "rust");
  const node = runBin(c.bin, argv, "node");
  const base = { name: c.name, bin: c.bin, exit: { rust: rust.exit, node: node.exit } };
  if (c.divergence) {
    const ok = rust.exit === c.divergence.rust.exit &&
      node.exit === c.divergence.node.exit &&
      node.stderr.toString() === c.divergence.node.stderr;
    ok ? divergent++ : fail++;
    results.push({
      ...base, status: ok ? "divergence" : "fail", note: c.divergence.note,
      rust: { stdoutSha: sha(rust.stdout), stderr: rust.stderr.toString().trim() },
      node: { stderr: node.stderr.toString().trim() },
    });
    continue;
  }
  const stdoutEq = rust.stdout.equals(node.stdout);
  const stderrEq = c.stderr === "category"
    ? rust.stderr.toString().startsWith(c.stderrPrefix) && node.stderr.toString().startsWith(c.stderrPrefix)
    : rust.stderr.equals(node.stderr);
  const ok = rust.exit === node.exit && stdoutEq && stderrEq;
  ok ? pass++ : fail++;
  results.push({
    ...base, status: ok ? "pass" : "fail",
    stdout: stdoutEq ? `identical (${rust.stdout.length}B)` : { rust: sha(rust.stdout), node: sha(node.stdout) },
    stderr: stderrEq ? (c.stderr === "category" ? `category:${c.stderrPrefix}` : `identical (${rust.stderr.length}B)`)
      : { rust: rust.stderr.toString().trim(), node: node.stderr.toString().trim() },
  });
}

// --------------------------------------------------------------- selftest
if (SELFTEST) {
  // Deliberate mismatch: Rust reads the valid day file, Node the truncated
  // one — the comparer must report the divergence.
  const rust = runBin("inspect_day", [P.dayValid], "rust");
  const node = runBin("inspect_day", [P.dayTrunc5], "node");
  const mismatch = !(rust.stdout.equals(node.stdout) && rust.stderr.equals(node.stderr) && rust.exit === node.exit);
  if (!mismatch) {
    console.error("SELFTEST BROKEN: deliberately mismatched inputs compared equal");
    process.exit(1);
  }
  console.error("selftest: comparer reported mismatch as required");
  process.exit(1); // negative selftest must exit nonzero
}

const summary = {
  probe: "early_bins",
  node: process.version,
  total: SCENARIOS.length, pass, fail, divergent, not_run: notRun,
  selftest: "checked separately (--selftest exits 1)",
};
const out = JSON.stringify({ summary, results }, null, 2) + "\n";
if (OUT) {
  mkdirSync(OUT, { recursive: true });
  writeFileSync(join(OUT, "results.json"), out);
  writeFileSync(join(OUT, "results.jsonl"),
    results.map((r) => JSON.stringify(r)).join("\n") + "\n");
}
console.log(`early_bins probe: ${pass} pass, ${fail} fail, ${divergent} divergence, ${notRun} not_run of ${SCENARIOS.length}`);
process.exit(fail === 0 ? 0 : 1);
