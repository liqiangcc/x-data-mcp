// NM-027 P2: validate src_node/governance/file_consistency.mjs against a
// live Rust oracle — synthetic decision matrix + committed #131/gen-2 inputs.
// Usage: node scripts/node_migration_p2/file_consistency_probe.mjs <root> [selftest]
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { validate, ConsistencyInputError } from "../../src_node/governance/file_consistency.mjs";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/file_consistency/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "file_consistency", id, status, detail, expected, actual }));
};

const H = (b) => String.fromCharCode(b).repeat(64);
const instant = (h) => `2026-09-10T${String(h).padStart(2, "0")}:00:00Z`;
const file = (role, b) => ({
  role, bytes: 10, sha256: H(b),
  source_url: "https://tdx.example.invalid/file",
  acquisition_started_at: instant(1), acquisition_completed_at: instant(2),
  last_modified: "revision-1", etag: "etag-1",
  byte_integrity: "verified",
  source_revision: "r1", source_identity: "tdx-public-set",
});
const evidence = (files) => ({
  evidence_ref: "fixture:compatible-set", rule_version: "official-file-consistency-v1",
  compatibility_set_revision: "r1", compatibility_set_identity: "tdx-public-set",
  member_bindings: Object.fromEntries(files.map((f) => [f.role, {
    sha256: f.sha256, source_revision: f.source_revision, source_identity: f.source_identity }])),
  valid_window: { valid_from: instant(0), valid_until: instant(3) },
});
const s3 = () => [file("base_industry", 98), file("day", 100), file("gbbq", 99)];
const ev = () => evidence(s3());

const CASES = {
  compatible_exact: () => [s3(), ev()],
  compatible_permuted: () => { const s = s3(); return [[s[2], s[0], s[1]], ev()]; },
  no_proof: () => [s3(), null],
  missing_role: () => { const s = s3(); return [[s[0], s[1]], null]; },
  dup_equal: () => { const s = s3(); return [[s[0], s[1], s[1], s[2]], ev()]; },
  dup_conflict: () => { const s = s3(); const dv = file("day", 100); dv.etag = "other";
    return [[s[0], s[1], dv, s[2]], ev()]; },
  unverified_integrity: () => { const s = s3(); s[1].byte_integrity = "unverified"; return [s, ev()]; },
  invalid_hash: () => { const s = s3(); s[1].sha256 = "BAD"; return [s, ev()]; },
  invalid_interval: () => { const s = s3(); s[1].acquisition_started_at = instant(3); s[1].acquisition_completed_at = instant(2); return [s, ev()]; },
  inc_empty_ref: () => { const p = ev(); p.evidence_ref = "  "; return [s3(), p]; },
  inc_wrong_version: () => { const p = ev(); p.rule_version = "v2"; return [s3(), p]; },
  inc_empty_rev: () => { const p = ev(); p.compatibility_set_revision = " "; return [s3(), p]; },
  inc_empty_ident: () => { const p = ev(); p.compatibility_set_identity = ""; return [s3(), p]; },
  inc_binding_count: () => { const p = ev(); delete p.member_bindings.day; return [s3(), p]; },
  inc_bad_binding_hash: () => { const p = ev(); p.member_bindings.gbbq.sha256 = "zz"; return [s3(), p]; },
  invalid_window: () => { const p = ev(); p.valid_window.valid_from = instant(4); return [s3(), p]; },
  hash_mismatch: () => { const p = ev(); p.member_bindings.day.sha256 = H(101); return [s3(), p]; },
  revision_mismatch: () => { const p = ev(); p.member_bindings.day.source_revision = "day-r8"; return [s3(), p]; },
  identity_mismatch: () => { const p = ev(); p.member_bindings.gbbq.source_identity = "other"; return [s3(), p]; },
  missing_bound_rev: () => { const s = s3(); s[1].source_revision = null; return [s, ev()]; },
  missing_bound_id: () => { const s = s3(); s[2].source_identity = null; return [s, ev()]; },
  per_role_rev_id: () => {
    const b = file("base_industry", 98); b.source_revision = "base-r1"; b.source_identity = "base-org";
    const d = file("day", 100); d.source_revision = "day-r7"; d.source_identity = "day-org";
    const g = file("gbbq", 99); g.source_revision = "gbbq-r3"; g.source_identity = "gbbq-org";
    return [[b, d, g], evidence([b, d, g])]; },
  outside_window_early: () => { const s = s3(); s[0].acquisition_started_at = "2026-09-09T23:00:00Z"; return [s, ev()]; },
  outside_window_late: () => { const s = s3(); s[2].acquisition_completed_at = instant(4); return [s, ev()]; },
  multi_reason_order: () => { const s = s3(); s[1].byte_integrity = "unverified"; s[1].sha256 = "zz"; return [s, null]; },
  conflict_plus_incomplete: () => { const s = s3(); const dv = file("day", 100); dv.etag = "other";
    const p = ev(); p.compatibility_set_identity = "";
    return [[s[0], s[1], dv, s[2]], p]; },
  dup_utf8_order: () => { const s = s3(); const d1 = file("day", 100); d1.etag = "\ue000";
    const d2 = file("day", 100); d2.etag = "\ud83d\ude00";
    return [[s[0], d2, d1, s[2]], ev()]; },
  real_131: () => {
    const inp = JSON.parse(readFileSync(join(ROOT, "evidence/issue-131-file-set-consistency/input.json"), "utf8"));
    return [inp.files, inp.compatibility];
  },
  gen2_facts_no_proof: () => {
    const m = JSON.parse(readFileSync(join(ROOT, "evidence/issue-3-acceptance-run/snapshot-manifest.json"), "utf8"));
    return [m.files.map((f) => ({
      role: f.role, bytes: f.bytes, sha256: f.sha256, source_url: f.source_url,
      acquisition_started_at: f.acquisition_started_at,
      acquisition_completed_at: f.acquisition_completed_at,
      last_modified: f.last_modified, etag: f.etag,
      byte_integrity: "verified", source_revision: null, source_identity: null })), null];
  },
};

