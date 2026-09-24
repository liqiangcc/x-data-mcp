// NM-045 P3: validate terminal transitions (cancel + finish_execution +
// finish_cancellation). Synthetic DBs only. Exit nonzero on FAIL.
// Usage: node terminal_transition_probe.mjs <root> [selftest]
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
  console.log(JSON.stringify({ family: "terminal_transition", id, status, detail, expected, actual }));
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

const FIX = mkdtempSync("/tmp/nm45-term-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const SRC = "/dev/shm/nm006";
const DB = join(FIX, "term.db");
copyFileSync(join(SRC, "db18.sqlite"), DB);

// helpers: build a task in a given lifecycle stage via the repo itself
let seq = 0;
const seedTask = (opts = {}) => {
  const { items = ["a", "b"], scopes = null, run = "run-0" } = opts;
  seq++;
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  const a = repo.admit({
    requestId: `req-t${seq}`, commandJson: JSON.stringify({ key: { operation: `op${seq}` } }),
    executionKeyJson: "{}", workJson: "{}",
    items: items.map((i) => ({ item_id: i, target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: "[]" })),
    scopes: scopes ?? [`s-t${seq}`], runId: run, createdAt: `2026-01-02T01:00:${String(seq).padStart(2, "0")}Z`,
  });
  d.close();
  return a.task_id;
};
const claim = (tid, run = "run-0") => {
  const d = openDatabase(DB, { mode: "readwrite" });
  new TaskAdmission(d, null).claim(tid, 0n, run, "2026-01-02T02:00:00Z");
  d.close();
};
const taskRow = (tid) => {
  const d = openDatabase(DB);
  const r = d.get("SELECT * FROM tasks WHERE task_id=?", tid);
  d.close(); return r;
};
const occCount = (tid) => {
  const d = openDatabase(DB);
  const n = Number(d.get("SELECT COUNT(*) c FROM scope_occupancy WHERE task_id=?", tid).c);
  d.close(); return n;
};
const withRepo = (fn) => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { return fn(repo); } finally { repo.close(); }
};

// ---- request_cancel ----
run("cancel_pending", () => {
  const tid = seedTask();
  const r = withRepo((repo) => repo.requestCancel(tid, 0n, "run-0", "2026-01-02T03:00:00Z"));
  const t = r.task;
  if (!r.changed || t.status !== "cancel_requested" || t.state_revision !== 1n ||
      t.result_json !== null || occCount(tid) !== 1) throw new Error(JSON.stringify(t));
  return { status: "cancel_requested", occupancy_kept: 1 };
});
run("cancel_idempotent_noop", () => {
  const tid = seedTask();
  withRepo((repo) => repo.requestCancel(tid, 0n, "run-0", "2026-01-02T03:01:00Z"));
  const before = sha(DB);
  const r = withRepo((repo) => repo.requestCancel(tid, 1n, "run-0", "2026-01-02T03:02:00Z"));
  const t = r.task;
  if (r.changed || t.state_revision !== 1n || sha(DB) !== before) throw new Error("no-op mutated");
  return { changed: false, bytes_unchanged: true };
});
// terminal no-op precedes run-identity: any run gets Ok unchanged
run("cancel_terminal_noop_any_run", () => {
  const tid = seedTask({ items: [] });
  // finish immediately (0 items -> succeeded vacuously)
  withRepo((repo) => repo.finishExecution(tid, 0n, "run-0", "2026-01-02T03:03:00Z"));
  const before = sha(DB);
  const r = withRepo((repo) => repo.requestCancel(tid, 1n, "run-FOREIGN", "2026-01-02T03:04:00Z"));
  if (r.changed || r.task.status !== "succeeded" || sha(DB) !== before) {
    throw new Error(JSON.stringify(r.task));
  }
  return { terminal_noop_precedes_run_gate: true };
});
run("cancel_cross_run_rejected", () => {
  const tid = seedTask();
  const before = sha(DB);
  try {
    withRepo((repo) => repo.requestCancel(tid, 0n, "run-X", "2026-01-02T03:05:00Z"));
    throw new Error("cross-run cancel accepted");
  } catch (e) {
    if (e.code !== "needs_verification" || sha(DB) !== before) throw e;
    return { code: e.code };
  }
});

