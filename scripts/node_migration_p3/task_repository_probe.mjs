// NM-040 P3: validate the read-only task/request repository over the
// NM-039 gate + NM-038 adapter. Synthetic rows only. Exit nonzero on FAIL.
// Usage: node task_repository_probe.mjs <root> [selftest]
import { createHash } from "node:crypto";
import { copyFileSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const { TaskRepository } = await import("../../src_node/storage/task_repository.mjs");
const { openDatabase, StorageError } = await import("../../src_node/storage/sqlite_adapter.mjs");

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "task_repository", id, status, detail, expected, actual }));
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

const FIX = mkdtempSync("/tmp/nm40-repo-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const SRC = "/dev/shm/nm006";
const DB = join(FIX, "tasks.db");
copyFileSync(join(SRC, "db18.sqlite"), DB);
copyFileSync(join(SRC, "garbage.bin"), join(FIX, "garbage.db"));
copyFileSync(join(SRC, "empty.db"), join(FIX, "empty.db"));

// ---- synthetic fixture rows (write once via adapter create-path, then
// the repo reads read-only) ----
const cmdJson = (k) => JSON.stringify({ key: { operation: k }, input: { symbol: "000001" } });
{
  const w = openDatabase(DB, { mode: "readwrite" });
  const ins = w._db ?? null;
  const tasks = [
    // task_id, status, owner, created_at, items
    ["t-001", "pending", "run-A", "2026-01-01T00:00:01Z", [3, 2, 1, 0, 0]],
    ["t-002", "running", "run-A", "2026-01-01T00:00:02Z", [2, 0, 1, 1, 1]],
    ["t-003", "succeeded", "run-B", "2026-01-01T00:00:03Z", [1, 0, 0, 1, 1]],
    ["t-004", "pending", null, "2026-01-01T00:00:04Z", [0, 0, 0, 0, 0]],
  ];
  for (const [tid, status, owner, ts, [ic, p, r, f, s]] of tasks) {
    w.run(`INSERT INTO tasks(task_id,command_key_json,status,state_revision,
      accepted_run_id,owner_run_id,created_at,last_progress_at,
      frozen_work_json,result_json,retry_of,item_count,items_pending,
      items_running,items_finished,items_succeeded)
      VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
      tid, `{"operation":"op-${tid}"}`, status, 1n, "acc-1", owner,
      ts, ts, "{}", status === "succeeded" ? "{}" : null, null,
      BigInt(ic), BigInt(p), BigInt(r), BigInt(f), BigInt(s));
  }
  const reqs = [
    ["r-001", "op-t-001", "task", "t-001", null],
    ["r-002", "op-t-002", "task", "t-002", null],
    ["r-003", "op-t-003", "task", "t-003", null],
    ["r-term", "report", "terminal", null, '{"ok":true}'],
  ];
  for (const [rid, op, kind, tid, term] of reqs) {
    const cj = cmdJson(op);
    w.run(`INSERT INTO requests(request_id,command_json,command_digest,
      outcome_kind,task_id,terminal_json) VALUES (?,?,?,?,?,?)`,
      rid, cj, createHash("sha256").update(cj, "utf8").digest("hex"), kind, tid, term);
  }
  w.close();
}
const shaAfterFixtures = sha(DB);

run("repo_opens_via_gate", () => {
  const repo = TaskRepository.open(DB);
  const c = repo.capability;
  repo.close();
  if (c.schemaVersion !== 18 || c.kind !== "legacy") throw new Error(JSON.stringify(c));
  return { schemaVersion: c.schemaVersion, kind: c.kind };
});

run("get_task_typed", () => {
  const repo = TaskRepository.open(DB);
  const t = repo.getTask("t-002");
  repo.close();
  if (t.status !== "running" || t.owner_run_id !== "run-A" ||
      t.item_count !== 2n || t.items_succeeded !== 1n ||
      t.result_json !== null || t.retry_of !== null ||
      typeof t.state_revision !== "bigint") throw new Error(JSON.stringify(t));
  return { status: t.status, owner: t.owner_run_id, counters: [String(t.item_count), String(t.items_succeeded)], nullable: t.result_json === null };
});
run("get_task_missing_null", () => {
  const repo = TaskRepository.open(DB);
  const t = repo.getTask("no-such");
  repo.close();
  if (t !== null) throw new Error("non-null for missing task");
  return { missing: null };
});
run("get_request_digest_verified", () => {
  const repo = TaskRepository.open(DB);
  const r = repo.getRequest("r-002");
  repo.close();
  if (r.outcome_kind !== "task" || r.task_id !== "t-002" || r.terminal_json !== null) {
    throw new Error(JSON.stringify(r));
  }
  return { outcome_kind: r.outcome_kind, task_id: r.task_id };
});
run("get_request_terminal", () => {
  const repo = TaskRepository.open(DB);
  const r = repo.getRequest("r-term");
  repo.close();
  if (r.outcome_kind !== "terminal" || r.task_id !== null || r.terminal_json !== '{"ok":true}') {
    throw new Error(JSON.stringify(r));
  }
  return { outcome_kind: "terminal" };
});
run("task_for_request", () => {
  const repo = TaskRepository.open(DB);
  const t = repo.getTaskForRequest("r-003");
  const none = repo.getTaskForRequest("r-term");
  repo.close();
  if (t.task_id !== "t-003" || none !== null) throw new Error("binding wrong");
  return { bound: "t-003", terminal_unbound: true };
});
run("request_for_task_reverse", () => {
  const repo = TaskRepository.open(DB);
  const rid = repo.getRequestForTask("t-001");
  const none = repo.getRequestForTask("t-004");
  repo.close();
  if (rid !== "r-001" || none !== null) throw new Error(`rid=${rid} none=${none}`);
  return { request_id: rid, unbound: null };
});

// ordering + pagination
run("list_sorted_paginated", () => {
  const repo = TaskRepository.open(DB);
  const p1 = repo.listTasks({ limit: 2 });
  const p2 = repo.listTasks({ limit: 2, cursor: p1.nextCursor });
  const all = repo.listTasks({ limit: 100 });
  repo.close();
  const ids1 = p1.items.map(t => t.task_id), ids2 = p2.items.map(t => t.task_id);
  if (ids1.join() !== "t-001,t-002" || ids2.join() !== "t-003,t-004" ||
      !p1.hasMore || p2.hasMore || all.items.length !== 4) {
    throw new Error(`p1=${ids1} p2=${ids2} all=${all.items.length}`);
  }
  return { page1: ids1, page2: ids2, deterministic_order: "created_at,task_id" };
});

// owner scoping is an explicit input
run("owner_scope_explicit", () => {
  const a = TaskRepository.open(DB, { owner: "run-A" });
  const la = a.listTasks({ limit: 100 });
  a.close();
  const b = TaskRepository.open(DB, { owner: "run-B" });
  const lb = b.listTasks({ limit: 100 });
  b.close();
  const all = TaskRepository.open(DB).listTasks({ limit: 100 });
  const aIds = la.items.map(t => t.task_id), bIds = lb.items.map(t => t.task_id);
  if (aIds.join() !== "t-001,t-002" || bIds.join() !== "t-003" || all.items.length !== 4) {
    throw new Error(`A=${aIds} B=${bIds}`);
  }
  return { owner_A: aIds, owner_B: bIds, unscoped_total: all.items.length };
});

// digest tamper -> inconsistent (fail closed)
run("digest_tamper_inconsistent", () => {
  const p = join(FIX, "tampered.db");
  copyFileSync(DB, p);
  const w = openDatabase(p, { mode: "readwrite" });
  w.run("UPDATE requests SET command_json='{\"tampered\":true}' WHERE request_id='r-001'");
  w.close();
  const repo = TaskRepository.open(p);
  try { repo.getRequest("r-001"); repo.close(); throw new Error("accepted tampered"); }
  catch (e) {
    repo.close();
    if (e.code !== "inconsistent" || e.message !== "command digest mismatch") throw e;
    return { code: e.code, message: e.message };
  }
});

// boundary rejects
expectCode("limit_zero", "invalid_input", () => TaskRepository.open(DB).listTasks({ limit: 0 }));
expectCode("limit_huge", "invalid_input", () => TaskRepository.open(DB).listTasks({ limit: 99999 }));
expectCode("cursor_malformed", "invalid_input",
  () => TaskRepository.open(DB).listTasks({ limit: 5, cursor: { bad: 1 } }));
expectCode("bad_request_id", "invalid_input", () => TaskRepository.open(DB).getRequest(""));
expectCode("bad_owner", "invalid_input", () => TaskRepository.open(DB, { owner: 42 }));

// fail-closed on bad DBs (gate rejects before query)
expectCode("repo_on_garbage", "corrupt",
  () => TaskRepository.open(join(FIX, "garbage.db")));
expectCode("repo_on_empty", "unavailable",
  () => TaskRepository.open(join(FIX, "empty.db")));
expectCode("repo_on_missing", "unavailable",
  () => TaskRepository.open(join(FIX, "no.db")));

// bytes unchanged end-to-end
run("repo_bytes_unchanged", () => {
  const before = sha(DB);
  const repo = TaskRepository.open(DB);
  repo.listTasks({ limit: 4 });
  repo.getRequest("r-001");
  repo.close();
  if (sha(DB) !== before || sha(DB) !== shaAfterFixtures) throw new Error("repo mutated DB");
  return { bytesUnchanged: true };
});

// selftest
if (SELFTEST === "repo") {
  const repo = TaskRepository.open(DB);
  emit("selftest_repo", repo.getTask("ghost-99") !== null ? "PASS" : "FAIL",
    "expected a phantom row — STOP");
  repo.close();
}

process.exitCode = failures === 0 ? 0 : 1;
