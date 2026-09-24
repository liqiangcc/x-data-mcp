// Regenerate src_node/data_supply/registry_descriptors.json from the
// Rust oracle: runs `mcp_data <scratch>` (a nonexistent scratch path is
// fine — tools/list never touches storage), sends tools/list, and writes
// [{name, input, output}] in registry order. The probe byte-compares the
// Node tools/list against this same oracle output on every run, so a
// regenerated file that drifted would fail the probe.
//
//   cargo build --bin mcp_data
//   TMPDIR=/dev/shm node scripts/node_migration_p4/gen_registry_descriptors.mjs

import { spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const scratch = join(mkdtempSync(join(tmpdir(), "nm063-gen-")), "scratch.db");
const req = JSON.stringify({
  jsonrpc: "2.0", id: 1, method: "tools/list", params: {},
});
const r = spawnSync(join(REPO, "target/debug/mcp_data"), [scratch], {
  input: req + "\n", encoding: "utf8", maxBuffer: 64 * 1024 * 1024,
});
if (r.status !== 0) {
  console.error("mcp_data failed:", r.stderr);
  process.exit(1);
}
const tools = JSON.parse(r.stdout).result.tools;
const descriptors = tools.map((t) => ({
  name: t.name, input: t.inputSchema, output: t.outputSchema,
}));
writeFileSync(
  join(REPO, "src_node/data_supply/registry_descriptors.json"),
  JSON.stringify(descriptors, null, 2) + "\n");
console.log(`wrote ${descriptors.length} descriptors`);
