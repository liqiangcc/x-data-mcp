// NM-106 — task_verification/determinate.rs: proof for audited
// executors that claim an item before any write. Plus the Audited
// inspector chain (never_started first, then determinate).
import { canonEq } from "../storage/task_repository.mjs";
import { insufficient, stopped } from "./decision.mjs";
import { NeverStarted } from "./never_started.mjs";
import { snapshotDigest, tcConflict, tcUnavailable } from "./mod.mjs";

const LEGACY_KEY = {
  operation: "start_collection",
  dataset: "legacy_market_compound",
  contract_version: "task-acceptance-v1",
};
const MARKET_CONTRACT = "explicit-range-task-v1";
const REPUBLISH_CONTRACT = "explicit-republish-task-v1"; // market_update::tasks
const YEARLY_CONTRACT = "explicit-yearly-task-v1";

const supported = (s) => {
  const key = s.task.key;
  return canonEq(key, LEGACY_KEY) ||
    (key.operation === "start_collection" &&
      key.contract_version === "v1" &&
      ["market.daily", "market.yearly", "securities.catalog"].includes(key.dataset)) ||
    (["update_data", "backfill_data"].includes(key.operation) &&
      ((key.dataset === "market.daily" && key.contract_version === MARKET_CONTRACT) ||
       (key.dataset === "market.yearly" && key.contract_version === YEARLY_CONTRACT))) ||
    (key.operation === "republish_data" &&
      key.dataset === "market.daily" &&
      key.contract_version === REPUBLISH_CONTRACT);
};

// determinate::EvidenceRepository — one verified SQLite read snapshot.
export class DeterminateEvidenceSeam {
  determinate_facts(_taskId) {
    throw tcUnavailable("task_verification seam not wired (NM-084 pending): determinate_facts");
  }
}

export class Determinate {
  constructor(repository) { this.repository = repository; }
  inspect(target) {
    if (!supported(target) || target.items.length === 0) {
      return insufficient(
        "STOP_EVIDENCE_NOT_VERIFIED: task is not an audited determinate execution");
    }
    const facts = this.repository.determinate_facts(target.task.task_id);
    if (!canonEq(facts.snapshot, target)) throw tcConflict();
    let finished = 0, landed = 0, uncommitted = 0;
    for (const item of target.items) {
      const started = facts.started_items.includes(item.spec.item_id);
      if (item.stage === "finished") {
        finished += 1;
        if (item.outcome === null || item.outcome === undefined || !started) {
          return insufficient(
            "STOP_EVIDENCE_NOT_VERIFIED: finished item lacks its audited start/outcome");
        }
      } else if (item.stage === "pending") {
        if (started) {
          return insufficient(
            "STOP_EVIDENCE_NOT_VERIFIED: pending item has a prior execution start; its write outcome is unknown");
        }
      } else if (item.stage === "running") {
        if (!started) {
          return insufficient(
            "STOP_EVIDENCE_NOT_VERIFIED: running item lacks its audited start marker");
        }
        if (facts.committed_items.includes(item.spec.item_id)) {
          landed += 1;
        } else {
          uncommitted += 1;
        }
      }
    }
    if (finished + landed === 0 && facts.has_durable_effects) {
      return insufficient(
        "STOP_EVIDENCE_NOT_VERIFIED: durable effects without a determinate write");
    }
    return stopped(snapshotDigest(target),
      `sqlite-determinate-items-v1: ${finished} finished items have audited ` +
      `start+outcome; ${target.items.length - finished - landed - uncommitted} ` +
      `pending items have no execution-start marker; ${landed} in-flight items ` +
      `landed (committed receipt), ${uncommitted} in-flight items have no ` +
      `committed receipt; conditional commit binds the inspected snapshot`);
  }
}

// The never-started proof first, then the started-but-determinate proof.
export class Audited {
  constructor(repository) { this.repository = repository; }
  inspect(target) {
    const first = new NeverStarted(this.repository).inspect(target);
    if (first.kind === "stopped") return first;
    return new Determinate(this.repository).inspect(target);
  }
}
