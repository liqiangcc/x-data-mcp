// NM-048 P3: verification storage — receipts, task links, inspection
// predicates, runtime guard. Synthetic DBs only. Exit nonzero on FAIL.
import { createHash } from "node:crypto";
import { copyFileSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const { TaskAdmission } = await import("../../src_node/storage/task_repository.mjs");
const { openDatabase, StorageError } = await import("../../src_node/storage/sqlite_adapter.mjs");
const V = await import("../../src_node/storage/verification_storage.mjs");
const RT = await import("../../src_node/storage/verification_runtime.mjs");

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "verification", id, status, detail, expected, actual }));
};
const run = (id, fn) => {
  try { emit(id, "PASS", null, fn()); }
  catch (e) { emit(id, "FAIL", `${e.code || ""} ${e.message}`); }
};
const expectCode = (id, code, fn) => {
  try { fn(); emit(id, "FAIL", "expected rejection but call succeeded"); }
  catch (e) {
    emit(id, e instanceof StorageError && e.code === code ? "PASS" : "FAIL", `${e.code} ${e.message}`, code, e.code);
  }
};
const sha = (p) => createHash("sha256").update(readFileSync(p)).digest("hex");
const canon = (j) => V.ser(j); // sorted-key canonical for comparisons in probe

const FIX = mkdtempSync("/dev/shm/nm48-"); // tmpfs — local-fs gate passes
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const DB = join(FIX, "verif.db");
copyFileSync("/dev/shm/nm006/db18.sqlite", DB);

const withRepo = (fn) => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { return fn(repo); } finally { repo.close(); }
};
const snapJson = (id) => withRepo((repo) => {
  const s = repo._readBoundTaskValidated(id);
  return s ? V.snapshotCanon(s) : null;
});

// --- seed a target task (driven through primitives) ---
const seedTarget = (name) => {
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  const a = repo.admit({
    requestId: `req-t-${name}`, commandJson: JSON.stringify({ key: { operation: "collect", dataset: "ds", contract_version: "1" }, intent: {} }),
    executionKeyJson: JSON.stringify({ operation: "collect", dataset: "ds", contract_version: "1" }),
    workJson: JSON.stringify({ items: [{ item_id: "i", target: {}, progress_unit: "d", frozen_inputs: {}, scope_keys: [`sc-${name}`] }], scopes: [`sc-${name}`] }),
    items: [{ item_id: "i", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: `["sc-${name}"]` }],
    scopes: [`sc-${name}`], runId: "run-0", createdAt: "2026-01-02T10:00:00Z",
  });
  d.close();
  return a.task_id;
};
const seedVerification = (targetId, run = "run-v", acceptedRun = "run-0") => {
  const scope = `verification:task:${targetId}`;
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  const a = repo.admit({
    requestId: `req-v-${targetId.slice(-8)}`,
    commandJson: JSON.stringify({ key: V.VERIFICATION_KEY, intent: { target_task_id: targetId } }),
    executionKeyJson: JSON.stringify(V.VERIFICATION_KEY),
    workJson: JSON.stringify({
      items: [{ item_id: "stopped-writes", target: {}, progress_unit: "evidence_check",
        frozen_inputs: { target_task_id: targetId, accepted_run_id: acceptedRun }, scope_keys: [scope] }],
      scopes: [scope],
    }),
    items: [{ item_id: "stopped-writes", target_json: "{}", progress_unit: "evidence_check",
      frozen_inputs_json: JSON.stringify({ target_task_id: targetId, accepted_run_id: acceptedRun }), scope_keys_json: `["${scope}"]` }],
    scopes: [scope], runId: run, createdAt: "2026-01-02T10:10:00Z",
  });
  d.close();
  return a.task_id;
};

