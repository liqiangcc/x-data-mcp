// NM-098 — publication_verification domain-service probe.
// Mirrors src/sqlite_market/publication_checks.rs tests (real-exit
// atomicity + known-publication binding) and the admission/decision/
// runner/existing arms of src/publication_verification/*.rs.
// Usage:
//   node publication_verification_probe.mjs [--out DIR] [--selftest]
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, copyFileSync,
  readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { openDatabase, StorageError } from "../../src_node/storage/sqlite_adapter.mjs";
import { gateDatabase } from "../../src_node/storage/schema_gate.mjs";
import { TaskAdmission } from "../../src_node/storage/task_repository.mjs";
import { snapshotCanon, snapshotDigest }
  from "../../src_node/storage/verification_storage.mjs";
import { canonEq } from "../../src_node/storage/task_repository.mjs";
import { PublicationVerification, commandKey, executionUncertainty }
  from "../../src_node/publication_verification/admission.mjs";
import { prepare } from "../../src_node/publication_verification/decision.mjs";
import { Runner } from "../../src_node/publication_verification/runner.mjs";
import { accept as existingAccept }
  from "../../src_node/publication_verification/existing.mjs";
import { PublicationStore }
  from "../../src_node/publication_verification/store.mjs";
import { ser } from "../../src_node/storage/verification_storage.mjs";
import {
  buildTargetFixture, acceptCheckTask, controlAccept,
  ensureSkeleton, serCommand, serKey, NOW,
} from "./pubcheck_fixture.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const REPO = join(here, "..", "..");

const SELFTEST = process.argv.includes("--selftest");
const OUT = process.argv.includes("--out")
  ? process.argv[process.argv.indexOf("--out") + 1] : null;
const FIX = mkdtempSync(join(tmpdir(), "nm098-"));
const SKELETON = ensureSkeleton(FIX);
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));

const results = [];
const emit = (id, status, detail) => {
  const rec = { family: "publication_verification", id, status, detail };
  results.push(rec);
  console.log(JSON.stringify(rec));
};
const run = (id, f) => {
  try { f(); emit(id, "PASS", ""); }
  catch (e) { emit(id, "FAIL", `${e?.kind ?? e?.code ?? "?"} ${e.message}`); }
};
const errText = (e) => String(e?.message ?? e);
const expectThrow = (f, want) => {
  try { f(); } catch (e) {
    if (errText(e) === want) return;
    throw new Error(`want ${JSON.stringify(want)} got ${JSON.stringify(errText(e))}`);
  }
  throw new Error(`want throw ${JSON.stringify(want)}; got return`);
};
const expectThrowContains = (f, want) => {
  try { f(); } catch (e) {
    if (errText(e).includes(want)) return;
    throw new Error(`want ~${JSON.stringify(want)} got ${JSON.stringify(errText(e))}`);
  }
  throw new Error(`want throw ~${JSON.stringify(want)}; got return`);
};

/** Fresh fixture: target task running under foreign run ("run") —
 *  visible_status for "check-run" = needs_verification + running item. */
const fixture = (name, opts = {}) => {
  const path = join(FIX, `${name}.db`);
  const f = buildTargetFixture(path, SKELETON, opts);
  const store = new PublicationStore(f.db, f.adm);
  return { ...f, store };
};
// The control object for existing::accept / plain accepts.
const control = (repo, runId, contract) => ({
  repository: repo,
  runId,
  accept: (id, key, intent) =>
    controlAccept(repo, contract, id, intent),
});

