// immutable generation staging + install — Node port of
// generation/mod.rs::{create_stage, staged_manifest, install_staged,
// installed_ids}. A fresh stage is created once per request; install
// byte-verifies every artifact against the staged canonical manifest,
// derives the generation ID from sha256(manifest bytes), refuses an
// existing identity, then renames the verified stage into
// generations/<id> and fsyncs the parent. No overwrite, no repair.
//
// Stricter than Rust (documented): request_id is boundary-checked
// (non-empty wellformed string; no '/', '\\', NUL, '.', '..') before
// any filesystem mutation — Rust's stage_dir would happily resolve
// ".." out of the staging area.
//
// Durability: same-filesystem rename + fsync of the generations dir.
// On Box overlayfs this is the same primitive Rust uses; crash-window
// limits are documented in the task evidence (not claimed absolute).

import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { closeSync, constants, fsyncSync, mkdirSync, openSync, readdirSync, statSync, writeSync } from "node:fs";
import { join } from "node:path";

// Atomic no-overwrite rename: renameat2(RENAME_NOREPLACE). Node's
// renameSync silently replaces an existing EMPTY target directory —
// that would break the immutable-generation identity under a racing
// writer. The binding must be built (gcc -shared -fPIC -O2 -o
// src_node/native/rename_noreplace.node rename_noreplace.c
// -I/usr/include/node); absence fails closed at load time.
const require_ = createRequire(import.meta.url);
const { rename_noreplace: renameNoreplace, openat_dir: openatDir, openat_create_excl: openatCreateExcl } =
  require_("../native/rename_noreplace.node");
import { inspect, strictJson, InspectionError } from "./snapshot.mjs";
import { readBoundedFile } from "../lib/zip_member.mjs";
import { DECISION_NAME, GenerationUsableError, MANIFEST_NAME } from "./generation_usable.mjs";

export const STAGING_DIR = "staging";
export const GENERATIONS_DIR = "generations";
const MAX_MANIFEST = 1 * 1024 * 1024;

const err = (kind, msg) => new GenerationUsableError(kind, msg);

// serde manifest struct-stage validation — mirrors
// from_slice::<SnapshotManifest> so field/type errors map to "json"
// rather than inspection's "corrupt"/"not_verified" equivalents.
import { validateManifestSerde } from "./generation_usable.mjs";

// request_id: reject path escapes / JS-only shapes before mutation.
function checkRequestId(requestId) {
  if (typeof requestId !== "string" || !requestId.isWellFormed() ||
      requestId === "" || requestId.includes("/") || requestId.includes("\\") ||
      requestId.includes("\0") || requestId === "." || requestId === "..") {
    throw err("invalid_request_id", `invalid request_id for staging`);
  }
}

export class GenerationStore {
  constructor(root) {
    if (typeof root !== "string" || !root.isWellFormed() || root === "") {
      throw err("invalid_root", "store root must be a string path");
    }
    this.root = root;
  }
  stageDir(requestId) {
    checkRequestId(requestId);
    return join(this.root, STAGING_DIR, requestId);
  }
  generationDir(generationId) {
    return join(this.root, GENERATIONS_DIR, generationId);
  }

  /** create_stage: once per request; exists -> staging refusal. */
  createStage(requestId) {
    const dir = this.stageDir(requestId);
    // Atomic exclusive mkdir: two concurrent creators must not both
    // succeed (recursive mkdir would accept a racing creation).
    try { mkdirSync(dir); }
    catch (e) {
      if (e.code === "EEXIST") {
        throw err("staging", `staging area for request ${requestId} is already sealed or missing`);
      }
      if (e.code === "ENOENT") {
        mkdirSync(join(this.root, STAGING_DIR), { recursive: true });
        try { mkdirSync(dir); }
        catch (e2) {
          if (e2.code === "EEXIST") {
            throw err("staging", `staging area for request ${requestId} is already sealed or missing`);
          }
          throw err("io", `io: ${e2.message}`);
        }
        return dir;
      }
      throw err("io", `io: ${e.message}`);
    }
    return dir;
  }

