// NM-106 — task_verification/admission.rs: explicit verification use
// cases; accepting an inspection never stops a writer.
import { createHash } from "node:crypto";
import { tStruct, tString } from "../market_update/serde.mjs";

import {
  DATASET, commandKey, serSnap, snapVisible, tcInvalid, tcNotFound,
  verificationScope,
} from "./mod.mjs";

const sha256 = (b) => createHash("sha256").update(b).digest("hex");

// #[serde(deny_unknown_fields)] struct Input { target_task_id }
const tInput = tStruct("Input", [{ name: "target_task_id", dec: tString }], true);

const invalid = (e) => tcInvalid(e.message ?? String(e));

export class StopVerification {
  constructor(repository, runId) {
    if (runId.trim() === "") throw tcInvalid("empty verification run");
    this.repository = repository;
    this.run_id = runId;
  }
  key() { return commandKey(); }
  // Contract::normalize — decode, trim gate, re-serialize (struct order).
  normalize(payload) {
    let input;
    try { input = tInput(payload); } catch (e) { throw invalid(e); }
    if (input.target_task_id.trim() === "") {
      throw tcInvalid("empty target_task_id");
    }
    return { target_task_id: input.target_task_id };
  }
  // Contract::resolve — bound target read + eligibility gates +
  // FrozenWork construction (serde_json::to_value ordering verbatim).
  resolve(intent) {
    let input;
    try { input = tInput(intent); } catch (e) { throw invalid(e); }
    const target = this.repository.read_task(input.target_task_id);
    if (target === null || target === undefined) throw tcNotFound();
    if (target.task.key.dataset === DATASET) {
      throw tcInvalid(
        "verification tasks require the separate bounded exception inspection");
    }
    if (snapVisible(target, this.run_id) !== "needs_verification") {
      throw tcInvalid(
        "verification_not_applicable: target is not awaiting verification");
    }
    const scopes = [verificationScope(input.target_task_id)];
    const item = {
      item_id: "stopped-writes",
      target: { task_id: input.target_task_id },
      progress_unit: "evidence_check",
      frozen_inputs: {
        target_task_id: input.target_task_id,
        admission_revision: target.task.state_revision,
        admission_snapshot_sha256: sha256(
          Buffer.from(serSnap(target), "utf8")),
        accepted_run_id: target.task.accepted_run_id,
        owner_run_id: target.task.owner_run_id,
      },
      scope_keys: scopes,
    };
    return {
      scopes: [...item.scope_keys],
      items: [item],
      shared_evidence: null,
    };
  }
}
export { invalid };
