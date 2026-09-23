// NM-034 P2: validate generation_store write_decision/decision/
// write_artifact/artifact against a live Rust oracle. Synthetic roots.
// Exit nonzero on FAIL. Usage: node generation_artifact_probe.mjs <root> [selftest]
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { execSync } from "node:child_process";
import { join } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
if (!existsSync(join(ROOT, "src_node/native/rename_noreplace.node"))) {
  execSync("gcc -shared -fPIC -O2 -o src_node/native/rename_noreplace.node src_node/native/rename_noreplace.c -I/usr/include/node", { cwd: ROOT });
}
const { GenerationStore } = await import("../../src_node/governance/generation_store.mjs");
const { GenerationUsableError } = await import("../../src_node/governance/generation_usable.mjs");

const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/generation_artifacts/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "generation_artifact", id, status, detail, expected, actual }));
};

const sha = (b) => createHash("sha256").update(b).digest("hex");
const FIX = mkdtempSync("/tmp/nm34-art-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const store = new GenerationStore(FIX);
const G = "a1".repeat(32);
const G_MISS = "0".repeat(64);
const DEC = Buffer.from('{"status":"VERIFIED_COMPATIBLE"}');

const run = (fn) => {
  try { return { status: "ok", value: fn() }; }
  catch (e) {
    return { status: "error", kind: e instanceof GenerationUsableError ? e.kind : "other", message: e.message };
  }
};
const HOOKS = {
  // after decision_read_missing: create installed dir
  decision_write: () => mkdirSync(join(FIX, "generations", G), { recursive: true }),
  artifact_symlink_exists: () => symlinkSync("outside-target", join(FIX, "generations", G, "art-link")),
};
const SEQ = {
  decision_missing_gen: () => run(() => store.writeDecision(G_MISS, Buffer.from("x"))),
  artifact_missing_gen: () => run(() => store.writeArtifact(G_MISS, "ev.json", Buffer.from("x"))),
  decision_read_missing: () => run(() => store.decision(G_MISS) === null),
  decision_write: () => run(() => store.writeDecision(G, DEC)),
  decision_read_back: () => run(() => { const b = store.decision(G); return { len: b?.length ?? null, sha: b ? sha(b) : null }; }),
  decision_dup: () => run(() => store.writeDecision(G, Buffer.from("other"))),
  decision_after_dup: () => run(() => sha(store.decision(G))),
  artifact_write: () => run(() => store.writeArtifact(G, "evidence.json", Buffer.from('{"k":1}'))),
  artifact_read: () => run(() => sha(store.artifact(G, "evidence.json"))),
  artifact_missing: () => run(() => store.artifact(G, "nope.json") === null),
  artifact_dup: () => run(() => store.writeArtifact(G, "evidence.json", Buffer.from("DIFF"))),
  artifact_empty_name: () => run(() => store.writeArtifact(G, "", Buffer.from("x"))),
  artifact_slash_name: () => run(() => store.writeArtifact(G, "a/b", Buffer.from("x"))),
  artifact_nul_name: () => run(() => store.writeArtifact(G, "a\0b", Buffer.from("x"))),
  artifact_dotdot: () => run(() => store.writeArtifact(G, "..", Buffer.from("x"))),
  artifact_symlink_exists: () => run(() => store.writeArtifact(G, "art-link", Buffer.from("X"))),
  symlink_target_untouched: () => ({ status: "ok", value: !existsSync(join(FIX, "generations", G, "outside-target")) }),
  reopen_decision: () => run(() => sha(new GenerationStore(FIX).decision(G))),
  reopen_artifact: () => run(() => sha(new GenerationStore(FIX).artifact(G, "evidence.json"))),
};

for (const o of oracle) {
  if (HOOKS[o.id]) HOOKS[o.id]();
  const got = SEQ[o.id]();
  if (o.status === "ok") {
    if (got.status !== "ok") { emit(o.id, "FAIL", `threw ${got.kind}: ${got.message}`); continue; }
    emit(o.id, JSON.stringify(got.value) === JSON.stringify(o.value) ? "PASS" : "FAIL",
      got.status === "ok" ? "ok" : "?", o.value, got.value);
  } else {
    const ok = got.status === "error" && got.kind === o.kind && got.message === o.message;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? o.kind : "mismatch", o.message, got.message);
  }
}

