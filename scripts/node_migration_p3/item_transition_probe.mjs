// NM-044 P3: validate atomic item start/finish transitions.
// Synthetic DBs only. Exit nonzero on FAIL.
// Usage: node item_transition_probe.mjs <root> [selftest]
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
  console.log(JSON.stringify({ family: "item_transition", id, status, detail, expected, actual }));
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

const FIX = mkdtempSync("/tmp/nm44-item-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const SRC = "/dev/shm/nm006";
const DB = join(FIX, "items.db");
copyFileSync(join(SRC, "db18.sqlite"), DB);

// fixture: admitted + claimed running task with 2 items
const mk = () => {
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  const a = repo.admit({
    requestId: "req-it", commandJson: JSON.stringify({ key: { operation: "collect" } }),
    executionKeyJson: JSON.stringify({ operation: "collect" }), workJson: "{}",
    items: [
      { item_id: "i-1", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: "[]" },
      { item_id: "i-2", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: "[]" },
    ],
    scopes: ["s-1"], runId: "run-0", createdAt: "2026-01-02T00:00:00Z",
  });
  repo.claim(a.task_id, 0n, "run-0", "2026-01-02T00:00:10Z");
  d.close();
  return a.task_id;
};
const TID = mk();
const itemRow = (iid) => {
  const d = openDatabase(DB);
  const r = d.get("SELECT * FROM task_items WHERE task_id=? AND item_id=?", TID, iid);
  d.close();
  return r;
};
const taskRow = () => {
  const d = openDatabase(DB);
  const r = d.get("SELECT * FROM tasks WHERE task_id=?", TID);
  d.close();
  return r;
};

run("start_item_happy", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  const t = repo.startItem(TID, 1n, "run-0", "i-1", "2026-01-02T00:01:00Z");
  repo.close();
  const i = itemRow("i-1");
  if (i.stage !== "running" || t.state_revision !== 2n ||
      t.items_pending !== 1n || t.items_running !== 1n) {
    throw new Error(`item=${JSON.stringify(i)} task=${JSON.stringify(t)}`);
  }
  const d = openDatabase(DB);
  const m = d.get("SELECT * FROM task_item_execution_starts WHERE task_id=? AND item_id='i-1'", TID);
  d.close();
  if (!m || m.run_id !== "run-0" || m.start_revision !== 2n ||
      m.started_at !== "2026-01-02T00:01:00Z") throw new Error(JSON.stringify(m));
  return { rev: "2", counters: "pending-1,running+1", marker: { run_id: m.run_id, rev: "2" } };
});
run("start_second_while_running_rejects", () => {
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { repo.startItem(TID, 2n, "run-0", "i-2", "2026-01-02T00:01:30Z"); repo.close(); throw new Error("allowed second running"); }
  catch (e) {
    repo.close();
    if (e.code !== "revision_conflict" || sha(DB) !== before) throw e;
    return { code: e.code };
  }
});
run("finish_item_success", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  const t = repo.finishItem(TID, 2n, "run-0", "i-1",
    { success: true, evidence: { rows: 5 } }, "2026-01-02T00:02:00Z");
  repo.close();
  const i = itemRow("i-1");
  if (i.stage !== "finished" || JSON.parse(i.outcome_json).success !== true ||
      JSON.parse(i.outcome_json).evidence?.rows !== 5 ||
      t.items_running !== 0n || t.items_finished !== 1n || t.items_succeeded !== 1n ||
      t.state_revision !== 3n) throw new Error(`item=${JSON.stringify(i)} task=${JSON.stringify(t)}`);
  return { stage: "finished", outcome_verbatim: true, counters: "running-1,finished+1,succeeded+1" };
});
run("start_and_finish_failure", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  repo.startItem(TID, 3n, "run-0", "i-2", "2026-01-02T00:03:00Z");
  const t = repo.finishItem(TID, 4n, "run-0", "i-2",
    { success: false, error: "io timeout" }, "2026-01-02T00:03:30Z");
  repo.close();
  const i = itemRow("i-2");
  if (i.stage !== "finished" || JSON.parse(i.outcome_json).error !== "io timeout" ||
      t.items_finished !== 2n || t.items_succeeded !== 1n ||
      t.state_revision !== 5n) throw new Error(JSON.stringify(t));
  return { outcome_error: true, succeeded_stays: "1" };
});

