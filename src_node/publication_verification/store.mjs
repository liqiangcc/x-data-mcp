// NM-098 — the storage seam binding the publication_verification
// domain layer to the landed storage surface:
//   TaskRepository       <- TaskAdmission bound reads + transitions
//   EvidenceRepository   <- verification_facts.readFacts (NM-056)
//   AuditStore           <- publication_checks.checkReceipt /
//                           commitCheckTx (NM-056 + NM-059)
//   KnownRepository      <- publication_checks.knownPublication /
//                           bind path (NM-059)
// Explicit BEGIN IMMEDIATE write tx (SqliteTasks::write parity —
// write_transaction(true)); the adapter's db.transaction() is deferred
// and commit-failure must map to outcome_unknown, matching gaps.mjs.
import { canonEq } from "../storage/task_repository.mjs";
import { snapshotCanon, ser, readRequestRecord }
  from "../storage/verification_storage.mjs";
import {
  commitCheckTx, checkReceipt as pcCheckReceipt,
  knownPublication as pcKnownPublication,
  validateTerminal as pcValidateTerminal,
} from "../storage/publication_checks.mjs";
import { readFacts } from "../market/verification_facts.mjs";
import { StorageError } from "../storage/sqlite_adapter.mjs";
import { MarketError } from "../market/record_codec.mjs";
import { createHash } from "node:crypto";
import { toTaskError, tcError, msError,
} from "./mod.mjs";

const sha256hex = (s) => createHash("sha256").update(s).digest("hex");

// explicit tx wrappers — inner domain errors propagate unmapped.
const readTx = (db, f) => {
  db.exec("BEGIN");
  try { const r = f(); db.exec("COMMIT"); return r; }
  catch (e) { try { db.exec("ROLLBACK"); } catch { /* rolled back */ } throw e; }
};
const writeTx = (db, f) => {
  db.exec("BEGIN IMMEDIATE");
  try {
    const r = f();
    try { db.exec("COMMIT"); }
    catch (e) {
      throw msError("outcome_unknown",
        `PUBLISH_OUTCOME_UNKNOWN: ${e.message}; inspect the same publication_id`);
    }
    return r;
  } catch (e) {
    try { db.exec("ROLLBACK"); } catch { /* rolled back */ }
    throw e;
  }
};

// ---------------------------------------------------------------------
export class PublicationStore {
  /** db = sqlite adapter handle (readwrite); adm = TaskAdmission. */
  constructor(db, adm) {
    this.db = db;
    this.adm = adm;
  }

  _readBound(id) { return this.adm._readBoundTaskValidated(id); }
  _readFacts(targetId) {
    return readFacts(this.db, targetId, (x) => this._readBound(x));
  }

  // -- TaskRepository seam (throws task-layer MarketError) ------------
  readTask(id) {
    try {
      const s = readTx(this.db, () => this._readBound(id));
      return s === null ? null : snapshotCanon(s);
    } catch (e) { throw toTaskError(e); }
  }

  readRequest(id) {
    try {
      return readTx(this.db, () =>
        readRequestRecord(this.db, id, (x) => this._readBound(x),
          (t) => this._readFacts(t)));
    } catch (e) { throw toTaskError(e); }
  }

  transition(id, revision, runId, change, now) {
    try {
      const rev = BigInt(revision);
      switch (change.kind) {
        case "claim": this.adm.claim(id, rev, runId, now); break;
        case "startItem":
          this.adm.startItem(id, rev, runId, change.item_id, now); break;
        case "finishItem":
          this.adm.finishItem(id, rev, runId, change.item_id,
            change.outcome, now); break;
        case "finishCancellation":
          this.adm.finishCancellation(id, rev, runId, now); break;
        case "requestCancel":
          this.adm.requestCancel(id, rev, runId, now); break;
        case "requireVerification":
          this.adm.requireVerification(id, rev, runId, change.item_id,
            change.reason, now); break;
        default: throw tcError("STATE_REVISION_CONFLICT");
      }
      const s = readTx(this.db, () => this._readBound(id));
      return s === null ? null : snapshotCanon(s);
    } catch (e) { throw toTaskError(e); }
  }

