// NM-076 — src/gap_verification/runner.rs port.
// Bounded per-item execution for accepted `verify_data_gap` tasks: the
// frozen target/subject go to the installed verifier through
// `decideGap`; the decision id derives deterministically from
// task/item identity. A committed decision never triggers
// update/backfill.
import { StorageError } from "../storage/sqlite_adapter.mjs";
import { MarketError } from "../market/record_codec.mjs";
import { commandKey, outcomeUnknown, decideGap } from "./mod.mjs";
import { parseGapFrozen, gapFrozenParts, taskError } from "./contract.mjs";

const TERMINAL = ["succeeded", "partially_failed", "failed", "cancelled", "interrupted"];

// task_control::visible_status — verbatim.
export const visibleStatus = (task, run) =>
  !TERMINAL.includes(task.status) &&
  (task.accepted_run_id !== run ||
   (task.owner_run_id !== null && task.owner_run_id !== run))
    ? "needs_verification" : task.status;

// market_storage::Error Display texts used by the runner itself.
const invalid = (m) =>
  Object.assign(new MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`), { detail: m });
const integrity = (m) =>
  Object.assign(new MarketError("integrity", `MARKET_INTEGRITY: ${m}`), { detail: m });

/// Decision identity derives deterministically from task/item
/// identity: stable across replay and runner restart, never caller- or
/// random-derived.
export const gapDecisionId = (taskId, itemId) => `${taskId}:${itemId}`;

/// GapRunOutcome — { decided: snapshot } | { notDecided: snapshot }.
export const decided = (task) => ({ decided: task });
export const notDecided = (task) => ({ notDecided: task });

// repository: GapDecisionStore + GapTaskStore (task/transitionTask/
// gapDecision/commitGapDecision/checkGapSubject/freezeGapSubject).
export class GapVerifyRunner {
  constructor(repository, runId) {
    this.repository = repository;
    this.run_id = runId;
  }
  run(taskId, verifier, now) {
    let task = this.repository.task(taskId);
    const k = task.task.key;
    const ck = commandKey();
    if (!k || k.operation !== ck.operation || k.dataset !== ck.dataset ||
        k.contract_version !== ck.contract_version) {
      throw invalid("unsupported gap verification contract");
    }
    if (task.items.length !== 1) {
      throw invalid("gap verification task must carry exactly one item");
    }
    const item = task.items[0];
    const itemId = item.spec.item_id;
    const decisionId = gapDecisionId(taskId, itemId);
    if (this.repository.gapDecision(decisionId) !== null) {
      // The decision fact is durable; a lost close-out never re-runs
      // verification. An unfinished item means the commit response was
      // lost — inspect, never overwrite.
      if (item.stage === "finished") return decided(task);
      throw outcomeUnknown(
        "committed gap decision has unconfirmed item close-out");
    }
    if (visibleStatus(task.task, this.run_id) === "needs_verification") {
      throw taskError(
        "ACCEPTANCE_NEEDS_VERIFICATION: old run or unresolved execution");
    }
    if (item.stage === "running") {
      throw outcomeUnknown(
        "running item has no verified result; inspect before any retry");
    }
    if (TERMINAL.includes(task.task.status) || item.stage === "finished") {
      if (item.outcome !== null && item.outcome.success === true) {
        throw integrity("successful gap item has no committed decision");
      }
      return notDecided(task);
    }
    if (task.task.status === "cancel_requested") {
      const next = this.repository.transitionTask(
        taskId, task.task.state_revision, this.run_id,
        { kind: "finishExecution" });
      return notDecided(next);
    }
    if (task.task.status === "pending") {
      task = this.repository.transitionTask(
        taskId, task.task.state_revision, this.run_id,
        { kind: "claim" });
    }
    this.repository.transitionTask(
      taskId, task.task.state_revision, this.run_id,
      { kind: "startItem", item_id: itemId });
    let outcome;
    try {
      this.decide(item.spec.frozen_inputs, decisionId, verifier, now);
      outcome = null;
    } catch (error) {
      outcome = error;
    }
    if (outcome === null) return this.close(taskId, itemId, true, null, false);
    // A response can be lost after commit. Never overwrite success.
    if (this.repository.gapDecision(decisionId) !== null) {
      return this.close(taskId, itemId, true, null, false);
    }
    if (isUncertainty(outcome)) {
      const current = this.repository.task(taskId);
      let recordingFailed = null;
      try {
        this.repository.transitionTask(
          taskId, current.task.state_revision, this.run_id,
          { kind: "requireVerification", item_id: itemId,
            reason: "gap decision result not confirmed after verifier decision; committed decision not yet established" });
      } catch (recording) {
        recordingFailed = recording;
      }
      if (recordingFailed !== null) {
        if (this.repository.gapDecision(decisionId) !== null) {
          return this.close(taskId, itemId, true, null, false);
        }
        throw recordingFailed;
      }
      throw outcome;
    }
    const current = this.repository.task(taskId);
    const cancelledWithoutDecision =
      outcome.kind === "conflict" &&
      current.task.status === "cancel_requested";
    return this.close(
      taskId, itemId, false, outcome, cancelledWithoutDecision);
  }
  decide(frozen, decisionId, verifier, now) {
    const [target, subject] = gapFrozenParts(parseGapFrozen(frozen));
    decideGap(this.repository, verifier, target, subject, decisionId, now);
  }
  /// Close the item and, once every item finished (or cancellation is
  /// pending), the task.
  close(taskId, itemId, success, error, cancelled) {
    const current = this.repository.task(taskId);
    let next = this.repository.transitionTask(
      taskId, current.task.state_revision, this.run_id,
      { kind: "finishItem", item_id: itemId,
        outcome: {
          success,
          error: error === null ? null
            : cancelled ? `cancelled before decision: ${error.message}`
            : error.message,
        } });
    if (next.items.every((i) => i.stage === "finished") ||
        next.task.status === "cancel_requested") {
      next = this.repository.transitionTask(
        taskId, next.task.state_revision, this.run_id,
        { kind: cancelled ? "finishCancellation" : "finishExecution" });
    }
    return success ? decided(next) : notDecided(next);
  }
}

// runner.rs decide-error classification: Error::OutcomeUnknown |
// Error::Storage | Error::Task(StorageUnavailable | OutcomeUnknown).
// In Node: MarketError kinds outcome_unknown/storage, plus the
// tc-layer StorageError codes for storage-level failures.
const isUncertainty = (e) =>
  (e instanceof MarketError &&
    (e.kind === "outcome_unknown" || e.kind === "storage")) ||
  (e instanceof StorageError &&
    (e.code === "outcome_unknown" || e.code === "unavailable" ||
     e.code === "busy" || e.code === "readonly" || e.code === "corrupt" ||
     e.code === "constraint" || e.code === "error"));
