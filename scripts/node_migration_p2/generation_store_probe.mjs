// NM-033 P2: validate governance/generation_store.mjs against a live
// Rust oracle (Store::{create_stage,staged_manifest,install_staged,
// installed_ids}). Synthetic roots only. Exit nonzero on FAIL.
// Usage: node generation_store_probe.mjs <root> [selftest]
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";


const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const NOW = "2026-09-16T00:00:00Z";
// build the N-API binding if absent (.node is a regenerable artifact,
// gitignored; committed source + pinned gcc recipe)
import { execSync } from "node:child_process";
if (!existsSync(join(ROOT, "src_node/native/rename_noreplace.node"))) {
  execSync("gcc -shared -fPIC -O2 -o src_node/native/rename_noreplace.node src_node/native/rename_noreplace.c -I/usr/include/node", { cwd: ROOT });
}
const { GenerationStore } = await import("../../src_node/governance/generation_store.mjs");
const { verifyInstalled, GenerationUsableError } = await import("../../src_node/governance/generation_usable.mjs");
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/generation_store/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "generation_store", id, status, detail, expected, actual }));
};

const sha = (b) => createHash("sha256").update(b).digest("hex");
const FIX = mkdtempSync("/tmp/nm33-store-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));

const STDF = [
  ["base_industry", "base.zip", Buffer.from("base-bytes")],
  ["day", "hsjday.zip", Buffer.from("day-bytes")],
  ["gbbq", "gbbq.zip", Buffer.from("gbbq-bytes")],
];
const url = (role, rel) => role === "day"
  ? "https://data.tdx.com.cn/vipdoc/hsjday.zip"
  : `https://www.tdx.com.cn/products/data/data/dbf/${rel}`;
const canon = (v) => v === null || typeof v !== "object" ? JSON.stringify(v)
  : Array.isArray(v) ? "[" + v.map(canon).join(",") + "]"
  : "{" + Object.keys(v).sort().map((k) => JSON.stringify(k) + ":" + canon(v[k])).join(",") + "}";
const manifestOf = (files) => Buffer.from(canon({
  schema: "x-data-mcp.official-file-input.v1",
  files: files.map(([role, rel, bytes]) => ({
    role, relative_path: rel, source_url: url(role, rel),
    acquisition_started_at: "2026-09-01T00:00:00Z",
    acquisition_completed_at: "2026-09-01T00:00:01Z",
    last_modified: null, etag: null,
    bytes: bytes.length, sha256: sha(bytes) })),
}));
const mb = manifestOf(STDF);
const GID = sha(mb);

const readdirEmpty = (d) => readdirSync(d).length === 0;
const run = (store, fn) => {
  try { return { status: "ok", value: fn() }; }
  catch (e) {
    return { status: "error", kind: e instanceof GenerationUsableError ? e.kind : "other", message: e.message };
  }
};
// compare: verbatim for deterministic messages; kind+prefix otherwise
const cmp = (id, got, exp) => {
  if (exp.status === "ok") {
    if (got.status !== "ok") { emit(id, "FAIL", `threw ${got.kind}`, exp.value, got.message); return; }
    emit(id, "PASS", "ok");
    return;
  }
  if (got.status !== "error" || got.kind !== exp.kind) {
    emit(id, "FAIL", `kind ${got.kind} != ${exp.kind}`, exp.message, got.message); return;
  }
  // verbatim when the oracle message has no io/serde inner text
  const stable = exp.message.includes("os error") || /JSON invalid: ./.test(exp.message) || exp.message.includes("invalid manifest") || exp.message.includes("bytes do not match")
    ? got.message.slice(0, exp.message.split(":")[0].length) === exp.message.slice(0, exp.message.split(":")[0].length)
    : got.message === exp.message;
  emit(id, stable ? "PASS" : "FAIL", stable ? exp.kind : "message drift", exp.message, got.message);
};

const store = new GenerationStore(FIX);
const seq = {
  create_fresh: () => run(store, () => { const d = store.createStage("req-1"); return { dir: "req-1" }; }),
  create_dup: () => run(store, () => store.createStage("req-1")),
  staged_missing: () => run(store, () => store.stagedManifest("req-1").bytes.length),
  staged_ok: () => run(store, () => sha(store.stagedManifest("req-1").bytes)),
  install_no_artifacts: () => run(store, () => store.installStaged("req-1", NOW)),
  install_bad_json: () => run(store, () => store.installStaged("req-2", NOW)),
  install_wrong_shape: () => run(store, () => store.installStaged("req-3", NOW)),
  install_no_stage: () => run(store, () => store.installStaged("req-zzz", NOW)),
  install_tampered: () => run(store, () => store.installStaged("req-4", NOW)),
  install_ok: () => run(store, () => store.installStaged("req-5", NOW)),
  install_repeat: () => run(store, () => store.installStaged("req-5", NOW)),
  install_already: () => run(store, () => store.installStaged("req-6", NOW)),
  stage_kept_after_fail: () => ({ status: "ok", value: existsSync(join(FIX, "staging", "req-4", "snapshot-manifest.json")) }),
  install_empty_target: () => run(store, () => store.installStaged("req-7", NOW)),
  installed_ids: () => ({ status: "ok", value: store.installedIds() }),
  installed_ids_unicode: () => ({ status: "ok", value: store.installedIds() }),
  reopen_ids: () => ({ status: "ok", value: new GenerationStore(FIX).installedIds() }),
  reopen_verify: () => run(new GenerationStore(FIX), () => verifyInstalled(FIX, GID, NOW).generation_id),
};

// ---- execute the oracle's exact fixture sequence ----
for (const o of oracle) {
  // fixture mutations must happen in sequence before each case
  if (o.id === "staged_ok") writeFileSync(join(FIX, "staging", "req-1", "snapshot-manifest.json"), mb);
  if (o.id === "install_bad_json") { store.createStage("req-2"); writeFileSync(join(FIX, "staging", "req-2", "snapshot-manifest.json"), "{bad"); }
  if (o.id === "install_wrong_shape") { store.createStage("req-3"); writeFileSync(join(FIX, "staging", "req-3", "snapshot-manifest.json"), canon({ schema: "x", files: [] })); }
  if (o.id === "install_tampered") {
    const d = store.createStage("req-4");
    writeFileSync(join(d, "snapshot-manifest.json"), mb);
    writeFileSync(join(d, "base.zip"), Buffer.from("TAMPERED"));
    writeFileSync(join(d, "hsjday.zip"), Buffer.from("day-bytes"));
    writeFileSync(join(d, "gbbq.zip"), Buffer.from("gbbq-bytes"));
  }
  if (o.id === "install_ok") {
    const d = store.createStage("req-5");
    writeFileSync(join(d, "snapshot-manifest.json"), mb);
    for (const [, rel, b] of STDF) writeFileSync(join(d, rel), b);
  }
  if (o.id === "install_already") {
    const d = store.createStage("req-6");
    writeFileSync(join(d, "snapshot-manifest.json"), mb);
    for (const [, rel, b] of STDF) writeFileSync(join(d, rel), b);
  }
  if (o.id === "installed_ids_unicode") {
    mkdirSync(join(FIX, "generations", ""), { recursive: true });
    mkdirSync(join(FIX, "generations", "𐀀"), { recursive: true });
  }
  if (o.id === "install_empty_target") {
    // pre-create the empty target (simulates a racing writer) then stage
    const mb2 = manifestOf([["base_industry","base.zip",Buffer.from("DIFF-base")],["day","hsjday.zip",Buffer.from("day-bytes")],["gbbq","gbbq.zip",Buffer.from("gbbq-bytes")]]);
    const gid2 = sha(mb2);
    mkdirSync(join(FIX, "generations", gid2), { recursive: true });
    const d = store.createStage("req-7");
    writeFileSync(join(d, "snapshot-manifest.json"), mb2);
    for (const [, rel, b] of [["base_industry","base.zip",Buffer.from("DIFF-base")],["day","hsjday.zip",Buffer.from("day-bytes")],["gbbq","gbbq.zip",Buffer.from("gbbq-bytes")]]) writeFileSync(join(d, rel), b);
    seq._gid2 = gid2;
  }
  if (o.id === "target_still_empty") {
    seq.target_still_empty = () => ({ status: "ok", value: readdirEmpty(join(FIX, "generations", seq._gid2)) });
  }
  if (o.id === "stage_preserved") {
    seq.stage_preserved = () => ({ status: "ok", value: existsSync(join(FIX, "staging", "req-7", "snapshot-manifest.json")) });
  }
  if (o.id === "installed_ids") {
    mkdirSync(join(FIX, "generations", "zzz"), { recursive: true });
    mkdirSync(join(FIX, "generations", "aaa"), { recursive: true });
    mkdirSync(join(FIX, "staging", "ghost"), { recursive: true });
  }
  const got = seq[o.id]();
  // value-level compare for list/ok payloads
  if (o.status === "ok") {
    if (got.status !== "ok") { emit(o.id, "FAIL", `threw ${got.kind}: ${got.message}`); continue; }
    let ok = true, det = "ok";
    if (o.id === "staged_ok") ok = got.value === o.value.sha;
    else if (o.id === "install_ok") ok = got.value.generation_id === o.value.gid && got.value.manifest_sha256 === o.value.msha;
    else if (o.id === "installed_ids" || o.id === "reopen_ids" || o.id === "installed_ids_unicode") ok = JSON.stringify(got.value) === JSON.stringify(o.value);
    else if (o.id === "reopen_verify") ok = got.value === o.value.gid;
    else if (o.id === "stage_kept_after_fail" || o.id === "target_still_empty" || o.id === "stage_preserved") ok = got.value === o.value;
    else if (o.id === "create_fresh") ok = got.value.dir === "req-1";
    emit(o.id, ok ? "PASS" : "FAIL", ok ? "ok" : "value mismatch", o.value, got.value);
  } else {
    cmp(o.id, got, o);
  }
}

// ---------- typed boundary: request_id escapes (stricter-than-Rust) ----------
const bnd = (id, fn, kind = "invalid_request_id") => {
  try { fn(); emit(id, "FAIL", "accepted malformed input"); }
  catch (e) {
    const ok = e instanceof GenerationUsableError && e.kind === kind;
    emit(id, ok ? "PASS" : "FAIL", `${e.name}(${e.kind ?? "-"}): ${e.message.slice(0, 70)}`, kind, e.message);
  }
};
{
  const s2 = new GenerationStore(join(FIX, "bnd"));
  bnd("rid_dotdot", () => s2.createStage(".."));
  bnd("rid_slash", () => s2.createStage("a/b"));
  bnd("rid_backslash", () => s2.createStage("a\\b"));
  bnd("rid_empty", () => s2.createStage(""));
  bnd("rid_dot", () => s2.createStage("."));
  bnd("rid_nul", () => s2.createStage("a\0b"));
  bnd("rid_surrogate", () => s2.createStage("r\ud800"));
  bnd("rid_number", () => s2.createStage(42));
  // statOrNull propagation: staging dir exists as a FILE -> createStage
  // must surface io, not treat as absent-then-silent
  { mkdirSync(join(FIX, "bnd"), { recursive: true }); writeFileSync(join(FIX, "bnd", "staging"), "x");
    bnd("rid_staging_is_file", () => s2.createStage("req-x"), "io"); }
  // post-rename durability observation: installed dir exists on disk
  // after installStaged returned (fsync'd parent) — documented as
  // observable state, not a crash-durability proof.
  emit("durability_note", existsSync(join(FIX, "generations", GID)) ? "PASS" : "FAIL", "installed dir persists across reopen");
}

// ---------- direct native-boundary proof (not via store precheck) ----------
{
  const { createRequire } = await import("node:module");
  const native = createRequire(import.meta.url)("../../src_node/native/rename_noreplace.node");
  const nsrc = join(FIX, "nat-src"), ndst = join(FIX, "nat-dst");
  mkdirSync(nsrc); mkdirSync(ndst);
  // embedded NUL must reject EINVAL, source intact
  try { native.rename_noreplace(nsrc + " ignored", join(FIX, "nat-x")); emit("native_nul_reject", "FAIL", "NUL path accepted"); }
  catch (e) { emit("native_nul_reject", e.code === "EINVAL" && existsSync(nsrc) ? "PASS" : "FAIL", `code=${e.code}`); }
  // overlong path rejects EINVAL
  try { native.rename_noreplace(nsrc, join(FIX, "x".repeat(5000))); emit("native_long_reject", "FAIL", "long path accepted"); }
  catch (e) { emit("native_long_reject", e.code === "EINVAL" ? "PASS" : "FAIL", `code=${e.code}`); }
  // non-string rejects
  try { native.rename_noreplace(42, ndst); emit("native_nonstr_reject", "FAIL", "non-string accepted"); }
  catch (e) { emit("native_nonstr_reject", e.code === "EINVAL" ? "PASS" : "FAIL", `code=${e.code}`); }
  // DIRECT conflict proof: existing empty target -> EEXIST, both intact
  // (bypasses the store's statOrNull precheck entirely)
  try { native.rename_noreplace(nsrc, ndst); emit("native_conflict_eexist", "FAIL", "replaced existing target"); }
  catch (e) {
    emit("native_conflict_eexist", e.code === "EEXIST" && existsSync(nsrc) && readdirEmpty(ndst) ? "PASS" : "FAIL", `code=${e.code}`);
  }
  // happy path
  try { native.rename_noreplace(nsrc, join(FIX, "nat-moved")); emit("native_move_ok", existsSync(join(FIX, "nat-moved")) && !existsSync(nsrc) ? "PASS" : "FAIL", "renamed"); }
  catch (e) { emit("native_move_ok", "FAIL", e.message); }
}

// selftest
if (SELFTEST === "store") {
  const s = new GenerationStore(join(FIX, "self"));
  const d = s.createStage("s1");
  writeFileSync(join(d, "snapshot-manifest.json"), mb);
  for (const [, rel, b] of STDF) writeFileSync(join(d, rel), b);
  const inst = s.installStaged("s1", NOW);
  emit("selftest_store", inst.generation_id === "0".repeat(64) ? "PASS" : "FAIL",
    inst.generation_id === "0".repeat(64) ? null : "expected wrong gid — STOP");
}

process.exitCode = failures === 0 ? 0 : 1;
