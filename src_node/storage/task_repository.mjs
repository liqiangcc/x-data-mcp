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

// ---------------------------------------------------------------------
// NM-042 — atomic task admission + scope occupancy (accept_tx port).
// One transaction: replay/conflict -> orphan + integrity + digest gates
// -> scope conflict check -> task/items/scopes/occupancy/request rows.
// The caller supplies an already-validated admission plan; business
// selection/resolve and retry handling are outside this slice.

const TERMINAL_STATUSES = ["succeeded", "partially_failed", "failed", "cancelled", "interrupted"];
const TERMINAL_SQL = TERMINAL_STATUSES.map((s) => `'${s}'`).join(",");

const wellformedStr = (v) => typeof v === "string" && v.isWellFormed();

function validatePlan(plan) {
  const bad = (m) => { throw new StorageError("invalid_input", `admission plan: ${m}`); };
  if (plan === null || typeof plan !== "object") bad("must be an object");
  const {
    requestId, commandJson, executionKeyJson, workJson,
    items, scopes, runId, retryOf = null, createdAt,
  } = plan;
  if (!wellformedStr(requestId) || requestId.trim() === "") bad("empty request_id");
  for (const [n, v] of [["commandJson", commandJson], ["executionKeyJson", executionKeyJson],
                        ["workJson", workJson]]) {
    if (!wellformedStr(v)) bad(`${n} must be wellformed JSON text`);
    try { JSON.parse(v); } catch { bad(`${n} is not valid JSON`); }
  }
  if (!Array.isArray(items)) bad("items must be an array");
  const seen = new Set();
  for (const i of items) {
    for (const [n, v] of [["item_id", i?.item_id], ["target_json", i?.target_json],
                          ["progress_unit", i?.progress_unit], ["frozen_inputs_json", i?.frozen_inputs_json],
                          ["scope_keys_json", i?.scope_keys_json]]) {
      if (!wellformedStr(v) || v === "") bad(`item ${n} must be a wellformed non-empty string`);
    }
    try { JSON.parse(i.target_json); JSON.parse(i.frozen_inputs_json); JSON.parse(i.scope_keys_json); }
    catch { bad(`item ${i.item_id} has invalid JSON field`); }
    if (seen.has(i.item_id)) bad(`duplicate item_id ${i.item_id}`);
    seen.add(i.item_id);
  }
  if (!Array.isArray(scopes)) bad("scopes must be an array");
  const seenScopes = new Set();
  for (const s of scopes) {
    if (!wellformedStr(s) || s === "") bad("scope_key must be a wellformed non-empty string");
    if (seenScopes.has(s)) bad(`duplicate scope_key ${s}`);
    seenScopes.add(s);
  }
  if (!wellformedStr(runId) || runId === "") bad("runId must be a wellformed non-empty string");
  if (retryOf !== null && !wellformedStr(retryOf)) bad("retryOf must be a string or null");
  if (!wellformedStr(createdAt)) bad("createdAt (RFC3339) must be supplied by the caller");
  // evidence_digest comes from the frozen work object, as in Rust
  // work.evidence_digest() — a JSON field, not a parameter.
  const work = JSON.parse(workJson);
  const evidenceDigest = (work && typeof work === "object" && !Array.isArray(work) &&
    wellformedStr(work.evidence_digest)) ? work.evidence_digest : null;
  return { requestId, commandJson, executionKeyJson, workJson, items, scopes, runId, retryOf, createdAt, evidenceDigest };
}

export class TaskAdmission {
  /** Gated writable repo for admission. mode readwrite|create explicit;
   *  schema gate runs before the writable open (NM-041 R1 ordering). */
  static open(path, { mode } = {}) {
    if (mode !== "readwrite" && mode !== "create") {
      throw new StorageError("invalid_input",
        "task_admission: explicit mode readwrite|create required");
    }
    const capability = gateDatabase(path);
    return new TaskAdmission(openDatabase(path, { mode }), capability);
  }
  constructor(db, capability) {
    this._db = db;
    this.capability = capability;
  }
  close() { this._db.close(); }

