// NM-106 — task_verification/never_started.rs: proof for audited
// executors that must claim before performing any work. This does not
// establish stopping of an executor which has already started.
import { canonEq } from "../storage/task_repository.mjs";
import { insufficient, stopped } from "./decision.mjs";
import { snapshotDigest, tcConflict, tcNeedsVerification, tcUnavailable } from "./mod.mjs";

// LegacyCollection.key() + supported contract table (never_started.rs).
const LEGACY_KEY = {
  operation: "start_collection",
  dataset: "legacy_market_compound",
  contract_version: "task-acceptance-v1",
};
const MARKET_CONTRACT = "explicit-range-task-v1";   // market_update::tasks
const YEARLY_CONTRACT = "explicit-yearly-task-v1";  // yearly_publication::tasks

const supported = (s) => {
  const key = s.task.key;
  return canonEq(key, LEGACY_KEY) ||
    (key.operation === "start_collection" &&
      key.contract_version === "v1" &&
      ["market.daily", "market.yearly", "securities.catalog"].includes(key.dataset)) ||
    (["update_data", "backfill_data"].includes(key.operation) &&
      ((key.dataset === "market.daily" && key.contract_version === MARKET_CONTRACT) ||
       (key.dataset === "market.yearly" && key.contract_version === YEARLY_CONTRACT)));
};

// never_started::EvidenceRepository — one verified SQLite read snapshot.
export class NeverStartedEvidenceSeam {
  never_started_facts(_taskId) {
    throw tcUnavailable("task_verification seam not wired (NM-084 pending): never_started_facts");
  }
}

export class NeverStarted {
  constructor(repository) { this.repository = repository; }
  inspect(target) {
    if (!supported(target) ||
        target.task.status !== "pending" ||
        target.task.state_revision !== 0 ||
        target.task.owner_run_id !== null ||
        target.task.result !== null ||
        target.items.length === 0 ||
        target.items.some((i) => i.stage !== "pending" ||
          (i.outcome !== null && i.outcome !== undefined))) {
      return insufficient(
        "STOP_EVIDENCE_NOT_VERIFIED: task is not an audited never-started execution");
    }
    const facts = this.repository.never_started_facts(target.task.task_id);
    if (!canonEq(facts.snapshot, target)) throw tcConflict();
    if (facts.has_durable_effects) {
      throw tcNeedsVerification(
        "never-started task has durable publication/result associations");
    }
    return stopped(snapshotDigest(target),
      "sqlite-never-started-v1: revision-zero/no-owner/no-outcomes/no-effects; " +
      "conditional commit excludes a later claim");
  }
}
