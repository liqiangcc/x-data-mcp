// NM-046 P3: validate recovery surface (require_verification /
// reconcile / candidates). Synthetic DBs only. Exit nonzero on FAIL.
// Usage: node recovery_probe.mjs <root> [selftest]
import { createHash } from "node:crypto";
import { copyFileSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const { TaskAdmission } = await import("../../src_node/storage/task_repository.mjs");
const { openDatabase, StorageError } = await import("../../src_node/storage/sqlite_adapter.mjs");

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "recovery", id, status, detail, expected, actual }));
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

const FIX = mkdtempSync("/tmp/nm46-rec-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const SRC = "/dev/shm/nm006";
const DB = join(FIX, "rec.db");
copyFileSync(join(SRC, "db18.sqlite"), DB);

let seq = 0;
const seed = (opts = {}) => {
  const { items = ["a"], run = "run-0" } = opts;
  seq++;
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  const a = repo.admit({
    requestId: `req-r${seq}`, commandJson: JSON.stringify({ key: { operation: `r${seq}` } }),
    executionKeyJson: "{}",
    workJson: JSON.stringify({
      items: items.map((i) => ({ item_id: i, target: {}, progress_unit: "d", frozen_inputs: {}, scope_keys: [`s-r${seq}`] })),
      scopes: [`s-r${seq}`],
    }),
    items: items.map((i) => ({ item_id: i, target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: `["s-r${seq}"]` })),
    scopes: [`s-r${seq}`], runId: run, createdAt: `2026-01-02T05:00:${String(seq).padStart(2, "0")}Z`,
  });
  d.close();
  return a.task_id;
};
const withRepo = (fn) => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { return fn(repo); } finally { repo.close(); }
};
const taskRow = (tid) => {
  const d = openDatabase(DB);
  const r = d.get("SELECT * FROM tasks WHERE task_id=?", tid);
  d.close(); return r;
};

