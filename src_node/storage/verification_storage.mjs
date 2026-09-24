// NM-048 — port of sqlite_verification.rs + sqlite_verification_inspection.rs
// + sqlite_verification_runtime.rs. Durable audit receipts, task-link
// validators (wired into the bound read), inspection predicates, and the
// runtime registration / claim guard. Fail-closed, verbatim error texts.
import { createHash } from "node:crypto";
import { canonEq } from "./task_repository.mjs";
import { serdeDateTime } from "../market/compact_market.mjs";
import { validateTerminal as pubValidateTerminal } from "./publication_checks.mjs";
import { validateInspectionAudit } from "./verification_inspection.mjs";
import { StorageError } from "./sqlite_adapter.mjs";

const sha256 = (b) => createHash("sha256").update(b).digest("hex");
// Error::NeedsVerification Display carries the ACCEPTANCE_ prefix — emit it
// verbatim (the differential comparator checks needs_verification messages).
const integrity = (m) =>
  new StorageError("needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: ${m}`);
const storage = (m) => new StorageError("unavailable", String(m));

export const VERIFICATION_KEY = {
  operation: "verify_stopped_writes",
  dataset: "task.verification",
  contract_version: "stopped-writes-task-v1",
};
export const PUBLICATION_KEY = {
  operation: "verify_publication_result",
  dataset: "task.verification",
  contract_version: "publication-check-task-v1",
};
export const INSPECTION_KEY = {
  operation: "inspect_verification_task",
  dataset: "task.verification",
  contract_version: "v1",
};
export const verificationScope = (taskId) => `verification:task:${taskId}`;
const supportedVerification = (key) =>
  canonEq(key, VERIFICATION_KEY) || canonEq(key, PUBLICATION_KEY);
const TERMINAL = ["succeeded", "partially_failed", "failed", "cancelled", "interrupted"];

// ---------------------------------------------------------------------
// Serde-canonical JSON: struct fields in declaration order, dynamic maps
// (Value positions) sorted — Rust serde_json/BTreeMap parity.
const STRUCT_ORDER = {
  snapshot: ["task", "items", "claims"],
  task: ["task_id", "key", "status", "state_revision", "accepted_run_id",
    "owner_run_id", "created_at", "last_progress_at", "work", "result", "retry_of"],
  key: ["operation", "dataset", "contract_version"],
  work: ["items", "scopes", "shared_evidence"],
  itemSpec: ["item_id", "target", "progress_unit", "frozen_inputs", "scope_keys"],
  item: ["spec", "stage", "outcome"],
  claim: ["scope_key", "task_id", "state_revision"],
  outcome: ["success", "error", "evidence"],
  receipt: ["verification_task_id", "target_task_id", "conclusion",
    "verification_revision", "target_revision", "reason"],
  audit: ["actor", "checked_at", "target_before", "target_after",
    "verification_before", "verification_after", "receipt", "evidence_ref"],
  descriptor: ["protocol", "task_id", "run_id", "boot_id", "database_path",
    "database_identity", "lock_path", "lock_identity"],
  identity: ["device", "inode"],
  registration: ["claim_revision", "descriptor"],
  inspectionAudit: ["request_id", "command", "actor", "run_id", "checked_at",
    "facts", "observation", "after", "result"],
  command: ["key", "intent"],
  facts: ["target", "original", "registration", "finished",
    "has_business_effects", "completion_digest"],
  checkAudit: ["actor", "checked_at", "facts", "verification_before",
    "verification_after", "receipt"],
  checkFacts: ["task", "items"],
  itemEvidence: ["item_id", "evidence"],
  evidence: ["Missing", "Committed"],
  committed: ["receipt", "reused_version", "evidence_sha256"],
  pubReceipt: ["publication_id", "version_id", "task_id", "task_revision"],
  inspectionResult: ["kind", "receipt"],
  checkReceipt: ["verification_task_id", "target_task_id",
    "verification_revision", "target_revision", "conclusion", "reason",
    "missing_receipt_item_ids"],
  inspectionReceipt: ["request_id", "target_task_id", "actor", "checked_at",
    "conclusion", "target_revision", "target_status", "target_result",
    "reason"],
};

const SKIP_NONE = new Set(["shared_evidence", "evidence"]); // skip_serializing_if

export const ser = (v, shape) => {
  if (v === null || v === undefined) return "null";
  if (typeof v !== "object") return JSON.stringify(v);
  // Arrays carry the element shape (childShape returns the element type
  // for array-typed keys) — each element is serialized with that shape.
  if (Array.isArray(v)) return `[${v.map((x) => ser(x, shape)).join(",")}]`;
  const order = shape ? STRUCT_ORDER[shape] : null;
  const keys = order
    ? order.filter((k) => k in v && !(SKIP_NONE.has(k) && v[k] === null))
    : Object.keys(v).sort();
  return `{${keys.map((k) => `${JSON.stringify(k)}:${ser(v[k], childShape(shape, k))}`).join(",")}}`;
};
const childShape = (shape, key) => {
  if (shape === "snapshot") return { task: "task", items: "item", claims: "claim" }[key];
  if (shape === "task") return { key: "key", work: "work" }[key];
  if (shape === "item") return { spec: "itemSpec", outcome: "outcome" }[key];
  if (shape === "work") return { items: "itemSpec" }[key];
  if (shape === "audit") {
    return { target_before: "snapshot", target_after: "snapshot",
      verification_before: "snapshot", verification_after: "snapshot",
      receipt: "receipt" }[key];
  }
  if (shape === "inspectionAudit") {
    return { command: "command", facts: "facts", after: "snapshot",
      result: "inspectionResult" }[key];
  }
  if (shape === "inspectionResult") return { receipt: "inspectionReceipt" }[key];
  if (shape === "itemEvidence") return { evidence: "evidence" }[key];
  if (shape === "evidence") return { Committed: "committed" }[key];
  if (shape === "committed") return { receipt: "pubReceipt" }[key];
  if (shape === "command") return { key: "key" }[key];
  if (shape === "facts") {
    return { target: "snapshot", original: "snapshot", registration: "registration" }[key];
  }
  if (shape === "checkAudit") {
    return { facts: "checkFacts", verification_before: "snapshot",
      verification_after: "snapshot", receipt: "checkReceipt" }[key];
  }
  if (shape === "checkFacts") {
    return { task: "snapshot", items: "itemEvidence" }[key];
  }
  if (shape === "registration") return { descriptor: "descriptor" }[key];
  if (shape === "descriptor") {
    return { database_identity: "identity", lock_identity: "identity" }[key];
  }
  return undefined;
};

/** Canonicalize a bound-read snapshot (raw rows) into the Rust serde
 *  shape used by audit embedding and equality comparisons. */
export const snapshotCanon = (snap) => ({
  task: {
    task_id: snap.task.task_id,
    key: JSON.parse(snap.task.command_key_json),
    status: snap.task.status,
    state_revision: Number(snap.task.state_revision),
    accepted_run_id: snap.task.accepted_run_id,
    owner_run_id: snap.task.owner_run_id,
    // serde DateTime<Utc> emits "Z"; the row stores chrono "+00:00".
    created_at: serdeDateTime(snap.task.created_at),
    last_progress_at: serdeDateTime(snap.task.last_progress_at),
    work: typeof snap.task.frozen_work_json === "string"
      ? JSON.parse(snap.task.frozen_work_json) : snap.task.work,
    result: snap.task.result_json === null || snap.task.result_json === undefined
      ? null : JSON.parse(snap.task.result_json),
    retry_of: snap.task.retry_of,
  },
  items: snap.items.map((i) => ({
    spec: {
      item_id: i.item_id, target: i.target, progress_unit: i.progress_unit,
      frozen_inputs: i.frozen_inputs,
      scope_keys: [...(i.scope_keys instanceof Set ? i.scope_keys : i.scope_keys)].sort(),
    },
    stage: i.stage, outcome: i.outcome,
  })),
  claims: snap.claims.map((c) => ({
    scope_key: c.scope_key, task_id: c.task_id ?? snap.task.task_id,
    state_revision: Number(c.state_revision),
  })),
});
export const snapshotEq = (snap, auditSide) =>
  canonEq(snapshotCanon(snap), auditSide);
export const snapshotDigest = (snap) =>
  sha256(Buffer.from(ser(snapshotCanon(snap), "snapshot"), "utf8"));

const DEBUG_STATUS = {
  pending: "Pending", running: "Running", cancel_requested: "CancelRequested",
  needs_verification: "NeedsVerification", succeeded: "Succeeded",
  partially_failed: "PartiallyFailed", failed: "Failed",
  cancelled: "Cancelled", interrupted: "Interrupted",
};

// ---------------------------------------------------------------------
// sqlite_verification.rs — read_audit / validate_audit / read_receipt /
// validate_task_link / validate_terminal / commit path primitives.

/** prepare() re-run for validate_audit — pure snapshot math. */
export const prepareStop = (verification, target, run, actor, now, observation) => {
  const revConflict = () => { throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT"); };
  if (actor.trim() === "" || run.trim() === "") {
    throw new StorageError("invalid_input", "verification actor/run missing");
  }
  const expectedScopes = [verificationScope(target.task.task_id)];
  const v0 = verification.items[0];
  const vis = (s, r) => (!TERMINAL.includes(s.task.status) &&
    (s.task.accepted_run_id !== r ||
      (s.task.owner_run_id !== null && s.task.owner_run_id !== r)))
    ? "needs_verification" : s.task.status;
  if (verification.task.key.dataset !== "task.verification" ||
      verification.task.key.operation !== "verify_stopped_writes" ||
      verification.task.key.contract_version !== "stopped-writes-task-v1" ||
      !canonEq(verification.task.work.scopes, expectedScopes) ||
      verification.task.owner_run_id !== run ||
      vis(verification, run) !== "running" ||
      verification.items.length !== 1 ||
      v0.spec.item_id !== "stopped-writes" ||
      v0.spec.progress_unit !== "evidence_check" ||
      !canonEq(v0.spec.scope_keys, expectedScopes) ||
      !canonEq(v0.spec.frozen_inputs?.accepted_run_id, target.task.accepted_run_id) ||
      !canonEq(v0.spec.frozen_inputs?.target_task_id, target.task.task_id)) {
    revConflict();
  }
  if (target.task.key.dataset === "task.verification") revConflict();
  const obs = vis(target, run) === "needs_verification" ? observation : null;
  const changed = (s) => {
    s.task.state_revision += 1;
    s.task.last_progress_at = now;
    s.claims = [];
  };
  let after = null, conclusion, reason, evidenceRef = null;
  if (obs === null) {
    conclusion = target.task.status === "interrupted" ? "already_verified" : "not_applicable";
    reason = `target state ${DEBUG_STATUS[target.task.status] ?? target.task.status}; no new inspection or original-task mutation`;
  } else if (obs.kind === "insufficient") {
    if (obs.reason.trim() === "") {
      throw new StorageError("invalid_input", "missing insufficiency reason");
    }
    conclusion = "insufficient_evidence";
    reason = obs.reason;
  } else {
    if (obs.snapshot_sha256 !== snapshotDigestJson(target) ||
        obs.evidence_ref.trim() === "") {
      throw integrity("stop evidence does not bind inspected target");
    }
    const next = JSON.parse(ser(target, "snapshot"));
    next.task.status = "interrupted";
    for (const i of next.items) {
      if (i.stage === "running") i.stage = "pending";
    }
    next.task.result = {
      interruption: {
        evidence_ref: obs.evidence_ref,
        verification_task_id: verification.task.task_id,
      },
      unfinished_items: next.items
        .filter((i) => !(i.outcome && i.outcome.success === true))
        .map((i) => i.spec.item_id),
    };
    changed(next);
    after = next;
    conclusion = "interrupted";
    reason = "verified stopped; original task not resumed";
    evidenceRef = obs.evidence_ref;
  }
  const finished = JSON.parse(ser(verification, "snapshot"));
  finished.task.status = "succeeded";
  finished.items[0].stage = "finished";
  finished.items[0].outcome = { success: true, error: null };
  changed(finished);
  const receipt = {
    verification_task_id: verification.task.task_id,
    target_task_id: target.task.task_id,
    conclusion,
    verification_revision: finished.task.state_revision,
    target_revision: after ? after.task.state_revision : target.task.state_revision,
    reason,
  };
  finished.task.result = { stopped_write_check: receipt };
  if (after) after.task.result.stopped_write_check = receipt;
  return {
    audit: {
      actor, checked_at: now, target_before: target, target_after: after,
      verification_before: verification, verification_after: finished,
      receipt, evidence_ref: evidenceRef,
    },
  };
};
const snapshotDigestJson = (snapJson) =>
  sha256(Buffer.from(ser(snapJson, "snapshot"), "utf8"));

export const readAudit = (db, id) => {
  const row = db.get(
    "SELECT target_task_id,audit_json,audit_sha256 FROM stopped_write_checks WHERE verification_task_id=?",
    id);
  if (!row) return null;
  if (sha256(row.audit_json) !== row.audit_sha256) {
    throw integrity("stopped check digest differs");
  }
  let audit;
  try { audit = JSON.parse(row.audit_json); }
  catch (e) { throw integrity(e.message); }
  if (audit?.receipt?.verification_task_id !== id ||
      audit?.receipt?.target_task_id !== row.target_task_id) {
    throw integrity("stopped check association differs");
  }
  // validate_audit: replay prepare with the Recorded inspector
  const run = audit.verification_before.task.owner_run_id ?? "";
  const observation = audit.evidence_ref !== null
    ? { kind: "stopped", snapshot_sha256: snapshotDigestJson(audit.target_before),
        evidence_ref: audit.evidence_ref }
    : { kind: "insufficient", reason: audit.receipt.reason };
  let expected;
  try {
    expected = prepareStop(audit.verification_before, audit.target_before,
      run, audit.actor, audit.checked_at, observation);
  } catch (e) {
    if (e instanceof StorageError) throw e;
    throw integrity(e.message);
  }
  if (!canonEq(expected.audit, audit)) {
    throw integrity("stopped check audit projections differ");
  }
  return audit;
};

export const readReceipt = (db, id, readBound) => {
  const audit = readAudit(db, id);
  if (!audit) {
    // A committed check with a missing audit is not an unexecuted check —
    // still validate the task row itself.
    readBound(id);
    return null;
  }
  const actual = readBound(id);
  if (!actual) throw integrity("verification task missing");
  if (!snapshotEq(actual, audit.verification_after)) {
    throw integrity("verification completion differs");
  }
  if (audit.target_after !== null) {
    const t = readBound(audit.receipt.target_task_id);
    if (!t || !snapshotEq(t, audit.target_after)) {
      throw integrity("interrupted target differs");
    }
  }
  if (audit.receipt.conclusion === "already_verified") {
    const priorId = audit.target_before.task.result?.interruption?.verification_task_id;
    if (typeof priorId !== "string") throw integrity("prior check link missing");
    const prior = readAudit(db, priorId);
    if (!prior) throw integrity("prior interruption audit missing");
    if (priorId === id || prior.receipt.conclusion !== "interrupted" ||
        prior.receipt.target_task_id !== audit.receipt.target_task_id) {
      throw integrity("prior interruption association differs");
    }
    const prev = readReceipt(db, priorId, readBound);
    if (!prev) throw integrity("prior interruption receipt missing");
  }
  return { audit, receipt: audit.receipt };
};

/** sqlite_verification::validate_task_link — wired into the bound read.
 *  readBound: validated snapshot reader; readRequestRow: request reader. */
export const verifyTaskLink = (db, snap, readBound) => {
  const result = snap.task.result_json === null ? null : JSON.parse(snap.task.result_json);
  if (!result) return;
  let id = null;
  if (snap.task.status === "interrupted") {
    const v = result.interruption?.verification_task_id;
    if (typeof v !== "string") throw integrity("interruption check link missing");
    id = v;
  } else if (result.stopped_write_check !== undefined) {
    id = snap.task.task_id;
  }
  if (id === null) return;
  const audit = readAudit(db, id);
  if (!audit) throw integrity("stopped check audit missing");
  const expected = snap.task.status === "interrupted"
    ? audit.target_after
    : audit.verification_after;
  if (expected === null || expected === undefined) {
    throw integrity("target interruption absent from audit");
  }
  if (!snapshotEq(snap, expected)) {
    throw integrity("task differs from stopped check audit");
  }
};

/** sqlite_verification_inspection::validate_task_link — returns true when
 *  the task's link is an inspection link (handled), false to fall through. */
export const inspectionTaskLink = (db, snap, readBound, readRequestRow) => {
  const result = snap.task.result_json === null ? null : JSON.parse(snap.task.result_json);
  const id = result?.interruption?.inspection_request_id;
  if (typeof id !== "string") return false;
  const key = JSON.parse(snap.task.command_key_json);
  if (snap.task.status !== "interrupted" || !supportedVerification(key)) {
    throw integrity("inspection interruption on unsupported task/state");
  }
  const audit = readInspectionAudit(db, id);
  if (!audit) throw integrity("inspection finish audit missing");
  if (audit.after === null || audit.after === undefined || !snapshotEq(snap, audit.after)) {
    throw integrity("inspection finish task projection differs");
  }
  const record = readRequestRow(id);
  if (!record) throw integrity("inspection finish request missing");
  if (!canonEq(JSON.parse(record.command_json), audit.command)) {
    throw integrity("inspection finish request differs");
  }
  return true;
};

export const readInspectionAudit = (db, id) => {
  const row = db.get(
    "SELECT target_task_id,audit_json,audit_sha256 FROM verification_inspections WHERE request_id=?",
    id);
  if (!row) return null;
  if (sha256(row.audit_json) !== row.audit_sha256) {
    throw integrity("inspection audit digest differs");
  }
  let audit;
  try { audit = JSON.parse(row.audit_json); }
  catch (e) { throw integrity(e.message); }
  if (audit?.request_id !== id || audit?.facts?.target?.task?.task_id !== row.target_task_id) {
    throw integrity("inspection association differs");
  }
  validateInspectionAudit(audit);
  return audit;
};

export const validateAbsentRequest = (db, id) => {
  if (db.get("SELECT EXISTS(SELECT 1 FROM verification_inspections WHERE request_id=?) x", id).x !== 0n) {
    throw integrity("inspection request binding missing");
  }
};

/** sqlite_verification::validate_terminal (existing-stop variant). */
export const validateTerminalStop = (db, command, value, readBound) => {
  if (!canonEq(command.key, VERIFICATION_KEY)) return;
  let result;
  try { result = typeof value === "string" ? JSON.parse(value) : value; }
  catch (e) { throw integrity(e.message); }
  if (result?.kind !== "already_verified" || typeof result.receipt !== "object") {
    throw integrity("invalid terminal result");
  }
  const expected = result.receipt;
  if (!canonEq(command.intent, { target_task_id: expected.target_task_id })) {
    throw integrity("known stop request intent differs");
  }
  const got = readReceipt(db, expected.verification_task_id, readBound)?.receipt;
  if (!got || !canonEq(got, expected)) {
    throw integrity("known stop receipt missing or changed");
  }
  if (expected.conclusion !== "interrupted") {
    throw integrity("known stop request has no interruption");
  }
};

/** sqlite_verification_inspection::validate_terminal. */
export const validateTerminalInspection = (db, id, command, value) => {
  if (!canonEq(command.key, INSPECTION_KEY)) {
    return validateAbsentRequest(db, id);
  }
  let result;
  try { result = typeof value === "string" ? JSON.parse(value) : value; }
  catch (e) { throw integrity(e.message); }
  const audit = readInspectionAudit(db, id);
  if (!audit) throw integrity("inspection audit missing");
  if (!canonEq(audit.command, command) || !canonEq(audit.result, result)) {
    throw integrity("inspection request differs from historical audit");
  }
};

/** sqlite_verification_inspection::validate_task_request — inspection
 *  commands must never bind an asynchronous task. */
export const validateTaskRequest = (db, id, command) => {
  if (canonEq(command.key, INSPECTION_KEY)) {
    throw integrity("bounded inspection must not bind an asynchronous task");
  }
  return validateAbsentRequest(db, id);
};

// ---------------------------------------------------------------------
// sqlite_verification_runtime.rs — registration read + claim guard.

export const readRegistration = (db, snap) => {
  const row = db.get(
    "SELECT claim_revision,descriptor_json,descriptor_sha256 FROM verification_runtime_guards WHERE task_id=?",
    snap.task.task_id);
  if (!row) return null;
  if (sha256(row.descriptor_json) !== row.descriptor_sha256) {
    throw integrity("runtime registration digest differs");
  }
  let descriptor;
  try { descriptor = JSON.parse(row.descriptor_json); }
  catch (e) { throw integrity(e.message); }
  const revision = Number(row.claim_revision);
  const key = JSON.parse(snap.task.command_key_json);
  if (revision === 0 || revision > Number(snap.task.state_revision) ||
      descriptor.protocol !== "verification-file-guard-v1" ||
      descriptor.task_id !== snap.task.task_id ||
      snap.task.owner_run_id !== descriptor.run_id ||
      snap.task.accepted_run_id !== descriptor.run_id ||
      !supportedVerification(key)) {
    throw integrity("runtime registration does not bind the claimed task");
  }
  return { claim_revision: revision, descriptor };
};

/** write_task parity: full snapshot write with revision CAS. */
export const writeTaskSnapshot = (db, snapJson, expected) => {
  for (const i of snapJson.items) {
    db.run("UPDATE task_items SET stage=?,outcome_json=? WHERE task_id=? AND item_id=?",
      i.stage, i.outcome === null ? null : JSON.stringify(i.outcome),
      snapJson.task.task_id, i.spec.item_id);
  }
  const res = db.run(
    `UPDATE tasks SET status=?,state_revision=?,owner_run_id=?,
     last_progress_at=?,result_json=?,
     item_count=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id),
     items_pending=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='pending'),
     items_running=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='running'),
     items_finished=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='finished'),
     items_succeeded=(SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='finished' AND json_extract(i.outcome_json,'$.success')=1)
     WHERE task_id=? AND state_revision=?`,
    snapJson.task.status, BigInt(snapJson.task.state_revision),
    snapJson.task.owner_run_id, snapJson.task.last_progress_at,
    snapJson.task.result === null ? null : ser(snapJson.task.result),
    snapJson.task.task_id, BigInt(expected));
  if (res.changes !== 1) {
    throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
  }
  if (TERMINAL.includes(snapJson.task.status)) {
    db.run("DELETE FROM scope_occupancy WHERE task_id=?", snapJson.task.task_id);
  }
};

/** claim_with_runtime_guard parity — triple verify_binding ordering:
 *  before the tx, inside the tx before write_task, and after the
 *  registration insert + re-read. */
export const claimWithRuntimeGuard = (db, repo, plan, guard, dbPath) => {
  guard.verifyBinding(dbPath);
  if (!descEqJson(guard.descriptor, plan.descriptor)) {
    throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
  }
  return db.transaction(() => {
    const current = repo._readBoundTaskValidated(plan.before.task.task_id);
    if (!current || !snapshotEq(current, plan.before)) {
      throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
    }
    if (readRegistration(db, current) !== null) {
      throw integrity("task already has a runtime claim");
    }
    guard.verifyBinding(dbPath);
    writeTaskSnapshot(db, plan.after, plan.before.task.state_revision);
    const raw = JSON.stringify(plan.descriptor);
    db.run("INSERT INTO verification_runtime_guards VALUES(?,?,?,?)",
      plan.after.task.task_id, BigInt(plan.after.task.state_revision),
      raw, sha256(raw));
    const stored = repo._readBoundTaskValidated(plan.after.task.task_id);
    if (!stored || !snapshotEq(stored, plan.after)) {
      throw integrity("claimed runtime task projection differs");
    }
    if (readRegistration(db, stored) === null) {
      throw integrity("runtime registration missing");
    }
    guard.verifyBinding(dbPath);
    return stored;
  });
};
const descEqJson = (a, b) => canonEq(a, b);

/** runtime_registration public read parity. */
export const runtimeRegistration = (db, repo, id) => {
  const task = repo._readBoundTaskValidated(id);
  if (task) return readRegistration(db, task);
  if (db.get(
    "SELECT EXISTS(SELECT 1 FROM verification_runtime_guards WHERE task_id=?) x", id).x !== 0n) {
    throw integrity("orphan runtime registration");
  }
  throw new StorageError("task_not_found", "TASK_NOT_FOUND");
};

// ---------------------------------------------------------------------
// NM-059 — sqlite_verification.rs commit surface: commit_stop_check /
// bind_existing_stop / stop_receipt + durable/facts readers.

/** sqlite_tasks::read_request — request row -> RequestRecord, with the
 *  terminal-arm validator chain (stop / inspection / publication-check
 *  validate_terminal) verbatim. readBound: validated bound-snapshot
 *  reader (read_task + link validators). */
export const readRequestRecord = (db, id, readBound, readFactsFn) => {
  const row = db.get(
    "SELECT command_json,command_digest,outcome_kind,task_id,terminal_json FROM requests WHERE request_id=?",
    id);
  if (!row) {
    validateAbsentRequest(db, id);
    return null;
  }
  if (sha256(row.command_json) !== row.command_digest) {
    throw integrity("command digest mismatch");
  }
  let command;
  try { command = JSON.parse(row.command_json); }
  catch (e) { throw integrity(e.message); }
  let outcome;
  if (row.outcome_kind === "task" && row.task_id !== null && row.terminal_json === null) {
    validateTaskRequest(db, id, command);
    const s = readBound(row.task_id);
    if (!s) throw integrity("bound execution record missing");
    const key = JSON.parse(s.task.command_key_json);
    const expected = s.task.retry_of !== null
      ? { operation: "retry_task", dataset: key.dataset, contract_version: key.contract_version }
      : key;
    if (!canonEq(command.key, expected)) {
      throw integrity("request/task contract mismatch");
    }
    outcome = { Task: snapshotCanon(s) };
  } else if (row.outcome_kind === "terminal" && row.task_id === null && row.terminal_json !== null) {
    let value;
    try { value = JSON.parse(row.terminal_json); }
    catch (e) { throw integrity(e.message); }
    validateTerminalStop(db, command, value, readBound);
    validateTerminalInspection(db, id, command, value);
    pubValidateTerminal(db, command, value, readBound, readFactsFn);
    outcome = { Terminal: value };
  } else {
    throw integrity("request outcome incomplete");
  }
  return { request_id: id, command, outcome };
};

/** StopRepository::commit_stop_check — SqliteTasks::write tx. */
export const commitStopCheck = (db, plan, readBound) => db.transaction(() => {
  const audit = plan.audit;
  const id = audit.receipt.verification_task_id;
  const prior = readReceipt(db, id, readBound);
  if (prior !== null) {
    if (!canonEq(prior.audit, audit)) {
      throw new StorageError("conflict", "REQUEST_CONFLICT");
    }
    return prior.receipt;
  }
  const target = readBound(audit.receipt.target_task_id);
  if (target === null) throw new StorageError("task_not_found", "TASK_NOT_FOUND");
  const verification = readBound(id);
  if (verification === null) throw new StorageError("task_not_found", "TASK_NOT_FOUND");
  if (!snapshotEq(target, audit.target_before) ||
      !snapshotEq(verification, audit.verification_before)) {
    throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
  }
  if (audit.target_after !== null && audit.target_after !== undefined) {
    writeTaskSnapshot(db, audit.target_after, target.task.state_revision);
  }
  writeTaskSnapshot(db, audit.verification_after, verification.task.state_revision);
  const raw = ser(audit, "audit");
  db.run("INSERT INTO stopped_write_checks VALUES(?,?,?,?)",
    id, audit.receipt.target_task_id, raw, sha256(raw));
  const stored = readReceipt(db, id, readBound);
  if (!stored) throw integrity("inserted stopped check missing");
  return stored.receipt;
});

/** StopRepository::stop_receipt. */
export const stopReceipt = (db, id, readBound) =>
  readReceipt(db, id, readBound)?.receipt ?? null;

/** ExistingStopRepository::bind_existing_stop — replay -> bound equality
 *  -> validate_terminal -> terminal request insert. */
export const bindExistingStop = (db, plan, readBound, readFactsFn) => db.transaction(() => {
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
  validateTerminalStop(db, plan.command, value, readBound);
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

/** durable_effects — schema-aware EXISTS union. */
export const durableEffects = (db, taskId) => {
  // is_compact_schema: publication_batches absent on schema-19.
  const compact = db.get(
    "SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name='publication_batches') x").x !== 0n;
  const sql = compact
    ? "SELECT EXISTS(SELECT 1 FROM task_publications WHERE task_id=? UNION ALL SELECT 1 FROM market_update_results WHERE task_id=? UNION ALL SELECT 1 FROM publication_results WHERE task_id=?) x"
    : "SELECT EXISTS(SELECT 1 FROM publication_batches WHERE task_id=? UNION ALL SELECT 1 FROM task_publications WHERE task_id=? UNION ALL SELECT 1 FROM market_update_results WHERE task_id=? UNION ALL SELECT 1 FROM yearly_execution_results WHERE task_id=?) x";
  const args = compact ? [taskId, taskId, taskId] : [taskId, taskId, taskId, taskId];
  return db.get(sql, ...args).x !== 0n;
};
const isCompactSchema = (db) =>
  db.get("SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name='publication_batches') x").x !== 0n;

/** never_started_facts — {snapshot, has_durable_effects}. */
export const neverStartedFacts = (db, taskId, readBound) => {
  const snapshot = readBound(taskId);
  if (!snapshot) throw new StorageError("task_not_found", "TASK_NOT_FOUND");
  return { snapshot: snapshotCanon(snapshot),
    has_durable_effects: durableEffects(db, taskId) };
};

/** determinate_facts — {snapshot, started_items, committed_items,
 *  has_durable_effects}; committed_items union is schema-aware. */
export const determinateFacts = (db, taskId, readBound) => {
  const snapshot = readBound(taskId);
  if (!snapshot) throw new StorageError("task_not_found", "TASK_NOT_FOUND");
  const startedItems = db.query(
    "SELECT item_id FROM task_item_execution_starts WHERE task_id=?", taskId)
    .map((r) => r.item_id).sort();
  const committedSql = isCompactSchema(db)
    ? "SELECT item_id FROM market_update_results WHERE task_id=? UNION SELECT item_id FROM publication_results WHERE task_id=?"
    : "SELECT item_id FROM market_update_results WHERE task_id=? UNION SELECT item_id FROM yearly_execution_results WHERE task_id=? UNION SELECT item_id FROM publication_batches WHERE task_id=?";
  const committedArgs = isCompactSchema(db)
    ? [taskId, taskId] : [taskId, taskId, taskId];
  const committedItems = db.query(committedSql, ...committedArgs)
    .map((r) => r.item_id).sort();
  return { snapshot: snapshotCanon(snapshot), started_items: startedItems,
    committed_items: committedItems,
    has_durable_effects: durableEffects(db, taskId) };
};
