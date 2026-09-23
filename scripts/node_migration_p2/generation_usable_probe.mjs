// NM-032 P2: validate governance/generation_usable.mjs against a live
// Rust oracle (Store::usable). Synthetic generation roots + real gen-2
// retained bytes (negative no-decision). Exit nonzero on FAIL.
// Usage: node generation_usable_probe.mjs <root> [selftest]
import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { usable, verifyInstalled, decisionArtifact, GenerationUsableError } from "../../src_node/governance/generation_usable.mjs";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const NOW = "2026-09-16T00:00:00Z";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/generation_usable/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "generation_usable", id, status, detail, expected, actual }));
};

const sha = (b) => createHash("sha256").update(b).digest("hex");
const FIX = mkdtempSync("/tmp/nm32-usable-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const rootOf = (name) => { const d = join(FIX, name); mkdirSync(d, { recursive: true }); return d; };

const STDF = [
  ["base_industry", "base.zip", Buffer.from("base-bytes")],
  ["day", "hsjday.zip", Buffer.from("day-bytes")],
  ["gbbq", "gbbq.zip", Buffer.from("gbbq-bytes")],
];
const url = (role, rel) => role === "day"
  ? "https://data.tdx.com.cn/vipdoc/hsjday.zip"
  : `https://www.tdx.com.cn/products/data/data/dbf/${rel}`;
// serde_json (BTreeMap) emits keys sorted — canonicalize so fixture
// manifest bytes are identical to the oracle's.
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
const decisionOf = (gid, msha, status, extra = {}) => canon({
  schema: "x-data-mcp.consistency-decision.v1", generation_id: gid,
  manifest_sha256: msha, issuer: "test",
  validator_contract_version: "official-file-consistency-v1",
  evaluated_at: "2026-09-01T00:00:02Z", status, reasons: [],
  evidence_sha256: null, probes: {}, ...extra });
const mkgen = (root, files, decision) => {
  const mb = manifestOf(files);
  const gid = sha(mb);
  const dir = join(root, "generations", gid);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "snapshot-manifest.json"), mb);
  for (const [, rel, bytes] of files) writeFileSync(join(dir, rel), bytes);
  if (decision !== null) writeFileSync(join(dir, "consistency-decision.json"), decision);
  return gid;
};

const FIXT = {
  missing_dir: () => [rootOf("a"), "0".repeat(64)],
  no_manifest: () => { const r = rootOf("b"); mkdirSync(join(r, "generations", "1".repeat(64)), { recursive: true }); return [r, "1".repeat(64)]; },
  digest_mismatch: () => { const r = rootOf("c"); const dir = join(r, "generations", "2".repeat(64)); mkdirSync(dir, { recursive: true }); writeFileSync(join(dir, "snapshot-manifest.json"), manifestOf(STDF)); return [r, "2".repeat(64)]; },
  manifest_bad_json: () => { const r = rootOf("d"); const mb = Buffer.from("{not json"); const gid = sha(mb); const dir = join(r, "generations", gid); mkdirSync(dir, { recursive: true }); writeFileSync(join(dir, "snapshot-manifest.json"), mb); return [r, gid]; },
  manifest_wrong_shape: () => { const r = rootOf("e"); const mb = Buffer.from(JSON.stringify({ schema: "x", files: [] })); const gid = sha(mb); const dir = join(r, "generations", gid); mkdirSync(dir, { recursive: true }); writeFileSync(join(dir, "snapshot-manifest.json"), mb); return [r, gid]; },
  artifact_corrupt: () => { const r = rootOf("f"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "base.zip"), Buffer.from("tampered")); return [r, gid]; },
  no_decision: () => { const r = rootOf("g"); return [r, mkgen(r, STDF, null)]; },
  decision_bad_json: () => { const r = rootOf("h"); const gid = mkgen(r, STDF, "{bad"); return [r, gid]; },
  decision_wrong_gid: () => { const r = rootOf("j"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf("other-gen", gid, "VERIFIED_COMPATIBLE")); return [r, gid]; },
  decision_manifest_mismatch: () => { const r = rootOf("k"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, "0".repeat(64), "VERIFIED_COMPATIBLE")); return [r, gid]; },
  status_NOT_VERIFIED: () => { const r = rootOf("l"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, gid, "NOT_VERIFIED")); return [r, gid]; },
  status_CONFLICT: () => { const r = rootOf("m"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, gid, "CONFLICT")); return [r, gid]; },
  verified_ok: () => { const r = rootOf("n"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, gid, "VERIFIED_COMPATIBLE")); return [r, gid]; },
  verified_with_reasons: () => { const r = rootOf("o"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, gid, "VERIFIED_COMPATIBLE", { reasons: [{ code: "missing_role", role: "day" }], evidence_sha256: "e".repeat(64) })); return [r, gid]; },
  decision_bad_status: () => { const r = rootOf("p"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, gid, "MAYBE")); return [r, gid]; },
  decision_bad_reason: () => { const r = rootOf("q"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, gid, "NOT_VERIFIED", { reasons: [{ code: "bogus", role: "day" }] })); return [r, gid]; },
  decision_missing_field: () => { const r = rootOf("s"); const gid = mkgen(r, STDF, null); const d = JSON.parse(decisionOf(gid, gid, "VERIFIED_COMPATIBLE")); delete d.issuer; writeFileSync(join(r, "generations", gid, "consistency-decision.json"), JSON.stringify(d)); return [r, gid]; },
  decision_unknown_field: () => { const r = rootOf("t"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, gid, "VERIFIED_COMPATIBLE", { extra: 1 })); return [r, gid]; },
  decision_bad_ts: () => { const r = rootOf("u"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, gid, "VERIFIED_COMPATIBLE", { evaluated_at: "junk" })); return [r, gid]; },
  real_gen2_no_decision: () => {
    const r = rootOf("gen2");
    const mb = readFileSync(join(ROOT, "evidence/issue-3-acceptance-run/snapshot-manifest.json"));
    const gid = sha(mb);
    const dir = join(r, "generations", gid); mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "snapshot-manifest.json"), mb);
    for (const [src, dst] of [["official-base.zip", "base.zip"], ["official-gbbq.zip", "gbbq.zip"], ["official-hsjday.zip", "hsjday.zip"]]) {
      copyFileSync(join("/home/box/retained-inputs", src), join(dir, dst));
    }
    return [r, gid];
  },
};
// wrong-schema fixture (gid known after mkgen)
FIXT.decision_wrong_schema = () => { const r = rootOf("i"); const gid = mkgen(r, STDF, null); writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, gid, "VERIFIED_COMPATIBLE", { schema: "wrong-schema" })); return [r, gid]; };

