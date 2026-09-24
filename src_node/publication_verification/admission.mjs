// NM-098 — src/publication_verification/admission.rs port.
// Admission checks task eligibility; heavy publication inspection
// runs later.
import { decode } from "../mcp/serde_value.mjs";
import { tStruct, tString, tEnumUnit, tOpt, anyValue }
  from "../mcp/serde_value.mjs";
import { createHash } from "node:crypto";
import { verificationScope, ser }
  from "../storage/verification_storage.mjs";
import { contractKind, CHECK_KEY,
  tcError, tcInvalidCommand, tcTaskNotFound, tcUnsupportedContract,
  tcNeedsVerification,
} from "./mod.mjs";

export const CONTRACT_VERSION = "publication-check-task-v1";
export const commandKey = () => CHECK_KEY;

// Input{target_task_id} deny_unknown_fields
const Input = tStruct("Input", [
  { name: "target_task_id", dec: tString },
]);

// serde_json::from_value decode -> InvalidCommand(e.to_string())
const decodeInput = (v) => {
  try { return decode(Input, v); }
  catch (e) { throw tcInvalidCommand(e.message); }
};

// task_control::visible_status — non-terminal && (accepted != run ||
// foreign owner) -> needs_verification.
const TERMINAL = ["succeeded", "partially_failed", "failed",
  "cancelled", "interrupted"];
export const visibleStatus = (task, run) =>
  !TERMINAL.includes(task.status) &&
    (task.accepted_run_id !== run ||
      (task.owner_run_id !== null && task.owner_run_id !== run))
    ? "needs_verification" : task.status;

// task_control::TaskSnapshot::execution_uncertainty — validates the
// persistent marker verbatim; absent marker is Ok(None), not failure.
const ExecutionUncertainty = tStruct("ExecutionUncertainty", [
  { name: "previous_status", dec: tEnumUnit("Status",
    ["pending", "running", "cancel_requested", "needs_verification",
      "succeeded", "partially_failed", "failed", "cancelled", "interrupted"]) },
  { name: "item_id", dec: tString },
  { name: "reason", dec: tString },
  { name: "previous_result", dec: tOpt(anyValue), opt: true }, // Option<Value>
]);
export const executionUncertainty = (snap) => {
  const value = snap.task.result?.execution_uncertainty;
  if (value === undefined) return null;
  let marker;
  try { marker = decode(ExecutionUncertainty, value); }
  catch (e) {
    throw tcNeedsVerification(`invalid execution uncertainty: ${e.message}`);
  }
  if (!["running", "cancel_requested"].includes(marker.previous_status) ||
      snap.task.status !== "needs_verification" ||
      snap.task.owner_run_id === null ||
      marker.reason.trim() === "" ||
      !snap.items.some((i) =>
        i.spec.item_id === marker.item_id && i.stage === "running")) {
    throw tcNeedsVerification(
      "execution uncertainty does not bind unresolved running item");
  }
  return marker;
};

export class PublicationVerification {
  constructor(repository, runId) {
    if (typeof runId !== "string" || runId.trim() === "") {
      throw tcInvalidCommand("empty verification run");
    }
    this.repository = repository;
    this.runId = runId;
  }
  key() { return commandKey(); }
  normalize(input) {
    const decoded = decodeInput(input);
    if (decoded.target_task_id.trim() === "") {
      throw tcInvalidCommand("empty target_task_id");
    }
    return { target_task_id: decoded.target_task_id };
  }
  resolve(intent) {
    const input = decodeInput(intent);
    const target = this.repository.readTask(input.target_task_id);
    if (target === null) throw tcTaskNotFound();
    if (contractKind(target.task.key) === null) throw tcUnsupportedContract();
    if (visibleStatus(target.task, this.runId) !== "needs_verification" ||
        !target.items.some((i) => i.stage === "running")) {
      throw tcInvalidCommand(
        "verification_not_applicable: no unresolved execution requiring publication inspection");
    }
    // Verified reads validate any persistent uncertainty marker. An old
    // run can also have crashed before recording it; not proof of failure.
    executionUncertainty(target);
    const scope = verificationScope(input.target_task_id);
    const item = {
      item_id: "publication-results",
      target: { task_id: input.target_task_id },
      progress_unit: "evidence_check",
      frozen_inputs: {
        target_task_id: input.target_task_id,
        admission_revision: target.task.state_revision,
        accepted_run_id: target.task.accepted_run_id,
        // snapshot_digest — sha256 of the canonical serde snapshot
        // bytes (identical to verification_storage.snapshotDigest on
        // the raw row; the seam already hands us the canonical shape).
        admission_snapshot_sha256: createHash("sha256")
          .update(ser(target, "snapshot"), "utf8").digest("hex"),
      },
      scope_keys: [scope],
    };
    return { scopes: [scope], items: [item], shared_evidence: null };
  }
}
