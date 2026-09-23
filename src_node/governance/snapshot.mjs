// Read-only snapshot manifest byte inspection — Node port of
// src/snapshot.rs::inspect.
//
// Verifies a bounded manifest plus three local archive bytes and returns
// the limited Inspection facts. It does NOT confer origin authenticity,
// archive-member binding, file-set consistency, business semantics,
// access/use approval, or production readiness.
//
// Error precedence (verbatim static texts preserved):
//   Invalid(&'static str) -> "invalid manifest: {str}"
//   Mismatch(FileRole)    -> "artifact bytes do not match manifest: {Role:?}"
//   Io                    -> "local input unavailable: {os err}"  (Node
//                            message differs from Rust io::Error text —
//                            category preserved, text delta documented)
//   Json                  -> "manifest JSON invalid: {msg}"       (serde
//                            position text differs — same category)
//
// Streaming artifact hashing (64 KiB chunks) — the 549 MB hsjday.zip is
// never loaded into memory. Input root is trusted/private as in Rust.

import { createHash } from "node:crypto";
import { openSync, readSync, realpathSync, statSync, closeSync } from "node:fs";

export class InspectionError extends Error {
  constructor(kind, message, extra = {}) {
    super(message);
    this.name = "InspectionError";
    this.kind = kind; // "invalid" | "mismatch" | "io" | "json"
    Object.assign(this, extra);
  }
}
const invalid = (s) => new InspectionError("invalid", `invalid manifest: ${s}`);
const ioErr = (e) => new InspectionError("io", `local input unavailable: ${e.message}`);
const jsonErr = (m) => new InspectionError("json", `manifest JSON invalid: ${m}`);

const ROLE_ORDER = { base_industry: 0, day: 1, gbbq: 2 };
const ROLE_DEBUG = { base_industry: "BaseIndustry", day: "Day", gbbq: "Gbbq" };
const ROLE_FILE = { base_industry: "base.zip", day: "hsjday.zip", gbbq: "gbbq.zip" };
const ROLE_URL_PATH = {
  base_industry: "products/data/data/dbf/base.zip",
  gbbq: "products/data/data/dbf/gbbq.zip",
  day: "vipdoc/hsjday.zip",
};
const FILE_FIELDS = new Set([
  "role", "relative_path", "source_url", "acquisition_started_at",
  "acquisition_completed_at", "last_modified", "etag", "bytes", "sha256",
]);

const isRfc3339 = (s) => typeof s === "string" && !Number.isNaN(Date.parse(s)) &&
  /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/.test(s);
// chrono serde emits RFC3339 UTC with `Z` and fractional digits only when
// nonzero; Date has ms precision (sub-ms input truncates — documented delta).
const canonTs = (s) => {
  const iso = new Date(s).toISOString();
  return iso.endsWith(".000Z") ? iso.slice(0, -5) + "Z" : iso;
};

