// NM-059 — sqlite_verification_inspection.rs commit surface:
// read_facts / inspection_facts / probe_execution / commit_inspection.
// The read/validate half (read_audit, validate_task_link,
// validate_terminal, validate_task_request, validate_absent_request)
// lives in verification_storage.mjs (NM-048).
import { createHash } from "node:crypto";
import { canonEq } from "./task_repository.mjs";
import {
  VERIFICATION_KEY, PUBLICATION_KEY, INSPECTION_KEY,
  snapshotCanon, snapshotEq, ser, readReceipt as readStopReceipt,
  readRegistration, writeTaskSnapshot, readRequestRecord, verificationScope,
} from "./verification_storage.mjs";
import { readCheckReceipt } from "./publication_checks.mjs";
import { readFacts as readPublicationFacts } from "../market/verification_facts.mjs";
import { tryAcquire, RuntimeGuardError } from "./verification_runtime.mjs";
import { StorageError } from "./sqlite_adapter.mjs";
import { MarketError } from "../market/record_codec.mjs";

const sha256 = (b) => createHash("sha256").update(b).digest("hex");
const integrity = (m) =>
  new StorageError("needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: ${m}`);
const taskNotFound = () => new StorageError("task_not_found", "TASK_NOT_FOUND");
const revConflict = () =>
  new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
const TERMINAL = ["succeeded", "partially_failed", "failed", "cancelled", "interrupted"];

// verification_execution::supported — the two verification task keys.
const supported = (key) =>
  canonEq(key, VERIFICATION_KEY) || canonEq(key, PUBLICATION_KEY);

// sqlite_verification_inspection::market_error — Task passthrough,
// Storage -> StorageUnavailable, other -> NeedsVerification(Display).
const marketError = (e) => {
  if (e instanceof StorageError) throw e; // Error::Task — transparent
  if (e instanceof MarketError && e.kind === "storage") {
    throw new StorageError("unavailable",
      `STORAGE_UNAVAILABLE: ${e.message.replace(/^MARKET_STORAGE: /, "")}`);
  }
  throw integrity(String(e?.message ?? e));
};

/** read_facts — the full inspection Facts: bound target, supported-key
 *  gate, original-task chain, stop/publication receipt kind gates,
 *  completion digest, business-effects union, runtime registration. */
export const readFacts = (db, id, readBound) => {
  const target = readBound(id);
  if (target === null) throw taskNotFound();
  const targetSnap = snapshotCanon(target);
  const key = targetSnap.task.key;
  if (!supported(key)) {
    return { target: targetSnap, original: null, registration: null,
      finished: false, has_business_effects: false, completion_digest: null };
  }
  const originalId = targetSnap.items[0]?.spec?.frozen_inputs?.target_task_id;
  if (typeof originalId !== "string") {
    throw integrity("verification original target missing");
  }
  if (originalId === id) throw integrity("recursive verification target");
  const original = readBound(originalId);
  if (original === null) throw integrity("verification original task missing");
  const originalSnap = snapshotCanon(original);
  if (originalSnap.task.key.dataset === "task.verification") {
    throw integrity("verification targets another verification task");
  }
  let stop, publication;
  try {
    stop = readStopReceipt(db, id, readBound);
    publication = readCheckReceipt(db, id, readBound,
      (tid) => readPublicationFacts(db, tid, readBound));
  } catch (e) {
    // read_receipt errors map through market_error only for the
    // publication arm — the stop arm is already task-typed.
    if (e instanceof MarketError) marketError(e);
    throw e;
  }
  if ((stop !== null && !canonEq(key, VERIFICATION_KEY)) ||
      (publication !== null && !canonEq(key, PUBLICATION_KEY))) {
    throw integrity("wrong verification receipt kind");
  }
  const completionDigest = stop !== null
    ? sha256(Buffer.from(ser(stop.audit, "audit"), "utf8"))
    : publication !== null
      ? sha256(Buffer.from(ser(publication[0], "audit"), "utf8"))
      : null;
  if (targetSnap.task.status === "succeeded" && completionDigest === null) {
    throw integrity("successful verification lacks its receipt");
  }
  const compact = db.get(
    "SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name='publication_batches') x").x !== 0n;
  const businessSql = compact
    ? "SELECT EXISTS(SELECT 1 FROM task_publications WHERE task_id=? UNION ALL SELECT 1 FROM market_update_results WHERE task_id=? UNION ALL SELECT 1 FROM publication_results WHERE task_id=?) x"
    : "SELECT EXISTS(SELECT 1 FROM publication_batches WHERE task_id=? UNION ALL SELECT 1 FROM task_publications WHERE task_id=? UNION ALL SELECT 1 FROM market_update_results WHERE task_id=? UNION ALL SELECT 1 FROM yearly_execution_results WHERE task_id=?) x";
  const hasBusinessEffects = db.get(businessSql,
    ...(compact ? [id, id, id] : [id, id, id, id])).x !== 0n;
  const registration = readRegistration(db, target);
  return { target: targetSnap, original: originalSnap, registration,
    finished: TERMINAL.includes(targetSnap.task.status),
    has_business_effects: hasBusinessEffects,
    completion_digest: completionDigest };
};

/** inspection_facts — SqliteTasks::read wrapper. */
export const inspectionFacts = (db, id, readBound) => readFacts(db, id, readBound);

/** probe_execution — runtime try_acquire semantics: Excluded + held
 *  guard on success, Busy on contention, Unverified on guard failure. */
export const probeExecution = (dbPath, facts) => {
  const registration = facts.registration;
  if (registration === null || registration === undefined) {
    throw integrity("runtime registration missing");
  }
  try {
    const guard = tryAcquire(dbPath, registration.descriptor);
    return { observation: "Excluded", guard };
  } catch (e) {
    if (e instanceof RuntimeGuardError && e.kind === "busy") {
      return { observation: "Busy", guard: null };
    }
    // Error Debug verbatim: NotVerified(m) -> NotVerified("m"),
    // Io(e) -> Io(<std::io::Error Debug>).
    const dbg = e instanceof RuntimeGuardError && e.kind === "not_verified"
      ? `NotVerified(${JSON.stringify(e.message)})`
      : `Io(${String(e?.message ?? e)})`;
    return { observation:
      { Unverified: `runtime exclusion not verified: ${dbg}` }, guard: null };
  }
};

/** commit_inspection — SqliteTasks::write tx; the probe's guard is held
 *  across the commit (runtime exclusion persists through the write). */
export const commitInspection = (db, dbPath, plan, probe, readBound, readFactsFn) => {
  const a = plan.audit;
  if (!canonEq(a.observation, probe.observation)) throw revConflict();
  return db.transaction(() => {
    const existing = readRequestRecord(db, a.request_id, readBound, readFactsFn);
    if (existing !== null) {
      if (!canonEq(existing.command, a.command)) {
        throw new StorageError("conflict", "REQUEST_CONFLICT");
      }
      return { record: existing, reused: true };
    }
    const actual = readFacts(db, a.facts.target.task.task_id, readBound);
    if (!canonEq(actual, a.facts)) throw revConflict();
    if (a.observation === "Excluded") {
      if (!probe.guard) throw integrity("actual runtime guard absent");
      probe.guard.verifyBinding(dbPath);
      const want = a.facts.registration?.descriptor ?? null;
      if (!canonEq(want, probe.guard.descriptor)) throw revConflict();
    }
    const cmdJson = ser(a.command, "command");
    const termJson = ser(a.result);
    db.run("INSERT INTO requests VALUES(?,?,?,'terminal',NULL,?)",
      a.request_id, cmdJson, sha256(cmdJson), termJson);
    const raw = ser(a, "inspectionAudit");
    db.run("INSERT INTO verification_inspections VALUES(?,?,?,?)",
      a.request_id, a.facts.target.task.task_id, raw, sha256(raw));
    if (a.after !== null && a.after !== undefined) {
      writeTaskSnapshot(db, a.after, a.facts.target.task.state_revision);
    }
    if (probe.guard) probe.guard.verifyBinding(dbPath);
    const post = readBound(a.facts.target.task.task_id);
    if (post === null) throw taskNotFound();
    const expectedAfter = a.after ?? a.facts.target;
    if (!snapshotEq(post, expectedAfter)) {
      throw integrity("inspection target postcondition differs");
    }
    const record = readRequestRecord(db, a.request_id, readBound, readFactsFn);
    if (!record) throw integrity("inspection request missing after insert");
    if (!(record.outcome?.Terminal !== undefined &&
          canonEq(record.outcome.Terminal, a.result))) {
      throw integrity("inspection terminal differs");
    }
    return { record, reused: false };
  });
};

// ---------------------------------------------------------------------
// decision::prepare + validate — required by read_audit's replay gate
// (the read path cannot validate a committed audit without it) and by
// the commit op's plan construction. serde field order is preserved in
// the returned audit so JSON.stringify emits the stored bytes verbatim.

const TERMINAL_S = ["succeeded", "partially_failed", "failed", "cancelled", "interrupted"];
const visStatus = (s, r) => (!TERMINAL_S.includes(s.task.status) &&
  (s.task.accepted_run_id !== r ||
    (s.task.owner_run_id !== null && s.task.owner_run_id !== r)))
  ? "needs_verification" : s.task.status;

export const command = (target) => ({
  key: { ...INSPECTION_KEY }, intent: { target_task_id: target },
});

const neverStarted = (f) => f.registration === null &&
  f.target.task.owner_run_id === null &&
  f.target.task.state_revision === 0 &&
  f.target.task.status === "pending" &&
  f.target.task.result === null &&
  f.target.items.every((i) => i.stage === "pending" && i.outcome === null);

export const prepareInspection = (id, commandValue, facts, observation, run, actor, now) => {
  if (typeof id !== "string" || id.trim() === "" || run.trim() === "" ||
      actor.trim() === "" ||
      !canonEq(commandValue, command(facts.target.task.task_id))) {
    throw new StorageError("invalid_input",
      "INVALID_COMMAND: inspection identity/command missing or inconsistent");
  }
  if (supported(facts.target.task.key)) {
    const original = facts.original;
    if (original === null || original === undefined) {
      throw integrity("inspection original target absent");
    }
    const scope = [verificationScope(original.task.task_id)];
    const expectedItem =
      canonEq(facts.target.task.key, VERIFICATION_KEY)
        ? "stopped-writes" : "publication-results";
    const item = facts.target.items[0];
    if (facts.target.items.length !== 1 ||
        !canonEq([...(facts.target.task.work?.scopes ?? [])].sort(), scope) ||
        !canonEq([...(item?.spec?.scope_keys ?? [])].sort(), scope) ||
        item?.spec?.item_id !== expectedItem ||
        item?.spec?.progress_unit !== "evidence_check" ||
        !canonEq(item?.spec?.target, { task_id: original.task.task_id }) ||
        item?.spec?.frozen_inputs?.target_task_id !== original.task.task_id ||
        facts.finished !== TERMINAL_S.includes(facts.target.task.status) ||
        (!facts.finished && facts.completion_digest !== null)) {
      throw integrity(
        "inspection target/work/serial protection association differs");
    }
  }
  const target = facts.target;
  const old = visStatus(target, run) === "needs_verification";
  let conclusion, reason;
  if (!supported(target.task.key)) {
    [conclusion, reason] = ["not_applicable",
      "target is not a supported verification task"];
  } else if (facts.finished) {
    [conclusion, reason] = ["already_finished",
      "verified persisted finish; existing result is not rewritten"];
  } else if (facts.has_business_effects) {
    [conclusion, reason] = ["insufficient_evidence",
      "verification task has unexpected business effects; protection retained"];
  } else if (observation === "Excluded" && facts.registration !== null) {
    [conclusion, reason] = ["interrupted",
      "registered execution excluded and verified transaction history has no committed finish; close only verification task"];
  } else if (old && neverStarted(facts)) {
    [conclusion, reason] = ["interrupted",
      "verified never-claimed execution with no effects; conditional commit excludes future Claim"];
  } else if (!old && (observation === "Busy" || observation === "NotNeeded")) {
    [conclusion, reason] = ["not_applicable",
      "current execution has no applicable abnormality; no forced interruption"];
  } else {
    [conclusion, reason] = ["insufficient_evidence",
      observation === "Busy"
        ? "registered execution is still active; no stopping proof"
        : observation?.Unverified !== undefined
          ? observation.Unverified
          : "no registered execution exclusion proof; old run or missing receipt does not prove stopped"];
  }
  let after = null;
  if (conclusion === "interrupted") {
    const next = JSON.parse(JSON.stringify(target));
    next.task.status = "interrupted";
    for (const i of next.items) {
      if (i.stage === "running") i.stage = "pending";
    }
    next.task.result = {
      interruption: { inspection_request_id: id },
      previous_result: target.task.result,
      unfinished_items: next.items
        .filter((i) => !(i.outcome && i.outcome.success === true))
        .map((i) => i.spec.item_id),
    };
    next.claims = [];
    after = next;
  } else if (conclusion === "insufficient_evidence" &&
             target.task.status !== "needs_verification") {
    const next = JSON.parse(JSON.stringify(target));
    next.task.status = "needs_verification";
    after = next;
  }
  if (after !== null) {
    after.task.state_revision = after.task.state_revision + 1;
    if (!Number.isSafeInteger(after.task.state_revision)) {
      throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
    }
    after.task.last_progress_at = now;
    for (const c of after.claims) c.state_revision = after.task.state_revision;
  }
  const actual = after ?? target;
  const result = { kind: "verification_inspection", receipt: {
    request_id: id, target_task_id: target.task.task_id, actor,
    checked_at: now, conclusion,
    target_revision: actual.task.state_revision,
    target_status: actual.task.status,
    target_result: actual.task.result, reason } };
  return { audit: { request_id: id, command: commandValue, actor,
    run_id: run, checked_at: now, facts, observation, after, result } };
};

/** decision::validate — replay prepare over the embedded audit fields. */
export const validateInspectionAudit = (a) => {
  const expected = prepareInspection(a.request_id, a.command, a.facts,
    a.observation, a.run_id, a.actor, a.checked_at);
  if (!canonEq(expected.audit, a)) {
    throw integrity("inspection audit projections differ");
  }
};