// -- A: admission arms --------------------------------------------------
run("A1_empty_run", () => {
  const f = fixture("a1");
  expectThrow(() => new PublicationVerification(f.store, "  "),
    "INVALID_COMMAND: empty verification run");
});
run("A2_normalize_decode", () => {
  const f = fixture("a2");
  const c = new PublicationVerification(f.store, "check-run");
  expectThrowContains(() => c.normalize({}),
    "INVALID_COMMAND: missing field `target_task_id`");
  expectThrowContains(() => c.normalize({ target_task_id: "t", extra: 1 }),
    "unknown field `extra`");
  expectThrow(() => c.normalize({ target_task_id: "  " }),
    "INVALID_COMMAND: empty target_task_id");
  expectThrowContains(() => c.normalize({ target_task_id: 7 }),
    "INVALID_COMMAND: invalid type");
});
run("A3_resolve_notfound", () => {
  const f = fixture("a3");
  const c = new PublicationVerification(f.store, "check-run");
  expectThrow(() => c.resolve({ target_task_id: "ghost" }),
    "TASK_NOT_FOUND");
});
run("A4_resolve_unsupported_contract", () => {
  // A check task's own key is not a publication fact contract.
  const f = fixture("a4");
  const c = new PublicationVerification(f.store, "check-run");
  const a = acceptCheckTask(f, c, "chk-self", f.taskId);
  expectThrow(() => c.resolve({ target_task_id: a.taskId }),
    "UNSUPPORTED_CONTRACT");
});
run("A5_resolve_terminal_target", () => {
  const f = fixture("a5", { publish: true });
  const c = new PublicationVerification(f.store, "check-run");
  expectThrow(() => c.resolve({ target_task_id: f.taskId }),
    "INVALID_COMMAND: verification_not_applicable: no unresolved execution requiring publication inspection");
});
run("A6_resolve_same_run_target", () => {
  // Target owned by THIS run: visible_status = running, not
  // needs_verification -> not applicable.
  const f = fixture("a6");
  const c = new PublicationVerification(f.store, "run");
  expectThrow(() => c.resolve({ target_task_id: f.taskId }),
    "INVALID_COMMAND: verification_not_applicable: no unresolved execution requiring publication inspection");
});
run("A7_resolve_ok_frozen_work", () => {
  const f = fixture("a7");
  const c = new PublicationVerification(f.store, "check-run");
  const w = c.resolve({ target_task_id: f.taskId });
  const scope = `verification:task:${f.taskId}`;
  if (w.items.length !== 1 || w.scopes.length !== 1) throw new Error("work shape");
  const it = w.items[0];
  if (it.item_id !== "publication-results" ||
      it.progress_unit !== "evidence_check" ||
      !canonEq(it.target, { task_id: f.taskId }) ||
      ![...it.scope_keys].includes(scope) || it.scope_keys.length !== 1 ||
      ![...w.scopes].includes(scope)) {
    throw new Error(`work diverges: ${JSON.stringify(w)}`);
  }
  const target = f.store.readTask(f.taskId);
  const fi = it.frozen_inputs;
  if (fi.target_task_id !== f.taskId ||
      fi.admission_revision !== target.task.state_revision ||
      fi.accepted_run_id !== target.task.accepted_run_id ||
      fi.admission_snapshot_sha256 !== snapshotDigest(
        f.adm._readBoundTaskValidated(f.taskId))) {
    throw new Error("frozen inputs diverge");
  }
});
run("A8_resolve_bad_uncertainty", () => {
  const f = fixture("a8");
  // Marker with an EXTRA field: the bound read's shape check passes
  // (types only), then the domain layer's deny_unknown_fields decode
  // fails — verbatim serde text (Rust: execution_uncertainty() inside
  // validate_snapshot produces the same error at read_task).
  f.db.run("UPDATE tasks SET status='needs_verification', result_json=? WHERE task_id=?",
    JSON.stringify({ execution_uncertainty: {
      previous_status: "running", item_id: SCOPE_ITEM(),
      reason: "x", previous_result: null, bogus: 1 } }), f.taskId);
  const c = new PublicationVerification(f.store, "check-run");
  expectThrowContains(() => c.resolve({ target_task_id: f.taskId }),
    "invalid execution uncertainty: unknown field `bogus`");
});
run("A9_uncertainty_unbound_item", () => {
  // Marker binds a non-running item -> w1's bound read rejects with
  // "invalid execution uncertainty" (Rust: "execution uncertainty does
  // not bind unresolved running item" — w1's port loses the verbatim
  // text; divergence flagged to lead, asserted as landed).
  const f = fixture("a9");
  f.db.run("UPDATE tasks SET status='needs_verification', result_json=? WHERE task_id=?",
    JSON.stringify({ execution_uncertainty: {
      previous_status: "running", item_id: "ghost-item",
      reason: "x", previous_result: null } }), f.taskId);
  const c = new PublicationVerification(f.store, "check-run");
  expectThrowContains(() => c.resolve({ target_task_id: f.taskId }),
    "invalid execution uncertainty");
});
function SCOPE_ITEM() { return JSON.stringify(["market", "sh.600006", "day"]); }

// -- B: prepare decision arms (canonical check/facts) -------------------
const checkSnap = (f, checkId) =>
  snapshotCanon(f.adm._readBoundTaskValidated(checkId));
const factsOf = (f) => f.store.publicationFacts(f.taskId);