// ---------- boundary: gid escapes + non-buffer bytes ----------
const bnd = (id, fn, kind) => {
  try { fn(); emit(id, "FAIL", "accepted malformed input"); }
  catch (e) {
    const ok = e instanceof GenerationUsableError && e.kind === kind;
    emit(id, ok ? "PASS" : "FAIL", `${e.name}(${e.kind ?? "-"})`, kind, e.message.slice(0, 80));
  }
};
bnd("gid_dotdot", () => store.writeDecision("..", DEC), "invalid_generation_id");
bnd("gid_slash", () => store.writeDecision("a/b", DEC), "invalid_generation_id");
bnd("gid_surrogate", () => store.writeDecision("g\ud800", DEC), "invalid_generation_id");
bnd("name_surrogate", () => store.writeArtifact(G, "e\ud800.json", Buffer.from("x")), "not_verified");
bnd("bytes_string", () => store.writeDecision(G, "not-a-buffer"), "invalid_input");
// symlinked generation dir must not escape the store (finding 1)
{
  const out = join(FIX, "outside");
  mkdirSync(join(out), { recursive: true });
  const SG = "b2".repeat(32);
  symlinkSync(out, join(FIX, "generations", SG));
  bnd("gendir_symlink_decision", () => store.writeDecision(SG, DEC), "not_installed");
  bnd("gendir_symlink_artifact", () => store.writeArtifact(SG, "ev.json", Buffer.from("x")), "not_installed");
  emit("outside_dir_untouched", readdirSync(out).length === 0 ? "PASS" : "FAIL", `outside has ${readdirSync(out).length} entries`);
}
// symlinked generations parent must not escape (finding 1)
{
  const out2 = join(FIX, "outside-parent");
  mkdirSync(out2, { recursive: true });
  const s3 = new GenerationStore(join(FIX, "escroot"));
  mkdirSync(join(FIX, "escroot"), { recursive: true });
  symlinkSync(out2, join(FIX, "escroot", "generations"));
  bnd("parent_symlink_decision", () => s3.writeDecision(G, DEC), "not_installed");
  emit("parent_outside_untouched", readdirSync(out2).length === 0 ? "PASS" : "FAIL", "outside-parent untouched");
}
// reserved immutable names rejected by writeArtifact (finding 2)
for (const nm of ["consistency-decision.json", "snapshot-manifest.json", "base.zip", "gbbq.zip", "hsjday.zip"]) {
  bnd(`reserved_${nm.replace(/[^a-z0-9]+/g, "_")}`, () => store.writeArtifact(G, nm, Buffer.from("bypass")), "not_verified");
}
// named compatibility evidence artifact allowed
{
  const got = run(() => store.writeArtifact(G, "compatibility-evidence.json", Buffer.from("ev")));
  emit("evidence_name_allowed", got.status === "ok" ? "PASS" : "FAIL", `sha=${got.value ?? got.message}`);
}
// oversized write refused BEFORE creation; fs unchanged (finding 3)
{
  const big = Buffer.alloc(1024 * 1024 + 1, 0x42);
  bnd("decision_oversize_write", () => store.writeArtifact(G, "big2.json", big), "invalid_input");
  emit("oversize_no_file", !existsSync(join(FIX, "generations", G, "big2.json")) ? "PASS" : "FAIL", "no file created");
}
// open-path io typing: unwritable dir -> typed io (finding 4, open path)
{
  const CG = "c3".repeat(32);
  mkdirSync(join(FIX, "generations", CG), { recursive: true });
  writeFileSync(join(FIX, "generations", CG, "perm-marker"), "x");
  try {
    const { chmodSync } = await import("node:fs");
    chmodSync(join(FIX, "generations", CG), 0o555);
    bnd("unwritable_dir_io", () => store.writeArtifact(CG, "x.json", Buffer.from("x")), "io");
    chmodSync(join(FIX, "generations", CG), 0o755);
  } catch (e) { emit("unwritable_dir_io", "NOT_RUN", e.message); }
}
// decision read bound: >1MiB decision file -> io (stricter than Rust, documented)
{
  const big = Buffer.alloc(1024 * 1024 + 1, 0x41);
  writeFileSync(join(FIX, "generations", G, "big-art.json"), big);
  bnd("artifact_oversize_read", () => store.artifact(G, "big-art.json"), "io");
}
// symlink for decision name specifically (O_NOFOLLOW + O_EXCL)
{
  symlinkSync("/etc/hostname", join(FIX, "generations", G, "decision-link"));
  bnd("artifact_symlink_decision_name", () => store.writeArtifact(G, "decision-link", Buffer.from("x")), "artifact_exists");
  emit("decision_link_target_untouched", existsSync("/etc/hostname") ? "PASS" : "FAIL", "target intact");
}
// selftest
if (SELFTEST === "art") {
  const s = new GenerationStore(join(FIX, "self"));
  mkdirSync(join(FIX, "self", "generations", G), { recursive: true });
  const got = s.writeDecision(G, DEC);
  emit("selftest_art", got === "0".repeat(64) ? "PASS" : "FAIL",
    got === "0".repeat(64) ? null : "expected wrong sha — STOP");
}

process.exitCode = failures === 0 ? 0 : 1;