  // -- EvidenceRepository + AuditStore (market_storage::Error) --------
  publicationFacts(targetId) {
    try {
      return readTx(this.db, () => {
        const f = this._readFacts(targetId);
        return { task: snapshotCanon(f.task), items: f.items };
      });
    } catch (e) { throw this._msMap(e); }
  }

  checkReceipt(id) {
    try {
      return readTx(this.db, () =>
        pcCheckReceipt(this.db, id, (x) => this._readBound(x),
          (t) => this._readFacts(t)));
    } catch (e) { throw this._msMap(e); }
  }

  // AuditStore::commit_check — write_transaction(true) parity:
  // BEGIN IMMEDIATE; commit failure -> outcome_unknown.
  commitCheck(plan) {
    try {
      return writeTx(this.db, () =>
        commitCheckTx(this.db, plan, (x) => this._readBound(x),
          (t) => this._readFacts(t)));
    } catch (e) { throw this._msMap(e); }
  }

  // -- KnownRepository (task_control::Error) ---------------------------
  knownPublication(target) {
    try {
      return readTx(this.db, () =>
        pcKnownPublication(this.db, target, (x) => this._readBound(x),
          (t) => this._readFacts(t)));
    } catch (e) { throw toTaskError(e); }
  }

  // bind_known_publication — SqliteTasks::write (BEGIN IMMEDIATE):
  // replay -> bound equality -> validate_terminal -> terminal insert.
  bindKnownPublication(plan) {
    try {
      return writeTx(this.db, () => {
        const record = readRequestRecord(this.db, plan.request_id,
          (x) => this._readBound(x), (t) => this._readFacts(t));
        if (record !== null) {
          if (!canonEq(record.command, plan.command)) {
            throw tcError("REQUEST_CONFLICT");
          }
          return { record, reused: true };
        }
        const actual = this._readBound(plan.target.task.task_id);
        if (actual === null ||
            !canonEq(snapshotCanon(actual), plan.target)) {
          throw tcError("STATE_REVISION_CONFLICT");
        }
        pcValidateTerminal(this.db, plan.command, plan.result,
          (x) => this._readBound(x), (t) => this._readFacts(t));
        const cmdJson = ser(plan.command, "command");
        // serde_json::to_string(Value) — BTreeMap (recursively sorted
        // keys), same as bind_known_publication's to_string(&value).
        const termJson = ser(plan.result);
        this.db.run(
          "INSERT INTO requests VALUES(?,?,?,'terminal',NULL,?)",
          plan.request_id, cmdJson, sha256hex(cmdJson), termJson);
        return {
          record: { request_id: plan.request_id, command: plan.command,
            outcome: { Terminal: plan.result } },
          reused: false,
        };
      });
    } catch (e) { throw toTaskError(e); }
  }

  // Errors -> market_storage::Error variant texts (verbatim).
  _msMap(e) {
    const kind = e?.kind;
    if (kind && ["task", "integrity", "storage", "conflict", "invalid",
      "outcome_unknown", "not_found", "base", "task_not_found"]
      .includes(kind)) {
      // marketTaskErr uses kind=code (e.g. "task_not_found") — the Rust
      // surface is Error::Task; keep the code, the message is verbatim.
      return kind === "task_not_found" ? msError("task", e.message) : e;
    }
    if (e instanceof MarketError) return msError("integrity", e.message);
    if (e instanceof StorageError) {
      if (e.code === "needs_verification") {
        return msError("task", e.message);
      }
      if (e.code === "revision_conflict" || e.code === "conflict") {
        return msError("conflict", `PUBLISH_PRECONDITION: ${e.message}`);
      }
      if (e.code === "not_found" || e.code === "task_not_found") {
        return msError("task", "TASK_NOT_FOUND");
      }
      return msError("storage", `MARKET_STORAGE: ${e.message}`);
    }
    return msError("storage", `MARKET_STORAGE: ${e?.message ?? e}`);
  }
}