// --- not_applicable audit: target terminal (succeeded) ---
run("receipt_read_happy", () => {
  const t = seedTarget("na");
  withRepo((repo) => {
    repo.claim(t, 0n, "run-0", "2026-01-02T10:00:10Z");
    repo.startItem(t, 1n, "run-0", "i", "2026-01-02T10:00:20Z");
    repo.finishItem(t, 2n, "run-0", "i", { success: true }, "2026-01-02T10:00:30Z");
    repo.finishExecution(t, 3n, "run-0", "2026-01-02T10:00:40Z");
  });
  const v = seedVerification(t);
  withRepo((repo) => {
    repo.claim(v, 0n, "run-v", "2026-01-02T10:10:10Z");
    repo.startItem(v, 1n, "run-v", "stopped-writes", "2026-01-02T10:10:20Z");
  });
  // freeze the verification's "before" snapshot: running, item running.
  // The audit's verification_before is the claimed-running projection.
  const vBefore = snapJson(v);
  const tBefore = snapJson(t);
  // build the audit via the prepare port (Recorded inspector)
  const { audit } = V.prepareStop(vBefore, tBefore, "run-v", "auditor",
    "2026-01-02T10:11:00Z", null);
  if (audit.receipt.conclusion !== "not_applicable") {
    throw new Error(`expected not_applicable, got ${audit.receipt.conclusion}`);
  }
  // persist: verification task -> verification_after, audit row
  const d = openDatabase(DB, { mode: "readwrite" });
  V.writeTaskSnapshot(d, audit.verification_after, vBefore.task.state_revision);
  const raw = V.ser(audit, "audit");
  d.run("INSERT INTO stopped_write_checks VALUES(?,?,?,?)",
    v, t, raw, createHash("sha256").update(raw).digest("hex"));
  d.close();
  const got = withRepo((repo) => repo.readReceipt(v));
  if (!canon(got.receipt) || got.receipt.conclusion !== "not_applicable" ||
      got.receipt.target_task_id !== t) throw new Error("receipt mismatch");
  // bound read of v is consistent (link check: verification_after == stored)
  snapJson(v);
  return { conclusion: "not_applicable", receipt_verbatim: true };
});

run("receipt_absent_null", () => {
  const ghost = seedTarget("ghost");
  const r = withRepo((repo) => repo.readReceipt(ghost));
  if (r !== null) throw new Error("expected null");
  return { absent: true };
});

// --- interrupted audit: needs_verification target ---
run("receipt_interrupted_link", () => {
  const t = seedTarget("int");
  withRepo((repo) => {
    repo.claim(t, 0n, "run-0", "2026-01-02T10:20:10Z");
    repo.startItem(t, 1n, "run-0", "i", "2026-01-02T10:20:20Z");
    repo.requireVerification(t, 2n, "run-0", "i", "lost writer", "2026-01-02T10:20:30Z");
  });
  const v = seedVerification(t);
  withRepo((repo) => {
    repo.claim(v, 0n, "run-v", "2026-01-02T10:21:10Z");
    repo.startItem(v, 1n, "run-v", "stopped-writes", "2026-01-02T10:21:20Z");
  });
  const vBefore = snapJson(v);
  const tBefore = snapJson(t);
  const evidence_ref = "evidence://stopped/1";
  const { audit } = V.prepareStop(vBefore, tBefore, "run-v", "auditor",
    "2026-01-02T10:22:00Z", {
      kind: "stopped",
      snapshot_sha256: createHash("sha256").update(Buffer.from(V.ser(tBefore, "snapshot"))).digest("hex"),
      evidence_ref,
    });
  if (audit.receipt.conclusion !== "interrupted" || !audit.target_after) {
    throw new Error("expected interrupted");
  }
  const d = openDatabase(DB, { mode: "readwrite" });
  V.writeTaskSnapshot(d, audit.target_after, tBefore.task.state_revision);
  V.writeTaskSnapshot(d, audit.verification_after, vBefore.task.state_revision);
  const raw = V.ser(audit, "audit");
  d.run("INSERT INTO stopped_write_checks VALUES(?,?,?,?)",
    v, t, raw, createHash("sha256").update(raw).digest("hex"));
  d.close();
  const got = withRepo((repo) => repo.readReceipt(v));
  if (got.receipt.conclusion !== "interrupted") throw new Error("receipt");
  // task-link wiring: bound read of interrupted target resolves
  // audit.target_after equality
  snapJson(t);
  return { conclusion: "interrupted", link_verified: true };
});

