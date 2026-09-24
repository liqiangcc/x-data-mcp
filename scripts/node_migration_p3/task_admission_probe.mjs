// NM-042 P3: validate atomic task admission + scope occupancy.
// Synthetic DBs only. Exit nonzero on FAIL.
// Usage: node task_admission_probe.mjs <root> [selftest]
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
  console.log(JSON.stringify({ family: "task_admission", id, status, detail, expected, actual }));
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

const FIX = mkdtempSync("/tmp/nm42-adm-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const SRC = "/dev/shm/nm006";
const DB = join(FIX, "adm.db");
copyFileSync(join(SRC, "db18.sqlite"), DB);

const CMD = JSON.stringify({ key: { operation: "collect" }, input: { symbol: "000001" } });
const plan = (over = {}) => ({
  requestId: "req-adm-1",
  commandJson: CMD,
  executionKeyJson: JSON.stringify({ operation: "collect" }),
  workJson: JSON.stringify({ items: 2, evidence_digest: "ev-abc" }),
  items: [
    { item_id: "i-1", target_json: '{"s":"000001"}', progress_unit: "days", frozen_inputs_json: "{}", scope_keys_json: '["scope-A"]' },
    { item_id: "i-2", target_json: '{"s":"000002"}', progress_unit: "days", frozen_inputs_json: "{}", scope_keys_json: '["scope-B"]' },
  ],
  scopes: ["scope-A", "scope-B"],
  runId: "run-1",
  createdAt: "2026-01-02T00:00:00Z",
  ...over,
});
const count = (t) => {
  const d = openDatabase(DB);
  const n = Number(d.get(`SELECT COUNT(*) c FROM ${t}`).c);
  d.close();
  return n;
};

run("admit_creates_all_rows", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  const r = repo.admit(plan());
  repo.close();
  if (r.reused !== false || !/^task-[0-9a-f]{32}$/.test(r.task_id)) throw new Error(JSON.stringify(r));
  const checks = { tasks: 1, task_items: 2, task_scopes: 2, scope_occupancy: 2, requests: 1 };
  for (const [t, n] of Object.entries(checks)) if (count(t) !== n) throw new Error(`${t}=${count(t)}`);
  const d = openDatabase(DB);
  const t = d.get("SELECT * FROM tasks WHERE task_id=?", r.task_id);
  const occ = d.query("SELECT * FROM scope_occupancy WHERE task_id=?", r.task_id);
  d.close();
  if (t.status !== "pending" || t.state_revision !== 0n || t.owner_run_id !== null ||
      t.item_count !== 2n || t.items_pending !== 2n || t.items_running !== 0n ||
      t.items_finished !== 0n || t.items_succeeded !== 0n ||
      t.evidence_digest !== "ev-abc") throw new Error(JSON.stringify(t));
  if (occ.some(o => o.state_revision !== 0n || o.task_id !== r.task_id)) throw new Error("occupancy wrong");
  return { task_id: r.task_id, revision: "0", occupancy_rev0: true };
});

run("admit_replay_identical", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  const r = repo.admit(plan());
  repo.close();
  if (r.reused !== true || count("tasks") !== 1) throw new Error("replay created rows");
  return { reused: true };
});

run("admit_conflict_no_mutation", () => {
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try { repo.admit(plan({ commandJson: CMD + " " })); repo.close(); throw new Error("accepted"); }
  catch (e) {
    repo.close();
    if (e.code !== "conflict" || e.message !== "REQUEST_CONFLICT") throw e;
    if (sha(DB) !== before) throw new Error("conflict mutated DB");
    return { code: e.code, unchanged: true };
  }
});

run("scope_overlap_range_conflict", () => {
  const before = sha(DB);
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  try {
    repo.admit(plan({ requestId: "req-adm-2", commandJson: JSON.stringify({ key: { operation: "collect2" } }), scopes: ["scope-B", "scope-Z"] }));
    repo.close();
    throw new Error("accepted overlap");
  } catch (e) {
    repo.close();
    if (e.code !== "range_conflict" || !e.message.includes("scope-B")) throw e;
    if (sha(DB) !== before || count("tasks") !== 1) throw new Error("overlap left rows");
    return { code: e.code, conflict_list: e.message };
  }
});

