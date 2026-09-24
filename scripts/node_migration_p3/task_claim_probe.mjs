// NM-043 P3: validate atomic task claim CAS. Synthetic DBs only.
// Exit nonzero on FAIL. Usage: node task_claim_probe.mjs <root> [selftest]
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
  console.log(JSON.stringify({ family: "task_claim", id, status, detail, expected, actual }));
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

const FIX = mkdtempSync("/tmp/nm43-claim-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const SRC = "/dev/shm/nm006";
const DB = join(FIX, "claim.db");
copyFileSync(join(SRC, "db18.sqlite"), DB);

// fixture: one admitted pending task with scopes via NM-042 admission
const mkTask = (db, repo) => repo.admit({
  requestId: "req-claim", commandJson: JSON.stringify({ key: { operation: "collect" } }),
  executionKeyJson: JSON.stringify({ operation: "collect" }),
  workJson: JSON.stringify({ evidence_digest: "ev-1" }),
  items: [{ item_id: "i-1", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: '["s-A"]' }],
  scopes: ["s-A"], runId: "run-0", createdAt: "2026-01-02T00:00:00Z",
});
const db0 = openDatabase(DB, { mode: "readwrite" });
const repo0 = new TaskAdmission(db0, null);
const adm = mkTask(db0, repo0);
const TID = adm.task_id;
db0.close();

run("claim_happy", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  // claimed by the ACCEPTING run (Rust decide run-identity gate)
  const t = repo.claim(TID, 0n, "run-0", "2026-01-02T00:01:00Z");
  repo.close();
  if (t.status !== "running" || t.owner_run_id !== "run-0" ||
      t.state_revision !== 1n || t.last_progress_at !== "2026-01-02T00:01:00Z") {
    throw new Error(JSON.stringify(t));
  }
  // items/occupancy intact
  const d = openDatabase(DB);
  const items = d.query("SELECT * FROM task_items WHERE task_id=?", TID);
  const occ = d.query("SELECT * FROM scope_occupancy WHERE task_id=?", TID);
  d.close();
  if (items.length !== 1 || items[0].stage !== "pending" ||
      occ.length !== 1 || occ[0].state_revision !== 0n) throw new Error("items/occupancy touched");
  return { status: "running", revision: "1", owner: "run-0", items_untouched: true, occupancy_rev0: true };
});

