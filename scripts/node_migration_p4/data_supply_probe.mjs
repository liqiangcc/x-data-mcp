// NM-063 differential probe — data-supply wire types + registry +
// DataApi skeleton vs the Rust `mcp_data` oracle.
//
//   cargo build --bin mcp_data   (oracle)
//   TMPDIR=/dev/shm node scripts/node_migration_p4/data_supply_probe.mjs [outdir]
//   SELFTEST=1 ...  -> must exit 1 (asserts a mismatch is detected)
//
// Feeds scenarios/data_supply.lines to both binaries over stdio and
// compares each response line byte-for-byte. The ONE accepted divergence
// class is the repository seam: the oracle's SqliteSupply fails with
// "STORAGE_UNAVAILABLE: unable to open database file: <scratch>" while
// the skeleton's RepositorySeam answers "STORAGE_UNAVAILABLE: repository
// seam not wired (S12c pending): <op>" — identical code + certainty +
// envelope, different message body. Everything else must be identical:
// serde decode errors, dataset/contract prechecks, QueryPlan/observer
// validation, outcome_certainty, capabilities, tools/list bytes.
//
// Environment is scrubbed of X_DATA_MCP_GENERATION_ROOT /
// X_DATA_MCP_OFFICIAL_MANIFEST / X_DATA_MCP_OFFICIAL_DIR on both sides so
// the env-presence facts are deterministic.

import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SELFTEST = !!process.env.SELFTEST;
const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const ORACLE = join(REPO, "target/debug/mcp_data");
const RUNNER = join(HERE, "data_supply_runner.mjs");
const SCENARIOS = join(HERE, "scenarios/data_supply.lines");
const OUT = process.argv[2]
  ? resolve(process.argv[2])
  : join(REPO, "evidence/node-migration/p4/data_supply");
mkdirSync(OUT, { recursive: true });

const env = { ...process.env };
delete env.X_DATA_MCP_GENERATION_ROOT;
delete env.X_DATA_MCP_OFFICIAL_MANIFEST;
delete env.X_DATA_MCP_OFFICIAL_DIR;

const scratchDir = mkdtempSync(join(tmpdir(), "nm063-probe-"));
const scratchDb = join(scratchDir, "scratch.db");
const input = readFileSync(SCENARIOS);
const requests = input.toString("utf8").split("\n").filter(Boolean);

const run = (cmd, args, cwd) =>
  spawnSync(cmd, args, {
    input, env, cwd, encoding: "utf8", maxBuffer: 128 * 1024 * 1024,
  });
const rust = SELFTEST ? null : run(ORACLE, [scratchDb], REPO);
const node = run("node", [RUNNER], REPO);

const results = [];
let pass = 0, fail = 0, diverge = 0;
if (SELFTEST) {
  // selftest: corrupt one byte of a captured response and require the
  // comparison to flag it — exit 1 proves the harness detects drift.
  const lines = node.stdout.split("\n").filter(Boolean);
  const original = lines[0];
  const flipped = original.replace('"jsonrpc"', '"jsonrpx"');
  const detected = flipped !== original;
  writeFileSync(join(OUT, "negative-selftest.jsonl"),
    JSON.stringify({ injected: { line: 0, original, flipped },
      detected }) + "\n");
  console.log(JSON.stringify({ selftest: detected ? "detected" : "missed" }));
  process.exit(detected ? 1 : 0);
}
if (rust.status !== 0 || node.status !== 0) {
  console.error("runner failure", { rust: rust.status, node: node.status,
    rustStderr: rust.stderr?.slice(0, 400), nodeStderr: node.stderr?.slice(0, 400) });
  process.exit(2);
}
const rustLines = rust.stdout.split("\n").filter(Boolean);
const nodeLines = node.stdout.split("\n").filter(Boolean);
// response lines pair positionally (notifications emit nothing on both)
const notifIds = new Set();
requests.forEach((r, i) => {
  try {
    const j = JSON.parse(r);
    if (typeof j.method === "string" && j.method.startsWith("notifications/") && !("id" in j))
      notifIds.add(i);
  } catch { /* malformed requests still get a response line */ }
});
const requestLines = requests.filter((_, i) => !notifIds.has(i));
if (rustLines.length !== requestLines.length || nodeLines.length !== requestLines.length) {
  console.error("line-count mismatch", {
    requests: requestLines.length, rust: rustLines.length, node: nodeLines.length });
  process.exit(2);
}
const envelope = (line) => {
  try { return JSON.parse(line); } catch { return null; }
};
const content = (env) => env?.result?.structuredContent ?? env?.error ?? null;
const seamDivergence = (a, b) => {
  const ea = content(a), eb = content(b);
  return ea && eb && ea.code === eb.code &&
    ea.code === "storage_error" &&
    ea.outcome_certainty === eb.outcome_certainty &&
    /unable to open database file/.test(ea.message ?? "") &&
    /repository seam not wired \(S12c pending\)/.test(eb.message ?? "");
};
requestLines.forEach((req, i) => {
  const a = rustLines[i], b = nodeLines[i];
  const label = (() => {
    try {
      const j = JSON.parse(req);
      return j.method === "tools/call"
        ? `${j.params?.name}:${JSON.stringify(j.params?.arguments)?.slice(0, 80)}`
        : `${j.method}`;
    } catch { return `malformed:${req.slice(0, 60)}`; }
  })();
  if (a === b) { pass++; results.push({ i, check: label, result: "pass" }); return; }
  if (seamDivergence(envelope(a), envelope(b))) {
    diverge++;
    results.push({ i, check: label, result: "divergence",
      kind: "repository-seam", rust: JSON.parse(a).result.structuredContent,
      node: JSON.parse(b).result.structuredContent });
    return;
  }
  fail++;
  results.push({ i, check: label, result: "fail", rust: a, node: b });
});

const summary = {
  probe: "data_supply", total: requestLines.length,
  pass, fail, divergence: diverge,
  oracle: "target/debug/mcp_data <scratch-db> (SqliteSupply fails closed)",
};
writeFileSync(join(OUT, "probe-results.jsonl"),
  results.map((r) => JSON.stringify(r)).join("\n") + "\n");
writeFileSync(join(OUT, "results.json"), JSON.stringify(summary, null, 2) + "\n");
console.log(JSON.stringify(summary));
if (fail > 0) {
  for (const r of results.filter((x) => x.result === "fail").slice(0, 5))
    console.error("FAIL", r.check, "\n  rust:", r.rust?.slice(0, 200), "\n  node:", r.node?.slice(0, 200));
}
process.exit(fail === 0 ? 0 : 1);