const STABLE = (kind) => ({
  not_installed: "is not installed",
  corrupt: "directory exists but fails inspection:",
  json: "manifest JSON invalid:",
  not_consistency_verified: "carries no verified compatibility decision",
}[kind]);
// verbatim-comparable inner messages (deterministic, no io/serde text)
const VERBATIM_INNER = new Set(["manifest digest does not match directory identity",
  "decision does not bind this generation"]);

for (const o of oracle) {
  const [root, gid] = FIXT[o.id]();
  try {
    const inst = usable(root, gid, NOW);
    if (o.status === "ok") {
      const ok = inst.generation_id === o.installed.generation_id && inst.manifest_sha256 === o.installed.manifest_sha256;
      emit(o.id, ok ? "PASS" : "FAIL", ok ? `usable gid=${inst.generation_id.slice(0, 12)}` : "installed mismatch", o.installed, inst);
    } else {
      emit(o.id, "FAIL", "accepted", o.kind, inst);
    }
  } catch (e) {
    if (o.status !== "error") { emit(o.id, "FAIL", `threw ${e.name}: ${e.message}`, "ok", e.message); continue; }
    if (!(e instanceof GenerationUsableError) || e.kind !== o.kind) {
      emit(o.id, "FAIL", `kind mismatch: ${e.name} ${e.kind ?? ""}`, o.kind, e.message); continue;
    }
    // full verbatim when deterministic, else kind + stable prefix
    const stable = STABLE(o.kind);
    const inner = e.message.split(": ").pop();
    const ok = o.kind === "corrupt" && VERBATIM_INNER.has(inner)
      ? e.message === o.message
      : (o.kind !== "corrupt" && o.kind !== "json")
        ? e.message === o.message
        : e.message.includes(stable);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? `${o.kind}` : `message drift`, o.message, e.message);
  }
}

// ---------- typed boundary (Node-only) ----------
const bnd = (id, fn) => {
  try { fn(); emit(id, "FAIL", "accepted malformed input"); }
  catch (e) { emit(id, e instanceof GenerationUsableError || e instanceof Error ? "PASS" : "FAIL", `${e.name}: ${e.message.slice(0, 80)}`); }
};
// decision artifact typed-boundary rejects (serde-unrepresentable)
{
  const r = rootOf("bnd");
  const gid = mkgen(r, STDF, null);
  const dpath = join(r, "generations", gid, "consistency-decision.json");
  const bad = (id, mut) => {
    const d = JSON.parse(decisionOf(gid, gid, "VERIFIED_COMPATIBLE"));
    mut(d);
    writeFileSync(dpath, JSON.stringify(d));
    bnd(id, () => usable(r, gid, NOW));
  };
  bad("rej_status_num", (d) => { d.status = 5; });
  bad("rej_reason_bad_role", (d) => { d.reasons = [{ code: "missing_role", role: "nope" }]; });
  bad("rej_reason_role_on_meta", (d) => { d.reasons = [{ code: "missing_compatibility_evidence", role: "day" }]; });
  bad("rej_evidence_num", (d) => { d.evidence_sha256 = 42; });
  // unpaired-surrogate escape inside a string field — serde rejects
  { writeFileSync(dpath, Buffer.from(
      '{"schema":"x-data-mcp.consistency-decision.v1","generation_id":"' + gid +
      '","manifest_sha256":"' + gid + '","issuer":"\\ud800","validator_contract_version":"official-file-consistency-v1","evaluated_at":"2026-09-01T00:00:02Z","status":"VERIFIED_COMPATIBLE","reasons":[],"evidence_sha256":null,"probes":{}}'));
    bnd("rej_surrogate_issuer", () => usable(r, gid, NOW)); }
}
// selftest
if (SELFTEST === "usable") {
  const r = rootOf("self");
  const gid = mkgen(r, STDF, null);
  writeFileSync(join(r, "generations", gid, "consistency-decision.json"), decisionOf(gid, gid, "VERIFIED_COMPATIBLE"));
  const inst = usable(r, gid, NOW);
  emit("selftest_usable", inst.generation_id === "0".repeat(64) ? "PASS" : "FAIL",
    inst.generation_id === "0".repeat(64) ? null : "expected wrong gid — STOP");
}

process.exitCode = failures === 0 ? 0 : 1;
