// NM-053 — atomic publish commit (legacy schema-18): update.rs
// commit_range + sqlite_market.rs commit_publication. Reuses the
// ported task write path (decideHead/persistDecided) and the NM-051
// read helpers. All writes happen inside one BEGIN IMMEDIATE tx.
import { createHash } from "node:crypto";
import { StorageError } from "../storage/sqlite_adapter.mjs";
import { decideHead, persistDecided, canonEq } from "../storage/task_repository.mjs";
import * as RC from "./record_codec.mjs";
import * as MR from "./market_read.mjs";
import * as MW from "./market_write.mjs";

const sha = (s) => createHash("sha256").update(s).digest("hex");
const integrity = (m) => new RC.MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const conflict = (m) => new RC.MarketError("conflict", `PUBLISH_PRECONDITION: ${m}`);
const invalid = (m) => new RC.MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);
const baseErr = (m) => new RC.MarketError("base", `BASE_UNAVAILABLE: ${m}`);

// serde struct-order serialization (Value members sorted via __ser)
const serPub = (p) =>
  `{"publication_id":${JSON.stringify(p.publication_id)},"version_id":${JSON.stringify(p.version_id)},"task_id":${JSON.stringify(p.task_id)},"item_id":${JSON.stringify(p.item_id)},"run_id":${JSON.stringify(p.run_id)},"expected_revision":${JSON.stringify(p.expected_revision)},"intent":${RC.__ser(p.intent)}}`;
const serReceipt = (r) =>
  `{"publication_id":${JSON.stringify(r.publication_id)},"version_id":${JSON.stringify(r.version_id)},"task_id":${JSON.stringify(r.task_id)},"task_revision":${JSON.stringify(r.task_revision)}}`;
const resultDigest = (auditJson, receiptJson) =>
  sha(`[${JSON.stringify(auditJson)},${JSON.stringify(receiptJson)}]`);

const periodName = (p) => p === "Day" ? "day" : "year";
const scopeOf = (v) => ({ symbol: v.symbol, period: v.period });
const occupancyKey = (scope) => `["market",${JSON.stringify(scope.symbol)},${JSON.stringify(periodName(scope.period))}]`;

const rawEq = (a, b) => RC.__ser(a) === RC.__ser(b);

// PublishPlan::check_task_binding — verbatim gates
const checkTaskBinding = (command, scope, binding) => {
  if (binding.head.state_revision !== BigInt(command.expected_revision) ||
      visibleStatus(binding.head, command.run_id) !== "running" ||
      binding.head.owner_run_id !== command.run_id) {
    throw conflict("task revision/owner/state");
  }
  const item = binding.item;
  if (item.stage !== "running" ||
      item.spec.item_id !== command.item_id ||
      !canonEq(item.spec.target, { symbol: scope.symbol, period: periodName(scope.period) }) ||
      !item.spec.scope_keys.includes(occupancyKey(scope)) ||
      !binding.scopes_bound) {
    throw conflict("task target/occupancy");
  }
};
const visibleStatus = (head, run) =>
  !["succeeded", "partially_failed", "failed", "cancelled", "interrupted"].includes(head.status) &&
  (head.accepted_run_id !== run || (head.owner_run_id !== null && head.owner_run_id !== run))
    ? "needs_verification" : head.status;

// PublishPlan::finish_task — binding check + decide FinishItem +
// conditional FinishExecution; mutates the binding in place.
const finishTask = (command, scope, binding, now) => {
  checkTaskBinding(command, scope, binding);
  decideHead(binding.head, binding.item, command.run_id, {
    kind: "finishItem", item_id: command.item_id,
    outcome: { success: true, error: null },
  }, now);
  if (binding.head.counts.finished === binding.head.counts.total) {
    decideHead(binding.head, null, command.run_id, { kind: "finishExecution" }, now);
  }
};

