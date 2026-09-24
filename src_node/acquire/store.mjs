// Node port of src/acquire/store.rs — durable acquisition state (D3):
// SQLite, not a mutable JSON file. Every mutating call carries a
// caller request_id; the request row is the idempotency registry and
// request_outcomes binds the terminal result. The single async unit
// of work is an acquisition task; every other row is a fact surviving
// process death, reconciled mechanically at open.
//
// Conventions: opens through storage/sqlite_adapter.mjs (pinned
// better-sqlite3, safeIntegers ON — u64 columns arrive as bigint,
// bound verbatim as i64 with the Rust .max(0) clamp on read).
// Rust opens a fresh Connection per op (self.write/read) — mirrored:
// each method opens and closes its own handle.
//
// Time: chrono to_rfc3339() emits +00:00 with subsec only when nonzero;
// Date is ms-precision so `.mmm` is emitted iff ms !== 0. parse_time
// falls back to UNIX_EPOCH verbatim.

import { existsSync } from "node:fs";
import { openDatabase } from "../storage/sqlite_adapter.mjs";

const SCHEMA = `
CREATE TABLE IF NOT EXISTS requests (
  request_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  payload TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS request_outcomes (
  request_id TEXT PRIMARY KEY REFERENCES requests(request_id),
  outcome TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS acquisition_tasks (
  task_id TEXT PRIMARY KEY,
  request_id TEXT NOT NULL UNIQUE REFERENCES requests(request_id),
  kind TEXT NOT NULL,
  state TEXT NOT NULL,
  generation_id TEXT,
  stage TEXT,
  bytes_done INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  detail TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS generations (
  generation_id TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  manifest_sha256 TEXT NOT NULL,
  consistency_status TEXT,
  consistency_reasons TEXT,
  decision_sha256 TEXT,
  evidence_json TEXT,
  acquisition_request_id TEXT,
  installed_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS seen (
  role TEXT PRIMARY KEY,
  etag TEXT,
  last_modified TEXT,
  content_length INTEGER,
  seen_at TEXT NOT NULL
);
`;

// store.rs Error enum: Storage / RequestConflict / Json — verbatim
// Display strings, carried on .kind.
export class AcquireStoreError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = "AcquireStoreError";
    this.kind = kind;
    this.code = kind;
  }
}
const storage = (e) =>
  new AcquireStoreError("storage", `acquire store unavailable: ${e.message}`);
const requestConflict = () => new AcquireStoreError(
  "request_conflict", "request_id already bound to a different command");
const jsonErr = (e) =>
  new AcquireStoreError("json", `json: ${e.message}`);

// chrono DateTime<Utc>::to_rfc3339(): +00:00 offset, subsecond digits
// only when nonzero (AutoSi). Date is ms-precision — emit .mmm when
// ms !== 0, else no fraction.
const toRfc3339 = (d) => {
  const iso = d.toISOString(); // YYYY-MM-DDTHH:MM:SS.mmmZ
  return iso.endsWith(".000Z")
    ? iso.slice(0, -5) + "+00:00"
    : iso.slice(0, -1) + "+00:00";
};

// store.rs:362-365 — unparseable timestamp degrades to UNIX_EPOCH.
const parseTime = (s) => {
  const d = new Date(s);
  return Number.isNaN(d.getTime()) ? new Date(0) : d;
};

const u64 = (v) => (v < 0n ? 0n : v); // i64.max(0) as u64 verbatim

const TASK_COLS =
  "task_id,request_id,kind,state,generation_id,stage,bytes_done,error,detail,created_at,updated_at";
const GEN_COLS =
  "generation_id,state,manifest_sha256,consistency_status,consistency_reasons,decision_sha256,evidence_json,acquisition_request_id,installed_at,updated_at";

const mapTask = (r) => ({
  task_id: r.task_id,
  request_id: r.request_id,
  kind: r.kind,
  state: r.state,
  generation_id: r.generation_id,
  stage: r.stage,
  bytes_done: u64(r.bytes_done),
  error: r.error,
  detail: r.detail,
  created_at: parseTime(r.created_at),
  updated_at: parseTime(r.updated_at),
});

const mapGeneration = (r) => ({
  generation_id: r.generation_id,
  state: r.state,
  manifest_sha256: r.manifest_sha256,
  consistency_status: r.consistency_status,
  consistency_reasons: r.consistency_reasons,
  decision_sha256: r.decision_sha256,
  evidence_json: r.evidence_json,
  acquisition_request_id: r.acquisition_request_id,
  installed_at: parseTime(r.installed_at),
  updated_at: parseTime(r.updated_at),
});

