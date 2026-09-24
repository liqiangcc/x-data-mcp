// NM-041 P3: validate the request idempotency admission primitive.
// Synthetic DBs only. Exit nonzero on FAIL.
// Usage: node request_repository_probe.mjs <root> [selftest]
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const { RequestRepository } = await import("../../src_node/storage/request_repository.mjs");
const { openDatabase, StorageError } = await import("../../src_node/storage/sqlite_adapter.mjs");

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "request_repository", id, status, detail, expected, actual }));
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

const FIX = mkdtempSync("/tmp/nm41-req-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const SRC = "/dev/shm/nm006";
const DB = join(FIX, "req.db");
copyFileSync(join(SRC, "db18.sqlite"), DB);
const CMD = JSON.stringify({ key: { operation: "collect" }, input: { symbol: "000001" } });
const CMD2 = JSON.stringify({ key: { operation: "collect" }, input: { symbol: "600000" } });
// requests.task_id is a real FK — create the tasks the fixture binds
{
  const w = openDatabase(DB, { mode: "readwrite" });
  for (const tid of ["t-1", "t-9", "t-x", "t"]) {
    w.run(`INSERT INTO tasks(task_id,command_key_json,status,state_revision,
      accepted_run_id,owner_run_id,created_at,last_progress_at,
      frozen_work_json,result_json,retry_of,item_count,items_pending,
      items_running,items_finished,items_succeeded)
      VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
      tid, "{}", "pending", 1n, "acc-1", null,
      "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z", "{}", null, null,
      0n, 0n, 0n, 0n, 0n);
  }
  w.close();
}

run("admit_first", () => {
  const repo = RequestRepository.open(DB, { mode: "readwrite" });
  const r = repo.admit("req-1", CMD, { kind: "task", taskId: "t-1" });
  repo.close();
  if (r.reused !== false || r.record.command_digest !== createHash('sha256').update(CMD,'utf8').digest('hex') ||
      r.record.outcome_kind !== "task" || r.record.task_id !== "t-1") throw new Error(JSON.stringify(r));
  return { reused: false, digest_verified: true };
});
run("admit_replay_identical", () => {
  const repo = RequestRepository.open(DB, { mode: "readwrite" });
  const r = repo.admit("req-1", CMD, { kind: "task", taskId: "t-1" });
  repo.close();
  if (r.reused !== true || r.record.command_json !== CMD) throw new Error(JSON.stringify(r));
  return { reused: true, bytes_identical: true };
});
run("admit_conflict_no_mutation", () => {
  const before = sha(DB);
  const repo = RequestRepository.open(DB, { mode: "readwrite" });
  try { repo.admit("req-1", CMD2, { kind: "task", taskId: "t-9" }); repo.close(); throw new Error("accepted conflict"); }
  catch (e) {
    repo.close();
    if (e.code !== "conflict" || e.message !== "REQUEST_CONFLICT") throw e;
    if (sha(DB) !== before) throw new Error("conflict mutated DB");
    return { code: e.code, unchanged: true };
  }
});
run("admit_terminal", () => {
  const repo = RequestRepository.open(DB, { mode: "readwrite" });
  const r = repo.admit("req-term", CMD, { kind: "terminal", terminalJson: '{"done":true}' });
  repo.close();
  if (r.record.outcome_kind !== "terminal" || r.record.task_id !== null ||
      r.record.terminal_json !== '{"done":true}') throw new Error(JSON.stringify(r));
  return { outcome_kind: "terminal", nullables_preserved: true };
});
// tx rollback on constraint failure: task outcome with missing task_id
// binding violates CHECK -> constraint, no row persisted
run("check_violation_rolls_back", () => {
  const before = sha(DB);
  const p2 = join(FIX, "req2.db");
  copyFileSync(DB, p2);
  // outcome kind 'task' with terminalJson (CHECK forces task->task_id)
  const repo = RequestRepository.open(p2, { mode: "readwrite" });
  try {
    repo.admit("req-bad", CMD, { kind: "task", taskId: "t-x" });
    // now corrupt path: bypass — call raw SQL to violate CHECK via tx
  } finally { /* fallthrough */ }
  // deterministic CHECK violation: admit uses validated shapes, so force
  // via a raw write inside the same handle to prove tx rollback
  const d = openDatabase(p2, { mode: "readwrite" });
  try {
    d.transaction(() => {
      d.run("INSERT INTO requests VALUES('req-x','{}','d','task',NULL,NULL)"); // violates CHECK
    });
    throw new Error("CHECK not enforced");
  } catch (e) {
    d.close();
    const rows = openDatabase(p2).query("SELECT COUNT(*) c FROM requests WHERE request_id='req-x'");
    if (Number(rows[0].c) !== 0) throw new Error("rolled-back row persisted");
    return { rolled_back: true, driver_code: e.driverCode };
  }
});

// boundary rejects before SQL
expectCode("empty_request_id", "invalid_input",
  () => RequestRepository.open(DB, { mode: "readwrite" }).admit("  ", CMD, { kind: "task", taskId: "t" }));
expectCode("bad_command_json", "invalid_input",
  () => RequestRepository.open(DB, { mode: "readwrite" }).admit("r-x", "{broken", { kind: "task", taskId: "t" }));
expectCode("bad_outcome_shape", "invalid_input",
  () => RequestRepository.open(DB, { mode: "readwrite" }).admit("r-x", CMD, { kind: "weird" }));
expectCode("task_outcome_no_taskid", "invalid_input",
  () => RequestRepository.open(DB, { mode: "readwrite" }).admit("r-x", CMD, { kind: "task" }));
expectCode("implicit_readonly_denied", "invalid_input",
  () => RequestRepository.open(DB)); // mode required explicitly
expectCode("open_garbage", "corrupt",
  () => { copyFileSync(join(SRC, "garbage.bin"), join(FIX, "g.db")); RequestRepository.open(join(FIX, "g.db"), { mode: "readwrite" }); });

// gate-before-write ordering: no artifact/mutation on rejection paths
run("create_missing_no_artifact", () => {
  const p = join(FIX, "never-created.db");
  try { RequestRepository.open(p, { mode: "create" }); } catch (e) {
    if (e.code !== "unavailable") throw e;
    if (existsSync(p) || existsSync(p + "-wal") || existsSync(p + "-shm")) {
      throw new Error("rejection left an artifact");
    }
    return { rejected: e.code, artifact_created: false };
  }
  throw new Error("create on missing path succeeded");
});
run("create_garbage_unchanged", () => {
  const p = join(FIX, "g2.db");
  copyFileSync(join(SRC, "garbage.bin"), p);
  const before = sha(p);
  try { RequestRepository.open(p, { mode: "create" }); } catch (e) {
    if (e.code !== "corrupt") throw e;
    if (sha(p) !== before) throw new Error("garbage mutated");
    return { rejected: e.code, bytes_unchanged: true };
  }
  throw new Error("create on garbage succeeded");
});
run("create_empty_unchanged", () => {
  const p = join(FIX, "e2.db");
  copyFileSync(join(SRC, "empty.db"), p);
  try { RequestRepository.open(p, { mode: "create" }); } catch (e) {
    if (e.code !== "unavailable" || !e.message.includes("schema 0")) throw e;
    if (statSync(p).size !== 0) throw new Error("empty file mutated");
    return { rejected: e.code, still_zero_bytes: true };
  }
  throw new Error("create on uninitialized succeeded");
});
run("readwrite_missing_no_artifact", () => {
  const p = join(FIX, "never2.db");
  try { RequestRepository.open(p, { mode: "readwrite" }); } catch (e) {
    if (e.code !== "unavailable" || existsSync(p)) throw e;
    return { rejected: e.code, artifact_created: false };
  }
  throw new Error("readwrite on missing path succeeded");
});

// replay of tampered store -> inconsistent (stored digest re-verified)
run("stored_digest_tamper", () => {
  const p = join(FIX, "tamper.db");
  copyFileSync(DB, p);
  const w = openDatabase(p, { mode: "readwrite" });
  w.run("UPDATE requests SET command_json='{\"x\":1}' WHERE request_id='req-1'");
  w.close();
  const repo = RequestRepository.open(p, { mode: "readwrite" });
  try { repo.admit("req-1", CMD, { kind: "task", taskId: "t-1" }); throw new Error("accepted"); }
  catch (e) {
    repo.close();
    if (e.code !== "inconsistent") throw e;
    return { code: e.code, message: e.message };
  }
});

// selftest
if (SELFTEST === "req") {
  const repo = RequestRepository.open(DB, { mode: "readwrite" });
  emit("selftest_req", repo.admit("req-ghost", CMD, { kind: "task", taskId: "t" }).reused ? "PASS" : "FAIL",
    "expected reuse of a nonexistent row — STOP");
  repo.close();
}

process.exitCode = failures === 0 ? 0 : 1;
