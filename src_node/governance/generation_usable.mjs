// installed-generation usability gate — Node port of
// generation/mod.rs::{verify_installed, decision_artifact, usable}
// (read-only). A generation is usable only when directory identity ==
// sha256(manifest bytes), snapshot.inspect verifies all artifacts, and
// a once-written decision binds this generation+manifest with
// VERIFIED_COMPATIBLE. Never creates/repairs/activates/pins/retires.
//
// Error parity: generation::Error Display categories are preserved as
// GenerationUsableError.kind — "not_installed" | "corrupt" | "json" |
// "not_consistency_verified" — with Rust Display text. serde_json parse
// text differs from serde's (stable portions identical); documented.

import { createHash } from "node:crypto";
import { statSync } from "node:fs";
import { readBoundedFile } from "../lib/zip_member.mjs";
import { join } from "node:path";
import { inspect, strictJson, InspectionError } from "./snapshot.mjs";
import { parseTs } from "./ts.mjs";

export const MANIFEST_NAME = "snapshot-manifest.json";
export const DECISION_NAME = "consistency-decision.json";
export const DECISION_SCHEMA = "x-data-mcp.consistency-decision.v1";

export class GenerationUsableError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = "GenerationUsableError";
    this.kind = kind;
  }
}
const notInstalled = (g) => new GenerationUsableError(
  "not_installed", `generation ${g} is not installed`);
const corrupt = (g, inner) => new GenerationUsableError(
  "corrupt", `generation ${g} directory exists but fails inspection: ${inner}`);
const manifestJson = (m) => new GenerationUsableError(
  "json", `manifest JSON invalid: ${m}`);
const notVerified = (g) => new GenerationUsableError(
  "not_consistency_verified",
  `generation ${g} carries no verified compatibility decision`);

// ---------- bounded fd reads ----------
// readBoundedFile (shared lib): bound = fstat size at open; reads at
// most that many bytes; 0-read EOF is an error; post-fstat growth is
// ignored (prefix only — digest/parse checks stay fail-closed).
// Stricter than Rust's fs::read for the manifest (>1MiB refuses before
// read rather than inside inspect; mid-read growth rejects rather than
// prefix-returning) — same Corrupt category, same outcome; documented.
// Not an atomic snapshot; see lib/zip_member.mjs.
const MAX_MANIFEST = 1 * 1024 * 1024;   // same bound inspect enforces
const MAX_DECISION = 1 * 1024 * 1024;   // decision artifact bound

// ---------- decision artifact typed boundary ----------
const ROLES = new Set(["base_industry", "day", "gbbq"]);
const STATUSES = new Set(["VERIFIED_COMPATIBLE", "NOT_VERIFIED", "CONFLICT"]);
const REASON_CODES = new Set([
  "missing_role", "conflicting_duplicate", "missing_compatibility_evidence",
  "file_integrity_not_verified", "invalid_file_hash", "invalid_file_interval",
  "incomplete_compatibility_evidence", "invalid_compatibility_window",
  "hash_mismatch", "source_revision_mismatch", "source_identity_mismatch",
  "missing_bound_source_revision", "missing_bound_source_identity",
  "acquisition_outside_compatibility_window",
]);
const isStr = (v) => typeof v === "string" && v.isWellFormed();
const isJsonValue = (v) => { // permissive JSON shape for probes Value
  if (v === null || typeof v === "boolean") return true;
  if (typeof v === "number") return Number.isFinite(v);
  if (typeof v === "string") return v.isWellFormed();
  if (Array.isArray(v)) return v.every(isJsonValue);
  if (v !== null && typeof v === "object" &&
      (Object.getPrototypeOf(v) === Object.prototype || Object.getPrototypeOf(v) === null)) {
    return Object.keys(v).every((k) => k.isWellFormed() && isJsonValue(v[k]));
  }
  return false;
};
// serde tagged ConsistencyReason: {"code": <snake>, "role": <role>}
const isReason = (r) => r !== null && typeof r === "object" && !Array.isArray(r) &&
  isStr(r.code) && REASON_CODES.has(r.code) &&
  (r.code === "missing_compatibility_evidence" || r.code === "incomplete_compatibility_evidence" ||
   r.code === "invalid_compatibility_window"
    ? Object.keys(r).length === 1
    : Object.keys(r).length === 2 && isStr(r.role) && ROLES.has(r.role));

const ART_KEYS = ["schema", "generation_id", "manifest_sha256", "issuer",
  "validator_contract_version", "evaluated_at", "status", "reasons",
  "evidence_sha256", "probes"].sort().join(",");
// Parse+validate the typed DecisionArtifact. Returns artifact or throws
// with the serde-style error text (caller wraps in Corrupt).
function parseDecision(text) {
  const d = strictJson(text); // throws InspectionError("json", "manifest JSON invalid: ...")
  const bad = (m) => { throw new Error(m); };
  if (d === null || typeof d !== "object" || Array.isArray(d)) bad("invalid type: non-map, expected struct DecisionArtifact");
  if (Object.keys(d).sort().join(",") !== ART_KEYS) bad("missing or unknown fields in DecisionArtifact");
  for (const k of ["schema", "generation_id", "manifest_sha256", "issuer", "validator_contract_version"]) {
    if (!isStr(d[k])) bad(`invalid type: expected string for ${k}`);
  }
  if (!isStr(d.evaluated_at) || parseTs(d.evaluated_at) === null) bad("evaluated_at invalid timestamp");
  if (!isStr(d.status) || !STATUSES.has(d.status)) bad(`unknown variant for status`);
  if (!Array.isArray(d.reasons) || !d.reasons.every(isReason)) bad("invalid reasons");
  if (d.evidence_sha256 !== null && !isStr(d.evidence_sha256)) bad("evidence_sha256 must be string|null");
  if (!isJsonValue(d.probes)) bad("probes must be a JSON value");
  return d;
}

