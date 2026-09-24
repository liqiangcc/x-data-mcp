// NM-098 — src/publication_verification/existing.rs port.
// Bind a new global request to a prior verified conclusion, without
// inspection.
import { PublicationVerification, commandKey } from "./admission.mjs";
import { CONCLUSION } from "./decision.mjs";
import { contractKind,
  tcInvalidCommand, tcTaskNotFound, tcUnsupportedContract,
  tcNeedsVerification, tcRequestConflict, tcError,
} from "./mod.mjs";

// TerminalResult — serde internally-tagged {"kind":"already_verified",
// "receipt":{...}}; receipt() accessor.
export const terminalAlreadyVerified = (rcpt) =>
  ({ kind: "already_verified", receipt: rcpt });
export const terminalReceipt = (r) => r.receipt;

// KnownPlan { request_id, command, target, result } — consumed by the
// store's bindKnownPublication (commit side lands with NM-059).

/** existing::known — target read + contract gate + prior-conclusion
 *  binding. Returns Acceptance or null when no known publication. */
const known = (repository, id, targetId) => {
  const target = repository.readTask(targetId);
  if (target === null) throw tcTaskNotFound();
  if (contractKind(target.task.key) === null) throw tcUnsupportedContract();
  const rcpt = repository.knownPublication(targetId);
  if (rcpt === null) return null;
  if (rcpt.conclusion !== CONCLUSION.ALL_RECORDED ||
      rcpt.target_task_id !== targetId) {
    throw tcNeedsVerification("known publication conclusion does not bind target");
  }
  return repository.bindKnownPublication({
    request_id: id,
    command: {
      key: commandKey(),
      intent: { target_task_id: targetId },
    },
    target,
    result: terminalAlreadyVerified(rcpt),
  });
};

// task_control::Error::InvalidCommand — either surface: domain-layer
// MarketError(kind "task") or storage-layer StorageError(invalid_input).
const isInvalidCommand = (e) =>
  e instanceof Error &&
  (e.kind === "task" || e.code === "invalid_input") &&
  typeof e.message === "string" && e.message.startsWith("INVALID_COMMAND: ");

/** existing::accept — TaskControl::accept wrapper with the known
 *  conclusion fast path and the InvalidCommand fallback re-check. */
export const accept = (control, id, target) => {
  if (typeof id !== "string" || id.trim() === "") {
    throw tcInvalidCommand("empty request_id");
  }
  const intent = new PublicationVerification(
    control.repository, control.runId)
    .normalize({ target_task_id: target });
  if (control.repository.readRequest(id) !== null) {
    return control.accept(id, commandKey(), intent);
  }
  const bound = known(control.repository, id, target);
  if (bound !== null) return bound;
  try {
    return control.accept(id, commandKey(), intent);
  } catch (error) {
    if (!isInvalidCommand(error)) throw error;
    return known(control.repository, id, target) ?? (() => { throw error; })();
  }
};
