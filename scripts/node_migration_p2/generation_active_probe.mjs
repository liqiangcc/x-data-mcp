// NM-035 P2: validate generation_store active/activate against a live
// Rust oracle. Synthetic roots only. Exit nonzero on FAIL.
// Usage: node generation_active_probe.mjs <root> [selftest]
import { createHash } from "node:crypto";
import { execSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync, writeSync as writeSyncReal, openSync, closeSync } from "node:fs";
import { join, resolve } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
if (!existsSync(join(ROOT, "src_node/native/rename_noreplace.node"))) {
  execSync("gcc -shared -fPIC -O2 -o src_node/native/rename_noreplace.node src_node/native/rename_noreplace.c -I/usr/include/node", { cwd: ROOT });
}
const { GenerationStore, writeAllFd } = await import("../../src_node/governance/generation_store.mjs");
const { GenerationUsableError } = await import("../../src_node/governance/generation_usable.mjs");

const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/generation_active/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "generation_active", id, status, detail, expected, actual }));
};

const sha = (b) => createHash("sha256").update(b).digest("hex");
const FIX = mkdtempSync("/tmp/nm35-act-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const store = new GenerationStore(FIX);
const NOW = "2026-09-16T00:00:00Z";
const G1 = "11".repeat(32), G2 = "22".repeat(32);
const AP = join(FIX, "active-generation.json");
const installed = (gid) => ({ generation_id: gid, manifest_sha256: sha(gid) });

const run = (fn) => {
  try { return { status: "ok", value: fn() }; }
  catch (e) {
    return { status: "error", kind: e instanceof GenerationUsableError ? e.kind : "other", message: e.message };
  }
};
const HOOKS = {
  active_bad_json: () => writeFileSync(AP, "{bad"),
  active_unknown_field: () => writeFileSync(AP, '{"schema":"x-data-mcp.active-generation.v1","generation_id":"a","manifest_sha256":"b","activated_at":"2026-09-16T00:00:00Z","previous_generation_id":null,"activation_request_id":"r","extra":1}'),
  active_bad_ts: () => writeFileSync(AP, '{"schema":"x-data-mcp.active-generation.v1","generation_id":"a","manifest_sha256":"b","activated_at":"junk","previous_generation_id":null,"activation_request_id":"r"}'),
  active_wrong_schema: () => writeFileSync(AP, '{"schema":"wrong","generation_id":"a","manifest_sha256":"b","activated_at":"2026-09-16T00:00:00Z","previous_generation_id":null,"activation_request_id":"r"}'),
  active_missing_field: () => writeFileSync(AP, '{"schema":"x-data-mcp.active-generation.v1","generation_id":"a","manifest_sha256":"b","activated_at":"2026-09-16T00:00:00Z","activation_request_id":"r"}'),
  activate_first: () => rmSync(AP, { force: true }),
};
const SEQ = {
  active_absent: () => run(() => store.active() === null),
  active_bad_json: () => run(() => store.active()),
  active_unknown_field: () => run(() => store.active()),
  active_bad_ts: () => run(() => store.active()),
  active_wrong_schema: () => run(() => store.active()?.schema),
  active_missing_field: () => run(() => store.active() !== null),
  activate_first: () => run(() => { const p = store.activate(installed(G1), null, "req-1", NOW); return { gid: p.generation_id, prev: p.previous_generation_id, bytes: [...readFileSync(AP)] }; }),
  activate_conflict_none: () => run(() => store.activate(installed(G2), null, "req-2", NOW)),
  pointer_after_conflict: () => ({ status: "ok", value: sha(readFileSync(AP)) }),
  activate_conflict_wrong: () => run(() => store.activate(installed(G2), G2, "req-2", NOW)),
  activate_second: () => run(() => { const p = store.activate(installed(G2), G1, "req-2", NOW); return { gid: p.generation_id, prev: p.previous_generation_id, bytes: [...readFileSync(AP)] }; }),
  reopen_active: () => run(() => { const p = new GenerationStore(FIX).active(); return [p.generation_id, p.previous_generation_id, p.activation_request_id]; }),
  activate_escapes: () => run(() => {
    const s2 = new GenerationStore(FIX_ESC);
    const p = s2.activate(installed(G1), null, oracle.find((x) => x.id === "activate_escapes").value.req, NOW);
    return { bytes: [...readFileSync(join(FIX_ESC, "active-generation.json"))], req: p.activation_request_id };
  }),
};
const FIX_ESC = join(FIX, "esc");
mkdirSync(FIX_ESC, { recursive: true });

for (const o of oracle) {
  if (HOOKS[o.id]) HOOKS[o.id]();
  const got = SEQ[o.id]();
  if (o.status === "ok") {
    if (got.status !== "ok") { emit(o.id, "FAIL", `threw ${got.kind}: ${got.message}`); continue; }
    let ok;
    if (typeof o.value === "object" && o.value !== null && !Array.isArray(o.value) &&
        typeof got.value === "object" && got.value !== null) {
      // serde_json emits map keys sorted — compare field-wise
      ok = Object.keys(o.value).length === Object.keys(got.value).length &&
        Object.keys(o.value).every((k) => JSON.stringify(o.value[k]) === JSON.stringify(got.value[k]));
    } else {
      ok = JSON.stringify(got.value) === JSON.stringify(o.value);
    }
    emit(o.id, ok ? "PASS" : "FAIL", ok ? "ok" : "value mismatch", o.value, got.value);
  } else {
    // serde error inner text differs; compare kind + stable prefix
    const ok = got.status === "error" && got.kind === o.kind &&
      (o.kind === "activation_conflict" ? got.message === o.message
        : got.message.startsWith("manifest JSON invalid"));
    emit(o.id, ok ? "PASS" : "FAIL", ok ? o.kind : "mismatch", o.message, got.message);
  }
}

// ---------- caller-held exclusion: real cross-process flock ----------
{
  // Acquire a kernel flock on a lease file via the P1 native binding;
  // a second process trying the same lock fails — proving exclusion is
  // real, not an in-process flag.
  const { createRequire } = await import("node:module");
  const lg = createRequire(import.meta.url)("../node_migration_p1/native/linux_guards.node");
  const { openSync, constants: FC, writeFileSync: wf } = await import("node:fs");
  const lease = join(FIX, "store.lease");
  wf(lease, "");
  const lfd = openSync(lease, FC.O_RDWR);
  const rc = lg.flockfd(lfd, 2 /* LOCK_EX|NB */);
  emit("lease_held", rc === 0 ? "PASS" : "FAIL", `flock rc=${rc}`);
  // second process cannot take it
  const child = execSync(`node -e "
    const m=require('${resolve(ROOT,'scripts/node_migration_p1/native/linux_guards.node')}');
    const fs=require('node:fs');
    const fd=fs.openSync('${lease}',fs.constants.O_RDWR);
    const rc=m.flockfd(fd, 6);
    process.stdout.write(String(rc));"`, { encoding: "utf8" });
  emit("lease_cross_process_conflict", Number(child) === 11 ? "PASS" : "FAIL", `child errno=${child} (EWOULDBLOCK=11)`);
  // activate while holding the lease works
  const s2 = new GenerationStore(FIX);
  const got = run(() => s2.activate(installed(G2), G2, "req-3", NOW));
  emit("activate_under_lease", got.status === "ok" ? "PASS" : "FAIL", got.status === "ok" ? "ok" : got.message);
}
// ---------- R1: tmp-symlink negative + injected short write ----------
{
  const { symlinkSync, readFileSync: rf } = await import("node:fs");
  const out = join(FIX, "tmp-escape-out.txt");
  writeFileSync(out, "ORIGINAL");
  const tmp = join(FIX, ".active-generation.json.tmp");
  rmSync(tmp, { force: true });
  symlinkSync(out, tmp);
  const before = rf(out, "utf8");
  try { store.activate(installed(G1), G2, "req-tmp", NOW); emit("tmp_symlink_reject", "FAIL", "wrote through symlink"); }
  catch (e) {
    emit("tmp_symlink_reject", e.kind === "io" && rf(out, "utf8") === before && readFileSync(tmp).length >= 0 ? "PASS" : "FAIL", e.kind);
  }
  rmSync(tmp, { force: true });
}
// short-write injection: writeAllFd must loop; zero-write is an error
{
  const { openSync, closeSync } = await import("node:fs");
  const tgt = join(FIX, "shortwrite.bin");
  const fd = openSync(tgt, "w");
  let calls = 0;
  writeAllFd(fd, Buffer.from("abcdef"), (f, b, o, l) => { calls++; return writeSyncReal(f, b, o, Math.min(l, 2)); });
  closeSync(fd);
  emit("short_write_loops", calls === 3 && readFileSync(tgt, "utf8") === "abcdef" ? "PASS" : "FAIL", `calls=${calls}`);
  const fd2 = openSync(tgt, "w");
  try { writeAllFd(fd2, Buffer.from("x"), () => 0); emit("zero_write_io", "FAIL", "accepted"); }
  catch (e) { emit("zero_write_io", e.kind === "io" ? "PASS" : "FAIL", e.kind); }
  closeSync(fd2);
  // string input is rejected (string overload would corrupt offsets)
  const fd3 = openSync(tgt, "w");
  try { writeAllFd(fd3, "abcdef", () => 1); emit("string_input_rejected", "FAIL", "accepted string"); }
  catch (e) { emit("string_input_rejected", e.kind === "invalid_input" ? "PASS" : "FAIL", e.kind); }
  closeSync(fd3);
  // multibyte short-write through the same byte path activate uses:
  // Buffer.from(pretty) with a multibyte request id, 1-byte writer
  const mbytes = Buffer.from(JSON.stringify({ r: "中😀x" }), "utf8");
  const fd4 = openSync(tgt, "w");
  writeAllFd(fd4, mbytes, (f, b, o, l) => writeSyncReal(f, b, o, 1));
  closeSync(fd4);
  emit("multibyte_short_write", readFileSync(tgt).equals(mbytes) ? "PASS" : "FAIL", `${mbytes.length}B persisted byte-exact`);
}
// malformed-JSON prefix normalized once (finding 4)
{
  writeFileSync(AP, "{bad");
  try { store.active(); emit("json_prefix_once", "FAIL", "no throw"); }
  catch (e) {
    emit("json_prefix_once", /^manifest JSON invalid: [^m]/.test(e.message) && !e.message.includes("manifest JSON invalid: manifest") ? "PASS" : "FAIL", e.message.slice(0, 80));
  }
  rmSync(AP);
  store.activate(installed(G2), null, "req-4", NOW); // restore pointer for consistency
}
// malformed now rejected before any mutation
{
  const before = sha(readFileSync(AP));
  try { store.activate(installed(G1), G2, "req-9", "not-a-date"); emit("bad_now_no_mutation", "FAIL", "accepted"); }
  catch (e) {
    emit("bad_now_no_mutation", e.kind === "invalid_input" && sha(readFileSync(AP)) === before ? "PASS" : "FAIL", e.kind);
  }
}

// selftest
if (SELFTEST === "act") {
  const s = new GenerationStore(join(FIX, "self"));
  mkdirSync(join(FIX, "self"), { recursive: true });
  const p = s.activate(installed(G1), null, "r", NOW);
  emit("selftest_act", p.generation_id === "0".repeat(64) ? "PASS" : "FAIL",
    p.generation_id === "0".repeat(64) ? null : "expected wrong gid — STOP");
}

process.exitCode = failures === 0 ? 0 : 1;