run("B1_insufficient_evidence", () => {
  const f = fixture("b1");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  const plan = prepare(checkSnap(f, checkId), factsOf(f), "check-run",
    "test-owner", NOW);
  const r = plan.audit.receipt;
  if (r.conclusion !== "insufficient_evidence" ||
      r.missing_receipt_item_ids.length !== 1 ||
      !r.reason.startsWith("one or more task items lack")) {
    throw new Error(`receipt diverges: ${JSON.stringify(r)}`);
  }
});
run("B2_all_recorded", () => {
  const f = fixture("b2", { publish: false });
  // commit the publication AFTER check acceptance — target succeeds,
  // committed evidence binds.
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  publishTarget(f);
  const plan = prepare(checkSnap(f, checkId), factsOf(f), "check-run",
    "test-owner", NOW);
  const r = plan.audit.receipt;
  if (r.conclusion !== "all_recorded" || r.missing_receipt_item_ids.length !== 0 ||
      !r.reason.startsWith("all task items have verified")) {
    throw new Error(`receipt diverges: ${JSON.stringify(r)}`);
  }
});
import * as MC from "../../src_node/market/market_commit.mjs";
import * as MRRead from "../../src_node/market/market_read.mjs";
import { createHash } from "node:crypto";
const sha256 = (s) => createHash("sha256").update(s).digest("hex");
function publishTarget(f) {
  const record = MRRead.readRecord(f.db, "legacy-daily", false, true);
  const n = f.db.run(
    "UPDATE data_versions SET state='validated' WHERE version_id=? AND state IN ('staging','validated')",
    "legacy-daily").changes;
  if (n !== 1) throw new Error("validate failed");
  const scope = { symbol: "sh.600006", period: "Day" };
  const pubPlan = {
    command: {
      publication_id: "p", version_id: "legacy-daily",
      task_id: f.taskId, item_id: f.itemId, run_id: "run",
      expected_revision: 2,
      intent: { Daily: { expected_head: null } },
    },
    record,
    required_heads: [[scope, null]],
    promote: scope,
    base_digest: null,
  };
  f.db.exec("BEGIN IMMEDIATE");
  try { MC.commitPublication(f.db, pubPlan, NOW); f.db.exec("COMMIT"); }
  catch (e) { try { f.db.exec("ROLLBACK"); } catch {} throw e; }
}
run("B3_not_applicable", () => {
  // missing evidence AND target no longer needs_verification ->
  // not_applicable. Cancel the target to a terminal cancelled state.
  const f2 = fixture("b3b");
  const c2 = new PublicationVerification(f2.store, "check-run");
  const { taskId: cid2 } = acceptCheckTask(f2, c2, "chk2", f2.taskId);
  f2.adm.claim(cid2, 0n, "check-run", NOW);
  f2.adm.startItem(cid2, 1n, "check-run", "publication-results", NOW);
  // cancel the target (requestCancel + finishCancellation -> cancelled)
  const t = f2.adm._readBoundTaskValidated(f2.taskId);
  f2.adm.requestCancel(f2.taskId, BigInt(t.task.state_revision), "run", NOW);
  const t2 = f2.adm._readBoundTaskValidated(f2.taskId);
  f2.adm.finishItem(f2.taskId, BigInt(t2.task.state_revision), "run",
    t2.items[0].item_id, { success: false, error: "cancelled", evidence: null }, NOW);
  const t3 = f2.adm._readBoundTaskValidated(f2.taskId);
  f2.adm.finishCancellation(f2.taskId, BigInt(t3.task.state_revision), "run", NOW);
  const plan = prepare(checkSnap(f2, cid2), f2.store.publicationFacts(f2.taskId),
    "check-run", "test-owner", NOW);
  if (plan.audit.receipt.conclusion !== "not_applicable" ||
      !plan.audit.receipt.reason.startsWith("target no longer requires")) {
    throw new Error(`receipt diverges: ${JSON.stringify(plan.audit.receipt)}`);
  }
});
run("B4_fact_unknown_item", () => {
  const f = fixture("b4");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  const facts = factsOf(f);
  // same length, different id -> unknown-item arm (len gate passed)
  facts.items = [{ item_id: "ghost", evidence: "Missing" }];
  expectThrow(() => prepare(checkSnap(f, checkId), facts, "check-run",
    "test-owner", NOW),
    "ACCEPTANCE_NEEDS_VERIFICATION: publication fact has unknown item");
});
run("B5_fact_len_gate_before_dup", () => {
  // Rust checks len/empty FIRST — an extra (duplicate) fact hits
  // "incomplete publication evidence item set" before the dup loop.
  const f = fixture("b5");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  const facts = factsOf(f);
  facts.items.push({ ...facts.items[0] });
  expectThrow(() => prepare(checkSnap(f, checkId), facts, "check-run",
    "test-owner", NOW),
    "ACCEPTANCE_NEEDS_VERIFICATION: incomplete publication evidence item set");
});
run("B6_missing_on_success", () => {
  // Target published (item finished+success, committed evidence) then
  // facts rewritten to Missing -> the successful-item guard fires.
  const f = fixture("b6");
  const c = new PublicationVerification(f.store, "check-run");
  // accept while target unresolved, then publish -> Committed evidence
  // exists; rewriting facts to Missing trips the successful-item guard.
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  publishTarget(f);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  const facts = factsOf(f);
  facts.items = facts.items.map((i) => ({ ...i, evidence: "Missing" }));
  expectThrow(() => prepare(checkSnap(f, checkId), facts, "check-run",
    "test-owner", NOW),
    "ACCEPTANCE_NEEDS_VERIFICATION: successful item lacks publication evidence");
});
run("B7_wrong_run_prepare_conflict", () => {
  const f = fixture("b7");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  expectThrow(() => prepare(checkSnap(f, checkId), factsOf(f),
    "other-run", "test-owner", NOW), "STATE_REVISION_CONFLICT");
});
run("B8_empty_facts", () => {
  const f = fixture("b8");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  expectThrow(() => prepare(checkSnap(f, checkId),
    { task: factsOf(f).task, items: [] }, "check-run", "test-owner", NOW),
    "ACCEPTANCE_NEEDS_VERIFICATION: incomplete publication evidence item set");
});