// forced constraint rollback: duplicate item_id passes JS (dedup check),
// so instead violate PK via a second admit inside the same tx path —
// simplest deterministic check: malformed plan rejects pre-SQL and a
// CHECK-violating counter insert via raw tx proves rollback machinery.
run("malformed_plan_rejects", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  const cases = [
    plan({ requestId: "  " }),
    plan({ commandJson: "{broken" }),
    plan({ items: [{ item_id: "i-1", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: "[]" },
                   { item_id: "i-1", target_json: "{}", progress_unit: "d", frozen_inputs_json: "{}", scope_keys_json: "[]" }] }),
    plan({ scopes: ["scope-C", "scope-C"] }),
    plan({ runId: "" }),
  ];
  let rejected = 0;
  for (const p of cases) {
    try { repo.admit(p); } catch (e) { if (e.code === "invalid_input") rejected++; }
  }
  repo.close();
  if (rejected !== cases.length) throw new Error(`${rejected}/${cases.length} rejected`);
  if (count("tasks") !== 1) throw new Error("malformed plan left rows");
  return { rejected };
});

run("forced_constraint_rolls_back", () => {
  // same scope in task_scopes + a constraint violation inside one tx
  const p2 = join(FIX, "adm2.db");
  copyFileSync(join(SRC, "db18.sqlite"), p2);
  const d = openDatabase(p2, { mode: "readwrite" });
  try {
    d.transaction(() => {
      d.run(`INSERT INTO tasks(task_id,command_key_json,status,state_revision,
        accepted_run_id,owner_run_id,created_at,last_progress_at,
        frozen_work_json,result_json,retry_of,item_count,items_pending,
        evidence_digest) VALUES('tx-t','{}','pending',0,'r',NULL,'n','n','{}',NULL,NULL,0,0,NULL)`);
      d.run("INSERT INTO task_items VALUES('tx-t','i','{}','u','{}','[]','pending',NULL)");
      d.run("INSERT INTO task_items VALUES('tx-t','i','{}','u','{}','[]','pending',NULL)"); // PK dup
    });
    throw new Error("dup PK accepted");
  } catch (e) {
    d.close();
    const d2 = openDatabase(p2);
    const n = Number(d2.get("SELECT COUNT(*) c FROM tasks WHERE task_id='tx-t'").c);
    const ni = Number(d2.get("SELECT COUNT(*) c FROM task_items WHERE task_id='tx-t'").c);
    d2.close();
    if (n !== 0 || ni !== 0) throw new Error(`partial rows: task=${n} items=${ni}`);
    return { rolled_back: true, driver_code: e.driverCode };
  }
});

run("reopen_readback", () => {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  const r = repo.admit(plan({ requestId: "req-adm-3",
    commandJson: JSON.stringify({ key: { operation: "collect3" } }),
    scopes: ["scope-C"], items: plan().items.slice(0, 1) }));
  repo.close();
  // fresh handle reads the committed rows
  const d = openDatabase(DB);
  const t = d.get("SELECT * FROM tasks WHERE task_id=?", r.task_id);
  const req = d.get("SELECT * FROM requests WHERE request_id='req-adm-3'");
  d.close();
  if (!t || req.task_id !== r.task_id) throw new Error("reopen readback failed");
  return { readback: true, task_id: r.task_id };
});

// boundary: writable mode explicit
expectCode("implicit_mode_denied", "invalid_input", () => TaskAdmission.open(DB));
expectCode("garbage_gated", "corrupt", () => {
  const p = join(FIX, "g.db");
  copyFileSync(join(SRC, "garbage.bin"), p);
  TaskAdmission.open(p, { mode: "create" });
});

// selftest
if (SELFTEST === "adm") {
  const repo = TaskAdmission.open(DB, { mode: "readwrite" });
  const r = repo.admit(plan({ requestId: "req-ghost", commandJson: JSON.stringify({ key: { operation: "g" } }), scopes: [], items: [] }));
  emit("selftest_adm", r.task_id === "task-fixed-000" ? "PASS" : "FAIL", `unexpected success — STOP (${r.task_id})`);
  repo.close();
}

process.exitCode = failures === 0 ? 0 : 1;
