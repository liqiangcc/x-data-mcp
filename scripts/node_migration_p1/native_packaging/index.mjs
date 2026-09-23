// Fail-closed loader for linux_guards.node.
// Throws at import time if the binding is absent — callers must never
// substitute shell flock helpers or PID files for the real syscalls.
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const addon = join(
  dirname(fileURLToPath(import.meta.url)), "..", "native", "linux_guards.node");
if (!existsSync(addon)) {
  throw new Error(
    "linux_guards.node is absent — run `npm run build:native` " +
    "(requires gcc + Node headers). There is no fallback: SO_PEERCRED " +
    "authorization and the flock fd lease are fail-closed.");
}
const { getpeereuid, flockfd } = createRequire(import.meta.url)(addon);
export { getpeereuid, flockfd };