/** sqlite_market::commit_publication — verbatim order. */
export const commitPublication = (db, plan, now) => {
  const command = plan.command;
  const old = db.get(
    "SELECT command_json,receipt_json FROM publication_batches WHERE publication_id=?",
    command.publication_id);
  if (old) {
    const storedCommand = JSON.parse(old.command_json);
    if (!canonEq(storedCommand, command)) {
      throw conflict("publication identity reused with different command");
    }
    const stored = MR.readProjection(db, command.version_id, true, true);
    if (!rawEq(stored.raw, plan.record.raw)) {
      throw integrity("published record differs from prepared command");
    }
    return JSON.parse(old.receipt_json);
  }
  const reused = db.get(
    "SELECT EXISTS(SELECT 1 FROM market_update_results WHERE result_id=? UNION ALL SELECT 1 FROM yearly_execution_results WHERE result_id=?) AS x",
    command.publication_id, command.publication_id).x;
  if (reused !== 0n) {
    throw conflict("publication id used by a range result");
  }
  const record = MR.readProjection(db, command.version_id, false, true);
  if (!rawEq(record.raw, plan.record.raw)) {
    throw integrity("validated candidate changed");
  }
  const state = db.get(
    "SELECT state FROM data_versions WHERE version_id=?", command.version_id);
  if (!state) throw integrity("market version row missing");
  if (state.state !== "validated") {
    throw conflict("candidate is not validated");
  }
  const base = MR.__baseRecord(db, record);
  const baseDigest = base === null ? null : base.raw.integrity_sha256;
  if (baseDigest !== (plan.base_digest ?? null)) {
    throw baseErr("fixed base changed since validation");
  }
  for (const [scope, expected] of plan.required_heads) {
    if ((MR.head(db, scope) ?? null) !== expected) {
      throw conflict("effective head changed");
    }
  }
  const binding = MR.__readItemBinding(db, command.task_id, command.item_id);
  if (!binding) throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" });
  finishTask(command, scopeOf(record.version), binding, now);
  const receipt = {
    publication_id: command.publication_id,
    version_id: command.version_id,
    task_id: command.task_id,
    task_revision: Number(binding.head.state_revision),
  };
  db.run("INSERT INTO publication_batches VALUES(?,?,?,?,?,?)",
    command.publication_id, command.version_id, command.task_id,
    command.item_id, serPub(command), serReceipt(receipt));
  db.run("INSERT INTO task_publications VALUES(?,?,?)",
    command.task_id, command.version_id, command.publication_id);
  db.run(
    "UPDATE data_versions SET state='published' WHERE version_id=? AND state='validated'",
    command.version_id);
  if (plan.promote) {
    const changed = db.run(
      "INSERT INTO effective_heads VALUES(?,?,'qfq',?) ON CONFLICT(symbol,period,adjustment) DO UPDATE SET version_id=excluded.version_id",
      plan.promote.symbol, periodName(plan.promote.period), command.version_id);
    if (changed.changes !== 1) {
      throw conflict("head promotion did not write one row");
    }
  }
  persistDecided(db, binding.head, binding.item, command.expected_revision);
  MR.readProjection(db, command.version_id, true, true);
  return receipt;
};

/** update::commit_range — verbatim order. */
export const commitRange = (db, plan, now) => {
  const command = plan.publication.command;
  const stored = MR.readResult(db, command.publication_id);
  if (stored !== null) {
    if (!canonEq(stored[0], plan.audit)) {
      throw conflict("range result id reused with different inputs");
    }
    return stored[1];
  }
  const reused = db.get(
    "SELECT EXISTS(SELECT 1 FROM publication_batches WHERE publication_id=? UNION ALL SELECT 1 FROM yearly_execution_results WHERE result_id=?) AS x",
    command.publication_id, command.publication_id).x;
  if (reused !== 0n) {
    throw conflict("publication id already used");
  }
  const base = MR.readRecord(db, plan.base.version.version_id, true, true);
  if (!rawEq(base.raw, plan.base.raw)) {
    throw integrity("update base changed after range approval");
  }
  let receipt;
  if (plan.no_change) {
    for (const [scope, expected] of plan.publication.required_heads) {
      if ((MR.head(db, scope) ?? null) !== expected) {
        throw conflict("effective head changed");
      }
    }
    const binding = MR.__readItemBinding(db, command.task_id, command.item_id);
    if (!binding) throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" });
    finishTask(command, scopeOf(base.version), binding, now);
    receipt = {
      publication_id: command.publication_id,
      version_id: command.version_id,
      task_id: command.task_id,
      task_revision: Number(binding.head.state_revision),
    };
    persistDecided(db, binding.head, binding.item, command.expected_revision);
  } else {
    receipt = commitPublication(db, plan.publication, now);
  }
  MW.writePayloads(db, plan.audit_payloads ?? []);
  const auditJson = RC.__ser(plan.audit);
  const receiptJson = serReceipt(receipt);
  db.run(
    "INSERT INTO market_update_results VALUES(?,?,?,?,?,?,?,?,?)",
    command.publication_id, command.task_id, command.item_id,
    base.version.version_id, command.version_id,
    plan.no_change ? 1 : 0, auditJson, resultDigest(auditJson, receiptJson), receiptJson);
  const back = MR.readResult(db, command.publication_id);
  if (back === null) throw integrity("range receipt missing after insert");
  return back[1];
};

/** One BEGIN IMMEDIATE tx wrapping commit_range. */
export function commitRangeTx(db, plan, now) {
  db.run("BEGIN IMMEDIATE");
  try {
    const r = commitRange(db, plan, now);
    db.run("COMMIT");
    return r;
  } catch (e) {
    try { db.run("ROLLBACK"); } catch { /* rolled back */ }
    if (e instanceof StorageError) {
      throw new RC.MarketError("storage", `MARKET_STORAGE: ${e.message}`);
    }
    throw e;
  }
}