// ---- require_verification ----
run("require_verification_happy", () => {
  const tid = seed();
  withRepo((repo) => repo.claim(tid, 0n, "run-0", "2026-01-02T05:10:00Z"));
  withRepo((repo) => repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T05:10:10Z"));
  const r = withRepo((repo) => repo.requireVerification(tid, 2n, "run-0", "a",
    "executor died mid-write", "2026-01-02T05:10:20Z"));
  const t = r.task;
  const expected = '{"execution_uncertainty":{"item_id":"a","previous_result":null,"previous_status":"running","reason":"executor died mid-write"}}';
  if (t.status !== "needs_verification" || t.state_revision !== 3n ||
      t.result_json !== expected) throw new Error(JSON.stringify(t));
  const d = openDatabase(DB);
  const i = d.get("SELECT stage FROM task_items WHERE task_id=? AND item_id='a'", tid);
  d.close();
  if (i.stage !== "running") throw new Error("item row mutated");
  return { status: "needs_verification", item_stays_running: true, result_verbatim: true };
});
run("require_verification_under_cancel", () => {
  const tid = seed();
  withRepo((repo) => {
    repo.claim(tid, 0n, "run-0", "2026-01-02T05:11:00Z");
    repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T05:11:10Z");
    repo.requestCancel(tid, 2n, "run-0", "2026-01-02T05:11:20Z");
  });
  const r = withRepo((repo) => repo.requireVerification(tid, 3n, "run-0", "a",
    "uncertain", "2026-01-02T05:11:30Z"));
  if (r.task.status !== "needs_verification") throw new Error(JSON.stringify(r.task));
  const pj = JSON.parse(r.task.result_json).execution_uncertainty;
  if (pj.previous_status !== "cancel_requested") throw new Error(JSON.stringify(pj));
  return { previous_status: "cancel_requested" };
});

const unchanged = (id, fn, code) => run(id, () => {
  const before = sha(DB);
  try { fn(); throw new Error("accepted"); }
  catch (e) {
    if (e.code !== code) throw e;
    if (sha(DB) !== before) throw new Error("rejection mutated DB");
    return { code: e.code };
  }
});
// arm guards -> revision_conflict verbatim
run("rv_guards", () => {
  const tid = seed();
  withRepo((repo) => {
    repo.claim(tid, 0n, "run-0", "2026-01-02T05:12:00Z");
    repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T05:12:10Z");
  });
  const before = sha(DB);
  const cases = [
    () => withRepo((r) => r.requireVerification(tid, 2n, "run-0", "a", "  ", "2026-01-02T05:12:20Z")), // empty reason
    () => withRepo((r) => r.requireVerification(tid, 2n, "run-0", "ghost", "x", "2026-01-02T05:12:20Z")), // item absent
    () => withRepo((r) => r.requireVerification(tid, 2n, "run-9", "a", "x", "2026-01-02T05:12:20Z")), // foreign run
  ];
  const codes = [];
  for (const fn of cases) {
    try { fn(); codes.push("ACCEPTED"); }
    catch (e) { codes.push(e.code); }
  }
  if (sha(DB) !== before) throw new Error("mutated");
  // foreign run hits run-identity gate first -> needs_verification
  if (codes.join() !== "revision_conflict,revision_conflict,needs_verification") {
    throw new Error(codes.join());
  }
  return { codes };
});
run("rv_pending_status", () => {
  const tid = seed(); // pending, owner NULL — run-identity passes (accepted==run)
  const before = sha(DB);
  try {
    withRepo((r) => r.requireVerification(tid, 0n, "run-0", "a", "x", "2026-01-02T05:13:00Z"));
    throw new Error("accepted");
  } catch (e) {
    if (e.code !== "revision_conflict" || sha(DB) !== before) throw e;
    return { code: e.code };
  }
});

// ---- reconcile ----
run("reconcile_marks_foreign_owned", () => {
  const tid = seed();
  withRepo((repo) => repo.claim(tid, 0n, "run-0", "2026-01-02T05:20:00Z")); // running, owner run-0
  const progressBefore = taskRow(tid).last_progress_at;
  const before = sha(DB);
  const r = withRepo((repo) => repo.reconcile("run-NEW"));
  if (r.reconciled < 1) throw new Error(`reconciled=${r.reconciled}`);
  const t = taskRow(tid);
  if (t.status !== "needs_verification" || t.state_revision !== 2n) throw new Error(JSON.stringify(t));
  if (t.last_progress_at !== progressBefore) throw new Error("recovery stamped progress");
  return { reconciled: r.reconciled, status: "needs_verification", progress_unchanged: true };
});
run("reconcile_noop_consistent", () => {
  const before = sha(DB);
  const r = withRepo((repo) => repo.reconcile("run-0"));
  if (sha(DB) !== before) throw new Error("noop reconcile wrote bytes");
  // consistent run-0 rows: owned-by-run-0 running tasks are NOT selected
  return { reconciled: r.reconciled, bytes_unchanged: true };
});

// ---- candidates ----
run("candidates_order_scope", () => {
  // seed two pending tasks for run-7 in reverse-created order plus one
  // for run-8 and one claimed
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  const mk = (rid, run, ts) => repo.admit({
    requestId: rid, commandJson: JSON.stringify({ key: { operation: rid } }),
    executionKeyJson: "{}",
    workJson: JSON.stringify({
      items: [{ item_id: "a", target: {}, progress_unit: "d", frozen_inputs: {}, scope_keys: [`sc-${rid}`] }],
      scopes: [`sc-${rid}`],
    }),
    items: [{ item_id: "a", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: `["sc-${rid}"]` }],
    scopes: [`sc-${rid}`], runId: run, createdAt: ts,
  });
  const t1 = mk("req-c1", "run-7", "2026-01-02T06:00:02Z");
  const t0 = mk("req-c0", "run-7", "2026-01-02T06:00:01Z");
  mk("req-cx", "run-8", "2026-01-02T06:00:03Z");
  const tClaimed = mk("req-cz", "run-7", "2026-01-02T06:00:04Z");
  repo.claim(tClaimed.task_id, 0n, "run-7", "2026-01-02T06:00:05Z");
  d.close();
  const c = withRepo((repo) => repo.candidates("run-7"));
  const ids = c.map((x) => x.task.task_id);
  if (ids.join() !== `${t0.task_id},${t1.task_id}`) throw new Error(ids.join());
  const first = c[0];
  if (first.claims.length !== 1 || first.claims[0].state_revision !== first.task.state_revision ||
      first.claims[0].scope_key !== "sc-req-c0") throw new Error(JSON.stringify(first.claims));
  return { ordered: ["created_at asc"], claims_stamped_live: true };
});
run("candidates_excludes_needs_verification", () => {
  const tid = seed({ run: "run-0" });
  withRepo((repo) => repo.claim(tid, 0n, "run-0", "2026-01-02T06:10:00Z"));
  withRepo((repo) => repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T06:10:10Z"));
  withRepo((repo) => repo.requireVerification(tid, 2n, "run-0", "a", "x", "2026-01-02T06:10:20Z"));
  const c = withRepo((repo) => repo.candidates("run-0"));
  if (c.some((x) => x.task.task_id === tid)) throw new Error("nv task in candidates");
  return { excluded: true };
});

// ---- validated bound read: tamper cases fail closed ----
// helper: fresh copy with one seeded pending task, then mutate raw
const seededCopy = (name, mutate) => {
  const p = join(FIX, `${name}.db`);
  copyFileSync(DB, p);
  const d = openDatabase(p, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  const a = repo.admit({
    requestId: `req-tp-${name}`, commandJson: JSON.stringify({ key: { operation: `tp-${name}` } }),
    executionKeyJson: "{}",
    workJson: JSON.stringify({
      items: [{ item_id: "a", target: {}, progress_unit: "d", frozen_inputs: {}, scope_keys: [`sc-${name}`] }],
      scopes: [`sc-${name}`],
    }),
    items: [{ item_id: "a", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: `["sc-${name}"]` }],
    scopes: [`sc-${name}`], runId: "run-tp", createdAt: "2026-01-02T08:00:00Z",
  });
  mutate(d, a.task_id);
  d.close();
  return { path: p, tid: a.task_id };
};
const tamperFailsClosed = (id, name, mutate, { pendingSafe = true } = {}) => run(id, () => {
  const { path, tid } = seededCopy(name, mutate);
  const repo = TaskAdmission.open(path, { mode: "readwrite" });
  const res = {};
  try { repo.candidates("run-tp"); res.candidates = "ACCEPTED"; }
  catch (e) { res.candidates = e.code; }
  try { repo.reconcile("run-OTHER"); res.reconcile = "ACCEPTED"; }
  catch (e) { res.reconcile = e.code; }
  repo.close();
  // pending-status-preserving tampers must fail BOTH paths; status-
  // changing tampers are legitimately absent from candidates but must
  // still fail reconcile (the tampered task is foreign-accepted).
  if (pendingSafe) {
    if (res.candidates !== "inconsistent" && res.candidates !== "needs_verification") {
      throw new Error(`candidates: ${res.candidates}`);
    }
  } else if (res.candidates !== "ACCEPTED") {
    throw new Error(`candidates: ${res.candidates}`);
  }
  if (res.reconcile !== "inconsistent" && res.reconcile !== "needs_verification") {
    throw new Error(`reconcile: ${res.reconcile}`);
  }
  return res;
});

tamperFailsClosed("tamper_evidence_digest", "evdig",
  (d, tid) => d.run("UPDATE tasks SET evidence_digest='tampered' WHERE task_id=?", tid));
tamperFailsClosed("tamper_occupancy_deleted", "occ",
  (d, tid) => d.run("DELETE FROM scope_occupancy WHERE task_id=?", tid));
// (finished without outcome_json is unreachable — the task_items CHECK
// rejects it at write time; proven by the driver's constraint error)
tamperFailsClosed("tamper_success_with_error", "fout",
  (d, tid) => d.run(
    "UPDATE task_items SET stage='finished',outcome_json='{\"success\":true,\"error\":\"bogus\"}' WHERE task_id=? AND item_id='a'",
    tid));
tamperFailsClosed("tamper_running_no_owner", "noowner",
  (d, tid) => d.run("UPDATE tasks SET status='running' WHERE task_id=?", tid), { pendingSafe: false });
tamperFailsClosed("tamper_uncertainty_marker", "unc",
  (d, tid) => d.run(
    `UPDATE tasks SET status='needs_verification',result_json='{"execution_uncertainty":{"item_id":5}}' WHERE task_id=?`,
    tid), { pendingSafe: false });

// boundary
expectCode("candidates_empty_run", "invalid_input",
  () => TaskAdmission.open(DB, { mode: "readwrite" }).candidates(" "));
expectCode("reconcile_empty_run", "invalid_input",
  () => TaskAdmission.open(DB, { mode: "readwrite" }).reconcile("  "));
expectCode("rv_empty_itemid", "invalid_input",
  () => TaskAdmission.open(DB, { mode: "readwrite" }).requireVerification("x", 0n, "run-0", "", "r", "2026-01-02T06:20:00Z"));
expectCode("implicit_mode_denied", "invalid_input", () => TaskAdmission.open(DB));

// selftest
if (SELFTEST === "rec") {
  const tid = seed();
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try {
    const r = repo.requireVerification(tid, 777n, "run-0", "a", "ghost", "2026-01-02T07:00:00Z");
    emit("selftest_rec", r.task.status === "needs_verification" ? "PASS" : "FAIL", "unexpected — STOP");
  } catch (e) {
    emit("selftest_rec", "FAIL", `correctly rejected — selftest FAIL as designed (${e.code})`);
  }
  repo.close();
}

process.exitCode = failures === 0 ? 0 : 1;
