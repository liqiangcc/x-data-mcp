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
import { closeSync, fsyncSync, mkdirSync, openSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

// Atomic no-overwrite rename: renameat2(RENAME_NOREPLACE). Node's
// renameSync silently replaces an existing EMPTY target directory —
// that would break the immutable-generation identity under a racing
// writer. The binding must be built (gcc -shared -fPIC -O2 -o
// src_node/native/rename_noreplace.node rename_noreplace.c
// -I/usr/include/node); absence fails closed at load time.
const require_ = createRequire(import.meta.url);
const { rename_noreplace: renameNoreplace } = require_("../native/rename_noreplace.node");
import { inspect, strictJson, InspectionError } from "./snapshot.mjs";
import { readBoundedFile } from "../lib/zip_member.mjs";
import { GenerationUsableError, MANIFEST_NAME } from "./generation_usable.mjs";

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
  const fd = openSync(dir, "r");
  try {
    // Node lacks fsync on dirs portably; use fs.fsyncSync on dir fd.
    fsyncFd(fd);
  } finally { closeSync(fd); }
};
const fsyncFd = fsyncSync;
