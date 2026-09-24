// NM-106 — task_verification/existing.rs: returning a prior verified
// conclusion is a persisted request outcome, not a new inspection.
import { StopVerification } from "./admission.mjs";
import { Conclusion } from "./decision.mjs";
import {
  commandKey, tcInvalid, tcNeedsVerification, tcNotFound, tcUnavailable,
} from "./mod.mjs";

// TerminalResult::AlreadyVerified — #[serde(tag="kind",
// rename_all="snake_case", deny_unknown_fields)].
export const alreadyVerified = (receipt) =>
  ({ kind: "already_verified", receipt });

// ExistingStopRepository — bind_existing_stop is a store call: typed
// seam until NM-084.
export class ExistingStopRepositorySeam {
  bind_existing_stop(_plan) {
    throw tcUnavailable("task_verification seam not wired (NM-084 pending): bind_existing_stop");
  }
}

const existing = (repo, id, target) => {
  const check = target.task.result?.interruption?.verification_task_id;
  if (typeof check !== "string") {
    throw tcNeedsVerification("interruption evidence link missing");
  }
  const receipt = repo.stop_receipt(check);
  if (receipt === null || receipt === undefined) {
    throw tcNeedsVerification("interruption receipt missing");
  }
  if (target.task.status !== "interrupted" ||
      receipt.conclusion !== Conclusion.Interrupted ||
      receipt.target_task_id !== target.task.task_id ||
      receipt.target_revision !== target.task.state_revision) {
    throw tcNeedsVerification("prior stopping conclusion does not bind target");
  }
  const plan = {
    request_id: id,
    command: {
      key: commandKey(),
      intent: { target_task_id: target.task.task_id },
    },
    target,
    result: alreadyVerified(receipt),
  };
  return repo.bind_existing_stop(plan);
};

// existing::accept — `control` is a TaskControl seam {repository,
// run_id, accept(id, key, intent)}.
export const accept = (control, id, targetId) => {
  if (id.trim() === "") throw tcInvalid("empty request_id");
  const intent = new StopVerification(control.repository, control.run_id)
    .normalize({ target_task_id: targetId });
  const known = control.repository.read_request(id);
  if (known !== null && known !== undefined) {
    return control.accept(id, commandKey(), intent);
  }
  const target = control.repository.read_task(targetId);
  if (target === null || target === undefined) throw tcNotFound();
  if (target.task.status === "interrupted") {
    return existing(control.repository, id, target);
  }
  try {
    return control.accept(id, commandKey(), intent);
  } catch (error) {
    // Bounded recheck if the original finished between resolution reads.
    if (!(error && error.code === "invalid_input")) throw error;
    const current = control.repository.read_task(targetId);
    if (current === null || current === undefined) throw tcNotFound();
    if (current.task.status === "interrupted") {
      return existing(control.repository, id, current);
    }
    throw error;
  }
};
