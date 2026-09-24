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
// NM-048: task-link validators (verification + inspection registries).
// Deferred-call imports — safe under the ESM cycle with
// verification_storage.mjs (which imports canonEq/StorageError).
import {
  inspectionTaskLink, verifyTaskLink, readReceipt, runtimeRegistration,
  claimWithRuntimeGuard, validateAbsentRequest, validateTaskRequest,
  validateTerminalStop, validateTerminalInspection, readAudit,
} from "./verification_storage.mjs";

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
  // RetryGuard (accept_tx retry branch): retry carries the original
  // binding; when present it IS the source of retry_of and the stored
  // command key (the original's, not retry_task's).
  const retry = plan.retry ?? null;
  if (retry !== null) {
    if (typeof retry !== "object" ||
        !wellformedStr(retry.originalTaskId) || retry.originalTaskId === "") {
      bad("retry.originalTaskId must be a wellformed non-empty string");
    }
    if (typeof retry.expectedRevision !== "bigint" || retry.expectedRevision < 0n) {
      bad("retry.expectedRevision must be a non-negative bigint");
    }
    if (!Array.isArray(retry.selectedItems) ||
        retry.selectedItems.some((x) => !wellformedStr(x) || x === "") ||
        new Set(retry.selectedItems).size !== retry.selectedItems.length) {
      bad("retry.selectedItems must be unique wellformed non-empty strings");
    }
    if (retryOf !== retry.originalTaskId) {
      bad("retryOf must equal retry.originalTaskId when retry is present");
    }
  }
  if (!wellformedStr(createdAt)) bad("createdAt (RFC3339) must be supplied by the caller");
  // evidence_digest comes from the frozen work object, as in Rust
  // work.evidence_digest() — a JSON field, not a parameter.
  const work = JSON.parse(workJson);
  const evidenceDigest = (work && typeof work === "object" && !Array.isArray(work) &&
    wellformedStr(work.evidence_digest)) ? work.evidence_digest : null;
  return { requestId, commandJson, executionKeyJson, workJson, items, scopes, runId, retryOf, retry, createdAt, evidenceDigest };
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
        // Identical command bytes alone do not authorize reuse: the
        // durable task binding must match the supplied plan — execution
        // key, run, retry, frozen work, items, ordered scopes.
        const bound = db.get(
          "SELECT t.task_id,command_key_json,accepted_run_id,retry_of,frozen_work_json FROM tasks t JOIN requests r ON r.task_id=t.task_id WHERE r.request_id=?",
          p.requestId);
        if (!bound) {
          throw new StorageError("inconsistent", "bound execution record missing");
        }
        const div = [];
        // For a retry the durable command key is the ORIGINAL's —
        // replay must compare against that, not the retry_task key.
        let expectedKeyJson = p.executionKeyJson;
        if (p.retry !== null) {
          const orig = db.get("SELECT command_key_json FROM tasks WHERE task_id=?",
            p.retry.originalTaskId);
          if (orig) expectedKeyJson = orig.command_key_json;
        }
        if (bound.command_key_json !== expectedKeyJson) div.push("command_key_json");
        if (bound.accepted_run_id !== p.runId) div.push("accepted_run_id");
        if (bound.retry_of !== p.retryOf) div.push("retry_of");
        if (bound.frozen_work_json !== p.workJson) div.push("frozen_work_json");
        const storedItems = db.query(
          "SELECT item_id,target_json,progress_unit,frozen_inputs_json,scope_keys_json FROM task_items WHERE task_id=? ORDER BY item_id",
          bound.task_id);
        const planItems = [...p.items].sort((a, b) => a.item_id < b.item_id ? -1 : 1);
        if (storedItems.length !== planItems.length ||
            storedItems.some((r, i) => {
              const q = planItems[i];
              return r.item_id !== q.item_id || r.target_json !== q.target_json ||
                r.progress_unit !== q.progress_unit || r.frozen_inputs_json !== q.frozen_inputs_json ||
                r.scope_keys_json !== q.scope_keys_json;
            })) div.push("items");
        const storedScopes = db.query(
          "SELECT scope_key FROM task_scopes WHERE task_id=? ORDER BY scope_key",
          bound.task_id).map((r) => r.scope_key);
        const planScopes = [...p.scopes].sort();
        if (storedScopes.length !== planScopes.length ||
            storedScopes.some((s, i) => s !== planScopes[i])) div.push("scopes");
        if (div.length) {
          throw new StorageError("inconsistent",
            `request binding diverges from plan: ${div.join(",")}`);
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
      // 4b. RetryGuard (accept_tx retry branch) — original exists ->
      // stored revision -> command intent+key -> request binding ->
      // item selection/spec/outcome -> count. Stored execution key is
      // the ORIGINAL's command_key_json.
      let storedKeyJson = p.executionKeyJson;
      if (p.retry !== null) {
        const old = this._readBoundTaskValidated(p.retry.originalTaskId);
        if (!old) throw new StorageError("task_not_found", "TASK_NOT_FOUND");
        // Rust InvalidRetrySelection is a unit variant — verbatim
        // INVALID_RETRY_SELECTION, no detail payload.
        const irs = () => { throw new StorageError("invalid_retry_selection", "INVALID_RETRY_SELECTION"); };
        if (old.task.state_revision !== p.retry.expectedRevision) {
          throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
        }
        const command = JSON.parse(p.commandJson);
        const oldKey = JSON.parse(old.task.command_key_json);
        const selected = [...p.retry.selectedItems].sort();
        // Rust compares the intent Value wholesale: exactly
        // {original_task_id, selected_items} — extra keys reject.
        if (!canonEq(command?.intent,
          { original_task_id: p.retry.originalTaskId, selected_items: selected })) {
          irs();
        }
        if (command?.key?.operation !== "retry_task" ||
            command.key.dataset !== oldKey.dataset ||
            command.key.contract_version !== oldKey.contract_version) {
          irs();
        }
        const boundReq = db.get(
          "SELECT request_id FROM requests WHERE task_id=?", p.retry.originalTaskId);
        if (boundReq && boundReq.request_id === p.requestId) {
          throw new StorageError("conflict", "REQUEST_CONFLICT");
        }
        const workItems = JSON.parse(p.workJson).items;
        for (const w of workItems) {
          const spec = old.items.find((i) => i.item_id === w.item_id);
          if (!p.retry.selectedItems.includes(w.item_id) || !spec ||
              !canonEq(spec.target, w.target) ||
              spec.progress_unit !== w.progress_unit ||
              !canonEq(spec.frozen_inputs, w.frozen_inputs) ||
              spec.scope_keys.size !== (w.scope_keys?.length ?? -1) ||
              ![...spec.scope_keys].every((k) => w.scope_keys.includes(k)) ||
              (spec.outcome !== null && spec.outcome.success === true)) {
            irs();
          }
        }
        if (workItems.length !== p.retry.selectedItems.length) {
          irs();
        }
        storedKeyJson = old.task.command_key_json;
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
        taskId, storedKeyJson, p.runId, p.createdAt, p.createdAt,
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

// ---------------------------------------------------------------------
// NM-043 — atomic task claim CAS (decide(Change::Claim) + transition_tx).
// One transaction: task existence -> pending && owner-NULL guard ->
// guarded UPDATE ... WHERE task_id=? AND state_revision=? (in-DB CAS;
// changed_rows!=1 -> RevisionConflict). Item rows and occupancy rows
// are not touched by a claim (Rust stamps claims at read time from the
// live revision). Counters recount verbatim.

const RECOUNT_COUNTERS =
  ",item_count=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id)" +
  ",items_pending=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='pending')" +
  ",items_running=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='running')" +
  ",items_finished=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='finished')" +
  ",items_succeeded=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='finished' AND json_extract(i.outcome_json,'$.success')=1)";

Object.assign(TaskAdmission.prototype, {
  /** Claim a pending task for runId under expectedRevision.
   *  Returns the new task row. Typed errors: invalid_input (empty
   *  run/now), task_not_found, revision_conflict (stale/wrong
   *  status/owned/CAS miss). No mutation on any failure. */
  claim(taskId, expectedRevision, runId, now) {
    if (!wellformedStr(taskId) || taskId === "") {
      throw new StorageError("invalid_input", "task_admission: task_id must be a non-empty string");
    }
    if (typeof expectedRevision !== "bigint" || expectedRevision < 0n) {
      throw new StorageError("invalid_input", "task_admission: expectedRevision must be a non-negative bigint");
    }
    if (!wellformedStr(runId) || runId.trim() === "") {
      throw new StorageError("invalid_input", "empty run identity");
    }
    if (!wellformedStr(now)) {
      throw new StorageError("invalid_input", "task_admission: now must be an RFC3339 string");
    }
    return this._db.transaction(() => {
      const t = this._db.get(
        "SELECT status,state_revision,owner_run_id,accepted_run_id FROM tasks WHERE task_id=?",
        taskId);
      if (!t) throw new StorageError("task_not_found", "TASK_NOT_FOUND");
      // decide order: stored revision -> terminal -> visible-status
      // (run identity) -> pending+owner-NULL arm -> CAS.
      if (t.state_revision !== expectedRevision) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      if (TERMINAL_STATUSES.includes(t.status)) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      if (t.accepted_run_id !== runId ||
          (t.owner_run_id !== null && t.owner_run_id !== runId)) {
        throw new StorageError("needs_verification",
          "ACCEPTANCE_NEEDS_VERIFICATION: old run or unresolved execution");
      }
      if (t.status !== "pending" || t.owner_run_id !== null) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      const next = expectedRevision + 1n;
      const res = this._db.run(
        `UPDATE tasks SET status='running',state_revision=?,owner_run_id=?,
         last_progress_at=?,result_json=result_json${RECOUNT_COUNTERS}
         WHERE task_id=? AND state_revision=?`,
        next, runId, now, taskId, expectedRevision);
      if (res.changes !== 1) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      return this._db.get("SELECT * FROM tasks WHERE task_id=?", taskId);
    });
  },
});

// ---------------------------------------------------------------------
// NM-044 — atomic item start/finish transitions (decide StartItem /
// FinishItem + write_item_row + counter_delta + execution-start marker).
// Same transaction gate order as claim: exists -> stored revision ->
// terminal -> run-identity (visible_status) -> decide arm -> guarded
// task-row CAS.

const COUNTER_DELTA = {
  start: ",items_pending=items_pending-1,items_running=items_running+1",
  finish_ok: ",items_running=items_running-1,items_finished=items_finished+1,items_succeeded=items_succeeded+1",
  finish_fail: ",items_running=items_running-1,items_finished=items_finished+1",
};

Object.assign(TaskAdmission.prototype, {
  /** Shared pre-arm gates for item transitions (decide order). */
  _transitionGate(taskId, expectedRevision, runId, now) {
    if (!wellformedStr(taskId) || taskId === "") {
      throw new StorageError("invalid_input", "task_admission: task_id must be a non-empty string");
    }
    if (typeof expectedRevision !== "bigint" || expectedRevision < 0n) {
      throw new StorageError("invalid_input", "task_admission: expectedRevision must be a non-negative bigint");
    }
    if (!wellformedStr(runId) || runId.trim() === "") {
      throw new StorageError("invalid_input", "empty run identity");
    }
    if (!wellformedStr(now)) {
      throw new StorageError("invalid_input", "task_admission: now must be an RFC3339 string");
    }
    const t = this._db.get(
      "SELECT status,state_revision,owner_run_id,accepted_run_id FROM tasks WHERE task_id=?",
      taskId);
    if (!t) throw new StorageError("task_not_found", "TASK_NOT_FOUND");
    if (t.state_revision !== expectedRevision ||
        TERMINAL_STATUSES.includes(t.status)) {
      throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
    }
    if (t.accepted_run_id !== runId ||
        (t.owner_run_id !== null && t.owner_run_id !== runId)) {
      throw new StorageError("needs_verification",
        "ACCEPTANCE_NEEDS_VERIFICATION: old run or unresolved execution");
    }
    return t;
  },

  /** decide(Change::StartItem): running+owned task, no other item
   *  running, named item pending -> stage='running' + counter delta +
   *  execution-start marker. Returns the new task row. */
  startItem(taskId, expectedRevision, runId, itemId, now) {
    if (!wellformedStr(itemId) || itemId === "") {
      throw new StorageError("invalid_input", "task_admission: item_id must be a non-empty string");
    }
    return this._db.transaction(() => {
      const t = this._transitionGate(taskId, expectedRevision, runId, now);
      // StartItem arm: status must be running AND owned by run
      if (t.status !== "running" || t.owner_run_id !== runId) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      if (this._db.get(
        "SELECT EXISTS(SELECT 1 FROM task_items WHERE task_id=? AND stage='running') x",
        taskId).x !== 0n) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      const item = this._db.get(
        "SELECT stage FROM task_items WHERE task_id=? AND item_id=?",
        taskId, itemId);
      if (!item || item.stage !== "pending") {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      const next = expectedRevision + 1n;
      // item row first (Rust writes items before the guarded task row)
      this._db.run(
        "UPDATE task_items SET stage='running',outcome_json=NULL WHERE task_id=? AND item_id=?",
        taskId, itemId);
      const res = this._db.run(
        `UPDATE tasks SET status='running',state_revision=?,owner_run_id=?,
         last_progress_at=?,result_json=result_json${COUNTER_DELTA.start}
         WHERE task_id=? AND state_revision=?`,
        next, runId, now, taskId, expectedRevision);
      if (res.changes !== 1) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      this._db.run(
        "INSERT INTO task_item_execution_starts VALUES(?,?,?,?,?)",
        taskId, itemId, runId, next, now);
      return this._db.get("SELECT * FROM tasks WHERE task_id=?", taskId);
    });
  },

  /** decide(Change::FinishItem): owner-bound (any non-terminal status,
   *  incl. cancel_requested); item running -> finished + outcome_json.
   *  outcome: {success, error?, evidence?} — serde shape verbatim.
   *  success with error is InvalidCommand ("success with error"). */
  finishItem(taskId, expectedRevision, runId, itemId, outcome, now) {
    if (!wellformedStr(itemId) || itemId === "") {
      throw new StorageError("invalid_input", "task_admission: item_id must be a non-empty string");
    }
    // Serde-canonical ItemOutcome: closed struct — only success /
    // error / evidence keys, order success->error->evidence, error
    // always present (null when absent), evidence omitted when absent.
    if (outcome === null || typeof outcome !== "object" || Array.isArray(outcome)) {
      throw new StorageError("invalid_input", "task_admission: outcome must be {success:boolean,...}");
    }
    for (const k of Object.keys(outcome)) {
      if (k !== "success" && k !== "error" && k !== "evidence") {
        throw new StorageError("invalid_input", `task_admission: unknown outcome key ${k}`);
      }
    }
    if (typeof outcome.success !== "boolean") {
      throw new StorageError("invalid_input", "task_admission: outcome.success must be boolean");
    }
    if (outcome.error !== undefined && outcome.error !== null && typeof outcome.error !== "string") {
      throw new StorageError("invalid_input", "task_admission: outcome.error must be string|null");
    }
    if (outcome.success && outcome.error != null) {
      throw new StorageError("invalid_input", "success with error");
    }
    const outcomeJson = JSON.stringify(
      outcome.evidence === undefined
        ? { success: outcome.success, error: outcome.error ?? null }
        : { success: outcome.success, error: outcome.error ?? null, evidence: outcome.evidence });
    return this._db.transaction(() => {
      const t = this._transitionGate(taskId, expectedRevision, runId, now);
      // FinishItem arm: owner-bound only (no task-status arm —
      // finish is allowed while cancel_requested)
      if (t.owner_run_id !== runId) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      const item = this._db.get(
        "SELECT stage FROM task_items WHERE task_id=? AND item_id=?",
        taskId, itemId);
      if (!item || item.stage !== "running") {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      const next = expectedRevision + 1n;
      this._db.run(
        "UPDATE task_items SET stage='finished',outcome_json=? WHERE task_id=? AND item_id=?",
        outcomeJson, taskId, itemId);
      const delta = outcome.success ? COUNTER_DELTA.finish_ok : COUNTER_DELTA.finish_fail;
      const res = this._db.run(
        `UPDATE tasks SET status=status,state_revision=?,owner_run_id=owner_run_id,
         last_progress_at=?,result_json=result_json${delta}
         WHERE task_id=? AND state_revision=?`,
        next, now, taskId, expectedRevision);
      if (res.changes !== 1) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      return this._db.get("SELECT * FROM tasks WHERE task_id=?", taskId);
    });
  },
});

// ---------------------------------------------------------------------
// NM-045 — terminal transitions (decide RequestCancel / FinishExecution
// / FinishCancellation + write_task_delta terminal occupancy release).
// Shared gate order; RequestCancel's terminal no-op precedes the
// run-identity gate. result_json keys are serde_json-sorted
// (BTreeMap): cancellation_acknowledged < finished_items <
// successful_items.

const resultJson = (finished, successful, ack) =>
  JSON.stringify(ack
    ? { cancellation_acknowledged: true, finished_items: finished, successful_items: successful }
    : { finished_items: finished, successful_items: successful });

Object.assign(TaskAdmission.prototype, {
  /** decide(Change::RequestCancel): no-op on terminal or already
   *  cancel_requested (terminal no-op precedes run-identity); else
   *  run-identity gate, then status -> cancel_requested, rev+1.
   *  Returns {changed, task} — changed=false on no-op. */
  requestCancel(taskId, expectedRevision, runId, now) {
    if (!wellformedStr(taskId) || taskId === "") {
      throw new StorageError("invalid_input", "task_admission: task_id must be a non-empty string");
    }
    if (typeof expectedRevision !== "bigint" || expectedRevision < 0n) {
      throw new StorageError("invalid_input", "task_admission: expectedRevision must be a non-negative bigint");
    }
    if (!wellformedStr(runId) || runId.trim() === "") {
      throw new StorageError("invalid_input", "empty run identity");
    }
    if (!wellformedStr(now)) {
      throw new StorageError("invalid_input", "task_admission: now must be an RFC3339 string");
    }
    return this._db.transaction(() => {
      const t = this._db.get(
        "SELECT status,state_revision,owner_run_id,accepted_run_id FROM tasks WHERE task_id=?",
        taskId);
      if (!t) throw new StorageError("task_not_found", "TASK_NOT_FOUND");
      if (t.state_revision !== expectedRevision) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      const full = () => this._db.get("SELECT * FROM tasks WHERE task_id=?", taskId);
      // terminal no-op precedes the run-identity gate (decide)
      if (TERMINAL_STATUSES.includes(t.status)) {
        return { changed: false, task: full() };
      }
      if (t.accepted_run_id !== runId ||
          (t.owner_run_id !== null && t.owner_run_id !== runId)) {
        throw new StorageError("needs_verification",
          "ACCEPTANCE_NEEDS_VERIFICATION: old run or unresolved execution");
      }
      // already-cancel_requested no-op lives inside the arm — AFTER
      // the run-identity gate (a foreign run gets needs_verification).
      if (t.status === "cancel_requested") {
        return { changed: false, task: full() };
      }
      const next = expectedRevision + 1n;
      const res = this._db.run(
        `UPDATE tasks SET status='cancel_requested',state_revision=?,
         last_progress_at=?${RECOUNT_COUNTERS}
         WHERE task_id=? AND state_revision=?`,
        next, now, taskId, expectedRevision);
      if (res.changes !== 1) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      return { changed: true, task: full() };
    });
  },

  /** decide(FinishExecution | FinishCancellation). Shared finish arm:
   *  cancellation requires cancel_requested; owned/never_started +
   *  no-running-items else needs_verification "execution has not
   *  safely ended"; status derivation; terminal write releases
   *  occupancy and recounters. */
  _finish(taskId, expectedRevision, runId, now, cancellation) {
    return this._db.transaction(() => {
      const t = this._transitionGate(taskId, expectedRevision, runId, now);
      if (cancellation && t.status !== "cancel_requested") {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      const owned = t.owner_run_id === runId;
      const agg = this._db.get(
        `SELECT count(*) n,
          sum(stage='pending') pend, sum(stage='running') run,
          sum(stage='finished') fin,
          sum(stage='finished' AND json_extract(outcome_json,'$.success')=1) suc
         FROM task_items WHERE task_id=?`, taskId);
      const n = Number(agg.n), pending = Number(agg.pend ?? 0n),
        running = Number(agg.run ?? 0n), finished = Number(agg.fin ?? 0n),
        succeeded = Number(agg.suc ?? 0n);
      const neverStarted = t.owner_run_id === null && pending === n;
      if ((!owned && !neverStarted) || running > 0) {
        throw new StorageError("needs_verification",
          "ACCEPTANCE_NEEDS_VERIFICATION: execution has not safely ended");
      }
      let status;
      if (cancellation && succeeded < n) status = "cancelled";
      else if (finished === n) {
        status = succeeded === finished ? "succeeded"
          : succeeded === 0 ? "failed" : "partially_failed";
      } else if (t.status === "cancel_requested") status = "cancelled";
      else throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      const next = expectedRevision + 1n;
      const ack = cancellation && status === "cancelled";
      // write_task_delta order: item diff (none) -> guarded task row ->
      // occupancy release on terminal.
      const res = this._db.run(
        `UPDATE tasks SET status=?,state_revision=?,owner_run_id=owner_run_id,
         last_progress_at=?,result_json=?${RECOUNT_COUNTERS}
         WHERE task_id=? AND state_revision=?`,
        status, next, now,
        resultJson(finished, succeeded, ack), taskId, expectedRevision);
      if (res.changes !== 1) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      this._db.run("DELETE FROM scope_occupancy WHERE task_id=?", taskId);
      return { changed: true, task: this._db.get("SELECT * FROM tasks WHERE task_id=?", taskId) };
    });
  },
  finishExecution(taskId, expectedRevision, runId, now) {
    if (!wellformedStr(now)) {
      throw new StorageError("invalid_input", "task_admission: now must be an RFC3339 string");
    }
    return this._finish(taskId, expectedRevision, runId, now, false);
  },
  finishCancellation(taskId, expectedRevision, runId, now) {
    if (!wellformedStr(now)) {
      throw new StorageError("invalid_input", "task_admission: now must be an RFC3339 string");
    }
    return this._finish(taskId, expectedRevision, runId, now, true);
  },
});

// ---------------------------------------------------------------------
// NM-046 — recovery surface: requireVerification + reconcile +
// candidates. Reconcile is the recovery arm: no last_progress_at stamp.

Object.assign(TaskAdmission.prototype, {
  /** decide(Change::RequireVerification): binds the owner run on a
   *  running/cancel_requested task with a named running item; writes
   *  status=needs_verification + execution_uncertainty result_json
   *  (serde-sorted inner keys; previous_* verbatim). The item row is
   *  NOT written — RequireVerification binds, it does not transition
   *  the item. */
  requireVerification(taskId, expectedRevision, runId, itemId, reason, now) {
    if (!wellformedStr(itemId) || itemId === "") {
      throw new StorageError("invalid_input", "task_admission: item_id must be a non-empty string");
    }
    if (!wellformedStr(reason)) {
      throw new StorageError("invalid_input", "task_admission: reason must be a string");
    }
    if (!wellformedStr(now)) {
      throw new StorageError("invalid_input", "task_admission: now must be an RFC3339 string");
    }
    return this._db.transaction(() => {
      const t = this._transitionGate(taskId, expectedRevision, runId, now);
      // arm guards verbatim — all revision_conflict
      if (reason.trim() === "" || t.owner_run_id !== runId ||
          (t.status !== "running" && t.status !== "cancel_requested")) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      const item = this._db.get(
        "SELECT stage FROM task_items WHERE task_id=? AND item_id=?",
        taskId, itemId);
      if (!item || item.stage !== "running") {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      const prev = this._db.get(
        "SELECT result_json FROM tasks WHERE task_id=?", taskId);
      const uncertainty = {
        execution_uncertainty: {
          item_id: itemId,
          previous_result: prev.result_json === null ? null : JSON.parse(prev.result_json),
          previous_status: t.status,
          reason,
        },
      };
      const next = expectedRevision + 1n;
      const res = this._db.run(
        `UPDATE tasks SET status='needs_verification',state_revision=?,
         last_progress_at=?,result_json=?${RECOUNT_COUNTERS}
         WHERE task_id=? AND state_revision=?`,
        next, now, JSON.stringify(uncertainty), taskId, expectedRevision);
      if (res.changes !== 1) {
        throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
      }
      return { changed: true, task: this._db.get("SELECT * FROM tasks WHERE task_id=?", taskId) };
    });
  },

  /** reconcile_tx: non-terminal tasks with accepted!=run or a foreign
   *  owner; each -> needs_verification, rev+1, NO progress stamp.
   *  Returns the number of tasks marked. */
  reconcile(runId) {
    if (!wellformedStr(runId) || runId.trim() === "") {
      throw new StorageError("invalid_input", "empty run identity");
    }
    return this._db.transaction(() => {
      const ids = this._db.query(
        `SELECT task_id FROM tasks WHERE status NOT IN (${TERMINAL_STATUSES.map((x) => `'${x}'`).join(",")})
         AND (accepted_run_id<>? OR (owner_run_id IS NOT NULL AND owner_run_id<>?))`,
        runId, runId).map((r) => r.task_id);
      let count = 0;
      for (const id of ids) {
        const t = this._db.get(
          "SELECT status,state_revision FROM tasks WHERE task_id=?", id);
        // visible_status(run) for this selection is needs_verification;
        // skip if already there (no write).
        if (t.status === "needs_verification") continue;
        const res = this._db.run(
          `UPDATE tasks SET status='needs_verification',state_revision=state_revision+1
           ${RECOUNT_COUNTERS} WHERE task_id=? AND state_revision=?`,
          id, t.state_revision);
        if (res.changes !== 1) {
          throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
        }
        count++;
      }
      return { reconciled: count };
    });
  },

  /** candidates_tx: claimable tasks for run — accepted=run, owner NULL,
   *  pending/cancel_requested, ORDER BY created_at,task_id. Read path
   *  only: claims stamped with the live revision (no writes). */
  candidates(runId) {
    if (!wellformedStr(runId) || runId.trim() === "") {
      throw new StorageError("invalid_input", "empty run identity");
    }
    return this._db.transaction(() => {
      const ids = this._db.query(
        `SELECT task_id FROM tasks WHERE accepted_run_id=? AND owner_run_id IS NULL
         AND status IN ('pending','cancel_requested') ORDER BY created_at,task_id`,
        runId).map((r) => r.task_id);
      return ids.map((id) => {
        const task = this._db.get("SELECT * FROM tasks WHERE task_id=?", id);
        const items = this._db.query(
          "SELECT * FROM task_items WHERE task_id=? ORDER BY item_id", id);
        const scopes = this._db.query(
          "SELECT scope_key FROM task_scopes WHERE task_id=? ORDER BY scope_key", id)
          .map((r) => r.scope_key);
        const claims = this._db.query(
          "SELECT scope_key,state_revision FROM scope_occupancy WHERE task_id=? ORDER BY scope_key",
          id).map((r) => ({ scope_key: r.scope_key, state_revision: task.state_revision }));
        return { task, items, scopes, claims };
      });
    });
  },
});

// ---------------------------------------------------------------------
// NM-046 R1 — validated bound read (read_task + validate_snapshot
// parity). Deferred by design: the two validate_task_link registries
// (sqlite_verification + publication_checks) belong to their own P3
// slices. Any violation -> typed inconsistent/needs_verification.

const TERMINAL_RESULT_REQUIRED = new Set(TERMINAL_STATUSES);

export const canonEq = (a, b) => {
  // structural equality for parsed JSON values
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a).sort(), kb = Object.keys(b).sort();
  return ka.length === kb.length && ka.every((k, i) => k === kb[i] && canonEq(a[k], b[k]));
};

Object.assign(TaskAdmission.prototype, {
  /** read_task + validate_snapshot for one bound task inside the
   *  current transaction. Returns {task, items, scopes, claims,
   *  requestId}. Throws typed errors on any divergence. */
  _readBoundTaskValidated(id) {
    const db = this._db;
    const t = db.get(
      "SELECT task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of,item_count,items_pending,items_running,items_finished,items_succeeded,evidence_digest FROM tasks WHERE task_id=?",
      id);
    if (!t) return null;
    const inc = (m) => { throw new StorageError("inconsistent", m); };
    const parseJson = (v, what) => {
      try { return JSON.parse(v); } catch { inc(`${what} unparseable`); }
    };
    const work = parseJson(t.frozen_work_json, "frozen work");
    const items = db.query(
      "SELECT item_id,target_json,progress_unit,frozen_inputs_json,scope_keys_json,stage,outcome_json FROM task_items WHERE task_id=? ORDER BY item_id",
      id).map((r) => ({
        item_id: r.item_id, target: parseJson(r.target_json, "item target"),
        progress_unit: r.progress_unit,
        frozen_inputs: parseJson(r.frozen_inputs_json, "item inputs"),
        scope_keys: new Set(parseJson(r.scope_keys_json, "item scope keys")),
        stage: r.stage,
        outcome: r.outcome_json === null ? null : parseJson(r.outcome_json, "item outcome"),
      }));
    const claims = db.query(
      "SELECT scope_key,task_id FROM scope_occupancy WHERE task_id=? ORDER BY scope_key", id);
    const scopes = new Set(db.query(
      "SELECT scope_key FROM task_scopes WHERE task_id=? ORDER BY scope_key", id)
      .map((r) => r.scope_key));

    // execution_uncertainty marker (result_json.execution_uncertainty)
    const result = t.result_json === null ? null : parseJson(t.result_json, "result");
    let uncertainty = null;
    if (result && typeof result === "object" && "execution_uncertainty" in result) {
      const m = result.execution_uncertainty;
      const okShape = m && typeof m === "object" &&
        typeof m.item_id === "string" && typeof m.reason === "string" &&
        typeof m.previous_status === "string" && "previous_result" in m;
      if (!okShape ||
          (m.previous_status !== "running" && m.previous_status !== "cancel_requested") ||
          t.status !== "needs_verification" || t.owner_run_id === null ||
          m.reason.trim() === "" ||
          !items.some((i) => i.item_id === m.item_id && i.stage === "running")) {
        throw new StorageError("needs_verification", "invalid execution uncertainty");
      }
      uncertainty = m;
    }

    // FrozenWork.validate() — items/scopes non-empty, unique non-blank
    // item ids + progress units, item scope_keys union == scopes and
    // each non-empty, no blank scope, evidence markers == shared digest
    const witems = Array.isArray(work?.items) ? work.items : null;
    const wscopes = Array.isArray(work?.scopes) ? work.scopes : null;
    const wfail = (m) => inc(m);
    if (!witems || !wscopes || witems.length === 0 || wscopes.length === 0) {
      wfail("empty work");
    }
    const ids = new Set();
    for (const it of witems) {
      if (typeof it?.item_id !== "string" || it.item_id.trim() === "" ||
          typeof it?.progress_unit !== "string" || it.progress_unit.trim() === "" ||
          ids.has(it.item_id)) {
        wfail("invalid/duplicate item");
      }
      ids.add(it.item_id);
    }
    const union = new Set();
    for (const it of witems) {
      const ks = Array.isArray(it?.scope_keys) ? it.scope_keys : [];
      if (ks.length === 0) wfail("item resource bindings differ from scopes");
      for (const k of ks) union.add(k);
    }
    if (union.size !== wscopes.length || !wscopes.every((k) => union.has(k))) {
      wfail("item resource bindings differ from scopes");
    }
    if (wscopes.some((s) => typeof s !== "string" || s.trim() === "")) {
      wfail("empty scope");
    }
    const sharedDigest = (work.shared_evidence && typeof work.shared_evidence === "object" &&
      typeof work.shared_evidence.evidence_digest === "string")
      ? work.shared_evidence.evidence_digest : null;
    for (const it of witems) {
      const marker = it?.frozen_inputs?.selection_evidence?.evidence_digest;
      if (typeof marker === "string" && marker !== sharedDigest) {
        wfail("item evidence marker mismatches shared evidence");
      }
    }

    // column digest vs blob digest
    if ((t.evidence_digest ?? null) !== sharedDigest) {
      inc("task evidence digest column/blob mismatch");
    }
    // task/frozen work binding
    if (t.accepted_run_id.trim() === "" ||
        wscopes.length !== scopes.size || !wscopes.every((k) => scopes.has(k)) ||
        items.length !== witems.length) {
      inc("task/frozen work incomplete");
    }
    // per-item: spec in work.items, finished<=>outcome, no success+error
    for (const i of items) {
      const spec = witems.find((w) => w.item_id === i.item_id);
      if (!spec || !canonEq(spec.target, i.target) ||
          spec.progress_unit !== i.progress_unit ||
          !canonEq(spec.frozen_inputs, i.frozen_inputs) ||
          ![...i.scope_keys].every((k) => spec.scope_keys?.includes(k)) ||
          i.scope_keys.size !== (spec.scope_keys?.length ?? -1)) {
        inc("item evidence inconsistent");
      }
      if ((i.stage === "finished") !== (i.outcome !== null) ||
          (i.outcome && i.outcome.success === true && i.outcome.error != null)) {
        inc("item evidence inconsistent");
      }
    }
    // owner/stage invariants
    if ((t.status === "running" && t.owner_run_id === null) ||
        (t.status === "pending" && (t.owner_run_id !== null ||
          items.some((i) => i.stage !== "pending")))) {
      inc("execution owner/stage missing or inconsistent");
    }
    // claims<->scopes binding on non-terminal; none on terminal
    const terminal = TERMINAL_STATUSES.includes(t.status);
    if (!terminal) {
      const claimSet = new Set(claims.map((c) => c.scope_key));
      if (claimSet.size !== scopes.size ||
          ![...claimSet].every((k) => scopes.has(k)) ||
          claims.some((c) => c.task_id !== t.task_id)) {
        inc("active occupancy missing/inconsistent");
      }
    } else if (claims.length !== 0 || t.result_json === null ||
               items.some((i) => i.stage === "running")) {
      inc("terminal execution/claims inconsistent");
    }
    // counters vs items
    const fin = items.filter((i) => i.stage === "finished").length;
    const suc = items.filter((i) => i.outcome?.success === true).length;
    const run = items.filter((i) => i.stage === "running").length;
    const total = items.length;
    if (Number(t.item_count) !== total ||
        Number(t.items_pending) !== total - fin - run ||
        Number(t.items_running) !== run ||
        Number(t.items_finished) !== fin ||
        Number(t.items_succeeded) !== suc) {
      inc("task item counters diverged from items");
    }
    // status/outcome validity
    const valid =
      t.status === "succeeded" ? fin === total && suc === total :
      t.status === "failed" ? fin === total && suc === 0 :
      t.status === "partially_failed" ? fin === total && suc > 0 && suc < total :
      t.status === "interrupted" ? result?.interruption !== undefined :
      t.status === "cancelled" ? fin < total ||
        (suc < total && result?.cancellation_acknowledged === true) :
      t.status === "needs_verification" ? t.result_json === null || uncertainty !== null :
      t.result_json === null;
    if (!valid || (t.owner_run_id !== null && t.owner_run_id.trim() === "") ||
        (t.owner_run_id === null && items.some((i) => i.stage !== "pending"))) {
      inc("execution status/outcomes inconsistent");
    }
    // request binding must exist for a task row (read_bound_task parity)
    const req = db.get("SELECT request_id FROM requests WHERE task_id=?", id);
    if (!req) inc("task binding/execution missing");
    // NM-048: registered task-link validators (Rust read_bound_task calls
    // both validate_task_link registries here). Inspection link runs
    // first; when it handles the task it returns true and the
    // verification side is skipped.
    const snapshot = {
      task: t, items, scopes: [...scopes],
      claims: claims.map((c) => ({ scope_key: c.scope_key, task_id: c.task_id,
        state_revision: t.state_revision })),
    };
    const handled = inspectionTaskLink(db, snapshot,
      (x) => this._readBoundTaskValidated(x),
      (x) => db.get("SELECT * FROM requests WHERE request_id=?", x));
    if (!handled) {
      verifyTaskLink(db, snapshot, (x) => this._readBoundTaskValidated(x));
    }
    return {
      task: t, items, scopes: [...scopes], requestId: req.request_id,
      claims: claims.map((c) => ({ scope_key: c.scope_key, state_revision: t.state_revision })),
    };
  },
});

// Route candidates + reconcile through the validated bound read:
// divergent rows fail the whole call closed.
Object.assign(TaskAdmission.prototype, {
  reconcile(runId) {
    if (!wellformedStr(runId) || runId.trim() === "") {
      throw new StorageError("invalid_input", "empty run identity");
    }
    return this._db.transaction(() => {
      const ids = this._db.query(
        `SELECT task_id FROM tasks WHERE status NOT IN (${TERMINAL_STATUSES.map((x) => `'${x}'`).join(",")})
         AND (accepted_run_id<>? OR (owner_run_id IS NOT NULL AND owner_run_id<>?))`,
        runId, runId).map((r) => r.task_id);
      let count = 0;
      for (const id of ids) {
        // validated bound read — a tampered row fails the whole call
        const snap = this._readBoundTaskValidated(id);
        if (!snap) throw new StorageError("task_not_found", "TASK_NOT_FOUND");
        const t = snap.task;
        if (t.status === "needs_verification") continue;
        const res = this._db.run(
          `UPDATE tasks SET status='needs_verification',state_revision=state_revision+1
           ${RECOUNT_COUNTERS} WHERE task_id=? AND state_revision=?`,
          id, t.state_revision);
        if (res.changes !== 1) {
          throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
        }
        count++;
      }
      return { reconciled: count };
    });
  },
  candidates(runId) {
    if (!wellformedStr(runId) || runId.trim() === "") {
      throw new StorageError("invalid_input", "empty run identity");
    }
    return this._db.transaction(() => {
      const ids = this._db.query(
        `SELECT task_id FROM tasks WHERE accepted_run_id=? AND owner_run_id IS NULL
         AND status IN ('pending','cancel_requested') ORDER BY created_at,task_id`,
        runId).map((r) => r.task_id);
      return ids.map((id) => {
        const snap = this._readBoundTaskValidated(id);
        if (!snap) throw new StorageError("task_not_found", "TASK_NOT_FOUND");
        const { task, items, scopes, claims } = snap;
        return { task, items, scopes, claims };
      });
    });
  },
});

// NM-048 — TaskAdmission facade methods for the verification surface.
Object.assign(TaskAdmission.prototype, {
  readReceipt(id) {
    return this._db.transaction(() =>
      readReceipt(this._db, id, (x) => this._readBoundTaskValidated(x)));
  },
  runtimeRegistration(id) {
    return runtimeRegistration(this._db, this, id);
  },
  claimWithRuntimeGuard(plan, guard, dbPath) {
    return claimWithRuntimeGuard(this._db, this, plan, guard, dbPath);
  },
});