// --- link tamper: verification_after diverges ---
run("link_tamper_fails_closed", () => {
  const t = seedTarget("tmp");
  withRepo((repo) => {
    repo.claim(t, 0n, "run-0", "2026-01-02T10:28:00Z");
    repo.startItem(t, 1n, "run-0", "i", "2026-01-02T10:28:10Z");
    repo.finishItem(t, 2n, "run-0", "i", { success: true }, "2026-01-02T10:28:20Z");
    repo.finishExecution(t, 3n, "run-0", "2026-01-02T10:28:30Z");
  });
  const v = seedVerification(t);
  withRepo((repo) => {
    repo.claim(v, 0n, "run-v", "2026-01-02T10:29:10Z");
    repo.startItem(v, 1n, "run-v", "stopped-writes", "2026-01-02T10:29:20Z");
  });
  const vBefore = snapJson(v);
  const tBefore = snapJson(t);
  const { audit } = V.prepareStop(vBefore, tBefore, "run-v", "auditor",
    "2026-01-02T10:30:00Z", null);
  const d = openDatabase(DB, { mode: "readwrite" });
  V.writeTaskSnapshot(d, audit.verification_after, vBefore.task.state_revision);
  const raw = V.ser(audit, "audit");
  d.run("INSERT INTO stopped_write_checks VALUES(?,?,?,?)",
    v, t, raw, createHash("sha256").update(raw).digest("hex"));
  // tamper: bump the verification task's progress stamp post-audit
  d.run("UPDATE tasks SET last_progress_at='2099-01-01T00:00:00Z' WHERE task_id=?", v);
  d.close();
  try {
    withRepo((repo) => repo.readReceipt(v));
    throw new Error("tampered audit accepted");
  } catch (e) {
    // tamper surfaces at the wired link check (task differs from
    // stopped check audit) or the receipt's own comparison — either is
    // the fail-closed needs_verification class
    if (e.code !== "needs_verification" ||
        !e.message.includes("differs")) throw e;
    return { code: e.code };
  }
});

// --- inspection predicates ---
run("inspection_predicates", () => {
  const d = openDatabase(DB, { mode: "readwrite" });
  // validate_task_request: inspection key -> integrity
  try {
    V.validateTaskRequest(d, "req-x", { key: V.INSPECTION_KEY, intent: {} });
    throw new Error("inspection bound to task");
  } catch (e) {
    if (!e.message.includes("must not bind")) throw e;
  }
  // validate_absent_request negative: plant a row
  const tSeed0 = seedTarget("insp-t");
  const auditRow = JSON.stringify({
    request_id: "req-insp", command: { key: V.INSPECTION_KEY, intent: { target_task_id: tSeed0 } },
    actor: "a", run_id: "r", checked_at: "2026-01-02T10:40:00Z",
    facts: { target: { task: { task_id: tSeed0 } } }, observation: "Excluded",
    after: null, result: { VerificationInspection: { receipt: { r: 1 } } },
  });
  const digest = createHash("sha256").update(auditRow).digest("hex");
  // request row first (FK); target task must exist — reuse a seeded one
  const cmd = JSON.stringify({ key: V.INSPECTION_KEY, intent: { target_task_id: tSeed0 } });
  d.run("INSERT INTO requests VALUES(?,?,?,'terminal',NULL,?)",
    "req-insp", cmd, createHash("sha256").update(cmd).digest("hex"),
    JSON.stringify({ VerificationInspection: { receipt: { r: 1 } } }));
  d.run("INSERT INTO verification_inspections VALUES(?,?,?,?)", "req-insp", tSeed0, auditRow, digest);
  try { V.validateAbsentRequest(d, "req-insp"); throw new Error("absent missed"); }
  catch (e) { if (!e.message.includes("binding missing")) throw e; }
  // validate_terminal inspection variant: replay equal -> ok; differ -> integrity
  V.validateTerminalInspection(d, "req-insp",
    { key: V.INSPECTION_KEY, intent: { target_task_id: tSeed0 } },
    { VerificationInspection: { receipt: { r: 1 } } });
  try {
    V.validateTerminalInspection(d, "req-insp",
      { key: V.INSPECTION_KEY, intent: { target_task_id: tSeed0 } },
      { VerificationInspection: { receipt: { r: 2 } } });
    throw new Error("differing terminal accepted");
  } catch (e) { if (!e.message.includes("differs")) throw e; }
  // validate_terminal stop variant: wrong key -> early Ok; inspection key -> absent
  V.validateTerminalStop(d, { key: { operation: "x" }, intent: {} }, {}, null);
  d.close();
  return { absent_negative: true, terminal_replay: true };
});