function declaredOfficialUrl(role, url) {
  if (typeof url !== "string") return false;
  const rest = url.startsWith("https://") ? url.slice(8)
    : url.startsWith("http://") ? url.slice(7) : null;
  if (rest === null) return false;
  const slash = rest.indexOf("/");
  if (slash < 0) return false;
  const host = rest.slice(0, slash), path = rest.slice(slash + 1);
  if (path === "") return false;
  if (/[\t\n\v\f\r ]/.test(url)) return false;
  if (/[@:?#]/.test(host)) return false;
  const hostOk = role === "day" ? host === "data.tdx.com.cn"
    : host === "tdx.com.cn" || host === "www.tdx.com.cn";
  return hostOk && path === ROLE_URL_PATH[role];
}

function validateShape(m) {
  // serde deny_unknown_fields + field types -> "json" category (text delta
  // documented: serde emits expected-list + line/column, Node emits a
  // descriptive message without positions).
  if (m === null || typeof m !== "object" || Array.isArray(m)) {
    throw jsonErr("manifest must be a JSON object");
  }
  for (const k of Object.keys(m)) {
    if (k !== "schema" && k !== "files") throw jsonErr(`unknown field \`${k}\``);
  }
  if (typeof m.schema !== "string") throw jsonErr("missing or invalid field `schema`");
  if (!Array.isArray(m.files)) throw jsonErr("missing or invalid field `files`");
  for (const [i, f] of m.files.entries()) {
    if (f === null || typeof f !== "object" || Array.isArray(f)) {
      throw jsonErr(`files[${i}] must be an object`);
    }
    for (const k of Object.keys(f)) {
      if (!FILE_FIELDS.has(k)) throw jsonErr(`unknown field \`${k}\``);
    }
    if (typeof f.role !== "string" || !(f.role in ROLE_ORDER)) {
      throw jsonErr(`files[${i}].role: unknown variant`);
    }
    if (typeof f.relative_path !== "string") throw jsonErr(`files[${i}].relative_path must be a string`);
    if (typeof f.source_url !== "string") throw jsonErr(`files[${i}].source_url must be a string`);
    if (!isRfc3339(f.acquisition_started_at)) throw jsonErr(`files[${i}].acquisition_started_at invalid`);
    if (!isRfc3339(f.acquisition_completed_at)) throw jsonErr(`files[${i}].acquisition_completed_at invalid`);
    if (f.last_modified !== null && typeof f.last_modified !== "string") throw jsonErr(`files[${i}].last_modified invalid`);
    if (f.etag !== null && typeof f.etag !== "string") throw jsonErr(`files[${i}].etag invalid`);
    if (!Number.isSafeInteger(f.bytes) || f.bytes < 0) throw jsonErr(`files[${i}].bytes invalid`);
    if (typeof f.sha256 !== "string") throw jsonErr(`files[${i}].sha256 must be a string`);
  }
}

/**
 * @param {Buffer|Uint8Array} manifestBytes
 * @param {string} root — trusted/private input root
 * @param {Date|string} now
 * @returns {Inspection} — files role-sorted
 */
export function inspect(manifestBytes, root, now) {
  if (!(manifestBytes instanceof Uint8Array)) {
    throw new InspectionError("invalid_input", "manifest_bytes must be a Buffer or Uint8Array");
  }
  if (typeof root !== "string") throw new InspectionError("invalid_input", "root must be a string path");
  const nowMs = now instanceof Date ? now.getTime() : Date.parse(now);
  if (!Number.isFinite(nowMs)) throw new InspectionError("invalid_input", "now must be a Date or timestamp");

  if (manifestBytes.length > 1024 * 1024) throw invalid("manifest exceeds 1 MiB");
  let manifest;
  try {
    manifest = JSON.parse(Buffer.from(manifestBytes).toString("utf8"));
  } catch (e) {
    throw jsonErr(e.message);
  }
  validateShape(manifest);
  if (manifest.schema !== "x-data-mcp.official-file-input.v1" || manifest.files.length !== 3) {
    throw invalid("schema or required role count");
  }
  const roles = new Set(), paths = new Set();
  // Complete contract checked before touching artifact paths.
  for (const input of manifest.files) {
    // Rust Path equality is component-normalized: repeated/trailing
    // separators and interior `.` collapse; a leading `./` is preserved as
    // CurDir and `..` stays ParentDir (both rejected below).
    const rel = input.relative_path;
    const rawSegs = rel.split("/");
    const isAbs = rel.startsWith("/");
    const segs = rawSegs.filter((s, i) => s !== "" && !(s === "." && i > 0));
    const normKey = (isAbs ? "/" : "") + segs.join("/");
    if (roles.has(input.role) || paths.has(normKey)) {
      throw invalid("duplicate role/path");
    }
    roles.add(input.role);
    paths.add(normKey);
    if (rel === "" || isAbs || segs.length === 0 ||
        segs.some((s, i) => s === ".." || (s === "." && i === 0))) {
      throw invalid("artifact path must be relative without parent/current components");
    }
    if (segs[segs.length - 1] !== ROLE_FILE[input.role]) {
      throw invalid("artifact filename does not match its official-file role");
    }
    if (!declaredOfficialUrl(input.role, input.source_url)) {
      throw invalid("expected role-specific declared TDX http(s) URL");
    }
    const started = Date.parse(input.acquisition_started_at);
    const completed = Date.parse(input.acquisition_completed_at);
    if (started > completed || completed > nowMs) {
      throw invalid("inverted/future acquisition interval");
    }
    if (input.bytes === 0 || !/^[0-9a-f]{64}$/.test(input.sha256)) {
      throw invalid("empty file or noncanonical sha256");
    }
  }

  let canonRoot;
  try {
    canonRoot = realpathSync(root);
  } catch (e) {
    throw ioErr(e);
  }
  const rootPrefix = canonRoot.endsWith("/") ? canonRoot : canonRoot + "/";
  for (const input of manifest.files) {
    let resolved;
    try {
      resolved = realpathSync(canonRoot + "/" + input.relative_path);
    } catch (e) {
      throw ioErr(e);
    }
    if (resolved !== canonRoot && !resolved.startsWith(rootPrefix)) {
      throw invalid("artifact resolves outside input root");
    }
    let st;
    try {
      st = statSync(resolved);
    } catch (e) {
      throw ioErr(e);
    }
    if (!st.isFile()) throw invalid("artifact is not a regular file");
    // streaming hash: 64 KiB chunks, never whole-file
    const hash = createHash("sha256");
    let bytes = 0, fd;
    try {
      fd = openSync(resolved, "r");
    } catch (e) {
      throw ioErr(e);
    }
    try {
      const buffer = Buffer.allocUnsafe(65536);
      for (;;) {
        const n = readSync(fd, buffer, 0, 65536, null);
        if (n === 0) break;
        bytes += n;
        hash.update(buffer.subarray(0, n));
      }
    } catch (e) {
      throw ioErr(e);
    } finally {
      closeSync(fd);
    }
    const digest = hash.digest("hex");
    if (bytes !== input.bytes || digest !== input.sha256) {
      throw new InspectionError("mismatch",
        `artifact bytes do not match manifest: ${ROLE_DEBUG[input.role]}`,
        { role: input.role });
    }
  }

  const files = manifest.files
    .map((f) => ({
      role: f.role,
      relative_path: f.relative_path,
      source_url: f.source_url,
      acquisition_started_at: canonTs(f.acquisition_started_at),
      acquisition_completed_at: canonTs(f.acquisition_completed_at),
      last_modified: f.last_modified,
      etag: f.etag,
      bytes: f.bytes,
      sha256: f.sha256,
    }))
    .sort((a, b) => ROLE_ORDER[a.role] - ROLE_ORDER[b.role]);
  return {
    schema: "x-data-mcp.official-file-inspection.v1",
    manifest_sha256: createHash("sha256").update(manifestBytes).digest("hex"),
    files,
    byte_integrity: "FILE_BYTES_VERIFIED",
    declared_origin: "NOT_AUTHENTICATED",
    archive_member_binding: "NOT_VERIFIED",
    file_set_consistency: "NOT_VERIFIED",
    business_semantics: "NOT_VERIFIED",
    access_use: "NOT_VERIFIED",
    production_ready: false,
    live_source: "NOT_RUN",
  };
}
