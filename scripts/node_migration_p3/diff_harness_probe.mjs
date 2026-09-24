// NM-049 P3: differential harness probe — runs every scenario through
// BOTH the Rust oracle (diff_harness::run, real SqliteTasks) and the
// Node runner, then diffs per-op outcomes. Exit nonzero on any DIFF.
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
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

const scenarios = readdirSync(SCEN).filter((f) => f.endsWith(".jsonl")).sort();
for (const file of scenarios) {
  const dir = mkdtempSync("/dev/shm/nm49-");
  const scen = join(SCEN, file);
  const rustOut = join(dir, "rust.jsonl");
  const nodeOut = join(dir, "node.jsonl");
  const name = file.replace(/\.jsonl$/, "");
  try {
    execFileSync(CARGO, ["test", "--lib", "diff_harness::run", "--",
      "--nocapture", "--test-threads=1"], {
      cwd: ROOT,
      env: {
        ...process.env, DIFF_SCENARIO: scen,
        DIFF_DB: join(dir, "rust.db"), DIFF_OUT: rustOut,
        TMPDIR: "/dev/shm",
      },
      stdio: ["ignore", "ignore", "pipe"], timeout: 600000,
    });
    execFileSync("node", [
      "scripts/node_migration_p3/diff_runner.mjs", scen,
      join(dir, "node.db"), nodeOut,
    ], { cwd: ROOT, stdio: ["ignore", "ignore", "pipe"] });
    const cmp = execFileSync("node", [
      "scripts/node_migration_p3/diff_compare.mjs", rustOut, nodeOut,
    ], { cwd: ROOT, encoding: "utf8" });
    const verdict = JSON.parse(cmp.trim().split("\n").pop());
    emit(name, verdict.verdict === "MATCH" ? "PASS" : "FAIL",
      `${verdict.ops} ops, ${verdict.diffs} diffs`);
  } catch (e) {
    const tail = e.stdout ? String(e.stdout).split("\n").slice(-8).join("\n") : String(e.message);
    emit(name, "FAIL", tail.slice(0, 400));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

if (SELFTEST === "diff") {
  emit("selftest_diff", "FAIL", "intentional — selftest exits 1");
}
process.exitCode = failures === 0 ? 0 : 1;