// --- runtime registration + claim guard ---
run("runtime_claim_guard", () => {
  const v = seedVerification("rt-target");
  const vBefore = snapJson(v);
  const after = JSON.parse(JSON.stringify(vBefore));
  after.task.status = "running";
  after.task.owner_run_id = "run-v";
  after.task.state_revision += 1;
  after.task.last_progress_at = "2026-01-02T10:50:00Z";
  after.claims = after.claims.map((c) => ({ ...c, state_revision: after.task.state_revision }));
  const guard = RT.create(DB, v, "run-v");
  const res = withRepo((repo) => repo.claimWithRuntimeGuard(
    { before: vBefore, after, descriptor: guard.descriptor }, guard, DB));
  const reg = withRepo((repo) => repo.runtimeRegistration(v));
  if (!reg || reg.claim_revision !== 1 || reg.descriptor.task_id !== v) {
    throw new Error(JSON.stringify(reg));
  }
  guard.close();
  return { claim_revision: reg.claim_revision, bound: true };
});
run("runtime_guard_busy", () => {
  const v = seedVerification("rt-busy");
  const g1 = RT.create(DB, v, "run-v");
  try {
    RT.tryAcquire(DB, g1.descriptor);
    throw new Error("acquired while held");
  } catch (e) {
    if (e.kind !== "busy") throw e;
  }
  g1.close();
  return { busy: true };
});
run("runtime_overlayfs_refused", () => {
  const p = "/tmp/nm48-ovl.db";
  copyFileSync(DB, p);
  try { RT.create(p, "task-x", "run-x"); throw new Error("overlay allowed"); }
  catch (e) {
    if (e.kind !== "not_verified") throw e;
    return { kind: e.kind };
  } finally { rmSync(p, { force: true }); }
});
run("registration_orphan", () => {
  const d = openDatabase(DB, { mode: "readwrite" });
  d.pragma("foreign_keys=OFF");
  d.run("INSERT INTO verification_runtime_guards VALUES('ghost',1,'{}','x')");
  d.close();
  try { withRepo((repo) => repo.runtimeRegistration("ghost")); throw new Error("orphan accepted"); }
  catch (e) {
    if (e.code !== "needs_verification" || !e.message.includes("orphan")) throw e;
    return { code: e.code };
  }
});

// boundary
expectCode("read_receipt_implicit_mode", "invalid_input", () => TaskAdmission.open(DB));

// selftest
if (SELFTEST === "ver") {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try {
    const r = repo.readReceipt("nonexistent-phantom-receipt");
    emit("selftest_ver", r === null && false ? "PASS" : "FAIL", "unexpected — STOP");
  } catch (e) {
    emit("selftest_ver", "FAIL", `correctly rejected — selftest FAIL as designed (${e.code})`);
  }
  repo.close();
}

process.exitCode = failures === 0 ? 0 : 1;
