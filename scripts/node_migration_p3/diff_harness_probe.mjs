// NM-049 P3: differential harness probe — runs every scenario through
// BOTH the Rust oracle (diff_harness::run, real SqliteTasks) and the
// Node runner, then diffs per-op outcomes. Exit nonzero on any DIFF.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { join } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const SCEN = join(ROOT, "scripts/node_migration_p3/scenarios");
const CARGO = `${process.env.HOME}/.cargo/bin/cargo`;

let failures = 0;
const emit = (id, status, detail) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "diff_harness", id, status, detail }));
};

const REC_FIXTURES = mkdtempSync("/dev/shm/nm49-rec-");
{
  // copy committed records + generate the overbudget fixture on the fly
  const { copyFileSync, mkdirSync } = await import("node:fs");
  const recDir = join(ROOT, "scripts/node_migration_p3/records");
  for (const f of readdirSync(recDir)) copyFileSync(join(recDir, f), join(REC_FIXTURES, f));
  const RC = await import("../../src_node/market/record_codec.mjs");
  const base = JSON.parse(readFileSync(join(recDir, "valid_daily.json"), "utf8"));
  const bar = base.batch.bars[0];
  base.records = Array.from({ length: 100001 }, () => bar);
  base.batch.bars = Array.from({ length: 100001 }, () => bar);
  base.integrity_sha256 = RC.legacyDigest(base);
  const { writeFileSync } = await import("node:fs");
  writeFileSync(join(REC_FIXTURES, "overbudget.json"), JSON.stringify(base));
}
process.on("exit", () => rmSync(REC_FIXTURES, { recursive: true, force: true }));
const scenarios = readdirSync(SCEN).filter((f) => f.endsWith(".jsonl")).sort();
for (const file of scenarios) {
  const dir = mkdtempSync("/dev/shm/nm49-");
  const scen = join(SCEN, file);
  const rustOut = join(dir, "rust.jsonl");
  const nodeOut = join(dir, "node.jsonl");
  const name = file.replace(/\.jsonl$/, "");
  try {
    const scenText = readFileSync(scen, "utf8");
    const firstOp = JSON.parse(scenText.split("\n").find((l) => l.trim()));
    const seed = firstOp.op === "seed";
    const rustDb = join(dir, "rust.db"), nodeDb = join(dir, "node.db");
    const extra = {};
    if (seed) {
      const fixture = join(dir, "fixture.db");
      const seedOp = firstOp;
      const recs = seedOp.records;
      if (seedOp.results) extra.DIFF_RESULTS = "1";
      if (seedOp.tasks) extra.DIFF_TASKS = seedOp.tasks;
      if (seedOp.compact) extra.DIFF_INIT = "19";
      if (seedOp.norm_ts) extra.DIFF_NORM_TS = "1";
      if (seedOp.check) extra.DIFF_CHECK = "1";
      execFileSync(CARGO, ["test", "--lib", "diff_harness::build_market_fixture", "--",
        "--nocapture", "--test-threads=1"], {
        cwd: ROOT,
        env: { ...process.env, DIFF_DB: fixture, DIFF_FIXTURES: REC_FIXTURES,
          DIFF_RECORDS: recs, TMPDIR: "/dev/shm", ...extra },
        stdio: ["ignore", "ignore", "pipe"], timeout: 600000,
      });
      copyFileSync(fixture, rustDb);
      extra.DIFF_NOINIT = "1";
      extra.DIFF_PRESEED = fixture;
    }
    execFileSync(CARGO, ["test", "--lib", "diff_harness::run", "--",
      "--nocapture", "--test-threads=1"], {
      cwd: ROOT,
      env: {
        ...process.env, DIFF_SCENARIO: scen,
        DIFF_DB: rustDb, DIFF_OUT: rustOut,
        DIFF_FIXTURES: REC_FIXTURES,
        TMPDIR: "/dev/shm", ...extra,
      },
      stdio: ["ignore", "ignore", "pipe"], timeout: 600000,
    });
    execFileSync("node", [
      "scripts/node_migration_p3/diff_runner.mjs", scen,
      nodeDb, nodeOut,
    ], { cwd: ROOT,
      env: { ...process.env, DIFF_FIXTURES: REC_FIXTURES, ...extra },
      stdio: ["ignore", "ignore", "pipe"] });
    const cmp = execFileSync("node", [
      "scripts/node_migration_p3/diff_compare.mjs", rustOut, nodeOut,
    ], { cwd: ROOT, encoding: "utf8" });
    const verdict = JSON.parse(cmp.trim().split("\n").pop());
    emit(name, verdict.verdict === "MATCH" ? "PASS" : "FAIL",
      `${verdict.ops} ops, ${verdict.diffs} diffs`);
  } catch (e) {
    const tail = (e.stdout ? String(e.stdout) : String(e.message)) + " | stderr:" + String(e.stderr||"");
    emit(name, "FAIL", tail.slice(0, 900));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

if (SELFTEST === "diff") {
  emit("selftest_diff", "FAIL", "intentional — selftest exits 1");
}
process.exitCode = failures === 0 ? 0 : 1;
