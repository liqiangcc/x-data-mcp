// Node port of src/acquire/service.rs — acquisition service state
// machine core (S14b): open/reconcile/check_updates/
// acquire_generation/worker_once/advance/fetch_role/write_manifest/
// task/request/sync_status. verify/activate/retire/probes are S14c.
//
// Every mutating call binds a caller request_id whose outcome is
// recorded once and replayed verbatim — a lost response never leaves
// an unrecoverable side effect. Downloads are durable tasks driven by
// worker_once; each stage commits before the next begins, so a crash
// at any point reconciles mechanically at open.

import { createHash } from "node:crypto";
import {
  chmodSync, closeSync, existsSync, fsyncSync, mkdirSync, openSync,
  readFileSync, rmSync, statSync, writeSync,
} from "node:fs";
import { join } from "node:path";
import { Db, AcquireStoreError, toRfc3339 } from "./store.mjs";
import { Helper, Mirror, FetchError } from "./fetch.mjs";
import { Change } from "./wire.mjs";
import {
  GENERATIONS_DIR, GenerationStore, PINS_DIR, STAGING_DIR,
} from "../governance/generation_store.mjs";
import { MANIFEST_NAME, verifyInstalled } from "../governance/generation_usable.mjs";
import { CONTRACT_VERSION as CONSISTENCY_CONTRACT } from "../governance/file_consistency.mjs";

// service.rs:33-49
export const GENERATION_CONTRACT_VERSION = "official-generation-v1";
export const PROBE_CONTRACT_VERSION = "official-generation-probe-v1";
export const MAX_HOT_GENERATIONS = 6;

const KIND_ACQUIRE = "acquire_generation";
const KIND_VERIFY = "verify_generation";
const KIND_ACTIVATE = "activate_generation";
const KIND_RETIRE = "retire_generation";
const KIND_TASK = "download_install";

const ST_ACCEPTED = "accepted";
const ST_RUNNING = "running";
const ST_SUCCEEDED = "succeeded";
const ST_FAILED = "failed";

const GEN_BYTE_VERIFIED = "byte_verified";
const GEN_VERIFIED = "consistency_verified";
const GEN_ACTIVE = "active";
const GEN_SUPERSEDED = "superseded";
const GEN_EVIDENCE_ONLY = "evidence_only";
const GEN_CORRUPT = "corrupt";

// ---------- Error (service.rs:71-98) ----------

export class AcquireServiceError extends Error {
  constructor(kind, message, reasonCode = null) {
    super(message);
    this.name = "AcquireServiceError";
    this.kind = kind;
    this.reasonCode = reasonCode;
  }
  /** Error::code() — stable reason code for wire replies/outcomes. */
  code() {
    switch (this.kind) {
      case "refused": return this.reasonCode;
      case "storage": return "storage_error";
      case "generation": return "generation_error";
      case "fetch": return "fetch_error";
      case "json": return "internal_error";
      case "io": return "io_error";
      default: return "internal_error";
    }
  }
}
const refused = (code) => new AcquireServiceError("refused", code, code);

// #[from] mapping: the error's source type fixes the variant —
// store→Storage, generation→Generation, fetch→Fetch, serde→Json,
// io→Io. TypeErrors and other engine errors are bugs — raw.
const svcErr = (e) => {
  if (e instanceof AcquireServiceError) return e;
  if (e instanceof AcquireStoreError)
    return new AcquireServiceError("storage", `storage: ${e.message}`);
  if (e instanceof FetchError)
    return new AcquireServiceError("fetch", `fetch: ${e.message}`);
  if (e?.name === "GenerationUsableError")
    return new AcquireServiceError("generation", `generation: ${e.message}`);
  if (e instanceof SyntaxError || e?.name === "WireError")
    return new AcquireServiceError("json", `json: ${e.message}`);
  if (typeof e?.code === "string" && /^(E[A-Z]+|ERR_)/.test(e.code))
    return new AcquireServiceError("io", `io: ${e.message}`);
  return e;
};

// ---------- Source (service.rs:104-129) ----------

