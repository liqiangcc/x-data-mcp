// NM-056 — sqlite_market/publication_checks.rs bound-read surface:
// read_audit + validate_audit (decision::prepare replay) +
// validate_task_link wired into the bound read. commit_check_tx /
// validate_terminal / known_publication are verification commit-side
// paths and stay out of this slice.
import { createHash } from "node:crypto";
import { canonEq } from "./task_repository.mjs";
import {
  snapshotCanon, snapshotEq, verificationScope, writeTaskSnapshot,
  readRequestRecord, ser,
} from "./verification_storage.mjs";
import { StorageError } from "./sqlite_adapter.mjs";
import { MarketError } from "../market/record_codec.mjs";
import { contractKind } from "../market/verification_facts.mjs";

const sha256 = (s) => createHash("sha256").update(s).digest("hex");

// command_key() — publication_verification::admission
export const CHECK_KEY = {
  operation: "verify_publication_result",
  dataset: "task.verification",
  contract_version: "publication-check-task-v1",
};

const TASK_TERMINAL = ["succeeded", "partially_failed", "failed", "cancelled", "interrupted"];
const visibleStatusOf = (status, accepted, owner, run) =>
  !TASK_TERMINAL.includes(status) && (accepted !== run || (owner !== null && owner !== run))
    ? "needs_verification" : status;

