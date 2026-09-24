// NM-041 P3 — request idempotency admission primitive.
//
// Ports the admission half of task_control/service.rs::accept +
// sqlite_tasks.rs::insert-request: one transaction, unique request_id,
// exact replay -> reused, same-ID different-command -> conflict.
// Independent of task creation, workers, occupancy, and services.
//
// Ordering preserved:
//   boundary (id/command) -> tx BEGIN -> SELECT existing ->
//   byte-compare stored command -> reused | conflict | INSERT -> COMMIT
//
// Open requires an explicit writable mode — the repo never silently
// creates a database (adapter modes gate that), and the schema gate
// must pass first.

import { createHash } from "node:crypto";
import { parseJsonValue } from "../mcp/serde_json.mjs";
import { gateDatabase } from "./schema_gate.mjs";
import { openDatabase, StorageError } from "./sqlite_adapter.mjs";

const sha256 = (s) => createHash("sha256").update(s, "utf8").digest("hex");

export class RequestRepository {
  /** Gated writable repo. Caller must pass mode:"readwrite"|"create"
   *  explicitly — there is no silent default. */
  static open(path, { mode } = {}) {
    if (mode !== "readwrite" && mode !== "create") {
      throw new StorageError("invalid_input",
        "request_repository: explicit mode readwrite|create required");
    }
    // Gate BEFORE any writable open: the gate's readonly handle can
    // never create or mutate a file, so a missing/garbage/uninitialized
    // path fails closed with zero artifacts. "create" remains explicit
    // but still requires a gated existing schema — it does not
    // bootstrap an uninitialized database (that is the schema-init
    // path, a future task).
    const capability = gateDatabase(path);
    return new RequestRepository(openDatabase(path, { mode }), capability);
  }
  constructor(db, capability) {
    this._db = db;
    this.capability = capability;
  }
  close() { this._db.close(); }

  /** Admit a request idempotently.
   *  requestId: non-empty after trim (Rust: "empty request_id").
   *  commandJson: canonical command bytes as a wellformed JSON string.
   *  outcome: {kind:"task", taskId} or {kind:"terminal", terminalJson}
   *    — the CHECK constraint's binding shape, validated before SQL.
   *  Returns {reused, record:{request_id,command_digest,outcome_kind,...}}.
   *  Exact replay (byte-identical command_json) -> reused:true.
   *  Same id, different bytes -> StorageError "conflict" (Rust
   *  RequestConflict). */
  admit(requestId, commandJson, outcome) {
    if (typeof requestId !== "string" || requestId.trim() === "" || !requestId.isWellFormed()) {
      throw new StorageError("invalid_input", "empty request_id");
    }
    if (typeof commandJson !== "string" || !commandJson.isWellFormed()) {
      throw new StorageError("invalid_input", "command must be wellformed JSON text");
    }
    try { parseJsonValue(commandJson); }
    catch { throw new StorageError("invalid_input", "command is not valid JSON"); }
    const isTask = outcome?.kind === "task";
    const isTerm = outcome?.kind === "terminal";
    if (!isTask && !isTerm) {
      throw new StorageError("invalid_input",
        "outcome must be {kind:'task',taskId} or {kind:'terminal',terminalJson}");
    }
    const taskId = isTask ? outcome.taskId : null;
    const terminalJson = isTerm ? outcome.terminalJson : null;
    if (isTask && (typeof taskId !== "string" || taskId === "")) {
      throw new StorageError("invalid_input", "task outcome requires non-empty taskId");
    }
    if (isTerm && (typeof terminalJson !== "string" || !terminalJson.isWellFormed())) {
      throw new StorageError("invalid_input", "terminal outcome requires wellformed terminalJson");
    }

    const digest = sha256(commandJson);
    return this._db.transaction(() => {
      const existing = this._db.get(
        "SELECT request_id,command_json,command_digest,outcome_kind,task_id,terminal_json FROM requests WHERE request_id=?",
        requestId);
      if (existing) {
        // stored digest is re-verified on every read path (Rust
        // read_request) — a corrupted store fails closed, not silently.
        if (sha256(existing.command_json) !== existing.command_digest) {
          throw new StorageError("inconsistent", "command digest mismatch");
        }
        if (existing.command_json !== commandJson) {
          throw new StorageError("conflict", "REQUEST_CONFLICT");
        }
        return { reused: true, record: existing };
      }
      this._db.run(
        "INSERT INTO requests(request_id,command_json,command_digest,outcome_kind,task_id,terminal_json) VALUES(?,?,?,?,?,?)",
        requestId, commandJson, digest,
        isTask ? "task" : "terminal",
        isTask ? taskId : null,
        isTerm ? terminalJson : null);
      const record = this._db.get(
        "SELECT request_id,command_json,command_digest,outcome_kind,task_id,terminal_json FROM requests WHERE request_id=?",
        requestId);
      return { reused: false, record };
    });
  }
}
