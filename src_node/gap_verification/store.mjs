// NM-076 — in-lane store adapter for gap_verification.
// Binds ONE opened db handle to the GapDecisionStore + GapTaskStore
// surface, composed only from already-landed exports (read-only
// consumption; no storage/market edits):
//   - task/transition     TaskAdmission (storage/task_repository.mjs)
//   - gap decisions       market/gaps.mjs (NM-056)
//   - subject freeze/check market/market_read.mjs via mod.mjs
// SqliteMarket parity: all runner transitions go through the same
// write transaction machinery w1 landed (decide gates + CAS +
// counter deltas + occupancy release + execution-start markers).
import { openDatabase, StorageError } from "../storage/sqlite_adapter.mjs";
import { parseJsonValue } from "../mcp/serde_json.mjs";
import { gateDatabase } from "../storage/schema_gate.mjs";
import { TaskAdmission } from "../storage/task_repository.mjs";
import * as GAPS from "../market/gaps.mjs";
import { freezeSubjectOn, checkSubjectOn } from "./mod.mjs";

// Snapshot/read parity: explicit BEGIN/COMMIT so inner MarketError
// propagates un-wrapped (the adapter's transaction() re-maps every
// throw — market_write.mjs documents this).
const readTx = (db, f) => {
  db.exec("BEGIN");
  try {
    const r = f();
    db.exec("COMMIT");
    return r;
  } catch (e) {
    try { db.exec("ROLLBACK"); } catch { /* rolled back */ }
    throw e;
  }
};

// Reshape w1's validated bound read into the TaskSnapshot shape the
// runner consumes (Rust task_control::TaskSnapshot verbatim fields).
const toSnapshot = (snap) => ({
  task: {
    task_id: snap.task.task_id,
    key: parseJsonValue(snap.task.command_key_json),
    status: snap.task.status,
    state_revision: snap.task.state_revision,
    accepted_run_id: snap.task.accepted_run_id,
    owner_run_id: snap.task.owner_run_id,
    last_progress_at: snap.task.last_progress_at,
    result: snap.task.result_json === null ? null
      : parseJsonValue(snap.task.result_json),
    counts: {
      total: snap.task.item_count,
      pending: snap.task.items_pending,
      running: snap.task.items_running,
      finished: snap.task.items_finished,
      succeeded: snap.task.items_succeeded,
    },
    evidence_digest: snap.task.evidence_digest,
  },
  items: snap.items.map((i) => ({
    spec: {
      item_id: i.item_id,
      target: i.target,
      progress_unit: i.progress_unit,
      frozen_inputs: i.frozen_inputs,
      scope_keys: i.scope_keys,
    },
    stage: i.stage,
    outcome: i.outcome,
  })),
});

const taskNotFound = () => new StorageError("task_not_found", "TASK_NOT_FOUND");

/// GapTaskStore + GapDecisionStore over one readwrite connection.
export class GapRepository {
  /** Gated writable open — schema gate runs before the writable
   *  handle (NM-041 ordering, same as TaskAdmission.open). */
  static open(path) {
    const capability = gateDatabase(path);
    const db = openDatabase(path, { mode: "readwrite" });
    return new GapRepository(db, new TaskAdmission(db, capability));
  }
  constructor(db, adm) {
    this.db = db;
    this.adm = adm;
  }
  close() { this.db.close(); }

  // ---- GapTaskStore ----
  /// MarketRepository::task — validated bound read; missing task is
  /// TaskNotFound (tc layer).
  task(id) {
    return readTx(this.db, () => {
      const snap = this.adm._readBoundTaskValidated(id);
      if (!snap) throw taskNotFound();
      return toSnapshot(snap);
    });
  }

  /// RangeRepository::transition_task (transition_tx): revision-gated
  /// decide + persist in one transaction, then re-read the snapshot.
  /// `change` kinds map onto w1's per-change tx methods — identical
  /// decide gates, CAS and post-conditions.
  transitionTask(id, revision, run, change, now) {
    const nowStr = now ?? new Date().toISOString();
    switch (change.kind) {
      case "claim":
        this.adm.claim(id, revision, run, nowStr);
        break;
      case "startItem":
        this.adm.startItem(id, revision, run, change.item_id, nowStr);
        break;
      case "finishItem":
        this.adm.finishItem(
          id, revision, run, change.item_id, change.outcome, nowStr);
        break;
      case "finishExecution":
        this.adm.finishExecution(id, revision, run, nowStr);
        break;
      case "finishCancellation":
        this.adm.finishCancellation(id, revision, run, nowStr);
        break;
      case "requireVerification":
        this.adm.requireVerification(
          id, revision, run, change.item_id, change.reason, nowStr);
        break;
      case "requestCancel":
        this.adm.requestCancel(id, revision, run, nowStr);
        break;
      default:
        throw new StorageError("invalid_input", `INVALID_COMMAND: unsupported change ${change.kind}`);
    }
    return this.task(id);
  }

  // ---- GapDecisionStore ----
  freezeGapSubject(target) {
    return readTx(this.db, () => freezeSubjectOn(this.db, target));
  }
  checkGapSubject(subject) {
    return readTx(this.db, () => checkSubjectOn(this.db, subject));
  }
  commitGapDecision(decision) {
    return GAPS.recordGapDecisionGuarded(this.db, decision);
  }
  gapDecision(decisionId) {
    return GAPS.readDecision(this.db, decisionId);
  }
}