// rejection paths — DB unchanged
const unchanged = (id, fn, code) => run(id, () => {
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { fn(repo); repo.close(); throw new Error("accepted"); }
  catch (e) {
    repo.close();
    if (e.code !== code) throw e;
    if (sha(DB) !== before) throw new Error("rejection mutated DB");
    return { code: e.code };
  }
});
unchanged("start_not_owner", (r) =>
  r.startItem(TID, 5n, "run-D", "i-1", "2026-01-02T00:04:00Z"), "needs_verification");
unchanged("start_finished_item", (r) =>
  r.startItem(TID, 5n, "run-0", "i-1", "2026-01-02T00:04:10Z"), "revision_conflict");
unchanged("finish_not_running_item", (r) =>
  r.finishItem(TID, 5n, "run-0", "i-1", { success: true }, "2026-01-02T00:04:20Z"), "revision_conflict");
unchanged("finish_wrong_owner", (r) =>
  r.finishItem(TID, 5n, "run-D", "i-1", { success: true }, "2026-01-02T00:04:30Z"), "needs_verification");
unchanged("transition_missing_task", (r) =>
  r.startItem("task-nope", 0n, "run-0", "i-1", "2026-01-02T00:04:40Z"), "task_not_found");
unchanged("transition_stale_rev", (r) =>
  r.finishItem(TID, 99n, "run-0", "i-1", { success: true }, "2026-01-02T00:04:50Z"), "revision_conflict");

// success-with-error -> invalid_input verbatim
expectCode("success_with_error", "invalid_input",
  () => TaskAdmission.open(DB, { mode: "readwrite" }).finishItem(TID, 5n, "run-0", "i-1",
    { success: true, error: "x" }, "2026-01-02T00:05:00Z"));
expectCode("empty_run_start", "invalid_input",
  () => TaskAdmission.open(DB, { mode: "readwrite" }).startItem(TID, 5n, " ", "i-1", "2026-01-02T00:05:00Z"));
expectCode("bad_outcome_shape", "invalid_input",
  () => TaskAdmission.open(DB, { mode: "readwrite" }).finishItem(TID, 5n, "run-0", "i-1", "yes", "2026-01-02T00:05:00Z"));
expectCode("implicit_mode_denied", "invalid_input", () => TaskAdmission.open(DB));

// start on a still-pending task -> revision_conflict (new fixture task)
run("start_on_pending_task", () => {
  const d0 = openDatabase(DB, { mode: "readwrite" });
  const r0 = new TaskAdmission(d0, null);
  const a = r0.admit({
    requestId: "req-it-pend", commandJson: JSON.stringify({ key: { operation: "c" } }),
    executionKeyJson: "{}", workJson: "{}",
    items: [{ item_id: "i-9", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: "[]" }],
    scopes: ["s-9"], runId: "run-9", createdAt: "2026-01-02T00:00:40Z",
  });
  d0.close();
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { repo.startItem(a.task_id, 0n, "run-9", "i-9", "2026-01-02T00:05:30Z"); repo.close(); throw new Error("started pending task"); }
  catch (e) {
    repo.close();
    if (e.code !== "revision_conflict" || sha(DB) !== before) throw e;
    return { code: e.code };
  }
});

// reopen readback
run("reopen_readback", () => {
  const t = taskRow();
  if (t.state_revision !== 5n || t.items_finished !== 2n) throw new Error(JSON.stringify(t));
  return { durable: true, revision: "5" };
});

// selftest
if (SELFTEST === "item") {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try {
    const r = repo.startItem(TID, 777n, "run-0", "i-1", "2026-01-02T00:09:00Z");
    emit("selftest_item", r.state_revision === 778n ? "PASS" : "FAIL", "unexpected — STOP");
  } catch (e) {
    emit("selftest_item", "FAIL", `correctly rejected — selftest FAIL as designed (${e.code})`);
  }
  repo.close();
}

process.exitCode = failures === 0 ? 0 : 1;