// -- C: runner arms ------------------------------------------------------
const runnerFor = (f, over = {}) => new Runner(
  f.store, { publicationFacts: over.facts ?? ((t) => f.store.publicationFacts(t)),
    checkReceipt: over.receipt ?? ((i) => f.store.checkReceipt(i)),
    commitCheck: over.commit ?? ((p) => f.store.commitCheck(p)) },
  over.runId ?? "check-run", over.actor ?? "test-owner", () => NOW);

run("C1_missing_identity", () => {
  const f = fixture("c1");
  expectThrow(() => runnerFor(f, { runId: "  " }).run("x"),
    "INVALID_MARKET_COMMAND: missing publication inspection runtime identity");
  expectThrow(() => runnerFor(f, { actor: "" }).run("x"),
    "INVALID_MARKET_COMMAND: missing publication inspection runtime identity");
});
run("C2_receipt_short_circuit", () => {
  const f = fixture("c2");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  const plan = prepare(checkSnap(f, checkId), factsOf(f), "check-run",
    "test-owner", NOW);
  const rcpt = f.store.commitCheck(plan);
  const out = runnerFor(f).run(checkId);
  if (out.outcome !== "completed" || !canonEq(out.receipt, rcpt)) {
    throw new Error("short-circuit diverged");
  }
});
run("C3_unsupported_key", () => {
  const f = fixture("c3");
  // the TARGET task (collection key) passed as the check id
  expectThrow(() => runnerFor(f).run(f.taskId),
    "UNSUPPORTED_CONTRACT");
});
run("C4_needs_verification_visible", () => {
  const f = fixture("c4");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  let t = f.adm._readBoundTaskValidated(checkId);
  f.adm.requireVerification(checkId, BigInt(t.task.state_revision),
    "check-run", "publication-results", "uncertain outcome", NOW);
  expectThrow(() => runnerFor(f).run(checkId),
    "PUBLISH_OUTCOME_UNKNOWN: old or running check has no reliable receipt; no automatic re-execution; inspect the same publication_id");
});
run("C5_item_running_outcome_unknown", () => {
  const f = fixture("c5");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  expectThrow(() => runnerFor(f).run(checkId),
    "PUBLISH_OUTCOME_UNKNOWN: old or running check has no reliable receipt; no automatic re-execution; inspect the same publication_id");
});
run("C6_cancel_pending_check", () => {
  const f = fixture("c6");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.requestCancel(checkId, 0n, "check-run", NOW);
  const out = runnerFor(f).run(checkId);
  if (out.outcome !== "cancelled" ||
      out.snapshot.task.status !== "cancelled") {
    throw new Error(`cancel diverged: ${JSON.stringify(out.snapshot?.task?.status)}`);
  }
});
run("C7_cancel_running_check", () => {
  const f = fixture("c7");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  // running item under this run -> visible_status == running -> the
  // running arm fires first (OutcomeUnknown). The cancel-after-claim
  // arm: status cancel_requested requires requestCancel while running.
  // Rust run() checks visible_status needs_verification OR item running
  // -> OutcomeUnknown BEFORE the cancel_requested arm — so a running
  // check under our own run cannot reach cancel. Cancel must arrive
  // while pending. Covered by C6; here verify the ordering guard.
  const t = f.adm._readBoundTaskValidated(checkId);
  f.adm.requestCancel(checkId, BigInt(t.task.state_revision), "check-run", NOW);
  expectThrow(() => runnerFor(f).run(checkId),
    "PUBLISH_OUTCOME_UNKNOWN: old or running check has no reliable receipt; no automatic re-execution; inspect the same publication_id");
});
run("C8_terminal_check", () => {
  const f = fixture("c8");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  let t = f.adm._readBoundTaskValidated(checkId);
  f.adm.finishItem(checkId, BigInt(t.task.state_revision), "check-run",
    "publication-results", { success: false, error: "x", evidence: null }, NOW);
  t = f.adm._readBoundTaskValidated(checkId);
  f.adm.finishExecution(checkId, BigInt(t.task.state_revision), "check-run", NOW);
  expectThrow(() => runnerFor(f).run(checkId),
    "PUBLISH_OUTCOME_UNKNOWN: check has no reliable final receipt; inspect the same publication_id");
});
run("C9_happy_path_end_to_end", () => {
  const f = fixture("c9");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  const before = f.adm._readBoundTaskValidated(f.taskId);
  const out = runnerFor(f).run(checkId);
  if (out.outcome !== "completed") throw new Error("not completed");
  const r = out.receipt;
  if (r.verification_task_id !== checkId ||
      r.target_task_id !== f.taskId ||
      r.conclusion !== "insufficient_evidence" ||
      r.missing_receipt_item_ids.length !== 1) {
    throw new Error(`receipt diverges: ${JSON.stringify(r)}`);
  }
  // committed check task == audit.verification_after, audit row exists,
  // target untouched.
  const after = f.adm._readBoundTaskValidated(checkId);
  const audit = JSON.parse(f.db.get(
    "SELECT audit_json FROM publication_checks WHERE verification_task_id=?",
    checkId).audit_json);
  if (audit.receipt.conclusion !== r.conclusion ||
      !canonEq(audit.verification_after, snapshotCanon(after))) {
    throw new Error("audit/commit mismatch");
  }
  const tAfter = f.adm._readBoundTaskValidated(f.taskId);
  if (!canonEq(snapshotCanon(before), snapshotCanon(tAfter))) {
    throw new Error("target mutated");
  }
  // checkReceipt read-back + replay commit both return the receipt.
  const reread = f.store.checkReceipt(checkId);
  if (!canonEq(reread, r)) throw new Error("receipt read-back differs");
});
run("C10_commit_conflict_same_state_retry", () => {
  const f = fixture("c10");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  let calls = 0;
  const out = runnerFor(f, {
    commit: (p) => {
      calls++;
      if (calls === 1) {
        throw Object.assign(new Error("PUBLISH_PRECONDITION: x"),
          { kind: "conflict" });
      }
      return f.store.commitCheck(p);
    } }).run(checkId);
  if (out.outcome !== "completed" || calls !== 2) {
    throw new Error(`conflict-retry diverged: calls=${calls}`);
  }
});
run("C11_commit_conflict_state_changed", () => {
  const f = fixture("c11");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  const out = runnerFor(f, {
    commit: (p) => {
      // mutate the check inside the commit attempt: finish the item
      // then cancel, so current != verification_before.
      const t = f.adm._readBoundTaskValidated(checkId);
      f.adm.requestCancel(checkId, BigInt(t.task.state_revision),
        "check-run", NOW);
      throw Object.assign(new Error("PUBLISH_PRECONDITION: x"),
        { kind: "conflict" });
    } });
  // current status becomes cancel_requested -> cancel() path.
  const r = out.run(checkId);
  if (r.outcome !== "cancelled") {
    throw new Error(`expected cancelled, got ${r.outcome}`);
  }
});
run("C12_commit_conflict_drift_preserves", () => {
  const f = fixture("c12");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  const r = runnerFor(f, {
    commit: (p) => {
      // drift WITHOUT cancel: finish item + execution via a different
      // shape (revision moves) then throw conflict.
      const t = f.adm._readBoundTaskValidated(checkId);
      f.adm.requireVerification(checkId, BigInt(t.task.state_revision),
        "check-run", "publication-results", "drift", NOW);
      throw Object.assign(new Error("PUBLISH_PRECONDITION: x"),
        { kind: "conflict" });
    } });
  expectThrow(() => r.run(checkId), "PUBLISH_PRECONDITION: x");
  const t = f.adm._readBoundTaskValidated(checkId);
  if (t.task.status !== "needs_verification") {
    throw new Error(`expected needs_verification, got ${t.task.status}`);
  }
});
run("C13_facts_error_preserves_unresolved", () => {
  const f = fixture("c13");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  const r = runnerFor(f, {
    facts: () => { throw Object.assign(new Error("MARKET_STORAGE: db gone"),
      { kind: "storage" }); } });
  expectThrow(() => r.run(checkId), "MARKET_STORAGE: db gone");
  const t = f.adm._readBoundTaskValidated(checkId);
  if (t.task.status !== "needs_verification") {
    throw new Error(`expected needs_verification, got ${t.task.status}`);
  }
});
run("C14_commit_error_receipt_fallback", () => {
  const f = fixture("c14");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  const plan = prepare((() => {
    f.adm.claim(checkId, 0n, "check-run", NOW);
    f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
    return checkSnap(f, checkId);
  })(), factsOf(f), "check-run", "test-owner", NOW);
  const rcpt = f.store.commitCheck(plan);
  const out = runnerFor(f, {
    commit: () => { throw new Error("boom"); } }).run(checkId);
  if (out.outcome !== "completed" || !canonEq(out.receipt, rcpt)) {
    throw new Error("receipt fallback diverged");
  }
});

