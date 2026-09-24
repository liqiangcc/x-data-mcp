// NM-047 P3: validate retry admission (accept_tx RetryGuard branch).
// Synthetic DBs only. Exit nonzero on FAIL.
// Usage: node retry_probe.mjs <root> [selftest]
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
  console.log(JSON.stringify({ family: "retry_admission", id, status, detail, expected, actual }));
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

const FIX = mkdtempSync("/tmp/nm47-retry-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const DB = join(FIX, "retry.db");
copyFileSync("/dev/shm/nm006/db18.sqlite", DB);

const KEY = { operation: "collect", dataset: "ds", contract_version: "1" };
const ITEMS = ["a", "b"];
const itemSpec = (i) => ({ item_id: i, target: {}, progress_unit: "d", frozen_inputs: {}, scope_keys: ["sc-0"] });
const WORK = JSON.stringify({ items: ITEMS.map(itemSpec), scopes: ["sc-0"] });

// seed: original task fails (item a ok, b fails) -> failed, terminal
const seedOriginal = () => {
  const d = openDatabase(DB, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  const a = repo.admit({
    requestId: "req-orig", commandJson: JSON.stringify({ key: KEY, intent: {} }),
    executionKeyJson: JSON.stringify(KEY), workJson: WORK,
    items: ITEMS.map((i) => ({ item_id: i, target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: '["sc-0"]' })),
    scopes: ["sc-0"], runId: "run-0", createdAt: "2026-01-02T09:00:00Z",
  });
  const tid = a.task_id;
  repo.claim(tid, 0n, "run-0", "2026-01-02T09:00:10Z");
  repo.startItem(tid, 1n, "run-0", "a", "2026-01-02T09:00:20Z");
  repo.finishItem(tid, 2n, "run-0", "a", { success: true }, "2026-01-02T09:00:30Z");
  repo.startItem(tid, 3n, "run-0", "b", "2026-01-02T09:00:40Z");
  repo.finishItem(tid, 4n, "run-0", "b", { success: false, error: "boom" }, "2026-01-02T09:00:50Z");
  repo.finishExecution(tid, 5n, "run-0", "2026-01-02T09:01:00Z");
  d.close();
  return tid;
};
const ORIG = seedOriginal(); // failed at rev 6; b failed, a succeeded
copyFileSync(DB, join(FIX, "pre-happy.db"));

const retryPlan = (over = {}) => ({
  requestId: "req-retry",
  commandJson: JSON.stringify({
    key: { operation: "retry_task", dataset: KEY.dataset, contract_version: KEY.contract_version },
    intent: { original_task_id: ORIG, selected_items: ["b"] },
  }),
  executionKeyJson: JSON.stringify({ operation: "retry_task", dataset: "ds", contract_version: "1" }),
  workJson: JSON.stringify({ items: [itemSpec("b")], scopes: ["sc-0"] }),
  items: [{ item_id: "b", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: '["sc-0"]' }],
  scopes: ["sc-0"], runId: "run-1",
  retryOf: ORIG,
  retry: { originalTaskId: ORIG, expectedRevision: 6n, selectedItems: ["b"] },
  createdAt: "2026-01-02T09:10:00Z",
  ...over,
});

run("retry_failed_item_happy", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  const r = repo.admit(retryPlan());
  repo.close();
  if (r.reused) throw new Error("reused");
  const d = openDatabase(DB);
  const t = d.get("SELECT * FROM tasks WHERE task_id=?", r.task_id);
  const i = d.get("SELECT item_id,stage FROM task_items WHERE task_id=?", r.task_id);
  const o = d.get("SELECT * FROM scope_occupancy WHERE task_id=?", r.task_id);
  d.close();
  if (t.retry_of !== ORIG || t.command_key_json !== JSON.stringify(KEY) ||
      t.status !== "pending" || i.item_id !== "b" || o.scope_key !== "sc-0") {
    throw new Error(JSON.stringify(t));
  }
  return { stored_key: "original", retry_of: true, item: "b" };
});

let rejSeq = 0;
const rej = (id, code, planOver) => run(id, () => {
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { repo.admit(retryPlan({ requestId: `req-${id}`, ...planOver })); repo.close(); throw new Error("accepted"); }
  catch (e) {
    repo.close();
    if (e.code !== code) throw e;
    if (sha(DB) !== before) throw new Error("rejection mutated DB");
    return { code: e.code };
  }
});

rej("retry_succeeded_item_rejected", "invalid_retry_selection", {
  commandJson: JSON.stringify({
    key: { operation: "retry_task", dataset: "ds", contract_version: "1" },
    intent: { original_task_id: ORIG, selected_items: ["a"] },
  }),
  workJson: JSON.stringify({ items: [itemSpec("a")], scopes: ["sc-0"] }),
  items: [{ item_id: "a", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: '["sc-0"]' }],
  retry: { originalTaskId: ORIG, expectedRevision: 6n, selectedItems: ["a"] },
});
rej("retry_spec_divergence", "invalid_retry_selection", {
  workJson: JSON.stringify({ items: [{ ...itemSpec("b"), progress_unit: "h" }], scopes: ["sc-0"] }),
  items: [{ item_id: "b", target_json: "{}", progress_unit: "h", frozen_inputs_json: "{}", scope_keys_json: '["sc-0"]' }],
});
rej("retry_stale_revision", "revision_conflict", {
  retry: { originalTaskId: ORIG, expectedRevision: 99n, selectedItems: ["b"] },
});
rej("retry_missing_original", "task_not_found", {
  commandJson: JSON.stringify({
    key: { operation: "retry_task", dataset: "ds", contract_version: "1" },
    intent: { original_task_id: "task-ghost", selected_items: ["b"] },
  }),
  retryOf: "task-ghost",
  retry: { originalTaskId: "task-ghost", expectedRevision: 0n, selectedItems: ["b"] },
});
rej("retry_intent_mismatch", "invalid_retry_selection", {
  commandJson: JSON.stringify({
    key: { operation: "retry_task", dataset: "ds", contract_version: "1" },
    intent: { original_task_id: ORIG, selected_items: ["a", "b"] }, // doesn't match guard
  }),
});
rej("retry_intent_extra_key", "invalid_retry_selection", {
  commandJson: JSON.stringify({
    key: { operation: "retry_task", dataset: "ds", contract_version: "1" },
    intent: { original_task_id: ORIG, selected_items: ["b"], extra: "not in the contract" },
  }),
});
rej("retry_key_not_retry_task", "invalid_retry_selection", {
  commandJson: JSON.stringify({
    key: { operation: "collect", dataset: "ds", contract_version: "1" },
    intent: { original_task_id: ORIG, selected_items: ["b"] },
  }),
});
rej("retry_reuse_original_request_id", "conflict", {
  // same request_id, DIFFERENT command bytes -> REQUEST_CONFLICT at the
  // replay gate (retry guard's request check is subsumed — read_request
  // runs first, identical to Rust ordering)
  requestId: "req-orig",
  commandJson: JSON.stringify({ key: KEY, intent: { different: true } }),
});
rej("retry_replay_binding_divergence", "needs_verification", {
  // same request_id AND same command bytes -> replay path verifies the
  // durable binding; it diverges from the retry plan -> needs_verification
  requestId: "req-orig",
  commandJson: JSON.stringify({ key: KEY, intent: {} }),
});
rej("retry_selection_size_mismatch", "invalid_retry_selection", {
  workJson: JSON.stringify({ items: [itemSpec("b"), { ...itemSpec("c"), scope_keys: ["sc-0"] }], scopes: ["sc-0"] }),
  items: [
    { item_id: "b", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: '["sc-0"]' },
    { item_id: "c", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: '["sc-0"]' },
  ],
});
// scope conflict still enforced for retries: occupy sc-9 first
run("retry_scope_conflict", () => {
  // retry work item must match the original spec exactly (sc-0), so the
  // conflict has to come from sc-0 being re-occupied. Fresh copy: the
  // happy retry on DB already occupies sc-0, so stage it on a copy.
  const p = join(FIX, "scope-conflict.db");
  copyFileSync(join(FIX, "pre-happy.db"), p);
  const d = openDatabase(p, { mode: "readwrite" });
  const repo = new TaskAdmission(d, null);
  repo.admit({
    requestId: "req-occupier", commandJson: JSON.stringify({ key: { operation: "x" } }),
    executionKeyJson: "{}", workJson: JSON.stringify({ items: [{ ...itemSpec("z"), scope_keys: ["sc-0"] }], scopes: ["sc-0"] }),
    items: [{ item_id: "z", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: '["sc-0"]' }],
    scopes: ["sc-0"], runId: "run-9", createdAt: "2026-01-02T09:20:00Z",
  });
  d.close();
  const before = sha(p);
  const repo2 = TaskAdmission.open(p, { mode: "readwrite" });
  try {
    repo2.admit(retryPlan({ requestId: "req-scope-conflict" }));
    repo2.close();
    throw new Error("scope conflict accepted");
  } catch (e) {
    repo2.close();
    if (e.code !== "range_conflict" || sha(p) !== before) throw e;
    return { code: e.code };
  }
});
// replay of the retry request_id replays the binding (NM-042 rules)
run("retry_request_replay", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  const r = repo.admit(retryPlan());
  repo.close();
  if (!r.reused || !r.record.task_id) throw new Error("not replayed");
  return { reused: true, bound: r.record.task_id.slice(0, 8) };
});

expectCode("implicit_mode_denied", "invalid_input", () => TaskAdmission.open(DB));

// selftest
if (SELFTEST === "retry") {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try {
    const r = repo.admit(retryPlan({
      requestId: "req-sf", retry: { originalTaskId: ORIG, expectedRevision: 999n, selectedItems: ["b"] },
    }));
    emit("selftest_retry", r.task_id ? "PASS" : "FAIL", "unexpected — STOP");
  } catch (e) {
    emit("selftest_retry", "FAIL", `correctly rejected — selftest FAIL as designed (${e.code})`);
  }
  repo.close();
}

process.exitCode = failures === 0 ? 0 : 1;