  /** staged_manifest: (bytes, parsed) or UnverifiedStage / Json. */
  stagedManifest(requestId) {
    const path = join(this.stageDir(requestId), MANIFEST_NAME);
    let bytes;
    try { bytes = readBoundedFile(path, MAX_MANIFEST); }
    catch { throw err("unverified_stage", `staged request ${requestId} has no canonical manifest`); }
    let manifest;
    try { manifest = strictJson(bytes.toString("utf8")); validateManifestSerde(manifest); }
    catch (e) {
      throw err("json", `manifest JSON invalid: ${e instanceof InspectionError
        ? e.message.replace(/^manifest JSON invalid: /, "") : e.message}`);
    }
    return { bytes, manifest };
  }

  /** install_staged: verify -> refuse existing -> rename -> fsync. */
  installStaged(requestId, now) {
    const { bytes: manifestBytes, manifest } = this.stagedManifest(requestId);
    const generationId = createHash("sha256").update(manifestBytes).digest("hex");
    const target = this.generationDir(generationId);
    if (statOrNull(target) !== null) {
      throw err("already_installed",
        `generation ${generationId} already installed; refusing to overwrite immutable bytes`);
    }
    const stage = this.stageDir(requestId);
    let inspection;
    try { inspection = inspect(manifestBytes, stage, now); }
    catch (e) { throw err("not_verified", `snapshot inspection failed: ${e.message}`); }
    if (inspection.production_ready) {
      throw err("not_verified",
        "snapshot inspection failed: inspection must not approve production readiness");
    }
    mkdirSync(join(this.root, GENERATIONS_DIR), { recursive: true });
    // Atomic no-overwrite: a target created between the check above and
    // here fails EEXIST (never replaced). Rust fs::rename relies on the
    // caller's single-writer exclusion; RENAME_NOREPLACE makes the
    // guarantee local to this call.
    try { renameNoreplace(stage, target); }
    catch (e) {
      if (e.code === "EEXIST") {
        throw err("already_installed",
          `generation ${generationId} already installed; refusing to overwrite immutable bytes`);
      }
      throw err("io", `io: ${e.message}`);
    }
    fsyncDir(join(this.root, GENERATIONS_DIR));
    return {
      generation_id: generationId,
      manifest_sha256: inspection.manifest_sha256,
      dir: target,
      manifest,
      manifest_bytes: manifestBytes,
    };
  }