// ---- finish_execution derivations ----
run("finish_all_success", () => {
  const tid = seedTask();
  claim(tid);
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T03:10:00Z");
  repo.finishItem(tid, 2n, "run-0", "a", { success: true }, "2026-01-02T03:10:30Z");
  repo.startItem(tid, 3n, "run-0", "b", "2026-01-02T03:11:00Z");
  repo.finishItem(tid, 4n, "run-0", "b", { success: true }, "2026-01-02T03:11:30Z");
  const r = repo.finishExecution(tid, 5n, "run-0", "2026-01-02T03:12:00Z");
  d.close();
  const t = r.task;
  if (t.status !== "succeeded" || t.state_revision !== 6n ||
      t.result_json !== '{"finished_items":2,"successful_items":2}' ||
      occCount(tid) !== 0) throw new Error(JSON.stringify(t));
  return { status: "succeeded", result: t.result_json, occupancy_released: true };
});
run("finish_partial", () => {
  const tid = seedTask();
  claim(tid);
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T03:13:00Z");
  repo.finishItem(tid, 2n, "run-0", "a", { success: true }, "2026-01-02T03:13:30Z");
  repo.startItem(tid, 3n, "run-0", "b", "2026-01-02T03:14:00Z");
  repo.finishItem(tid, 4n, "run-0", "b", { success: false, error: "x" }, "2026-01-02T03:14:30Z");
  const r = repo.finishExecution(tid, 5n, "run-0", "2026-01-02T03:15:00Z");
  d.close();
  if (r.task.status !== "partially_failed" ||
      r.task.result_json !== '{"finished_items":2,"successful_items":1}') {
    throw new Error(JSON.stringify(r.task));
  }
  return { status: "partially_failed" };
});
run("finish_all_failed", () => {
  const tid = seedTask({ items: ["a"] });
  claim(tid);
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T03:16:00Z");
  repo.finishItem(tid, 2n, "run-0", "a", { success: false, error: "x" }, "2026-01-02T03:16:30Z");
  const r = repo.finishExecution(tid, 3n, "run-0", "2026-01-02T03:17:00Z");
  d.close();
  if (r.task.status !== "failed" ||
      r.task.result_json !== '{"finished_items":1,"successful_items":0}') {
    throw new Error(JSON.stringify(r.task));
  }
  return { status: "failed" };
});
run("finish_with_running_item_needs_verification", () => {
  const tid = seedTask();
  claim(tid);
  const d = openDatabase(DB, { mode: "readwrite" });
  new TaskAdmission(d, null).startItem(tid, 1n, "run-0", "a", "2026-01-02T03:18:00Z");
  d.close();
  const before = sha(DB);
  try {
    withRepo((repo) => repo.finishExecution(tid, 2n, "run-0", "2026-01-02T03:18:30Z"));
    throw new Error("finished with running item");
  } catch (e) {
    if (e.code !== "needs_verification" ||
        !e.message.includes("execution has not safely ended") || sha(DB) !== before) throw e;
    return { code: e.code, verbatim: true };
  }
});
run("finish_never_started_revision_conflict", () => {
  // pending task with pending items — never_started is true but items
  // unfinished -> status derivation falls through to revision_conflict
  const tid = seedTask();
  const before = sha(DB);
  try {
    withRepo((repo) => repo.finishExecution(tid, 0n, "run-0", "2026-01-02T03:19:00Z"));
    throw new Error("finished pending task");
  } catch (e) {
    if (e.code !== "revision_conflict" || sha(DB) !== before) throw e;
    return { code: e.code };
  }
});
run("finish_non_owner_needs_verification", () => {
  const tid = seedTask();
  claim(tid);
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T03:20:00Z");
  repo.finishItem(tid, 2n, "run-0", "a", { success: true }, "2026-01-02T03:20:30Z");
  repo.startItem(tid, 3n, "run-0", "b", "2026-01-02T03:21:00Z");
  repo.finishItem(tid, 4n, "run-0", "b", { success: true }, "2026-01-02T03:21:30Z");
  d.close();
  const before = sha(DB);
  // run-0 owns; another run finishing -> run-identity gate first
  try {
    withRepo((repo) => repo.finishExecution(tid, 5n, "run-Z", "2026-01-02T03:22:00Z"));
    throw new Error("non-owner finished");
  } catch (e) {
    if (e.code !== "needs_verification" || sha(DB) !== before) throw e;
    return { code: e.code };
  }
});