// canonical compare: recursive key sort (serde struct field order differs
// from input.json order — content equality is what the oracle verifies)
const canon = (v) => v === null || typeof v !== "object" ? JSON.stringify(v)
  : Array.isArray(v) ? "[" + v.map(canon).join(",") + "]"
  : "{" + Object.keys(v).sort().map((k) => JSON.stringify(k) + ":" + canon(v[k])).join(",") + "}";

for (const o of oracle) {
  const [files, compat] = CASES[o.id]();
  const want = SELFTEST === "decision" && o.id === "compatible_exact"
    ? { ...o.decision, status: "CONFLICT" } : o.decision;
  try {
    const got = validate({ files, compatibility: compat });
    const ok = canon(got) === canon(want);
    emit(o.id, ok ? "PASS" : "FAIL",
      ok ? `${got.status}${got.reasons.length ? "/" + got.reasons.length : ""}` : "decision mismatch",
      o.decision, got);
  } catch (e) {
    emit(o.id, "FAIL", `threw ${e.name}: ${e.message}`, o.decision.status, e.message);
  }
}

// ---------- typed boundary (fail closed) ----------
const bnd = (id, fn) => {
  try { fn(); emit(id, "FAIL", "accepted"); }
  catch (e) { emit(id, e instanceof ConsistencyInputError ? "PASS" : "FAIL", `${e.name}: ${e.message}`); }
};
const s = s3();
bnd("reject_files_not_array", () => validate({ files: "x", compatibility: null }));
bnd("reject_bad_role", () => validate({ files: [{ ...s[0], role: "x" }], compatibility: null }));
bnd("reject_bad_integrity", () => validate({ files: [{ ...s[0], byte_integrity: "ok" }], compatibility: null }));
bnd("reject_bad_ts", () => validate({ files: [{ ...s[0], acquisition_started_at: "junk" }], compatibility: null }));
bnd("reject_ts_no_offset", () => validate({ files: [{ ...s[0], acquisition_started_at: "2026-09-10T01:00:00" }], compatibility: null }));
bnd("reject_bad_bytes", () => validate({ files: [{ ...s[0], bytes: -1 }], compatibility: null }));
bnd("reject_evidence_bad_window", () => validate({ files: [], compatibility: { ...ev(), valid_window: { valid_from: "junk", valid_until: instant(3) } } }));
bnd("reject_binding_bad_role", () => validate({ files: [], compatibility: { ...ev(), member_bindings: { base_industry: ev().member_bindings.base_industry, day: ev().member_bindings.day, gbbq: ev().member_bindings.gbbq, extra: { sha256: H(1) } } } }));
bnd("reject_role_constructor", () => validate({ files: [{ ...s[0], role: "constructor" }], compatibility: null }));
bnd("reject_role_toString", () => validate({ files: [{ ...s[0], role: "toString" }], compatibility: null }));
bnd("reject_integrity_hasOwnProperty", () => validate({ files: [{ ...s[0], byte_integrity: "hasOwnProperty" }], compatibility: null }));
bnd("reject_binding_constructor", () => validate({ files: [], compatibility: { ...ev(), member_bindings: { base_industry: ev().member_bindings.base_industry, day: ev().member_bindings.day, gbbq: ev().member_bindings.gbbq, constructor: { sha256: H(1) } } } }));

process.exit(failures === 0 ? 0 : 1);