export class Db {
  /** Db::new(path) — deferred open; open() creates+initialises. */
  constructor(path) {
    this.path = path;
  }

  _connect(mode = "readwrite") {
    // store.rs:153-159 — READ_WRITE (fileMustExist via adapter),
    // busy_timeout 250ms.
    try {
      const c = openDatabase(this.path, { mode, busyTimeoutMs: 250 });
      // better-sqlite3 enables foreign_keys by default; rusqlite leaves
      // the SQLite default OFF. The schema's REFERENCES are declarative
      // — Rust relies on app-level ordering, so parity requires OFF.
      c.pragma("foreign_keys=OFF");
      return c;
    } catch (e) {
      throw storage(e);
    }
  }
  _with(fn, mode) {
    const c = this._connect(mode);
    try {
      return fn(c);
    } catch (e) {
      // rusqlite .map_err(storage) equivalent: adapter/driver errors
      // become "acquire store unavailable: ..."; typed store errors
      // (RequestConflict, Json) and genuine bugs pass through.
      if (e instanceof AcquireStoreError) throw e;
      if (e?.name === "StorageError") throw storage(e);
      throw e;
    } finally {
      c.close();
    }
  }

  /** store.rs:126-136 — create-or-open, idempotent schema v1. */
  open() {
    this._with((c) => c.exec(SCHEMA), "create");
  }

  /** store.rs:140-152 — read-only consumer check; a missing database
   *  is an absent store, not an error. */
  existsReadable() {
    if (!existsSync(this.path)) return false;
    return this._with((c) =>
      c.get("SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table' AND name='generations'")
        .n === 1n); // safeIntegers: COUNT(*) arrives as bigint
  }

  /** store.rs:171-200 — insert the request row once; replaying the
   *  same id with same kind+payload returns the existing row
   *  (reused=true); a different payload conflicts. Rust returns
   *  (Request, bool); the port returns {request, reused}. */
  acceptRequest(requestId, kind, payload, now) {
    return this._with((c) => {
      const existing = requestRow(c, requestId);
      if (existing) {
        if (existing.kind === kind && existing.payload === payload) {
          return { request: existing, reused: true };
        }
        throw requestConflict();
      }
      c.run(
        "INSERT INTO requests(request_id,kind,payload,created_at) VALUES(?,?,?,?)",
        requestId, kind, payload, toRfc3339(now));
      return {
        request: { request_id: requestId, kind, payload,
                   created_at: now },
        reused: false,
      };
    });
  }

  request(requestId) {
    return this._with((c) => requestRow(c, requestId));
  }

  /** store.rs:204-218 — INSERT OR IGNORE: the first bound outcome wins. */
  bindOutcome(requestId, outcome, now) {
    return this._with((c) => {
      c.run(
        "INSERT OR IGNORE INTO request_outcomes(request_id,outcome,created_at) VALUES(?,?,?)",
        requestId, outcome, toRfc3339(now));
    });
  }

  /** store.rs:219-233 — outcome<T>: stored JSON text parsed back. */
  outcome(requestId) {
    return this._with((c) => {
      const row = c.get(
        "SELECT outcome FROM request_outcomes WHERE request_id=?",
        requestId);
      if (!row) return null;
      try {
        return JSON.parse(row.outcome);
      } catch (e) {
        throw jsonErr(e);
      }
    });
  }

  /** store.rs:234-241 — record_outcome<T: Serialize>. */
  recordOutcome(requestId, outcome, now) {
    let serialized;
    try {
      serialized = JSON.stringify(outcome);
    } catch (e) {
      throw jsonErr(e);
    }
    return this.bindOutcome(requestId, serialized, now);
  }

  /** store.rs:243-252 — task: {task_id,request_id,kind,state,
   *  generation_id?,stage?,bytes_done,error?,detail?,created_at,
   *  updated_at} (Date timestamps, u64 as bigint|number). */
  createTask(task) {
    return this._with((c) => {
      c.run(
        `INSERT INTO acquisition_tasks(${TASK_COLS}) VALUES(?,?,?,?,?,?,?,?,?,?,?)`,
        task.task_id, task.request_id, task.kind, task.state,
        task.generation_id ?? null, task.stage ?? null,
        BigInt(task.bytes_done), task.error ?? null, task.detail ?? null,
        toRfc3339(task.created_at), toRfc3339(task.updated_at));
    });
  }

  task(taskId) {
    return this._with((c) => taskRow(c, "task_id", taskId));
  }
  taskByRequest(requestId) {
    return this._with((c) => taskRow(c, "request_id", requestId));
  }
  /** store.rs:259-269 — state IN (marks) ORDER BY created_at. */
  tasksIn(states) {
    return this._with((c) => {
      const marks = states.map(() => "?").join(",");
      return c.query(
        `SELECT ${TASK_COLS} FROM acquisition_tasks WHERE state IN (${marks}) ORDER BY created_at`,
        ...states).map(mapTask);
    });
  }