// ---- finish_cancellation ----
run("finish_cancellation_acknowledged", () => {
  const tid = seedTask({ items: ["a", "b"] });
  claim(tid);
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T03:30:00Z"); // rev 2, i-a running
  repo.requestCancel(tid, 2n, "run-0", "2026-01-02T03:30:30Z"); // rev 3, cancel_requested
  repo.finishItem(tid, 3n, "run-0", "a", { success: false, error: "stopped" }, "2026-01-02T03:31:00Z"); // rev 4
  const r = repo.finishCancellation(tid, 4n, "run-0", "2026-01-02T03:31:30Z"); // rev 5
  d.close();
  const t = r.task;
  if (t.status !== "cancelled" ||
      t.result_json !== '{"cancellation_acknowledged":true,"finished_items":1,"successful_items":0}' ||
      occCount(tid) !== 0) throw new Error(JSON.stringify(t));
  return { status: "cancelled", acknowledged: true, occupancy_released: true };
});
run("finish_cancellation_requires_cancel", () => {
  const tid = seedTask({ items: ["a"] });
  claim(tid);
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T03:32:00Z");
  repo.finishItem(tid, 2n, "run-0", "a", { success: true }, "2026-01-02T03:32:30Z");
  d.close();
  const before = sha(DB);
  try {
    withRepo((repo) => repo.finishCancellation(tid, 3n, "run-0", "2026-01-02T03:33:00Z"));
    throw new Error("finish_cancellation on running accepted");
  } catch (e) {
    if (e.code !== "revision_conflict" || sha(DB) !== before) throw e;
    return { code: e.code };
  }
});
run("finish_execution_under_cancel_becomes_cancelled", () => {
  // FinishExecution (not cancellation) on cancel_requested task with
  // unfinished items -> cancelled WITHOUT the acknowledged flag
  const tid = seedTask({ items: ["a"] });
  claim(tid);
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  repo.requestCancel(tid, 1n, "run-0", "2026-01-02T03:34:00Z"); // rev2
  const r = repo.finishExecution(tid, 2n, "run-0", "2026-01-02T03:34:30Z");
  d.close();
  if (r.task.status !== "cancelled" ||
      r.task.result_json !== '{"finished_items":0,"successful_items":0}') {
    throw new Error(JSON.stringify(r.task));
  }
  return { status: "cancelled", no_ack_flag: true };
});

// reopen readback on last mutated task set
run("reopen_readback", () => {
  const d = openDatabase(DB);
  const c = Number(d.get("SELECT COUNT(*) c FROM tasks WHERE status IN ('succeeded','partially_failed','failed','cancelled')").c);
  d.close();
  if (c < 4) throw new Error(`only ${c} terminal tasks`);
  return { terminal_tasks: c };
});

// selftest
if (SELFTEST === "term") {
  const tid = seedTask({ items: ["a"] });
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try {
    const r = repo.finishExecution(tid, 999n, "run-0", "2026-01-02T04:00:00Z");
    emit("selftest_term", r.task.status === "succeeded" ? "PASS" : "FAIL", "unexpected — STOP");
  } catch (e) {
    emit("selftest_term", "FAIL", `correctly rejected — selftest FAIL as designed (${e.code})`);
  }
  repo.close();
}

process.exitCode = failures === 0 ? 0 : 1;
