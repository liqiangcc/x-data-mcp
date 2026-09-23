// NM-010: reproducible build for linux_guards.node.
// Hard-stops (exit 1, JSON error) when toolchain or headers are absent —
// never degrades to a substitute lease/credential mechanism.
// Env override: NODE_INCLUDE (default /usr/include/node), CC (default gcc).
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..", "..");
const src = join(root, "scripts/node_migration_p1/native/linux_guards.c");
const out = join(root, "scripts/node_migration_p1/native/linux_guards.node");
const include = process.env.NODE_INCLUDE || "/usr/include/node";
const cc = process.env.CC || "gcc";

const missing = [];
if (!existsSync(src)) missing.push(src);
if (!existsSync(join(include, "node_api.h"))) missing.push(join(include, "node_api.h"));
const ccv = spawnSync(cc, ["--version"], { encoding: "utf8" });
if (ccv.status !== 0) missing.push(cc);
if (missing.length) {
  console.error(JSON.stringify({
    status: "BLOCKED",
    reason: "native toolchain/headers absent — addon cannot be built; " +
            "no fallback lease or credential mechanism exists",
    missing,
  }));
  process.exit(1);
}

const r = spawnSync(cc, ["-shared", "-fPIC", "-O2", "-o", out, src, `-I${include}`], { stdio: "inherit" });
if (r.status !== 0) {
  console.error(JSON.stringify({ status: "FAIL", step: "compile", exit: r.status }));
  process.exit(r.status ?? 1);
}

console.log(JSON.stringify({
  status: "BUILT",
  artifact: out,
  sha256: createHash("sha256").update(readFileSync(out)).digest("hex"),
  node: process.version,
  napi_version: process.versions.napi,
  node_module_version: process.versions.modules,
  compiler: ccv.stdout.split("\n")[0],
  include,
}));