  /** store.rs:272-281 — whole-row transition write: a crash can never
   *  leave a state/stage/bytes mix the reconciler has to guess. */
  updateTask(task) {
    return this._with((c) => {
      c.run(
        "UPDATE acquisition_tasks SET state=?,generation_id=?,stage=?,bytes_done=?,error=?,detail=?,updated_at=? WHERE task_id=?",
        task.state, task.generation_id ?? null,
        task.stage ?? null, BigInt(task.bytes_done), task.error ?? null,
        task.detail ?? null, toRfc3339(task.updated_at), task.task_id);
    });
  }

  /** store.rs:283-292 — INSERT .. ON CONFLICT(generation_id) DO UPDATE. */
  upsertGeneration(generation) {
    return this._with((c) => {
      c.run(
        `INSERT INTO generations(${GEN_COLS}) VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(generation_id) DO UPDATE SET state=?,manifest_sha256=?,consistency_status=?,consistency_reasons=?,decision_sha256=?,evidence_json=?,acquisition_request_id=?,installed_at=?,updated_at=?`,
        generation.generation_id, generation.state,
        generation.manifest_sha256, generation.consistency_status ?? null,
        generation.consistency_reasons ?? null,
        generation.decision_sha256 ?? null, generation.evidence_json ?? null,
        generation.acquisition_request_id ?? null,
        toRfc3339(generation.installed_at), toRfc3339(generation.updated_at),
        // ON CONFLICT args — the Rust ?2..?10 reuse, rebound positionally
        generation.state, generation.manifest_sha256,
        generation.consistency_status ?? null,
        generation.consistency_reasons ?? null,
        generation.decision_sha256 ?? null, generation.evidence_json ?? null,
        generation.acquisition_request_id ?? null,
        toRfc3339(generation.installed_at), toRfc3339(generation.updated_at));
    });
  }

  generation(generationId) {
    return this._with((c) => {
      const r = c.get(
        `SELECT ${GEN_COLS} FROM generations WHERE generation_id=?`,
        generationId);
      return r ? mapGeneration(r) : null;
    });
  }
  /** store.rs:306-317 — lifecycle states, ORDER BY installed_at. */
  generationsIn(states) {
    return this._with((c) => {
      const marks = states.map(() => "?").join(",");
      return c.query(
        `SELECT ${GEN_COLS} FROM generations WHERE state IN (${marks}) ORDER BY installed_at`,
        ...states).map(mapGeneration);
    });
  }
  generations() {
    return this._with((c) =>
      c.query(`SELECT ${GEN_COLS} FROM generations ORDER BY installed_at`)
        .map(mapGeneration));
  }

  /** store.rs:328-346. */
  seen(role) {
    return this._with((c) => {
      const r = c.get(
        "SELECT role,etag,last_modified,content_length,seen_at FROM seen WHERE role=?",
        role);
      return r ? {
        role: r.role,
        etag: r.etag,
        last_modified: r.last_modified,
        content_length: r.content_length === null
          ? null : u64(r.content_length),
        seen_at: parseTime(r.seen_at),
      } : null;
    });
  }
  /** store.rs:347-356 — upsert on role. */
  recordSeen(seen) {
    return this._with((c) => {
      c.run(
        "INSERT INTO seen(role,etag,last_modified,content_length,seen_at) VALUES(?,?,?,?,?) ON CONFLICT(role) DO UPDATE SET etag=?,last_modified=?,content_length=?,seen_at=?",
        seen.role, seen.etag ?? null, seen.last_modified ?? null,
        seen.content_length == null ? null : BigInt(seen.content_length),
        toRfc3339(seen.seen_at),
        // ON CONFLICT args — the Rust ?2..?5 reuse, rebound positionally
        seen.etag ?? null, seen.last_modified ?? null,
        seen.content_length == null ? null : BigInt(seen.content_length),
        toRfc3339(seen.seen_at));
    });
  }
}

const requestRow = (c, requestId) => {
  const r = c.get(
    "SELECT request_id,kind,payload,created_at FROM requests WHERE request_id=?",
    requestId);
  return r ? {
    request_id: r.request_id,
    kind: r.kind,
    payload: r.payload,
    created_at: parseTime(r.created_at),
  } : null;
};

const taskRow = (c, column, key) => {
  const r = c.get(
    `SELECT ${TASK_COLS} FROM acquisition_tasks WHERE ${column}=?`,
    key);
  return r ? mapTask(r) : null;
};
