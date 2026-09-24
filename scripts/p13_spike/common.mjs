// NM-061 S13 spike shared helpers. Each proof emits JSONL
// {primitive,id,status,detail,evidence} and exits nonzero on any FAIL,
// matching the P1/P2/P3 probe convention.
import fs from "node:fs";
import { execSync } from "node:child_process";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

export const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const require = createRequire(import.meta.url);

export const ADDON = join(ROOT, "src_node/native/rename_noreplace.node");
const ADDON_SRC = join(ROOT, "src_node/native/rename_noreplace.c");
export function loadNative() {
  if (!fs.existsSync(ADDON)) {
    execSync(
      `gcc -shared -fPIC -O2 -o ${ADDON} ${ADDON_SRC} -I/usr/include/node`,
      { cwd: ROOT, stdio: "inherit" });
  }
  return require(ADDON);
}

let failures = 0;
export const emit = (primitive, id, status, detail, evidence = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ primitive, id, status, detail, evidence }));
};
export const run = (p, id, fn) => {
  try { emit(p, id, "PASS", null, fn()); }
  catch (e) { emit(p, id, "FAIL", `${e.code || ""} ${e.message}`); }
};
export const expectErr = (p, id, re, fn) => {
  try { fn(); emit(p, id, "FAIL", "expected rejection but succeeded"); }
  catch (e) {
    const msg = `${e.code || ""} ${e.message}`;
    emit(p, id, re.test(msg) ? "PASS" : "FAIL", msg);
  }
};
export const finish = () => process.exit(failures ? 1 : 0);

// argv[2]: private (0700, same-UID, tmpfs) scratch dir for fixtures.
export const dir = process.argv[2];