// serde_json::from_slice::<SnapshotManifest> stage: struct field/type
// validation produces Error::Json; semantic checks happen later inside
// inspect (Corrupt). Mirror that split so error categories match.
const FILE_FIELDS = new Set(["role", "relative_path", "source_url",
  "acquisition_started_at", "acquisition_completed_at", "last_modified",
  "etag", "bytes", "sha256"]);
const FILE_ROLES = new Set(["base_industry", "day", "gbbq"]);
const jbad = (m) => { throw new Error(m); };
function serdeManifestShape(m) {
  if (m === null || typeof m !== "object" || Array.isArray(m)) {
    jbad("invalid type: expected struct SnapshotManifest");
  }
  for (const k of Object.keys(m)) {
    if (k !== "schema" && k !== "files") jbad(`unknown field \`${k}\``);
  }
  if (!isStr(m.schema)) jbad("missing/invalid field `schema`");
  if (!Array.isArray(m.files)) jbad("missing/invalid field `files`");
  for (const f of m.files) {
    if (f === null || typeof f !== "object" || Array.isArray(f)) {
      jbad("invalid type: expected struct FileInput");
    }
    for (const k of Object.keys(f)) {
      if (!FILE_FIELDS.has(k)) jbad(`unknown field \`${k}\``);
    }
    for (const k of ["role", "relative_path", "source_url",
      "acquisition_started_at", "acquisition_completed_at", "bytes", "sha256"]) {
      if (!(k in f)) jbad(`missing field \`${k}\``);
    }
    if (!isStr(f.role) || !FILE_ROLES.has(f.role)) jbad(`unknown variant for role`);
    for (const k of ["relative_path", "source_url", "sha256"]) {
      if (!isStr(f[k])) jbad(`invalid type for ${k}`);
    }
    for (const k of ["acquisition_started_at", "acquisition_completed_at"]) {
      if (!isStr(f[k]) || parseTs(f[k]) === null) jbad(`invalid timestamp ${k}`);
    }
    for (const k of ["last_modified", "etag"]) { // Option<String>: absent|null|string
      if (k in f && f[k] !== null && !isStr(f[k])) jbad(`invalid type for ${k}`);
    }
    if (!Number.isSafeInteger(f.bytes) || f.bytes < 0) jbad("invalid type for bytes");
  }
}

// ---------- Store::verify_installed ----------
export function verifyInstalled(root, generationId, nowMs) {
  const dir = join(root, "generations", generationId);
  if (!statDir(dir)) throw notInstalled(generationId);
  let manifestBytes;
  try { manifestBytes = readBoundedFile(join(dir, MANIFEST_NAME), MAX_MANIFEST); }
  catch (e) { throw corrupt(generationId, e.message); }
  const sha = createHash("sha256").update(manifestBytes).digest("hex");
  if (sha !== generationId) {
    throw corrupt(generationId, "manifest digest does not match directory identity");
  }
  let manifest;
  try {
    manifest = strictJson(manifestBytes.toString("utf8"));
    serdeManifestShape(manifest); // serde struct stage -> Json errors
  } catch (e) {
    throw manifestJson(e instanceof InspectionError
      ? e.message.replace(/^manifest JSON invalid: /, "")
      : e.message);
  }
  let inspection;
  try {
    inspection = inspect(manifestBytes, dir, nowMs);
  } catch (e) {
    throw corrupt(generationId, e.message);
  }
  return {
    generation_id: generationId,
    manifest_sha256: inspection.manifest_sha256,
    dir,
    manifest,
    manifest_bytes: manifestBytes,
  };
}
const statDir = (dir) => {
  try { return statSync(dir).isDirectory(); } catch { return false; }
};

// ---------- Store::decision_artifact ----------
export function decisionArtifact(root, generationId) {
  const path = join(root, "generations", generationId, DECISION_NAME);
  let bytes;
  try { bytes = readBoundedFile(path, MAX_DECISION); }
  catch (e) {
    if (e.code === "ENOENT") return null;
    // Oversized or mid-read-grown decision: Rust reads unbounded then
    // serde fails -> Corrupt either way; keep the category.
    if (/exceeds declared bound|grew during read/.test(e.message)) {
      throw corrupt(generationId, `decision artifact rejected by bounded read: ${e.message}`);
    }
    throw new GenerationUsableError("io", `io: ${e.message}`);
  }
  let artifact;
  try { artifact = parseDecision(bytes.toString("utf8")); }
  catch (e) {
    throw corrupt(generationId, e instanceof InspectionError
      ? e.message.replace(/^manifest JSON invalid: /, "")
      : e.message);
  }
  if (artifact.schema !== DECISION_SCHEMA || artifact.generation_id !== generationId) {
    throw corrupt(generationId, "decision does not bind this generation");
  }
  return artifact;
}

// ---------- Store::usable ----------
export function usable(root, generationId, nowMs) {
  const installed = verifyInstalled(root, generationId, nowMs);
  const decision = decisionArtifact(root, generationId);
  if (decision === null) throw notVerified(generationId);
  if (decision.manifest_sha256 !== installed.manifest_sha256 ||
      decision.status !== "VERIFIED_COMPATIBLE") {
    throw notVerified(generationId);
  }
  return installed;
}
