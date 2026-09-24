// NM-037 P2: validate generation_store retireBytes against a live Rust
// oracle. Synthetic roots only. Exit nonzero on FAIL.
// Usage: node generation_retire_probe.mjs <root> [selftest]
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { execSync } from "node:child_process";
import { join } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
if (!existsSync(join(ROOT, "src_node/native/rename_noreplace.node"))) {
  execSync("gcc -shared -fPIC -O2 -o src_node/native/rename_noreplace.node src_node/native/rename_noreplace.c -I/usr/include/node", { cwd: ROOT });
}
const { GenerationStore, REQUIRED_ARTIFACTS } = await import("../../src_node/governance/generation_store.mjs");
const { GenerationUsableError } = await import("../../src_node/governance/generation_usable.mjs");

const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/generation_retire/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "generation_retire", id, status, detail, expected, actual }));
};

const FIX = mkdtempSync("/tmp/nm37-ret-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const G = "cc".repeat(32), G_MISS = "dd".repeat(32), G2 = "ee".repeat(32), G3 = "ff".repeat(32);
const gdir = join(FIX, "generations", G);
mkdirSync(gdir, { recursive: true });
for (const n of REQUIRED_ARTIFACTS) writeFileSync(join(gdir, n), "raw");
writeFileSync(join(gdir, "snapshot-manifest.json"), "{}");
writeFileSync(join(gdir, "consistency-decision.json"), "{}");
writeFileSync(join(gdir, "compatibility-evidence.json"), "{}");
const store = new GenerationStore(FIX);

const run = (fn) => {
  try { return { status: "ok", value: fn() }; }
  catch (e) {
    return { status: "error", kind: e instanceof GenerationUsableError ? e.kind : "other", message: e.message };
  }
};
const HOOKS = {
  retire_partial: () => {
    mkdirSync(join(FIX, "generations", G2), { recursive: true });
    writeFileSync(join(FIX, "generations", G2, "base.zip"), "raw");
  },
  retire_artifact_is_dir: () => {
    mkdirSync(join(FIX, "generations", G3, "base.zip"), { recursive: true });
  },
};
const listing = (g) => readdirSync(join(FIX, "generations", g)).sort();
const SEQ = {
  artifacts_set: () => ({ status: "ok", value: REQUIRED_ARTIFACTS }),
  retire_missing_gen: () => run(() => { store.retireBytes(G_MISS); return true; }),
  retire_first: () => run(() => { store.retireBytes(G); return true; }),
  listing_after: () => ({ status: "ok", value: listing(G) }),
  retire_idempotent: () => run(() => { store.retireBytes(G); return true; }),
  listing_after_repeat: () => ({ status: "ok", value: listing(G) }),
  retire_partial: () => run(() => { store.retireBytes(G2); return true; }),
  listing_partial: () => ({ status: "ok", value: listing(G2) }),
  retire_artifact_is_dir: () => run(() => { store.retireBytes(G3); return true; }),
  retire_bad_gid: () => run(() => { store.retireBytes("../x"); return true; }),
};

for (const o of oracle) {
  if (HOOKS[o.id]) HOOKS[o.id]();
  const got = SEQ[o.id]();
  if (o.status === "ok") {
    if (got.status !== "ok") { emit(o.id, "FAIL", `threw ${got.kind}: ${got.message}`); continue; }
    emit(o.id, JSON.stringify(got.value) === JSON.stringify(o.value) ? "PASS" : "FAIL", "ok", o.value, got.value);
  } else {
    // Node rejects bad gids at the boundary (stricter, documented);
    // io inner text is OS-formatted — compare kind + stable prefix.
    const ok = got.status === "error" && (
      (o.id === "retire_bad_gid" && got.kind === "invalid_generation_id") ||
      (got.kind === o.kind && (o.kind === "io" ? got.message.startsWith("io:") : got.message === o.message)));
    emit(o.id, ok ? "PASS" : "FAIL", ok ? `${o.kind} (boundary)` : "mismatch", o.message, got.message);
  }
}

// ---------- Node-only: symlink artifact + pinned-dir removal ----------
{
  const s2 = new GenerationStore(join(FIX, "r2"));
  const g = "ab".repeat(32);
  const gd = join(FIX, "r2", "generations", g);
  mkdirSync(gd, { recursive: true });
  const outside = join(FIX, "r2-out.bin");
  writeFileSync(outside, "MUST-SURVIVE");
  // base.zip is a symlink to the outside file — unlink removes the link,
  // never the target (fd-pinned unlinkat, AT_NOFOLLOW semantics)
  symlinkSync(outside, join(gd, "base.zip"));
  writeFileSync(join(gd, "gbbq.zip"), "r"); writeFileSync(join(gd, "hsjday.zip"), "r");
  const g2 = run(() => s2.retireBytes(g));
  emit("retire_symlink_artifact", g2.status === "ok" && readFileSync(outside, "utf8") === "MUST-SURVIVE" &&
    !existsSync(join(gd, "base.zip")) ? "PASS" : "FAIL",
    `symlink removed, outside untouched, status=${g2.status}`);
}
// symlinked generation dir -> not_installed, outside dir untouched
{
  const out = join(FIX, "r3-out");
  mkdirSync(out, { recursive: true });
  const s3 = new GenerationStore(join(FIX, "r3"));
  mkdirSync(join(FIX, "r3", "generations"), { recursive: true });
  const SG = "cd".repeat(32);
  symlinkSync(out, join(FIX, "r3", "generations", SG));
  try { s3.retireBytes(SG); emit("retire_symlink_gendir", "FAIL", "followed symlink"); }
  catch (e) {
    emit("retire_symlink_gendir", e.kind === "not_installed" && readdirSync(out).length === 0 ? "PASS" : "FAIL", e.kind);
  }
}

// selftest
if (SELFTEST === "retire") {
  const s = new GenerationStore(join(FIX, "self"));
  const g = "ef".repeat(32);
  mkdirSync(join(FIX, "self", "generations", g), { recursive: true });
  s.retireBytes(g);
  emit("selftest_retire", readdirSync(join(FIX, "self", "generations", g)).includes("ghost.zip") ? "PASS" : "FAIL",
    "expected leftover artifact — STOP");
}

process.exitCode = failures === 0 ? 0 : 1;
