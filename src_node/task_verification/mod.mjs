// NM-106 — task_verification decision layer (pure half). Ports
// task_verification/mod.rs constants + the task_control::Error surface
// and FrozenWork::validate needed by admission/decision/existing.
// Repository/control calls are injected seams — unwired calls throw
// typed errors, never silent success.
import { createHash } from "node:crypto";
import { ser } from "../storage/verification_storage.mjs";

export const DATASET = "task.verification";
export const CONTRACT_VERSION = "stopped-writes-task-v1";

// task_verification::command_key()
export const commandKey = () => ({
  operation: "verify_stopped_writes",
  dataset: DATASET,
  contract_version: CONTRACT_VERSION,
});

// admission::verification_scope — one resource per target task; taking
// it cannot release the original claims.
export const verificationScope = (taskId) => `verification:task:${taskId}`;

// ------------------------------------------------- task_control::Error
// Display parity: message carries the full Display text, `code` is the
// wire kind (same convention as market_update's taskErr helpers).
const tc = (code, message) =>
  Object.assign(new Error(message), { code, taskControl: true });
export const tcInvalid = (m) => tc("invalid_input", `INVALID_COMMAND: ${m}`);
export const tcConflict = () => tc("revision_conflict", "STATE_REVISION_CONFLICT");
export const tcNeedsVerification = (m) =>
  tc("needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: ${m}`);
export const tcUnavailable = (m) => tc("unavailable", `STORAGE_UNAVAILABLE: ${m}`);
export const tcOutcomeUnknown = (m) =>
  tc("outcome_unknown", `OUTCOME_UNKNOWN: ${m}; query the same request_id before retrying`);
export const tcRequestConflict = () => tc("request_conflict", "REQUEST_CONFLICT");
export const tcUnsupported = () => tc("unsupported_contract", "UNSUPPORTED_CONTRACT");
export const tcNotFound = () => tc("task_not_found", "TASK_NOT_FOUND");
export const tcInvalidRetry = () =>
  tc("invalid_retry_selection", "INVALID_RETRY_SELECTION");

export const TERMINAL = [
  "succeeded", "partially_failed", "failed", "cancelled", "interrupted",
];

// task_control::visible_status
export const visibleStatus = (status, accepted, owner, run) =>
  !TERMINAL.includes(status) &&
    (accepted !== run || (owner !== null && owner !== run))
    ? "needs_verification" : status;
export const snapVisible = (s, run) =>
  visibleStatus(s.task.status, s.task.accepted_run_id, s.task.owner_run_id, run);

// ------------------------------------------------- serde_json emitters
// serSnap: serde_json::to_vec(&TaskSnapshot) bytes — declaration-order
// struct fields, sorted dynamic Value keys, skip_serializing_if. Trunk
// `ser` (NM-059 fix) propagates child shapes into array elements, so it
// is byte-identical for canonical snapshots; keep the named wrapper so
// call sites state intent.
export const serSnap = (snap) => ser(snap, "snapshot");

// decision::snapshot_digest — sha256(serde_json::to_vec(&target)).
export const snapshotDigest = (snap) =>
  createHash("sha256").update(Buffer.from(serSnap(snap), "utf8")).digest("hex");

// FrozenWork::evidence_digest + validate (task_control/mod.rs).
export const evidenceDigest = (work) =>
  work.shared_evidence?.evidence_digest ?? null;
export const validateWork = (work) => {
  const ids = new Set();
  if (work.items.length === 0 || work.scopes.length === 0) {
    throw tcInvalid("empty work");
  }
  for (const item of work.items) {
    if (item.item_id.trim() === "" || item.progress_unit.trim() === "" ||
        ids.has(item.item_id)) {
      throw tcInvalid("invalid/duplicate item");
    }
    ids.add(item.item_id);
  }
  const bound = new Set();
  for (const i of work.items) for (const s of i.scope_keys) bound.add(s);
  if (bound.size !== work.scopes.length ||
      ![...bound].every((s) => work.scopes.includes(s)) ||
      work.items.some((i) => i.scope_keys.length === 0)) {
    throw tcInvalid("item resource bindings differ from scopes");
  }
  if (work.scopes.some((s) => s.trim() === "")) throw tcInvalid("empty scope");
  const digest = evidenceDigest(work);
  for (const item of work.items) {
    const marker = item.frozen_inputs?.selection_evidence?.evidence_digest;
    if (typeof marker === "string" && digest !== marker) {
      throw tcInvalid("item evidence marker mismatches shared evidence");
    }
  }
};

// ------------------------------------------------- repository seams
// Reads may be injected; control/store calls beyond reads throw typed
// task_control errors until the NM-084/S2 wiring lands.
const seam = (op) =>
  tcUnavailable(`task_verification seam not wired (NM-084 pending): ${op}`);

export class TaskRepositorySeam {
  read_task(_id) { throw seam("repository.read_task"); }
  read_request(_id) { throw seam("repository.read_request"); }
  accept(_id, _command, _work, _run, _guard) {
    throw seam("repository.accept");
  }
  transition(_id, _rev, _run, _change) {
    throw seam("repository.transition");
  }
}

export class TaskControlSeam {
  constructor(repository, runId) {
    this.repository = repository;
    this.run_id = runId;
  }
  accept(_id, _key, _payload) { throw seam("control.accept"); }
}