  /** installed_ids: directory names under generations/, UTF-8-byte
   *  sorted (Rust Vec<String>::sort orders bytes, not UTF-16 units). */
  installedIds() {
    const dir = join(this.root, GENERATIONS_DIR);
    if (statOrNull(dir) === null) return [];
    return readdirSync(dir, { withFileTypes: true })
      .filter((e) => e.isDirectory() && e.name.isWellFormed())
      .map((e) => e.name)
      .sort((a, b) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8")));
  }

  /** write_decision: once-only consistency-decision.json — exclusive
   *  create, full write, fsync file + dir, returns sha256. The whole
   *  operation runs relative to fds pinned with O_DIRECTORY|O_NOFOLLOW,
   *  so a same-UID process swapping a checked directory for a symlink
   *  between check and write cannot redirect the mutation. */
  writeDecision(generationId, decisionBytes) {
    checkGenerationId(generationId);
    const { gfd, dfd } = this.openGenerationDirFds(generationId);
    let firstErr = null;
    try {
      writeOnceAt(dfd, DECISION_NAME, decisionBytes, () => {
        throw err("decision_exists",
          `consistency decision already recorded for generation ${generationId}`);
      });
      fsyncFd(dfd);
    } catch (e) {
      firstErr = e;
    }
    for (const fd of [dfd, gfd]) {
      try { closeSync(fd); }
      catch (e) { if (firstErr === null) firstErr = err("io", `io: ${e.message}`); }
    }
    if (firstErr) throw firstErr;
    return createHash("sha256").update(decisionBytes).digest("hex");
  }

  /** decision: raw once-written decision bytes, or null. */
  decision(generationId) {
    checkGenerationId(generationId);
    const path = join(this.generationDir(generationId), DECISION_NAME);
    try { return readBoundedFile(path, MAX_MANIFEST); }
    catch (e) {
      if (e.code === "ENOENT") return null;
      throw err("io", `io: ${e.message}`);
    }
  }

  /** write_artifact: once-only supplementary evidence artifact —
   *  fd-pinned like writeDecision (no ancestor-symlink race). */
  writeArtifact(generationId, name, bytes) {
    checkGenerationId(generationId);
    checkArtifactName(name);
    const { gfd, dfd } = this.openGenerationDirFds(generationId);
    let firstErr = null;
    try {
      writeOnceAt(dfd, name, bytes, () => {
        throw err("artifact_exists",
          `artifact ${name} for generation ${generationId} already exists`);
      });
      fsyncFd(dfd);
    } catch (e) {
      firstErr = e;
    }
    for (const fd of [dfd, gfd]) {
      try { closeSync(fd); }
      catch (e) { if (firstErr === null) firstErr = err("io", `io: ${e.message}`); }
    }
    if (firstErr) throw firstErr;
    return createHash("sha256").update(bytes).digest("hex");
  }

  /** artifact: supplementary artifact bytes, or null. */
  artifact(generationId, name) {
    checkGenerationId(generationId);
    checkArtifactName(name);
    try { return readBoundedFile(join(this.generationDir(generationId), name), MAX_MANIFEST); }
    catch (e) {
      if (e.code === "ENOENT") return null;
      throw err("io", `io: ${e.message}`);
    }
  }

  /** Installed-generation dir for writes, pinned by fds: opens
   *  generations/ with O_DIRECTORY|O_NOFOLLOW, then generations/<gid>
   *  via openat relative to that fd — a symlink swap after opening
   *  cannot redirect subsequent writes. Returns {gfd, dfd}; caller
   *  closes both. Stricter than Rust's is_dir() (documented). */
  openGenerationDirFds(generationId) {
    const parent = join(this.root, GENERATIONS_DIR);
    let gfd;
    try {
      gfd = openSync(parent, constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW);
    } catch (e) {
      if (e.code === "ENOENT" || e.code === "ENOTDIR" || e.code === "ELOOP") {
        throw err("not_installed", `generation ${generationId} is not installed`);
      }
      throw err("io", `io: ${e.message}`);
    }
    let dfd;
    try {
      dfd = openatDir(gfd, generationId);
    } catch (e) {
      closeFdQuiet(gfd);
      if (e.code === "ENOENT" || e.code === "ENOTDIR" || e.code === "ELOOP") {
        throw err("not_installed", `generation ${generationId} is not installed`);
      }
      throw err("io", `io: ${e.message}`);
    }
    return { gfd, dfd };
  }
}

// gid/name boundary checks — stricter than Rust (documented): Rust
// joins raw strings; Node additionally refuses '.'/'..' components and
// unpaired surrogates (which would WTF-8-alias to a different name).
// Artifact names keep Rust's check verbatim (empty, '/', '\\', NUL)
// plus wellformedness; '..' resolves to the dir itself and fails
// EEXIST -> artifact_exists, matching Rust's outcome exactly.
function checkGenerationId(gid) {
  if (typeof gid !== "string" || !gid.isWellFormed() || gid === "" ||
      gid.includes("/") || gid.includes("\\") || gid.includes("\0") ||
      gid === "." || gid === "..") {
    throw err("invalid_generation_id", "invalid generation_id");
  }
}
// Reserved immutable inputs: writeArtifact must not be able to create or
// resurrect the manifest, the decision, or declared artifact bytes —
// those have dedicated writers (install staging, write_decision) with
// their own verification. compatibility-evidence.json remains allowed.
const RESERVED_ARTIFACT_NAMES = new Set([
  MANIFEST_NAME, DECISION_NAME, "base.zip", "gbbq.zip", "hsjday.zip",
]);
function checkArtifactName(name) {
  if (typeof name !== "string" || !name.isWellFormed() || name === "" ||
      name.includes("/") || name.includes("\\") || name.includes("\0") ||
      RESERVED_ARTIFACT_NAMES.has(name)) {
    throw err("not_verified", "snapshot inspection failed: invalid artifact name");
  }
}
// Once-only write relative to a pinned directory fd:
// openat(dfd, name, O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW) — an existing
// regular file OR symlink both fail EEXIST; the target is never
// opened for writing. Full write + file fsync; a failed or partial
// write leaves the exclusive-created (possibly partial) file in
// place — never silently repaired, matching Rust. close() errors are
// surfaced as typed io when no earlier failure occurred.
export function writeOnceAt(dirFd, name, bytes, onExists, _impl = null) {
  if (!(bytes instanceof Uint8Array)) {
    throw err("invalid_input", "artifact bytes must be Buffer/Uint8Array");
  }
  // Read bound is 1MiB (aligned with NM-032's decision bound): writing a
  // larger object would create an unreadable, unreplaceable file — refuse
  // before exclusive creation. Stricter than Rust (documented).
  if (bytes.length > MAX_MANIFEST) {
    throw err("invalid_input", `artifact bytes exceed bound ${MAX_MANIFEST}`);
  }
  const impl = _impl ?? { openat: openatCreateExcl, writeSync, fsyncSync, closeSync };
  let fd;
  try {
    fd = impl.openat(dirFd, name);
  } catch (e) {
    if (e.code === "EEXIST") return onExists();
    throw err("io", `io: ${e.message}`);
  }
  let firstErr = null;
  try {
    let off = 0;
    while (off < bytes.length) {
      const n = impl.writeSync(fd, bytes, off, bytes.length - off);
      if (n === 0) throw err("io", "io: zero-length write");
      off += n;
    }
    impl.fsyncSync(fd);
  } catch (e) {
    firstErr = e instanceof GenerationUsableError ? e : err("io", `io: ${e.message}`);
  }
  try { impl.closeSync(fd); }
  catch (e) {
    if (firstErr === null) {
      firstErr = e instanceof GenerationUsableError ? e : err("io", `io: ${e.message}`);
    }
  }
  if (firstErr) throw firstErr;
  return null;
}

// ENOENT/ENOTDIR mean absent; every other stat failure propagates —
// permission or I/O errors must not masquerade as "not present".
const statOrNull = (p) => {
  try { return statSync(p); }
  catch (e) {
    if (e.code === "ENOENT" || e.code === "ENOTDIR") return null;
    throw err("io", `io: ${e.message}`);
  }
};
const fsyncDir = (dir) => {
  try {
    const fd = openSync(dir, "r");
    fsyncFdClose(fd);
  } catch (e) {
    if (e instanceof GenerationUsableError) throw e;
    throw err("io", `io: ${e.message}`);
  }
};
// fsync(fd) then close(fd); the FIRST error wins, and a close-only
// failure still surfaces as typed io — close errors are never swallowed.
const fsyncFdClose = (fd, impl = { fsyncSync, closeSync }) => {
  let firstErr = null;
  try { impl.fsyncSync(fd); }
  catch (e) { firstErr = e instanceof GenerationUsableError ? e : err("io", `io: ${e.message}`); }
  try { impl.closeSync(fd); }
  catch (e) {
    if (firstErr === null) {
      firstErr = e instanceof GenerationUsableError ? e : err("io", `io: ${e.message}`);
    }
  }
  if (firstErr) throw firstErr;
};
const fsyncFd = (fd) => {
  try { fsyncSync(fd); }
  catch (e) { throw err("io", `io: ${e.message}`); }
};
// Resource cleanup only — deliberately not a typed-error path: used for
// pinned dir fds after the durable fsync already succeeded or failed.
const closeFdQuiet = (fd) => { try { closeSync(fd); } catch { /* cleanup */ } };
