// NM-040 P3 — read-only task/request repository slice.
//
// Opens only through the NM-039 schema gate + NM-038 readonly adapter:
// unsupported schema, tampered chain, or corrupt bytes fail closed
// before any row is read. Typed row mapping preserves nullable fields
// and i64 counters as bigint (safeIntegers is the adapter default).
// No writes, no CAS/revision/occupancy, no worker behavior.
//
// Owner scoping is an explicit repository input — callers pass
// owner_run_id; absent owner means "all owners", never silently
// broadened. Pagination is keyset on (created_at, task_id), matching
// Rust's ORDER BY created_at,task_id claim ordering. All values are
// bound parameters — no SQL interpolation.

import { createHash } from "node:crypto";
import { gateDatabase } from "./schema_gate.mjs";
import { openDatabase, StorageError } from "./sqlite_adapter.mjs";

const sha256 = (s) => createHash("sha256").update(s, "utf8").digest("hex");

const TASK_COLS = `task_id,command_key_json,status,state_revision,accepted_run_id,
  owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,
  retry_of,item_count,items_pending,items_running,items_finished,
  items_succeeded,evidence_digest`;
const REQ_COLS = `request_id,command_json,command_digest,outcome_kind,task_id,terminal_json`;

const MAX_LIMIT = 10_000;

export class TaskRepository {
  /** path -> gated readonly repo. owner_run_id scopes listTasks. */
  static open(path, { owner = null } = {}) {
    if (owner !== null && (typeof owner !== "string" || owner === "" || !owner.isWellFormed())) {
      throw new StorageError("invalid_input", "task_repository: owner must be a wellformed non-empty string or null");
    }
    const capability = gateDatabase(path); // throws typed error on failure
    const db = openDatabase(path);         // readonly, fileMustExist
    return new TaskRepository(db, capability, owner);
  }
  constructor(db, capability, owner) {
    this._db = db;
    this.capability = capability;
    this.owner = owner;
  }
  close() { this._db.close(); }

  /** One request row by id, or null. command_digest verified verbatim
   *  (Rust read_request) — mismatch is an inconsistency, fail closed. */
  getRequest(requestId) {
    if (typeof requestId !== "string" || requestId === "") {
      throw new StorageError("invalid_input", "task_repository: request_id must be a non-empty string");
    }
    const r = this._db.get(
      `SELECT ${REQ_COLS} FROM requests WHERE request_id=?`, requestId);
    if (!r) return null;
    if (sha256(r.command_json) !== r.command_digest) {
      throw new StorageError("inconsistent", "command digest mismatch");
    }
    return r;
  }

  /** One task row by id, or null. */
  getTask(taskId) {
    if (typeof taskId !== "string" || taskId === "") {
      throw new StorageError("invalid_input", "task_repository: task_id must be a non-empty string");
    }
    return this._db.get(`SELECT ${TASK_COLS} FROM tasks WHERE task_id=?`, taskId) ?? null;
  }

  /** Task bound to a request id, or null when unbound/absent. */
  getTaskForRequest(requestId) {
    const req = this.getRequest(requestId);
    if (!req || req.task_id === null) return null;
    return this.getTask(req.task_id);
  }

  /** Reverse binding: request_id owning a task, or null. */
  getRequestForTask(taskId) {
    if (typeof taskId !== "string" || taskId === "") {
      throw new StorageError("invalid_input", "task_repository: task_id must be a non-empty string");
    }
    const r = this._db.get("SELECT request_id FROM requests WHERE task_id=?", taskId);
    return r ? r.request_id : null;
  }

  /** Keyset-paginated task listing, deterministic order
   *  (created_at, task_id) — Rust's owner-claim ordering.
   *  opts.limit: positive integer <= 10000 (required, explicit bound).
   *  opts.cursor: {created_at, task_id} — rows strictly after it.
   *  Owner scoping comes from the repo's `owner` input; owner=null
   *  lists all owners (explicit opt-out, not silent broadening). */
  listTasks({ limit, cursor = null } = {}) {
    if (!Number.isInteger(limit) || limit < 1 || limit > MAX_LIMIT) {
      throw new StorageError("invalid_input",
        `task_repository: limit must be an integer 1..${MAX_LIMIT}`);
    }
    if (cursor !== null) {
      if (typeof cursor !== "object" ||
          typeof cursor.created_at !== "string" || typeof cursor.task_id !== "string") {
        throw new StorageError("invalid_input",
          "task_repository: cursor must be {created_at, task_id} strings");
      }
    }
    const where = [];
    const params = [];
    if (this.owner !== null) { where.push("owner_run_id=?"); params.push(this.owner); }
    if (cursor) {
      where.push("(created_at>? OR (created_at=? AND task_id>?))");
      params.push(cursor.created_at, cursor.created_at, cursor.task_id);
    }
    const sql = `SELECT ${TASK_COLS} FROM tasks` +
      (where.length ? ` WHERE ${where.join(" AND ")}` : "") +
      ` ORDER BY created_at, task_id LIMIT ?`;
    params.push(limit + 1); // fetch one extra to expose hasMore
    const rows = this._db.query(sql, ...params);
    const hasMore = rows.length > limit;
    const items = hasMore ? rows.slice(0, limit) : rows;
    const last = items[items.length - 1];
    return {
      items,
      hasMore,
      nextCursor: hasMore && last
        ? { created_at: last.created_at, task_id: last.task_id } : null,
    };
  }
}