const tcNeedsVerification = (m) =>
  new StorageError("needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: ${m}`);
const tcConflict = () =>
  new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
const integrity = (m) =>
  new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const storage = (m) =>
  new MarketError("storage", `MARKET_STORAGE: ${m}`);

// publication_checks::read_audit — sha256 gate, association, replay
export const readCheckAudit = (db, id) => {
  const row = db.get(
    "SELECT target_task_id,audit_json,audit_sha256 FROM publication_checks WHERE verification_task_id=?",
    id);
  if (!row) return null;
  if (sha256(row.audit_json) !== row.audit_sha256) {
    throw integrity("publication check audit digest differs");
  }
  let audit;
  try { audit = JSON.parse(row.audit_json); }
  catch (e) { throw integrity(e.message); }
  if (audit.receipt?.verification_task_id !== id ||
      audit.receipt?.target_task_id !== row.target_task_id) {
    throw integrity("publication check association differs");
  }
  validateCheckAudit(audit);
  return audit;
};

// decision::prepare — rebuild the expected audit from inputs
export const prepareCheck = (check, facts, run, actor, now) => {
  const target = facts.task;
  const scope = [verificationScope(target.task.task_id)].sort();
  const checkScopes = [...(check.task.work.scopes ?? [])].sort();
  const item = check.items[0];
  const itemScopes = item === undefined ? [] : [...item.spec.scope_keys].sort();
  if (typeof run !== "string" || run.trim() === "" ||
      typeof actor !== "string" || actor.trim() === "" ||
      !canonEq(check.task.key, CHECK_KEY) ||
      visibleStatusOf(check.task.status, check.task.accepted_run_id,
        check.task.owner_run_id, run) !== "running" ||
      check.task.owner_run_id !== run ||
      !canonEq(checkScopes, scope) ||
      check.items.length !== 1 || item.stage !== "running" ||
      item.spec.item_id !== "publication-results" ||
      item.spec.progress_unit !== "evidence_check" ||
      !canonEq(itemScopes, scope) ||
      item.spec.frozen_inputs?.target_task_id !== target.task.task_id ||
      item.spec.frozen_inputs?.accepted_run_id !== target.task.accepted_run_id ||
      contractKind(target.task.key) === null) {
    throw tcConflict();
  }
  if (facts.items.length !== target.items.length || facts.items.length === 0) {
    throw tcNeedsVerification("incomplete publication evidence item set");
  }
  const ids = new Set();
  let all = true;
  for (const fact of facts.items) {
    const item2 = target.items.find((i) => i.spec.item_id === fact.item_id);
    if (!item2) {
      throw tcNeedsVerification("publication fact has unknown item");
    }
    if (ids.has(fact.item_id)) {
      throw tcNeedsVerification("duplicate publication evidence item");
    }
    ids.add(fact.item_id);
    if (fact.evidence === "Missing") {
      if (item2.outcome !== null && item2.outcome?.success === true) {
        throw tcNeedsVerification("successful item lacks publication evidence");
      }
      all = false;
    } else {
      const receipt = fact.evidence?.Committed?.receipt;
      const ev = fact.evidence?.Committed?.evidence_sha256;
      if (item2.stage !== "finished" ||
          item2.outcome === null || item2.outcome?.success !== true ||
          receipt.task_id !== target.task.task_id ||
          receipt.task_revision > target.task.state_revision ||
          typeof ev !== "string" || ev.length !== 64 ||
          !/^[0-9a-f]+$/.test(ev)) {
        throw tcNeedsVerification("publication evidence does not bind successful item");
      }
    }
  }
  const conclusion = all ? "all_recorded"
    : visibleStatusOf(target.task.status, target.task.accepted_run_id,
        target.task.owner_run_id, run) === "needs_verification"
      ? "insufficient_evidence" : "not_applicable";
  const missingIds = facts.items
    .filter((f) => f.evidence === "Missing")
    .map((f) => f.item_id);
  const reason = conclusion === "all_recorded"
    ? "all task items have verified fixed publication evidence; this does not prove writers stopped"
    : conclusion === "insufficient_evidence"
      ? "one or more task items lack a committed publication receipt; absence does not prove failure or non-publication; original protection retained"
      : "target no longer requires publication inspection; missing receipts are not classified as publication failure";
  const after = JSON.parse(JSON.stringify(check));
  after.task.status = "succeeded";
  const rev = BigInt(after.task.state_revision) + 1n;
  if (rev > 18446744073709551615n) throw tcConflict();
  after.task.state_revision = Number(rev);
  after.task.last_progress_at = now;
  // ItemOutcome serde: evidence is skip_serializing_if Option::is_none —
  // the finished item emits {success:true,error:null} verbatim.
  after.items[0].stage = "finished";
  after.items[0].outcome = { success: true, error: null };
  after.claims = [];
  const receipt = {
    verification_task_id: check.task.task_id,
    target_task_id: target.task.task_id,
    verification_revision: Number(rev),
    target_revision: Number(target.task.state_revision),
    conclusion, reason, missing_receipt_item_ids: missingIds,
  };
  after.task.result = { publication_check: receipt };
  return {
    audit: {
      actor, checked_at: now, facts,
      verification_before: check, verification_after: after,
      receipt,
    },
  };
};

// decision::validate_audit — replay prepare over the embedded snapshots
export const validateCheckAudit = (audit) => {
  const before = audit.verification_before;
  const run = before?.task?.owner_run_id ?? "";
  const expected = prepareCheck(before, audit.facts, run, audit.actor,
    audit.checked_at);
  if (!canonEq(expected.audit, audit)) {
    throw new StorageError("needs_verification",
      "ACCEPTANCE_NEEDS_VERIFICATION: publication check audit projections differ");
  }
};

// publication_checks::read_receipt — audit + task completion link;
// an all_recorded conclusion re-verifies the facts read.
export const readCheckReceipt = (db, id, readBound, readFactsFn) => {
  const audit = readCheckAudit(db, id);
  if (audit === null) {
    // read_bound_task — binding must exist for the row-less id
    readBound(id);
    return null;
  }
  const task = readBound(id);
  if (task === null) throw integrity("publication check task missing");
  if (!snapshotEq(task, audit.verification_after)) {
    throw integrity("publication check completion differs");
  }
  if (audit.receipt.conclusion === "all_recorded") {
    const current = readFactsFn(audit.receipt.target_task_id);
    const currentItems = current.items.map((f) => ({
      item_id: f.item_id, evidence: f.evidence }));
    if (!canonEq(currentItems, audit.facts.items.map((f) => ({
      item_id: f.item_id, evidence: f.evidence })))) {
      throw integrity("confirmed publication evidence changed");
    }
  }
  return [audit, audit.receipt];
};

// publication_checks::validate_task_link — the bound-read registry.
// Maps read-side failures to task errors verbatim (Task passthrough,
// Storage -> StorageUnavailable, other -> NeedsVerification(Display)).
export const validateTaskLink = (db, snap) => {
  const key = typeof snap.task.command_key_json === "string"
    ? JSON.parse(snap.task.command_key_json)
    : snap.task.key;
  if (!canonEq(key, CHECK_KEY) || snap.task.status !== "succeeded") {
    return;
  }
  let audit;
  try {
    audit = readCheckAudit(db, snap.task.task_id);
  } catch (e) {
    if (e instanceof StorageError && e.code === "needs_verification") throw e;
    if (e.kind === "storage") {
      throw new StorageError("unavailable", `STORAGE_UNAVAILABLE: ${e.message}`);
    }
    throw tcNeedsVerification(e.message);
  }
  if (audit === null) {
    throw tcNeedsVerification("publication check audit missing");
  }
  // audit.verification_after == *task — the stored snapshot uses the
  // canonical serde shape; compare canonically.
  if (!snapshotEq(snap, audit.verification_after)) {
    throw tcNeedsVerification("publication check task differs from audit");
  }
};

// ---------------------------------------------------------------------
// NM-059 — publication_checks.rs commit surface: commit_check_tx /
// commit_check / check_receipt / known_publication / bind_known_publication
// / validate_terminal.

const marketTaskErr = (code, msg) => new MarketError(code, msg);
const mConflict = (m) => new MarketError("conflict", `PUBLISH_PRECONDITION: ${m}`);

/** commit_check_tx — connection-level, shared by both storage backends
 *  (the publication_checks audit table is schema-neutral). Callers hold
 *  the tx. */
export const commitCheckTx = (db, plan, readBound, readFactsFn) => {
  const audit = plan.audit;
  const id = audit.receipt.verification_task_id;
  const prior = readCheckReceipt(db, id, readBound, readFactsFn);
  if (prior !== null) {
    if (!canonEq(prior[0], audit)) {
      throw mConflict("publication check replay differs");
    }
    return prior[1];
  }
  const check = readBound(id);
  if (check === null) {
    throw marketTaskErr("task_not_found", "TASK_NOT_FOUND");
  }
  const facts = readFactsFn(audit.receipt.target_task_id);
  const liveFacts = { ...facts, task: snapshotCanon(facts.task) };
  if (!snapshotEq(check, audit.verification_before) ||
      !canonEq(liveFacts, audit.facts)) {
    throw mConflict("publication check revision or evidence changed");
  }
  writeTaskSnapshot(db, audit.verification_after, check.task.state_revision);
  const raw = ser(audit, "checkAudit");
  try {
    db.run("INSERT INTO publication_checks VALUES(?,?,?,?)",
      id, audit.receipt.target_task_id, raw, sha256(raw));
  } catch (e) { throw new MarketError("storage", `MARKET_STORAGE: ${e.message}`); }
  const stored = readCheckReceipt(db, id, readBound, readFactsFn);
  if (!stored) throw integrity("publication check receipt missing after insert");
  return stored[1];
};

/** AuditStore::commit_check — SqliteMarket::write_transaction(true) —
 *  BEGIN IMMEDIATE; connection-level commit wrapped in one tx. */
export const commitCheck = (db, plan, readBound, readFactsFn) =>
  db.transaction(() => commitCheckTx(db, plan, readBound, readFactsFn));

/** AuditStore::check_receipt — receipt-only read. */
export const checkReceipt = (db, id, readBound, readFactsFn) =>
  readCheckReceipt(db, id, readBound, readFactsFn)?.[1] ?? null;

// publication_checks::validate_terminal — the known-check terminal arm.
// Error mapping: Task passthrough, Storage -> StorageUnavailable, other
// -> NeedsVerification(Display).
export function validateTerminal(db, command, value, readBound, readFactsFn) {
  const key = command?.key ?? command;
  if (!canonEq(key, CHECK_KEY)) return;
  const tc = (e) => {
    if (e instanceof StorageError) throw e;
    if (e instanceof MarketError && e.kind !== "integrity") {
      // Error::Task is transparent — rethrow the carried code/message
      throw new StorageError(e.code, e.message);
    }
    if (e?.kind === "storage") {
      throw new StorageError("unavailable", `STORAGE_UNAVAILABLE: ${e.message}`);
    }
    throw tcNeedsVerification(String(e?.message ?? e));
  };
  let result;
  try { result = typeof value === "string" ? JSON.parse(value) : value; }
  catch (e) { throw tc(e); }
  // TerminalResult serde is internally tagged: {"kind":"already_verified",...}
  const expected = result?.kind === "already_verified" ? result.receipt : undefined;
  if (expected === undefined) throw tc(integrity("invalid terminal result"));
  if (expected.conclusion !== "all_recorded" ||
      !canonEq(command.intent, { target_task_id: expected.target_task_id })) {
    throw tc(integrity("known publication request intent/conclusion differs"));
  }
  let got;
  try { got = readCheckReceipt(db, expected.verification_task_id, readBound, readFactsFn); }
  catch (e) { throw tc(e); }
  if (!got || !canonEq(got[1], expected)) {
    throw tc(integrity("known publication receipt missing or changed"));
  }
}

/** KnownRepository::known_publication — audit-table UNION +
 *  succeeded-task scan; first all_recorded receipt wins. */
export const knownPublication = (db, target, readBound, readFactsFn) => {
  const rows = db.query(
    `SELECT verification_task_id AS id FROM publication_checks WHERE target_task_id=?
     UNION
     SELECT t.task_id FROM tasks t JOIN task_items i ON i.task_id=t.task_id
      WHERE t.status='succeeded'
        AND json_extract(t.command_key_json,'$.operation')='verify_publication_result'
        AND json_extract(i.target_json,'$.task_id')=?
     ORDER BY id`, target, target);
  for (const r of rows) {
    let got;
    try { got = readCheckReceipt(db, r.id, readBound, readFactsFn); }
    catch (e) { tc(e); }
    if (got !== null) {
      const receipt = got[1];
      if (receipt.target_task_id !== target) {
        tc(integrity("known check targets another task"));
      }
      if (receipt.conclusion === "all_recorded") return receipt;
    }
  }
  return null;
};
const tc = (e) => {
  if (e instanceof StorageError) throw e;
  if (e?.kind === "storage") {
    throw new StorageError("unavailable", `STORAGE_UNAVAILABLE: ${e.message}`);
  }
  throw tcNeedsVerification(String(e?.message ?? e));
};

/** KnownRepository::bind_known_publication — replay -> bound equality ->
 *  validate_terminal -> terminal request insert (SqliteTasks::write). */
export const bindKnownPublication = (db, plan, readBound, readFactsFn) =>
  db.transaction(() => {
    const record = readRequestRecord(db, plan.request_id, readBound, readFactsFn);
    if (record !== null) {
      if (!canonEq(record.command, plan.command)) {
        throw new StorageError("conflict", "REQUEST_CONFLICT");
      }
      return { record, reused: true };
    }
    const actual = readBound(plan.target.task.task_id);
    if (!actual || !snapshotEq(actual, plan.target)) {
      throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
    }
    const value = plan.result;
    validateTerminal(db, plan.command, value, readBound, readFactsFn);
    const cmdJson = ser(plan.command, "command");
    const termJson = ser(value);
    db.run("INSERT INTO requests VALUES(?,?,?,'terminal',NULL,?)",
      plan.request_id, cmdJson, sha256(cmdJson), termJson);
    return {
      record: { request_id: plan.request_id, command: plan.command,
        outcome: { Terminal: value } },
      reused: false,
    };
  });