// -- D: existing::accept -------------------------------------------------
run("D1_empty_request", () => {
  const f = fixture("d1");
  const ctl = control({ db: f.db, adm: f.adm }, "check-run",
    new PublicationVerification(f.store, "check-run"));
  expectThrow(() => existingAccept(ctl, "  ", f.taskId),
    "INVALID_COMMAND: empty request_id");
});
run("D2_empty_run", () => {
  const f = fixture("d2");
  const ctl = control({ db: f.db, adm: f.adm }, "",
    new PublicationVerification(f.store, "check-run"));
  expectThrow(() => existingAccept(ctl, "req-x", f.taskId),
    "INVALID_COMMAND: empty verification run");
});
run("D3_target_missing", () => {
  const f = fixture("d3");
  const storeCtl = { repository: f.store, runId: "check-run",
    accept: () => { throw new Error("unreachable"); } };
  expectThrow(() => existingAccept(storeCtl, "req-x", "ghost"),
    "TASK_NOT_FOUND");
});
run("D4_unsupported_target_contract", () => {
  const f = fixture("d4");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: selfCheck } = acceptCheckTask(f, c, "chk-self", f.taskId);
  const storeCtl = { repository: f.store, runId: "check-run",
    accept: () => { throw new Error("unreachable"); } };
  expectThrow(() => existingAccept(storeCtl, "req-x", selfCheck),
    "UNSUPPORTED_CONTRACT");
});
run("D5_no_known_falls_to_accept", () => {
  const f = fixture("d5");
  const c = new PublicationVerification(f.store, "check-run");
  const ctl = control({ db: f.db, adm: f.adm }, "check-run", c);
  ctl.repository = f.store;
  const a = existingAccept(ctl, "req-new", f.taskId);
  if (!a.taskId || a.reused) throw new Error(`accept diverged: ${JSON.stringify(a)}`);
});
run("D6_known_receipt_binds_terminal", () => {
  const f = fixture("d6");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  // commit an all_recorded check: publish target first
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  publishTarget(f);
  const plan = prepare(checkSnap(f, checkId), factsOf(f), "check-run",
    "test-owner", NOW);
  const rcpt = f.store.commitCheck(plan);
  if (rcpt.conclusion !== "all_recorded") throw new Error("setup: not all_recorded");
  const ctl = { repository: f.store, runId: "check-run",
    accept: () => { throw new Error("unreachable — known must bind first"); } };
  const a = existingAccept(ctl, "req-known", f.taskId);
  if (!a.record || a.reused !== false ||
      a.record.outcome?.Terminal?.kind !== "already_verified" ||
      !canonEq(a.record.outcome.Terminal.receipt, rcpt) ||
      a.record.command.intent.target_task_id !== f.taskId) {
    throw new Error(`binding diverged: ${JSON.stringify(a)}`);
  }
  const row = f.db.get(
    "SELECT outcome_kind,task_id,terminal_json FROM requests WHERE request_id='req-known'");
  if (row.outcome_kind !== "terminal" || row.task_id !== null ||
      JSON.parse(row.terminal_json).receipt.conclusion !== "all_recorded") {
    throw new Error("terminal row diverged");
  }
});
run("D7_known_replay_reuses", () => {
  const f = fixture("d7");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  publishTarget(f);
  const plan = prepare(checkSnap(f, checkId), factsOf(f), "check-run",
    "test-owner", NOW);
  f.store.commitCheck(plan);
  const ctl = { repository: f.store, runId: "check-run",
    accept: () => { throw new Error("unreachable"); } };
  const a1 = existingAccept(ctl, "req-known", f.taskId);
  // replay: read_request exists -> control.accept path -> replay.
  const ctl2 = { repository: f.store, runId: "check-run",
    accept: (id, key, intent) => {
      // TaskControl::accept replay arm: stored command must match.
      const ex = f.store.readRequest(id);
      if (!canonEq(ex.command, { key, intent })) {
        throw Object.assign(new Error("REQUEST_CONFLICT"), { kind: "task" });
      }
      return { record: ex, reused: true };
    } };
  const a2 = existingAccept(ctl2, "req-known", f.taskId);
  if (!a2.reused || a1.record.request_id !== a2.record.request_id) {
    throw new Error("replay diverged");
  }
});
run("D8_known_conclusion_not_all_recorded", () => {
  const f = fixture("d8");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  // insufficient_evidence commit — does NOT bind a new request.
  const plan = prepare(checkSnap(f, checkId), factsOf(f), "check-run",
    "test-owner", NOW);
  const rcpt = f.store.commitCheck(plan);
  if (rcpt.conclusion !== "insufficient_evidence") throw new Error("setup");
  // known() returns null (not all_recorded) -> falls to control.accept:
  // target still unresolved+running -> a NEW check task is accepted.
  const c2 = new PublicationVerification(f.store, "check-run");
  const ctl = control({ db: f.db, adm: f.adm }, "check-run", c2);
  ctl.repository = f.store;
  const a = existingAccept(ctl, "req-x", f.taskId);
  if (!a.taskId || a.reused || a.taskId === checkId) {
    throw new Error(`expected fresh check accept, got ${JSON.stringify(a)}`);
  }
});
run("D9_request_exists_short_circuit", () => {
  const f = fixture("d9");
  const c = new PublicationVerification(f.store, "check-run");
  const ctl = control({ db: f.db, adm: f.adm }, "check-run", c);
  ctl.repository = f.store;
  const a1 = existingAccept(ctl, "req-t", f.taskId);
  const a2 = existingAccept(ctl, "req-t", f.taskId);
  if (!a2.reused || a2.taskId !== a1.taskId) throw new Error("replay diverged");
});