export const Source = Object.freeze({
  remote: () => ({ type: "remote" }),
  mirror: (dir) => ({ type: "mirror", dir }),
  fetcher: (fetcher) => ({ type: "fetcher", fetcher }),
});
const toFetcher = (s) => {
  if (s.type === "remote") return new Helper();
  if (s.type === "mirror") return new Mirror(s.dir);
  if (s.type === "fetcher") {
    // Rust's trait bound — shape-check at the boundary.
    if (typeof s.fetcher?.head !== "function" ||
        typeof s.fetcher?.fetch !== "function") {
      throw new TypeError("fetcher must implement head(url) and fetch(url,dest)");
    }
    return s.fetcher;
  }
  throw new TypeError(`unknown source type ${s.type}`);
};

// ---------- ROLES (service.rs:139-164) ----------

const ROLES = [
  { role: "base_industry", key: "base_industry",
    url: "https://www.tdx.com.cn/products/data/data/dbf/base.zip",
    file: "base.zip" },
  { role: "gbbq", key: "gbbq",
    url: "https://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
    file: "gbbq.zip" },
  { role: "day", key: "day",
    url: "https://data.tdx.com.cn/vipdoc/hsjday.zip",
    file: "hsjday.zip" },
];
const sha256hex = (b) => createHash("sha256").update(b).digest("hex");
const now = () => new Date();