  /** Atomic admission. plan fields validated; returns
   *  {reused, record|task_id}. Any failure rolls back all rows. */
  admit(plan) {
    const p = validatePlan(plan);
    return this._db.transaction(() => {
      const db = this._db;
      // 1. replay/conflict on the request id (Rust read_request order)
      const existing = db.get(
        "SELECT command_json,command_digest FROM requests WHERE request_id=?",
        p.requestId);
      if (existing) {
        if (sha256(existing.command_json) !== existing.command_digest) {
          throw new StorageError("inconsistent", "command digest mismatch");
        }
        if (existing.command_json !== p.commandJson) {
          throw new StorageError("conflict", "REQUEST_CONFLICT");
        }
        return { reused: true, record: db.get(
          "SELECT request_id,command_json,command_digest,outcome_kind,task_id,terminal_json FROM requests WHERE request_id=?",
          p.requestId) };
      }
      // 2. orphan unbound-task check
      if (db.get(`SELECT EXISTS(SELECT 1 FROM tasks t LEFT JOIN requests r
        ON r.task_id=t.task_id WHERE r.request_id IS NULL) x`).x !== 0n) {
        throw new StorageError("inconsistent", "unbound execution record");
      }
      // 3. registry integrity gates (accept_tx, verbatim order/text)
      const gates = [
        [`SELECT EXISTS(SELECT 1 FROM requests r LEFT JOIN tasks t ON t.task_id=r.task_id
          WHERE r.task_id IS NOT NULL AND t.task_id IS NULL) x`,
         "request bound to missing task"],
        [`SELECT EXISTS(SELECT 1 FROM tasks t LEFT JOIN task_items i ON i.task_id=t.task_id
          GROUP BY t.task_id HAVING count(i.item_id)!=t.item_count
          OR sum(i.stage='pending')!=t.items_pending
          OR sum(i.stage='running')!=t.items_running
          OR sum(i.stage='finished')!=t.items_finished) x`,
         "task item rows diverge from counters"],
        [`SELECT EXISTS(SELECT 1 FROM tasks t WHERE t.status NOT IN (${TERMINAL_SQL})
          AND (EXISTS(SELECT 1 FROM task_scopes s WHERE s.task_id=t.task_id
            AND NOT EXISTS(SELECT 1 FROM scope_occupancy o WHERE o.task_id=t.task_id
              AND o.scope_key=s.scope_key))
           OR EXISTS(SELECT 1 FROM scope_occupancy o WHERE o.task_id=t.task_id
            AND NOT EXISTS(SELECT 1 FROM task_scopes s WHERE s.task_id=t.task_id
              AND s.scope_key=o.scope_key)))) x`,
         "active occupancy missing/inconsistent"],
        [`SELECT EXISTS(SELECT 1 FROM tasks t WHERE t.status IN (${TERMINAL_SQL})
          AND (t.result_json IS NULL
           OR EXISTS(SELECT 1 FROM scope_occupancy o WHERE o.task_id=t.task_id)
           OR EXISTS(SELECT 1 FROM task_items i WHERE i.task_id=t.task_id AND i.stage='running')
           OR (t.status='succeeded' AND NOT(t.items_finished=t.item_count AND t.items_succeeded=t.item_count))
           OR (t.status='failed' AND NOT(t.items_finished=t.item_count AND t.items_succeeded=0))
           OR (t.status='partially_failed' AND NOT(t.items_finished=t.item_count AND t.items_succeeded>0 AND t.items_succeeded<t.item_count)))) x`,
         "terminal execution/claims inconsistent"],
        [`SELECT EXISTS(SELECT 1 FROM tasks t WHERE
          (t.status='running' AND t.owner_run_id IS NULL)
          OR (t.status='pending' AND (t.owner_run_id IS NOT NULL
            OR EXISTS(SELECT 1 FROM task_items i WHERE i.task_id=t.task_id AND i.stage!='pending')))
          OR (t.owner_run_id IS NULL
            AND EXISTS(SELECT 1 FROM task_items i WHERE i.task_id=t.task_id AND i.stage!='pending'))) x`,
         "execution owner/stage missing or inconsistent"],
      ];
      for (const [sql, why] of gates) {
        if (db.get(sql).x !== 0n) {
          throw new StorageError("inconsistent", why);
        }
      }
      // 4. digest gate over all stored requests
      for (const r of db.query("SELECT command_json,command_digest FROM requests")) {
        if (sha256(r.command_json) !== r.command_digest) {
          throw new StorageError("inconsistent", "command digest mismatch");
        }
      }
      // 5. scope conflicts — collected in plan order (Rust work.scopes order)
      const conflicts = [];
      for (const scope of p.scopes) {
        if (db.get("SELECT EXISTS(SELECT 1 FROM scope_occupancy WHERE scope_key=?) x", scope).x !== 0n) {
          conflicts.push(scope);
        }
      }
      if (conflicts.length) {
        throw new StorageError("range_conflict",
          `RANGE_CONFLICT: ${JSON.stringify(conflicts)}`);
      }
      // 6. inserts: task (pending/rev0/owner NULL) -> items -> scopes ->
      // occupancy -> request binding. task_id from the same randomblob
      // expression Rust uses.
      const taskId = db.get(
        "SELECT 'task-' || lower(hex(randomblob(16))) id").id;
      db.run(`INSERT INTO tasks(task_id,command_key_json,status,state_revision,
        accepted_run_id,owner_run_id,created_at,last_progress_at,
        frozen_work_json,result_json,retry_of,item_count,items_pending,
        evidence_digest) VALUES(?,?,'pending',0,?,NULL,?,?,?,NULL,?,?,?,?)`,
        taskId, p.executionKeyJson, p.runId, p.createdAt, p.createdAt,
        p.workJson, p.retryOf, BigInt(p.items.length), BigInt(p.items.length),
        p.evidenceDigest);
      for (const i of p.items) {
        db.run(`INSERT INTO task_items VALUES(?,?,?,?,?,?,'pending',NULL)`,
          taskId, i.item_id, i.target_json, i.progress_unit,
          i.frozen_inputs_json, i.scope_keys_json);
      }
      for (const scope of p.scopes) {
        db.run("INSERT INTO task_scopes VALUES(?,?)", taskId, scope);
      }
      for (const scope of p.scopes) {
        db.run("INSERT INTO scope_occupancy VALUES(?,?,0)", scope, taskId);
      }
      db.run(`INSERT INTO requests VALUES(?,?,?,'task',?,NULL)`,
        p.requestId, p.commandJson, sha256(p.commandJson), taskId);
      return { reused: false, task_id: taskId,
        record: db.get(
          "SELECT request_id,command_json,command_digest,outcome_kind,task_id,terminal_json FROM requests WHERE request_id=?",
          p.requestId) };
    });
  }
}
