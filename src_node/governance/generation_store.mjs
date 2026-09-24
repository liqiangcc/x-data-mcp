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
import { parseTs } from "./ts.mjs";
import { renameSync } from "node:fs";

export const STAGING_DIR = "staging";
export const GENERATIONS_DIR = "generations";
export const ACTIVE_NAME = "active-generation.json";
export const PINS_DIR = "pins";
export const ACTIVE_SCHEMA = "x-data-mcp.active-generation.v1";
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

  /** active: raw pointer read — null when absent; malformed bytes or
   *  schema violations reject (Rust serde from_slice -> Json). */
  active() {
    const path = join(this.root, ACTIVE_NAME);
    let bytes;
    try { bytes = readBoundedFile(path, MAX_MANIFEST); }
    catch (e) {
      if (e.code === "ENOENT") return null;
      throw err("io", `io: ${e.message}`);
    }
    let p;
    try { p = strictJson(bytes.toString("utf8")); }
    catch (e) {
      const m = e.message.replace(/^manifest JSON invalid: /, "");
      throw err("json", `manifest JSON invalid: ${m}`);
    }
    validateActivePointer(p);
    return p;
  }

  /** activate: conditional CAS swap — expected_previous compared against
   *  the current generation_id (or null); conflict leaves the pointer
   *  bytes untouched. On success writes serde_json::to_vec_pretty bytes
   *  into `.active-generation.json.tmp`, fsyncs, renames over the
   *  pointer, fsyncs the root dir. Callers MUST hold single-writer
   *  exclusion — Rust assumes it; this port documents it. */
  activate(installed, expectedPrevious, requestId, now) {
    checkGenerationId(installed?.generation_id);
    if (typeof installed?.manifest_sha256 !== "string" || !installed.manifest_sha256.isWellFormed()) {
      throw err("invalid_input", "installed.manifest_sha256 must be a string");
    }
    // activation_request_id is a JSON field, not a path component —
    // wellformed string is the only boundary (Rust accepts any &str).
    if (typeof requestId !== "string" || !requestId.isWellFormed()) {
      throw err("invalid_request_id", "invalid request_id for activation");
    }
    if (expectedPrevious !== null && typeof expectedPrevious !== "string") {
      throw err("invalid_input", "expected_previous must be null or string");
    }
    const activatedAt = emitTs(now); // reject malformed timestamps pre-mutation
    const current = this.active();
    const actual = current ? current.generation_id : null;
    if (actual !== expectedPrevious) {
      throw err("activation_conflict",
        `active pointer conflict: expected ${fmtOpt(expectedPrevious)}, found ${fmtOpt(actual)}`);
    }
    const pointer = {
      schema: ACTIVE_SCHEMA,
      generation_id: installed.generation_id,
      manifest_sha256: installed.manifest_sha256,
      activated_at: activatedAt,
      previous_generation_id: current ? current.generation_id : null,
      activation_request_id: requestId,
    };
    const bytes = Buffer.from(toPrettyJson(pointer), "utf8");
    const tmp = join(this.root, `.${ACTIVE_NAME}.tmp`);
    mkdirSync(this.root, { recursive: true });
    // write + fsync file, rename over pointer, fsync parent — same order
    // as Rust's activate. O_NOFOLLOW: a stale/pre-planted symlink at the
    // tmp name must not redirect the write (stricter than Rust fs::write,
    // documented). A regular tmp file is truncated and rewritten.
    let fd;
    try {
      fd = openSync(tmp, constants.O_WRONLY | constants.O_CREAT | constants.O_TRUNC | constants.O_NOFOLLOW, 0o644);
    } catch (e) { throw err("io", `io: ${e.message}`); }
    let firstErr = null;
    try {
      writeAllFd(fd, bytes);
      fsyncSync(fd);
    } catch (e) { firstErr = e instanceof GenerationUsableError ? e : err("io", `io: ${e.message}`); }
    try { closeSync(fd); }
    catch (e) { if (firstErr === null) firstErr = err("io", `io: ${e.message}`); }
    if (firstErr) throw firstErr;
    try { renameSync(tmp, join(this.root, ACTIVE_NAME)); }
    catch (e) { throw err("io", `io: ${e.message}`); }
    fsyncDir(this.root);
    return pointer;
  }

  /** pin: one durable marker pins/<gid>/<holder> protecting the
   *  installed generation from retire. Marker is a 0-byte file, write+
   *  create+truncate, file fsync + dir fsync — same ordering as Rust.
   *  Returns a Pin handle; release()/dispose() unpins. */
  pin(generationId, holder) {
    checkGenerationId(generationId);
    if (!statOrNull(join(this.root, GENERATIONS_DIR, generationId))?.isDirectory()) {
      throw err("not_installed", `generation ${generationId} is not installed`);
    }
    checkPinHolder(holder);
    const dir = join(this.root, PINS_DIR, generationId);
    mkdirSync(dir, { recursive: true });
    // write+create+truncate, O_NOFOLLOW: a symlink at the holder name is
    // refused rather than followed (stricter than Rust, documented).
    let fd;
    try {
      fd = openSync(join(dir, holder), constants.O_WRONLY | constants.O_CREAT | constants.O_TRUNC | constants.O_NOFOLLOW, 0o644);
    } catch (e) { throw err("io", `io: ${e.message}`); }
    let firstErr = null;
    try { fsyncSync(fd); }
    catch (e) { firstErr = err("io", `io: ${e.message}`); }
    try { closeSync(fd); }
    catch (e) { if (firstErr === null) firstErr = err("io", `io: ${e.message}`); }
    if (firstErr) throw firstErr;
    fsyncDir(dir);
    return new Pin(this, generationId, holder);
  }

  /** pins: sorted regular-file holder names (UTF-8 byte order). */
  pins(generationId) {
    checkGenerationId(generationId);
    const dir = join(this.root, PINS_DIR, generationId);
    const st = statOrNull(dir);
    if (st === null || !st.isDirectory()) return [];
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); }
    catch (e) { throw err("io", `io: ${e.message}`); }
    return entries
      .filter((e) => e.isFile() && isWellformedUtf8Name(e.name))
      .map((e) => e.name)
      .sort((a, b) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8")));
  }

  /** unpin: remove one holder marker; missing marker is idempotent.
   *  Holder is validated BEFORE any filesystem operation — a raw
   *  `../` or separator-bearing name must not traverse outside
   *  pins/<gid> (stricter than Rust, which lets the error surface). */
  unpin(generationId, holder) {
    checkGenerationId(generationId);
    checkPinHolder(holder);
    try { rmSync(join(this.root, PINS_DIR, generationId, holder)); }
    catch (e) {
      if (e.code !== "ENOENT") throw err("io", `io: ${e.message}`);
    }
    fsyncDir(join(this.root, PINS_DIR, generationId));
  }

  /** sweep_pins: remove every holder rejected by keep(). */
  sweepPins(generationId, keep) {
    const removed = [];
    for (const holder of this.pins(generationId)) {
      if (!keep(holder)) {
        this.unpin(generationId, holder);
        removed.push(holder);
      }
    }
    return removed;
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

import { rmSync } from "node:fs";

// Pin handle — RAII release. JS has no Drop: release()/dispose() is the
// deterministic path (Symbol.dispose/AsyncDisposable for `using`); a
// FinalizationRegistry is deliberately NOT used because GC timing is
// not a semantic guarantee.
export class Pin {
  constructor(store, generationId, holder) {
    this._store = store;
    this.generationId = generationId;
    this.holder = holder;
    this._released = false;
  }
  /** Best-effort unpin, mirroring Rust Drop. Idempotent. */
  release() {
    if (this._released) return;
    this._released = true;
    this._store.unpin(this.generationId, this.holder);
  }
  get generation_id() { return this.generationId; }
  [Symbol.dispose]() { this.release(); }
}

// Pin holder boundary — empty/'/'/'\\'/NUL reject verbatim
// ("invalid pin holder"); '.', '..' and unpaired surrogates rejected as
// a stricter Node boundary (they would traverse out of pins/<gid>).
function checkPinHolder(holder) {
  if (typeof holder !== "string" || !holder.isWellFormed() ||
      holder === "" || holder === "." || holder === ".." ||
      holder.includes("/") || holder.includes("\\") || holder.includes("\0")) {
    throw err("not_verified", "snapshot inspection failed: invalid pin holder");
  }
}

// Node readdir surfaces non-UTF-8 names as WTF-8 mangled strings; Rust
// to_str() simply skips them. Wellformed check approximates that skip.
const isWellformedUtf8Name = (name) => name.isWellFormed();

// Counted write loop — Node may return a short count; zero-write is an
// error. BYTE-INPUT ONLY: strings are rejected because Node's string
// overload treats `offset` as a file position and `length` as UTF-16
// code units — silently persisting wrong bytes for multibyte content.
// Exported for probe injection of deterministic short writes.
export function writeAllFd(fd, bytes, _writeSync = writeSync) {
  if (!(bytes instanceof Uint8Array)) {
    throw err("invalid_input", "writeAllFd requires Buffer/Uint8Array");
  }
  let off = 0;
  while (off < bytes.length) {
    const n = _writeSync(fd, bytes, off, bytes.length - off);
    if (n === 0) throw err("io", "io: zero-length write");
    off += n;
  }
}

// Rust Debug fmt for Option<String>: Some("x") / None
const fmtOpt = (v) => v === null || v === undefined ? "None" : `Some(${JSON.stringify(v)})`;

// serde deny_unknown_fields ActivePointer boundary — exact known-field
// set; `schema` is a plain String (serde does NOT enforce the constant —
// oracle verified), `previous_generation_id` is Option and defaults to
// null when absent.
const POINTER_FIELDS = ["schema", "generation_id", "manifest_sha256",
  "activated_at", "previous_generation_id", "activation_request_id"];
const POINTER_REQUIRED = POINTER_FIELDS.filter((k) => k !== "previous_generation_id");
function validateActivePointer(p) {
  if (p === null || typeof p !== "object" || Array.isArray(p)) {
    throw err("json", "manifest JSON invalid: invalid type: expected struct ActivePointer");
  }
  for (const k of Object.keys(p)) {
    if (!POINTER_FIELDS.includes(k)) {
      throw err("json", `manifest JSON invalid: unknown field \`${k}\``);
    }
  }
  for (const k of POINTER_REQUIRED) {
    if (!(k in p)) {
      throw err("json", `manifest JSON invalid: missing field \`${k}\``);
    }
  }
  const strField = (k) => {
    if (typeof p[k] !== "string" || !p[k].isWellFormed()) {
      throw err("json", `manifest JSON invalid: invalid type for field \`${k}\``);
    }
  };
  strField("schema"); strField("generation_id"); strField("manifest_sha256");
  strField("activated_at"); strField("activation_request_id");
  if ("previous_generation_id" in p &&
      p.previous_generation_id !== null && typeof p.previous_generation_id !== "string") {
    throw err("json", "manifest JSON invalid: invalid type for field `previous_generation_id`");
  }
  if (!("previous_generation_id" in p)) p.previous_generation_id = null;
  if (parseTs(p.activated_at) === null) {
    throw err("json", "manifest JSON invalid: invalid timestamp for field `activated_at`");
  }
}

// serde_json::to_vec_pretty equivalent for the flat ActivePointer —
// 2-space indent, `"k": v`, field order preserved. activated_at arrives
// pre-serialized by emitTs (chrono AutoSi format).
function toPrettyJson(pointer) {
  const q = (s) => {
    // serde_json escaping: short escapes for " \ \b \t \n \f \r, other
    // C0 controls -> \u00XX, DEL and non-ASCII emitted raw (UTF-8).
    return `"${s.replace(/["\\\x00-\x1f]/g, (c) => {
      switch (c) {
        case '"': return '\\"';
        case "\\": return "\\\\";
        case "\b": return "\\b";
        case "\t": return "\\t";
        case "\n": return "\\n";
        case "\f": return "\\f";
        case "\r": return "\\r";
        default: return `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`;
      }
    })}"`;
  };
  const lines = POINTER_FIELDS.map((k) => {
    const v = pointer[k];
    return `  ${q(k)}: ${v === null ? "null" : q(v)}`;
  });
  return `{\n${lines.join(",\n")}\n}`;
}

// chrono serde DateTime<Utc> emission — parseTs already returns the
// canonical AutoSi `out` (Z, leap second literal, 3-digit ns groups).
function emitTs(now) {
  const s = now instanceof Date ? now.toISOString() : now;
  const p = parseTs(s);
  if (p === null) throw err("invalid_input", `invalid timestamp: ${String(s).slice(0, 40)}`);
  return p.out;
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
