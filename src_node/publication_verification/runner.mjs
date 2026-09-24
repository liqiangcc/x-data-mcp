// NM-098 — src/publication_verification/runner.rs port.
// Bounded execution of an accepted read-only publication inspection.
import { CHECK_KEY } from "../storage/publication_checks.mjs";
import { canonEq } from "../storage/task_repository.mjs";
import { prepare } from "./decision.mjs";
import { visibleStatus } from "./admission.mjs";
import {
  msError, msInvalid, msIntegrity, msConflict, msOutcomeUnknown,
  tcTaskNotFound, tcUnsupportedContract,
} from "./mod.mjs";

const TERMINAL = ["succeeded", "partially_failed", "failed",
  "cancelled", "interrupted"];

// market_storage::Error::Task(task_control::Error) — Display keeps the
// inner tc text verbatim.
const msTaskError = (tc) => msError("task", tc.message);

export class Runner {
  /** now = RFC3339 timestamp supplier (chrono::Utc::now equivalent). */
  constructor(tasks, evidence, runId, actor, now = () => new Date().toISOString().replace(/\.\d+Z$/, "Z")) {
    this.tasks = tasks;
    this.evidence = evidence;
    this.runId = runId;
    this.actor = actor;
    this._nowFn = now;
  }

  // runner::task — read_task -> Task(TaskNotFound)
  _task(id) {
    const s = this.tasks.readTask(id);
    if (s === null) throw msTaskError(tcTaskNotFound());
    return s;
  }

  // runner::cancel — verbatim
  _cancel(check) {
    if (visibleStatus(check.task, this.runId) !== "cancel_requested") {
      throw msConflict("check cancellation state changed");
    }
    // Called only before inspection or after synchronous inspection
    // returned.
    if (check.items.some((i) => i.stage === "running")) {
      check = this.tasks.transition(
        check.task.task_id, check.task.state_revision, this.runId,
        { kind: "finishItem",
          item_id: "publication-results",
          outcome: {
            success: false,
            error: "publication inspection cancelled; target unchanged",
            evidence: null,
          } }, this._nowFn());
    }
    return { outcome: "cancelled",
      snapshot: this.tasks.transition(
        check.task.task_id, check.task.state_revision, this.runId,
        { kind: "finishCancellation" }, this._nowFn()) };
  }

  _preserveUnresolved(id) {
    const check = this._task(id);
    if (["running", "cancel_requested"].includes(
      visibleStatus(check.task, this.runId))) {
      this.tasks.transition(
        id, check.task.state_revision, this.runId,
        { kind: "requireVerification",
          item_id: "publication-results",
          reason: "publication inspection ended without a reliable committed check receipt; original task unchanged" },
        this._nowFn());
    }
  }

  _facts(checkId, target) {
    try {
      return this.evidence.publicationFacts(target);
    } catch (error) {
      this._preserveUnresolved(checkId);
      throw error;
    }
  }

  _prepare(check, facts) {
    const id = check.task.task_id;
    try {
      return prepare(check, facts, this.runId, this.actor,
        this._nowFn());
    } catch (error) {
      this._preserveUnresolved(id);
      throw this._toStorage(error);
    }
  }

  _toStorage(e) {
    // task_control::Error -> market_storage::Error::Task passthrough;
    // StorageError/other -> verbatim variant mapping.
    if (e?.kind === "task") return msTaskError(e);
    return e;
  }

  /** runner::run — the bounded execution. Returns
   *  {outcome:"completed", receipt} | {outcome:"cancelled", snapshot}. */
  run(id) {
    if (typeof this.runId !== "string" || this.runId.trim() === "" ||
        typeof this.actor !== "string" || this.actor.trim() === "") {
      throw msInvalid("missing publication inspection runtime identity");
    }
    const prior = this.evidence.checkReceipt(id);
    if (prior !== null) {
      return { outcome: "completed", receipt: prior };
    }
    let check = this._task(id);
    if (!canonEq(check.task.key, CHECK_KEY) || check.items.length !== 1) {
      throw msTaskError(tcUnsupportedContract());
    }
    if (check.task.status === "cancelled") {
      return { outcome: "cancelled", snapshot: check };
    }
    if (visibleStatus(check.task, this.runId) === "needs_verification" ||
        check.items[0].stage === "running") {
      throw msOutcomeUnknown(
        "old or running check has no reliable receipt; no automatic re-execution");
    }
    if (check.task.status === "cancel_requested") {
      return this._cancel(check);
    }
    if (check.task.status !== "pending") {
      throw msOutcomeUnknown("check has no reliable final receipt");
    }
    const target = check.items[0].spec.frozen_inputs?.target_task_id;
    if (typeof target !== "string") {
      throw msIntegrity("publication check target missing");
    }
    check = this.tasks.transition(id, check.task.state_revision,
      this.runId, { kind: "claim" }, this._nowFn());
    check = this.tasks.transition(id, check.task.state_revision,
      this.runId, { kind: "startItem", item_id: "publication-results" },
      this._nowFn());
    const facts = this._facts(id, target);
    const plan = this._prepare(check, facts);
    const current = this._task(id);
    if (current.task.status === "cancel_requested") {
      return this._cancel(current);
    }
    try {
      return { outcome: "completed",
        receipt: this.evidence.commitCheck(plan) };
    } catch (error) {
      const receipt = this.evidence.checkReceipt(id);
      if (receipt !== null) {
        return { outcome: "completed", receipt };
      }
      if (error?.kind !== "conflict") {
        this._preserveUnresolved(id);
        throw error;
      }
      const cur = this._task(id);
      if (cur.task.status === "cancel_requested") {
        return this._cancel(cur);
      }
      if (!canonEq(cur, plan.audit.verification_before)) {
        this._preserveUnresolved(id);
        throw error;
      }
      // One fresh verified read, never the stale proof or a loop.
      const freshFacts = this._facts(id, target);
      const fresh = this._prepare(cur, freshFacts);
      try {
        return { outcome: "completed",
          receipt: this.evidence.commitCheck(fresh) };
      } catch (error2) {
        const receipt2 = this.evidence.checkReceipt(id);
        if (receipt2 !== null) {
          return { outcome: "completed", receipt: receipt2 };
        }
        this._preserveUnresolved(id);
        throw error2;
      }
    }
  }
}