run("claim_again_revision_conflict", () => {
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { repo.claim(TID, 0n, "run-0", "2026-01-02T00:02:00Z"); repo.close(); throw new Error("double-claimed"); }
  catch (e) {
    repo.close();
    if (e.code !== "revision_conflict") throw e;
    if (sha(DB) !== before) throw new Error("conflict mutated DB");
    return { code: e.code, unchanged: true };
  }
});
run("claim_stale_revision", () => {
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { repo.claim(TID, 99n, "run-0", "2026-01-02T00:03:00Z"); repo.close(); throw new Error("stale accepted"); }
  catch (e) {
    repo.close();
    if (e.code !== "revision_conflict") throw e;
    if (sha(DB) !== before) throw new Error("mutated");
    return { code: e.code };
  }
});
run("claim_missing_task", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { repo.claim("task-nope", 0n, "run-0", "2026-01-02T00:03:00Z"); repo.close(); throw new Error("claimed missing"); }
  catch (e) {
    repo.close();
    if (e.code !== "task_not_found") throw e;
    return { code: e.code };
  }
});
run("claim_cross_run_pending", () => {
  // a SECOND pending task admitted by run-B, claimed by run-C ->
  // needs_verification (accepted_run_id != run)
  const d0 = openDatabase(DB, { mode: "readwrite" });
  const r0 = new TaskAdmission(d0, null);
  const a = r0.admit({
    requestId: "req-claim-b", commandJson: JSON.stringify({ key: { operation: "c2" } }),
    executionKeyJson: JSON.stringify({ operation: "c2" }), workJson: "{}",
    items: [{ item_id: "i-1", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: "[]" }],
    scopes: ["s-B"], runId: "run-B", createdAt: "2026-01-02T00:00:30Z",
  });
  d0.close();
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { repo.claim(a.task_id, 0n, "run-C", "2026-01-02T00:04:00Z"); repo.close(); throw new Error("cross-run claimed"); }
  catch (e) {
    repo.close();
    if (e.code !== "needs_verification" || !e.message.includes("old run or unresolved execution")) throw e;
    if (sha(DB) !== before) throw new Error("mutated");
    return { code: e.code, message: e.message };
  }
});
run("claim_nonowner_on_running", () => {
  // running task owned by run-0; claim by run-D -> needs_verification
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { repo.claim(TID, 1n, "run-D", "2026-01-02T00:04:30Z"); repo.close(); throw new Error("non-owner claimed"); }
  catch (e) {
    repo.close();
    if (e.code !== "needs_verification") throw e;
    if (sha(DB) !== before) throw new Error("mutated");
    return { code: e.code };
  }
});
run("claim_owner_on_nonpending", () => {
  // owning run claims the already-running task -> revision_conflict
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { repo.claim(TID, 1n, "run-0", "2026-01-02T00:04:45Z"); repo.close(); throw new Error("reclaimed running"); }
  catch (e) {
    repo.close();
    if (e.code !== "revision_conflict") throw e;
    if (sha(DB) !== before) throw new Error("mutated");
    return { code: e.code };
  }
});
// terminal task cannot claim: mark task terminal first on a copy
run("claim_terminal_rejected", () => {
  const p = join(FIX, "term.db");
  copyFileSync(DB, p);
  const w = openDatabase(p, { mode: "readwrite" });
  w.run("UPDATE tasks SET status='succeeded',result_json='{}',items_finished=item_count,items_succeeded=item_count WHERE task_id=?", TID);
  w.run("DELETE FROM scope_occupancy WHERE task_id=?", TID);
  w.close();
  const before = sha(p);
  const repo = TaskAdmission.open(p, { mode: "readwrite" });
  try { repo.claim(TID, 1n, "run-0", "2026-01-02T00:05:00Z"); repo.close(); throw new Error("claimed terminal"); }
  catch (e) {
    repo.close();
    if (e.code !== "revision_conflict" || sha(p) !== before) throw e;
    return { code: e.code };
  }
});

// boundary rejects
expectCode("claim_empty_run", "invalid_input",
  () => TaskAdmission.open(DB, { mode: "readwrite" }).claim(TID, 1n, "  ", "2026-01-02T00:06:00Z"));
expectCode("claim_bad_revision_type", "invalid_input",
  () => TaskAdmission.open(DB, { mode: "readwrite" }).claim(TID, 1, "run-x", "2026-01-02T00:06:00Z"));
expectCode("claim_empty_taskid", "invalid_input",
  () => TaskAdmission.open(DB, { mode: "readwrite" }).claim("", 0n, "run-x", "2026-01-02T00:06:00Z"));
expectCode("claim_implicit_mode", "invalid_input", () => TaskAdmission.open(DB));

// reopen readback: durable committed state visible on a fresh handle
run("claim_reopen_readback", () => {
  const d = openDatabase(DB);
  const t = d.get("SELECT * FROM tasks WHERE task_id=?", TID);
  d.close();
  if (t.status !== "running" || t.owner_run_id !== "run-0" || t.state_revision !== 1n) {
    throw new Error(JSON.stringify(t));
  }
  return { durable: true };
});

// selftest
if (SELFTEST === "claim") {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try {
    const r = repo.claim(TID, 9999n, "ghost", "2026-01-02T00:09:00Z");
    emit("selftest_claim", r.state_revision === 9999n ? "PASS" : "FAIL",
      `unexpected success — STOP (rev ${r.state_revision})`);
  } catch (e) {
    emit("selftest_claim", "FAIL", `correctly rejected — selftest FAIL as designed (${e.code})`);
  }
  repo.close();
}

process.exitCode = failures === 0 ? 0 : 1;