// Mutex<()> — the single-writer mutation lock. Service methods are
// synchronous; a held lock at entry is reentrancy (a bug), so the port
// trips fail-closed instead of Rust's blocking wait — contention can
// never arise inside one synchronous call.
class MutationLock {
  #held = false;
  acquire() {
    if (this.#held) throw new Error("mutation lock poisoned");
    this.#held = true;
    let released = false;
    return () => { if (!released) { released = true; this.#held = false; } };
  }
}

// ---------- serde_json::Value serializers ----------
// Value::to_string / to_value of json!() maps are BTreeMap — keys sort
// byte-wise. Struct serialization keeps declaration order.
const serValueSorted = (v) => { // compact, object keys sorted
  if (v === null) return "null";
  if (typeof v === "bigint") return v.toString();
  if (typeof v !== "object") return JSON.stringify(v);
  if (Array.isArray(v))
    return "[" + v.map(serValueSorted).join(",") + "]";
  return "{" + Object.keys(v).sort().map((k) =>
    JSON.stringify(k) + ":" + serValueSorted(v[k])).join(",") + "}";
};
// serde_json::to_vec_pretty: 2-space indent, "k": v, field order.
const serPretty = (v, depth = 0) => {
  const pad = "  ".repeat(depth), padIn = "  ".repeat(depth + 1);
  if (v === null) return "null";
  if (typeof v === "bigint") return v.toString();
  if (typeof v !== "object") return JSON.stringify(v);
  if (Array.isArray(v)) {
    if (v.length === 0) return "[]";
    return "[\n" + v.map((x) => padIn + serPretty(x, depth + 1))
      .join(",\n") + "\n" + pad + "]";
  }
  const ks = Object.keys(v);
  if (ks.length === 0) return "{}";
  return "{\n" + ks.map((k) =>
    padIn + JSON.stringify(k) + ": " + serPretty(v[k], depth + 1))
    .join(",\n") + "\n" + pad + "}";
};
// chrono serde DateTime<Utc>: to_rfc3339_opts(AutoSi, use_z=true) —
// "Z" suffix, .mmm only when nonzero. NOT the same as store.rs's
// to_rfc3339() (+00:00) — both shapes preserved deliberately.
const serTs = (d) => {
  const iso = d.toISOString();
  return iso.endsWith(".000Z") ? iso.slice(0, -5) + "Z" : iso;
};

// serde_json::Value u64 read: integer ≥0 (number) or bigint.
const asU64 = (v) =>
  typeof v === "bigint" && v >= 0n ? v :
  typeof v === "number" && Number.isInteger(v) && v >= 0 ? BigInt(v) :
  null;

export class AcquireService {
  constructor({ store, db, fetcher }) {
    this.store = store;
    this.db = db;
    this.fetcher = fetcher;
    this.mutations = new MutationLock();
  }

  /** service.rs:171-195 — create dirs, open+chmod the acquire db,
   *  then reconcile every crash-reachable fact. */
  static open(root, source) {
    try {
      const store = new GenerationStore(root);
      mkdirSync(join(root, STAGING_DIR), { recursive: true });
      mkdirSync(join(root, GENERATIONS_DIR), { recursive: true });
      mkdirSync(join(root, PINS_DIR), { recursive: true });
      const dbPath = join(root, "acquire.sqlite"); // acquire_db_path
      const db = new Db(dbPath);
      db.open();
      // The daemon's single-writer lease requires owner-private bytes.
      chmodSync(dbPath, 0o600);
      const service = new AcquireService({
        store, db, fetcher: toFetcher(source) });
      service.reconcile();
      return service;
    } catch (e) {
      throw svcErr(e);
    }
  }

  /** The durable acquisition store path — the daemon leases this inode. */
  dbPath() { return join(this.store.root, "acquire.sqlite"); }
  /** The generation store — source-session pinning + read paths. */
  generations() { return this.store; }

  // remove_stage (mod.rs:242-248) — in-lane: stageDir() path +
  // remove_dir_all-if-exists verbatim.
  _removeStage(requestId) {
    const dir = this.store.stageDir(requestId);
    if (existsSync(dir)) rmSync(dir, { recursive: true });
  }

  /** service.rs:207-270 — running→accepted resume; orphan install
   *  adoption (byte_verified / corrupt — recorded, never deleted);
   *  pointer-wins state realign; terminal-stage residue cleanup. */
  reconcile() {
    try {
      for (const task of this.db.tasksIn([ST_RUNNING])) {
        this.db.updateTask({
          ...task, state: ST_ACCEPTED, updated_at: now() });
      }
      for (const id of this.store.installedIds()) {
        if (this.db.generation(id) !== null) continue;
        let state, sha;
        try {
          const installed = verifyInstalled(this.store.root, id, now());
          state = GEN_BYTE_VERIFIED;
          sha = installed.manifest_sha256;
        } catch (e) {
          state = GEN_CORRUPT;
          sha = `unverified: ${e.message}`;
        }
        this.db.upsertGeneration({
          generation_id: id, state, manifest_sha256: sha,
          consistency_status: null, consistency_reasons: null,
          decision_sha256: null, evidence_json: null,
          acquisition_request_id: null,
          installed_at: now(), updated_at: now() });
      }
      const pointer = this.store.active();
      if (pointer) {
        for (const row of this.db.generationsIn(
          [GEN_ACTIVE, GEN_SUPERSEDED, GEN_VERIFIED])) {
          const expected = row.generation_id === pointer.generation_id
            ? GEN_ACTIVE
            : row.state === GEN_ACTIVE ? GEN_SUPERSEDED : null;
          if (expected === null) continue;
          if (row.state !== expected) {
            this.db.upsertGeneration({
              ...row, state: expected, updated_at: now() });
          }
        }
      }
      for (const task of this.db.tasksIn([ST_SUCCEEDED, ST_FAILED])) {
        this._removeStage(task.request_id);
      }
    } catch (e) {
      throw svcErr(e);
    }
  }

  // ---------- remote observation ----------

  /** service.rs:277-342 — read-only remote probe; ETag is the only
   *  reliable validator (without it never `unchanged`, AC-A03);
   *  fresh observations recorded. */
  checkUpdates() {
    try {
      const files = [], changed = [], unknown = [];
      for (const role of ROLES) {
        const previous = this.db.seen(role.key);
        let head = null;
        try { head = this.fetcher.head(role.url); } catch { /**/ }
        let change;
        if (head === null || previous === null) {
          change = Change.Unknown;
        } else if (previous.etag !== null && head.etag !== null) {
          change = previous.etag === head.etag
            ? Change.Unchanged : Change.Changed;
        } else if (previous.etag !== null || head.etag !== null) {
          change = Change.Unknown;
        } else {
          const pl = previous.last_modified, hl = head.last_modified,
                pc = previous.content_length, hc = head.content_length;
          change = (pl !== null && hl !== null &&
                    pc !== null && hc !== null && (pl !== hl || pc !== hc))
            ? Change.Changed : Change.Unknown;
        }
        if (change === Change.Changed) changed.push(role.key);
        if (change === Change.Unknown) unknown.push(role.key);
        if (head !== null) {
          this.db.recordSeen({
            role: role.key, etag: head.etag,
            last_modified: head.last_modified,
            content_length: head.content_length, seen_at: now() });
        }
        files.push({
          role: role.key, url: role.url,
          status: head === null ? 0 : head.status,
          etag: head?.etag ?? null,
          last_modified: head?.last_modified ?? null,
          content_length: head?.content_length ?? null,
          change,
        });
      }
      return { checked_at: toRfc3339(now()), files, changed, unknown };
    } catch (e) {
      throw svcErr(e);
    }
  }

  // ---------- acquisition task ----------

  /** service.rs:350-414 — accept a generation download; request row is
   *  the idempotency registry, capacity fail-closed at
   *  MAX_HOT_GENERATIONS (AC-R04). */
  acquireGeneration(requestId) {
    try {
      const { request, reused } = this.db.acceptRequest(
        requestId, KIND_ACQUIRE, "{}", now());
      if (reused) {
        const task = this.db.taskByRequest(request.request_id);
        if (task === null) throw refused("request_binding_lost");
        return {
          request_id: request.request_id, task_id: task.task_id,
          state: task.state, reused: true,
        };
      }
      const release = this.mutations.acquire();
      try {
        const hot = this.db.generations()
          .filter((g) => g.state !== GEN_EVIDENCE_ONLY).length;
        if (hot >= MAX_HOT_GENERATIONS) {
          try {
            this.db.recordOutcome(requestId,
              JSON.parse(serValueSorted(
                { state: "refused", reason_code: "generation_capacity" })),
              now());
          } catch { /* best-effort (let _ =) */ }
          throw refused("generation_capacity");
        }
        const taskId = `acq-${sha256hex(requestId).slice(0, 24)}`;
        const task = {
          task_id: taskId, request_id: request.request_id,
          kind: KIND_TASK, state: ST_ACCEPTED, generation_id: null,
          stage: `fetch:${ROLES[0].key}`, bytes_done: 0n,
          error: null, detail: serValueSorted({ files: {} }),
          created_at: now(), updated_at: now(),
        };
        this.db.createTask(task);
        // A stale staging area from an earlier attempt belongs to a
        // recorded task outcome; this fresh request starts clean.
        if (existsSync(this.store.stageDir(requestId))) {
          this._removeStage(requestId);
        }
        this.store.createStage(requestId);
        return {
          request_id: request.request_id, task_id: taskId,
          state: ST_ACCEPTED, reused: false,
        };
      } finally {
        release();
      }
    } catch (e) {
      throw svcErr(e);
    }
  }

  /** service.rs:419-453 — advance the oldest open task by one stage;
   *  called only by the service worker loop (AC-A02). */
  workerOnce() {
    try {
      const release = this.mutations.acquire();
      try {
        const task = this.db.tasksIn([ST_ACCEPTED, ST_RUNNING])[0];
        if (task === undefined) return null;
        if (task.state !== ST_RUNNING) {
          this.db.updateTask({ ...task, state: ST_RUNNING, updated_at: now() });
        }
        try {
          const next = this.advance(task);
          return this.db.task(task.task_id) ?? next;
        } catch (e) {
          this.db.updateTask({
            ...task, state: ST_FAILED, error: e.message,
            updated_at: now() });
          try {
            this.db.recordOutcome(task.request_id, JSON.parse(serValueSorted(
              { state: "failed", task_id: task.task_id,
                error: e.message })), now());
          } catch { /* best-effort (let _ =) */ }
          return this.db.task(task.task_id) ?? task;
        }
      } finally {
        release();
      }
    } catch (e) {
      throw svcErr(e);
    }
  }

  /** service.rs:455-531 — one durable stage: fetch:<role> chain →
   *  manifest → install; unknown stage refuses. */
  advance(task) {
    let detail;
    try {
      detail = JSON.parse(task.detail ?? '{"files":{}}');
    } catch (e) {
      throw svcErr(e);
    }
    const stage = task.stage ?? "";
    if (stage.startsWith("fetch:")) {
      const key = stage.slice("fetch:".length);
      const role = ROLES.find((r) => r.key === key);
      if (!role) throw refused("unknown_stage");
      this.fetchRole(task, role, detail);
      const next = role.key === "day"
        ? "manifest"
        : `fetch:${ROLES[ROLES.findIndex((r) => r.key === role.key) + 1].key}`;
      this.db.updateTask({
        ...task, state: ST_RUNNING, generation_id: null, stage: next,
        bytes_done: detailBytes(detail), error: null,
        detail: serValueSorted(detail), updated_at: now() });
      return this.db.task(task.task_id) ?? { ...task };
    }
    switch (stage) {
      case "manifest": {
        this.writeManifest(task, detail);
        this.db.updateTask({
          ...task, state: ST_RUNNING, generation_id: null,
          stage: "install", bytes_done: detailBytes(detail),
          error: null, detail: serValueSorted(detail), updated_at: now() });
        break;
      }
      case "install": {
        const installed = this.store.installStaged(task.request_id, now());
        this.db.upsertGeneration({
          generation_id: installed.generation_id,
          state: GEN_BYTE_VERIFIED,
          manifest_sha256: installed.manifest_sha256,
          consistency_status: null, consistency_reasons: null,
          decision_sha256: null, evidence_json: null,
          acquisition_request_id: task.request_id,
          installed_at: now(), updated_at: now() });
        this.db.updateTask({
          ...task, state: ST_SUCCEEDED,
          generation_id: installed.generation_id, stage: "done",
          bytes_done: detailBytes(detail), error: null,
          detail: serValueSorted(detail), updated_at: now() });
        this.db.recordOutcome(task.request_id, JSON.parse(serValueSorted({
          state: "succeeded", task_id: task.task_id,
          generation_id: installed.generation_id,
          manifest_sha256: installed.manifest_sha256 })), now());
        this._removeStage(task.request_id);
        break;
      }
      default:
        throw refused("unknown_stage");
    }
    return this.db.task(task.task_id) ?? { ...task };
  }

  /** service.rs:536-562 — fetch one role artifact into staging; a
   *  staged file still matching its recorded pin is kept (resume),
   *  anything else is fetched fresh. */
  fetchRole(task, role, detail) {
    const stage = this.store.stageDir(task.request_id);
    const dest = join(stage, role.file);
    const recorded = detail?.files?.[role.key];
    const recSha = typeof recorded?.sha256 === "string"
      ? recorded.sha256 : null;
    const recBytes = asU64(recorded?.bytes);
    if (recSha !== null && recBytes !== null &&
        isFile(dest) && BigInt(statSync(dest).size) === recBytes &&
        sha256hex(readFileSync(dest)) === recSha) {
      return; // staged bytes still match the recorded pin — resume
    }
    const started = now();
    const fetched = this.fetcher.fetch(role.url, dest);
    const completed = now();
    detail.files[role.key] = fileInputValue({
      role: role.role, relative_path: role.file, source_url: role.url,
      acquisition_started_at: started,
      acquisition_completed_at: completed,
      last_modified: fetched.head.last_modified,
      etag: fetched.head.etag, bytes: fetched.bytes,
      sha256: fetched.sha256 });
  }

  /** service.rs:564-585 — canonical SnapshotManifest bytes into the
   *  staging area (create + write_all + fsync). */
  writeManifest(task, detail) {
    const files = [];
    for (const role of ROLES) {
      const value = detail?.files?.[role.key];
      if (value === null || value === undefined) {
        throw refused("manifest_incomplete");
      }
      files.push(fileInputFromValue(value));
    }
    const manifest = {
      schema: "x-data-mcp.official-file-input.v1", files };
    const bytes = Buffer.from(serPretty(manifest), "utf8");
    const path = join(this.store.stageDir(task.request_id), MANIFEST_NAME);
    const fd = openSync(path, "w");
    try {
      writeSync(fd, bytes);
      fsyncSync(fd);
    } finally {
      closeSync(fd);
    }
  }

  // ---------- task/request views ----------

  /** service.rs:589-595. */
  task(taskId) {
    try {
      const task = this.db.task(taskId);
      if (task === null) throw refused("task_not_found");
      return taskView(task);
    } catch (e) {
      throw svcErr(e);
    }
  }

  /** service.rs:596-606. */
  request(requestId) {
    try {
      const request = this.db.request(requestId);
      const task = this.db.taskByRequest(requestId);
      const outcome = this.db.outcome(requestId);
      return {
        request_id: requestId,
        bound: request !== null || task !== null || outcome !== null,
        task: task === null ? null : taskView(task),
        terminal_result: outcome,
      };
    } catch (e) {
      throw svcErr(e);
    }
  }

  // ---------- status (service.rs:990-1022) ----------

  syncStatus() {
    try {
      const generations = this.db.generations().map((g) => ({
        generation_id: g.generation_id, state: g.state,
        manifest_sha256: g.manifest_sha256,
        consistency_status: g.consistency_status,
        consistency_reasons: g.consistency_reasons === null ? null
          : (() => { try { return JSON.parse(g.consistency_reasons); }
                     catch { return null; } })(),
        decision_sha256: g.decision_sha256,
        acquisition_request_id: g.acquisition_request_id,
        installed_at: toRfc3339(g.installed_at) }));
      const inflight = this.db.tasksIn([ST_ACCEPTED, ST_RUNNING])
        .map(taskView);
      return {
        active: this.store.active(),
        generations,
        inflight_tasks: inflight,
        generation_contract_version: GENERATION_CONTRACT_VERSION,
        consistency_contract_version:
          `${CONSISTENCY_CONTRACT}+${PROBE_CONTRACT_VERSION}`,
      };
    } catch (e) {
      throw svcErr(e);
    }
  }
}

// ---------- helpers (service.rs:1025-1045) ----------

const isFile = (p) => {
  try { return statSync(p).isFile(); } catch { return false; }
};

// detail_bytes: sum of files.*.bytes as u64 (serde as_u64 semantics).
function detailBytes(detail) {
  const files = detail?.files;
  if (files === null || typeof files !== "object" || Array.isArray(files))
    return 0n;
  let acc = 0n;
  for (const v of Object.values(files)) {
    const b = asU64(v?.bytes);
    if (b !== null) acc += b;
  }
  return acc;
}

const taskView = (task) => ({
  task_id: task.task_id, request_id: task.request_id, kind: task.kind,
  state: task.state, generation_id: task.generation_id,
  stage: task.stage, bytes_done: task.bytes_done, error: task.error,
  detail: task.detail, created_at: toRfc3339(task.created_at),
  updated_at: toRfc3339(task.updated_at),
});

// FileInput stored into detail (snapshot.rs:21-31 shape; serde
// to_value → JSON object). Timestamps kept as Date and emitted by the
// sorted-value serializer as chrono-Z rfc3339 — matching serde's
// DateTime<Utc> serialization of FileInput.
function fileInputValue(f) {
  return {
    role: f.role, relative_path: f.relative_path, source_url: f.source_url,
    acquisition_started_at: serTs(f.acquisition_started_at),
    acquisition_completed_at: serTs(f.acquisition_completed_at),
    last_modified: f.last_modified ?? null, etag: f.etag ?? null,
    bytes: u64json(f.bytes), sha256: f.sha256,
  };
}

// u64 in detail JSON: exact digits; >2^53 keeps bigint and serValueSorted
// emits the literal (serde_json stores u64 exactly — document: a JSON
// parse back may lose >2^53 in f64, which only re-triggers a fetch).
const u64json = (v) =>
  typeof v === "bigint" && v > BigInt(Number.MAX_SAFE_INTEGER) ? v
    : Number(v);

// serde_json::from_value::<FileInput> — deny_unknown_fields + typed.
const FILE_INPUT_FIELDS = [
  "role", "relative_path", "source_url", "acquisition_started_at",
  "acquisition_completed_at", "last_modified", "etag", "bytes", "sha256",
];
const FILE_ROLES = new Set(["base_industry", "day", "gbbq"]);
function fileInputFromValue(v) {
  const bad = (m) => new AcquireServiceError("json", `json: ${m}`);
  if (v === null || typeof v !== "object" || Array.isArray(v))
    throw bad(`invalid type: expected struct FileInput`);
  for (const k of Object.keys(v)) {
    if (!FILE_INPUT_FIELDS.includes(k))
      throw bad(`unknown field \`${k}\``);
  }
  for (const k of ["role", "relative_path", "source_url",
                   "acquisition_started_at", "acquisition_completed_at",
                   "bytes", "sha256"]) {
    if (!(k in v)) throw bad(`missing field \`${k}\``);
  }
  if (!FILE_ROLES.has(v.role))
    throw bad(`unknown variant \`${v.role}\``);
  for (const k of ["relative_path", "source_url", "sha256"]) {
    if (typeof v[k] !== "string") throw bad(`invalid type for \`${k}\``);
  }
  const b = asU64(v.bytes);
  if (b === null) throw bad("invalid type for `bytes`");
  const started = new Date(v.acquisition_started_at);
  const completed = new Date(v.acquisition_completed_at);
  if (Number.isNaN(started.getTime()) ||
      Number.isNaN(completed.getTime())) {
    throw bad("invalid timestamp for FileInput");
  }
  // Re-emit canonical — serde re-serializes DateTime fields (Z form).
  return {
    role: v.role, relative_path: v.relative_path,
    source_url: v.source_url,
    acquisition_started_at: serTs(started),
    acquisition_completed_at: serTs(completed),
    last_modified: v.last_modified ?? null, etag: v.etag ?? null,
    bytes: b, sha256: v.sha256,
  };
}