// -- E: committed-audit tamper (fail closed) ----------------------------
run("E1_audit_digest_tamper", () => {
  const f = fixture("e1");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  f.store.commitCheck(prepare(checkSnap(f, checkId), factsOf(f),
    "check-run", "test-owner", NOW));
  f.db.run("UPDATE publication_checks SET audit_sha256=? WHERE verification_task_id=?",
    "f".repeat(64), checkId);
  expectThrow(() => f.store.checkReceipt(checkId),
    "MARKET_INTEGRITY: publication check audit digest differs");
});
run("E2_audit_association_tamper", () => {
  const f = fixture("e2");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  f.store.commitCheck(prepare(checkSnap(f, checkId), factsOf(f),
    "check-run", "test-owner", NOW));
  const row = f.db.get("SELECT audit_json FROM publication_checks WHERE verification_task_id=?", checkId);
  const a = JSON.parse(row.audit_json);
  a.receipt.target_task_id = "tampered";
  const raw = JSON.stringify(a);
  f.db.run("UPDATE publication_checks SET audit_json=?, audit_sha256=? WHERE verification_task_id=?",
    raw, sha256(raw), checkId);
  expectThrow(() => f.store.checkReceipt(checkId),
    "MARKET_INTEGRITY: publication check association differs");
});
run("E3_audit_projection_tamper", () => {
  const f = fixture("e3");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  f.store.commitCheck(prepare(checkSnap(f, checkId), factsOf(f),
    "check-run", "test-owner", NOW));
  const row = f.db.get("SELECT audit_json FROM publication_checks WHERE verification_task_id=?", checkId);
  const a = JSON.parse(row.audit_json);
  a.receipt.reason = "tampered reason";
  const raw = JSON.stringify(a);
  f.db.run("UPDATE publication_checks SET audit_json=?, audit_sha256=? WHERE verification_task_id=?",
    raw, sha256(raw), checkId);
  expectThrowContains(() => f.store.checkReceipt(checkId),
    "publication check audit projections differ");
});
run("E4_completion_drift", () => {
  const f = fixture("e4");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  f.store.commitCheck(prepare(checkSnap(f, checkId), factsOf(f),
    "check-run", "test-owner", NOW));
  // drift the task row so it differs from audit.verification_after —
  // the bound read fails closed before the completion compare.
  f.db.run("UPDATE tasks SET status='failed' WHERE task_id=?", checkId);
  expectThrowContains(() => f.store.checkReceipt(checkId),
    "");
});
run("E5_evidence_changed", () => {
  const f = fixture("e5");
  const c = new PublicationVerification(f.store, "check-run");
  const { taskId: checkId } = acceptCheckTask(f, c, "chk", f.taskId);
  f.adm.claim(checkId, 0n, "check-run", NOW);
  f.adm.startItem(checkId, 1n, "check-run", "publication-results", NOW);
  publishTarget(f);
  const plan = prepare(checkSnap(f, checkId), factsOf(f), "check-run",
    "test-owner", NOW);
  f.store.commitCheck(plan);
  // all_recorded receipt re-reads facts — corrupt the committed
  // evidence: the legacy arm reads publication_batches.
  f.db.run("UPDATE publication_batches SET receipt_json='{}' WHERE task_id=?",
    f.taskId);
  expectThrowContains(() => f.store.checkReceipt(checkId),
    "");
});
// -- F: byte parity — serAuditJson vs Rust-written audit bytes ----------
run("F1_audit_bytes_vs_rust", () => {
  // Extract the Rust-written audit_json from a DIFF_CHECK fixture built
  // by the oracle (target/debug built earlier; build via cargo test).
  const rustFix = join(FIX, "rust-check.db");
  execFileSync(join(REPO, "target/debug/task_sqlite"), ["init", rustFix]);
  // Use the cargo test harness to build the DIFF_CHECK fixture.
  try {
    execFileSync("cargo", ["test", "--lib",
      "diff_harness::build_market_fixture", "--", "--nocapture",
      "--test-threads=1"], {
      cwd: REPO,
      env: { ...process.env, DIFF_DB: rustFix, DIFF_CHECK: "1",
        DIFF_RESULTS: "1",
        DIFF_RECORDS: "valid_daily.json,valid_yearly.json,nm052_result.json|pub=res-ch|nohead",
        DIFF_FIXTURES: join(REPO, "scripts/node_migration_p3/records"),
        TMPDIR: "/dev/shm" },
      stdio: ["ignore", "ignore", "pipe"], timeout: 600000,
    });
  } catch (e) {
    emit("F1_audit_bytes_vs_rust", "NOT_RUN",
      `rust fixture build failed: ${e.message.slice(0, 120)}`);
    return;
  }
  const rdb = openDatabase(rustFix, { mode: "readonly" });
  const row = rdb.get(
    "SELECT audit_json FROM publication_checks LIMIT 1");
  if (!row) {
    rdb.close();
    emit("F1_audit_bytes_vs_rust", "NOT_RUN", "no audit row in fixture");
    return;
  }
  const audit = JSON.parse(row.audit_json);
  const mine = ser(audit, "checkAudit");
  if (mine !== row.audit_json) {
    throw new Error(
      `audit bytes differ:\nrust=${row.audit_json.slice(0, 200)}\nmine=${mine.slice(0, 200)}`);
  }
  rdb.close();
});

// -- selftest: intentional fail must exit nonzero -----------------------
if (SELFTEST) {
  emit("ZZ_selftest_injected_failure", "FAIL", "intentional");
}
const failed = results.filter((r) => r.status === "FAIL");
const notRun = results.filter((r) => r.status === "NOT_RUN");
emit("summary",
  failed.length ? "FAIL" : "PASS",
  `${results.length} checks, ${failed.length} failed, ${notRun.length} not_run`);
if (OUT) {
  mkdirSync(OUT, { recursive: true });
  writeFileSync(join(OUT, "probe-results.jsonl"),
    results.map((r) => JSON.stringify(r)).join("\n") + "\n");
  writeFileSync(join(OUT, "results.json"), JSON.stringify({
    family: "publication_verification", total: results.length,
    pass: results.filter((r) => r.status === "PASS").length,
    fail: failed.length, not_run: notRun.length,
  }, null, 2));
}
process.exit(failed.length ? 1 : 0);
