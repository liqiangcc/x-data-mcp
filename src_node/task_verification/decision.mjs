// NM-106 — task_verification/decision.rs: decisions from trusted
// inspection output, never MCP evidence fields. StopPlan audit shape is
// identical to the NM-048 storage-bound port (verification_storage.
// prepareStop); this module is the contract-facing decision layer with
// task_control::Error Display text at the boundary.

import { canonEq } from "../storage/task_repository.mjs";
import {
  DATASET, CONTRACT_VERSION, serSnap, snapVisible, snapshotDigest,
  tcInvalid, tcNeedsVerification, tcConflict, tcUnavailable,
  verificationScope,
} from "./mod.mjs";

const DEBUG_STATUS = {
  pending: "Pending", running: "Running", cancel_requested: "CancelRequested",
  needs_verification: "NeedsVerification", succeeded: "Succeeded",
  partially_failed: "PartiallyFailed", failed: "Failed",
  cancelled: "Cancelled", interrupted: "Interrupted",
};

// -------------------------------------------------------- Observation
// Serialize-only shape in Rust (never Deserialize). Constructors below
// keep the tagged shape for probes/seams.
export const insufficient = (reason) => ({ kind: "insufficient", reason });
export const stopped = (snapshotSha256, evidenceRef) =>
  ({ kind: "stopped", snapshot_sha256: snapshotSha256, evidence_ref: evidenceRef });

// Conclusion serde values (snake_case).
export const Conclusion = {
  Interrupted: "interrupted",
  InsufficientEvidence: "insufficient_evidence",
  AlreadyVerified: "already_verified",
  NotApplicable: "not_applicable",
};

// ---------------------------------------------------------- seams
export class StopInspectorSeam {
  inspect(_target) {
    throw tcUnavailable(
      "task_verification seam not wired (NM-084 pending): inspector.inspect");
  }
}
export class UnverifiedStopInspector {
  inspect(_target) {
    return insufficient(
      "STOP_EVIDENCE_NOT_VERIFIED: no installed writer/effect evidence protocol");
  }
}
export class StopRepositorySeam {
  commit_stop_check(_plan) { throw tcUnavailable("stop_repository.commit_stop_check"); }
  stop_receipt(_verificationTaskId) {
    throw tcUnavailable("stop_repository.stop_receipt");
  }
}

// ------------------------------------------------------------ prepare
// decision::prepare verbatim. `verification`/`target` are canonical
// TaskSnapshot JSON objects; `now` is the RFC3339 'Z' timestamp string.
export const prepare = (verification, target, run, actor, now, inspector) => {
  if (actor.trim() === "" || run.trim() === "") {
    throw tcInvalid("verification actor/run missing");
  }
  const expectedScopes = [verificationScope(target.task.task_id)];
  const v0 = verification.items[0];
  if (verification.task.key.dataset !== DATASET ||
      verification.task.key.operation !== "verify_stopped_writes" ||
      verification.task.key.contract_version !== CONTRACT_VERSION ||
      !canonEq(verification.task.work.scopes, expectedScopes) ||
      verification.task.owner_run_id !== run ||
      snapVisible(verification, run) !== "running" ||
      verification.items.length !== 1 ||
      v0 === undefined ||
      v0.stage !== "running" ||
      v0.spec.item_id !== "stopped-writes" ||
      v0.spec.progress_unit !== "evidence_check" ||
      !canonEq(v0.spec.scope_keys, expectedScopes) ||
      // serde Value indexing on a missing/non-object yields Null.
      !canonEq(v0.spec.frozen_inputs?.accepted_run_id ?? null,
               target.task.accepted_run_id) ||
      !canonEq(v0.spec.frozen_inputs?.target_task_id ?? null,
               target.task.task_id)) {
    throw tcConflict();
  }
  if (target.task.key.dataset === DATASET) throw tcConflict();
  const observation = snapVisible(target, run) === "needs_verification"
    ? inspector.inspect(target) : null;

  const changed = (s) => {
    if (!Number.isSafeInteger(s.task.state_revision + 1)) throw tcConflict();
    s.task.state_revision += 1;
    s.task.last_progress_at = now;
    s.claims = [];
  };
  let after = null, conclusion, reason, evidenceRef = null;
  if (observation === null) {
    conclusion = target.task.status === "interrupted"
      ? Conclusion.AlreadyVerified : Conclusion.NotApplicable;
    reason = `target state ${DEBUG_STATUS[target.task.status] ?? target.task.status}; ` +
      "no new inspection or original-task mutation";
  } else if (observation.kind === "insufficient") {
    if (observation.reason.trim() === "") {
      throw tcInvalid("missing insufficiency reason");
    }
    conclusion = Conclusion.InsufficientEvidence;
    reason = observation.reason;
  } else {
    if (observation.snapshot_sha256 !== snapshotDigest(target) ||
        observation.evidence_ref.trim() === "") {
      throw tcNeedsVerification("stop evidence does not bind inspected target");
    }
    const next = JSON.parse(serSnap(target));
    next.task.status = "interrupted";
    for (const item of next.items) {
      if (item.stage === "running") item.stage = "pending";
    }
    next.task.result = {
      interruption: {
        verification_task_id: verification.task.task_id,
        evidence_ref: observation.evidence_ref,
      },
      unfinished_items: next.items
        .filter((i) => !(i.outcome !== null && i.outcome !== undefined &&
                         i.outcome.success === true))
        .map((i) => i.spec.item_id),
    };
    changed(next);
    after = next;
    conclusion = Conclusion.Interrupted;
    reason = "verified stopped; original task not resumed";
    evidenceRef = observation.evidence_ref;
  }
  const finished = JSON.parse(serSnap(verification));
  finished.task.status = "succeeded";
  finished.items[0].stage = "finished";
  finished.items[0].outcome = { success: true, error: null };
  changed(finished);
  const receipt = {
    verification_task_id: verification.task.task_id,
    target_task_id: target.task.task_id,
    conclusion,
    verification_revision: finished.task.state_revision,
    target_revision: after !== null ? after.task.state_revision
      : target.task.state_revision,
    reason,
  };
  finished.task.result = { stopped_write_check: receipt };
  if (after !== null) after.task.result.stopped_write_check = receipt;
  return {
    audit: {
      actor, checked_at: now,
      target_before: target, target_after: after,
      verification_before: verification, verification_after: finished,
      receipt, evidence_ref: evidenceRef,
    },
    receipt() { return this.audit.receipt; },
  };
};

// decision::validate_audit — replay prepare with the Recorded inspector
// (validates a historical audit, not fresh external evidence).
export const validateAudit = (audit) => {
  const recorded = {
    inspect(target) {
      if (audit.evidence_ref !== null && audit.evidence_ref !== undefined) {
        return stopped(snapshotDigest(target), audit.evidence_ref);
      }
      return insufficient(audit.receipt.reason);
    },
  };
  const run = audit.verification_before.task.owner_run_id ?? "";
  const expected = prepare(audit.verification_before, audit.target_before,
    run, audit.actor, audit.checked_at, recorded);
  if (!canonEq(expected.audit, audit)) {
    throw tcNeedsVerification("stopped check audit projections differ");
  }
};
