// NM-049 — Node side of the differential harness. Reads the same JSONL
// scenario vocabulary as the Rust oracle (src/diff_harness.rs) and emits
// identical {seq,op,status,code,message,task,unchanged} JSONL rows.
//
// Usage: node diff_runner.mjs <scenario.jsonl> <db> <out.jsonl>
import { createHash } from "node:crypto";
import { copyFileSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [, , scenarioPath, dbPath, outPath] = process.argv;
const { TaskAdmission } = await import("../../src_node/storage/task_repository.mjs");
const { openDatabase, StorageError } = await import("../../src_node/storage/sqlite_adapter.mjs");
const { ser } = await import("../../src_node/storage/verification_storage.mjs");
const RC = await import("../../src_node/market/record_codec.mjs");

copyFileSync("/dev/shm/nm006/db18.sqlite", dbPath);
const repo = TaskAdmission.open(dbPath, { mode: "readwrite" });
const db = repo._db;

const sha = (s) => createHash("sha256").update(s).digest("hex");
const syms = new Map(); // symbol -> real task_id; "rev:<sym>" -> revision
const symOf = (id) => {
  for (const [k, v] of syms) if (!k.startsWith("rev:") && v === id) return `$${k}`;
  return id;
};
const normalizeIds = (v) => {
  let raw = JSON.stringify(v);
  for (const [k, real] of syms) {
    if (!k.startsWith("rev:")) raw = raw.split(real).join(`$${k}`);
  }
  return JSON.parse(raw);
};
const resolve = (v) => {
  if (typeof v === "string") {
    if (v.startsWith("$REV:")) {
      const r = syms.get(`rev:${v.slice(5)}`);
      return r === undefined ? -1n : BigInt(r);
    }
    return v.startsWith("$") ? (syms.get(v.slice(1)) ?? v) : v;
  }
  if (Array.isArray(v)) return v.map(resolve);
  if (v !== null && typeof v === "object") {
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, resolve(x)]));
  }
  return v;
};

const sortedValue = (v) => JSON.parse(ser(v)); // canonical Value (sorted keys)
const sortedArr = (a) => [...a].sort();

const stateDigest = () => {
  const lines = [];
  const dump = (sql, tag) => {
    for (const r of db.query(sql)) {
      lines.push(`${tag}:${Object.values(r).map((x) => x === null ? "" : String(x)).join("|")}`);
    }
  };
  dump("SELECT task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,frozen_work_json,result_json,retry_of,item_count,items_pending,items_running,items_finished,items_succeeded FROM tasks ORDER BY task_id", "T");
  dump("SELECT task_id,item_id,stage,outcome_json FROM task_items ORDER BY task_id,item_id", "I");
  dump("SELECT scope_key,task_id,state_revision FROM scope_occupancy ORDER BY 1,2", "O");
  dump("SELECT task_id,scope_key FROM task_scopes ORDER BY 1,2", "S");
  dump("SELECT request_id,command_digest,outcome_kind,task_id,terminal_json FROM requests ORDER BY 1", "R");
  dump("SELECT task_id,item_id,run_id,start_revision FROM task_item_execution_starts ORDER BY 1,2", "X");
  let joined = lines.join("\n");
  for (const [k, real] of syms) {
    if (!k.startsWith("rev:")) joined = joined.split(real).join(`$${k}`);
  }
  return sha(joined);
};

const projection = (s) => {
  if (!s) return null;
  const t = s.task;
  return {
    task_id: symOf(t.task_id),
    status: t.status,
    revision: Number(t.state_revision),
    accepted_run_id: t.accepted_run_id,
    owner_run_id: t.owner_run_id,
    retry_of: t.retry_of === null ? null : symOf(t.retry_of),
    result: t.result_json === null ? null : normalizeIds(JSON.parse(t.result_json)),
    command_key: JSON.parse(t.command_key_json),
    items: s.items.map((i) => ({
      item_id: i.item_id, stage: i.stage, outcome: i.outcome,
    })),
    claims: s.claims.map((c) => ({
      scope_key: c.scope_key, state_revision: Number(c.state_revision),
    })),
  };
};
const refreshRev = (taskId, rev) => {
  for (const [k, v] of syms) {
    if (!k.startsWith("rev:") && v === taskId) {
      syms.set(`rev:${k}`, String(rev));
    }
  }
};

const errCode = (e) => e instanceof StorageError ? e.code : "internal";
const results = [];
const lines = readFileSync(scenarioPath, "utf8").split("\n").filter((l) => l.trim());
let seq = 0;
for (const line of lines) {
  const op = resolve(JSON.parse(line));
  const name = op.op;
  const before = stateDigest();
  let rec;
  try {
    switch (name) {
      case "admit":
      case "admit_retry": {
        const work = op.work;
        const commandJson = ser(op.command, "command");
        const plan = {
          requestId: op.request_id,
          commandJson,
          executionKeyJson: ser(op.command.key, "key"),
          workJson: ser({
            items: work.items.map((i) => ({ ...i, scope_keys: sortedArr(i.scope_keys) })),
            scopes: sortedArr(work.scopes),
          }, "work"),
          items: work.items.map((i) => ({
            item_id: i.item_id,
            target_json: ser(sortedValue(i.target)),
            progress_unit: i.progress_unit,
            frozen_inputs_json: ser(sortedValue(i.frozen_inputs)),
            scope_keys_json: JSON.stringify(sortedArr(i.scope_keys)),
          })),
          scopes: work.scopes,
          runId: op.run,
          retryOf: op.retry ? op.retry.task : null,
          retry: op.retry ? {
            originalTaskId: op.retry.task,
            expectedRevision: BigInt(op.retry.expected_revision),
            selectedItems: op.retry.selected_items,
          } : null,
          createdAt: "2026-01-02T00:00:00Z",
        };
        const a = repo.admit(plan);
        if (op.as) {
          syms.set(op.as, a.task_id);
          syms.set(`rev:${op.as}`, "0");
        }
        rec = { status: "ok", task: { task_id: symOf(a.task_id), reused: a.reused } };
        break;
      }
      case "claim":
      case "startItem":
      case "finishItem":
      case "requestCancel":
      case "finishExecution":
      case "finishCancellation":
      case "requireVerification": {
        const tid = op.task;
        const rev = BigInt(op.revision);
        if (name === "claim") repo.claim(tid, rev, op.run, "2026-01-02T00:00:10Z");
        else if (name === "startItem") repo.startItem(tid, rev, op.run, op.item, "2026-01-02T00:00:10Z");
        else if (name === "finishItem") repo.finishItem(tid, rev, op.run, op.item, op.outcome, "2026-01-02T00:00:10Z");
        else if (name === "requestCancel") repo.requestCancel(tid, rev, op.run, "2026-01-02T00:00:10Z");
        else if (name === "finishExecution") repo.finishExecution(tid, rev, op.run, "2026-01-02T00:00:10Z");
        else if (name === "finishCancellation") repo.finishCancellation(tid, rev, op.run, "2026-01-02T10:00:00Z");
        else repo.requireVerification(tid, rev, op.run, op.item, op.reason, "2026-01-02T00:00:10Z");
        // transition_tx re-reads the decided snapshot — same here
        const s = repo._readBoundTaskValidated(tid);
        refreshRev(tid, s.task.state_revision);
        rec = { status: "ok", task: projection(s) };
        break;
      }
      case "candidates": {
        const list = repo.candidates(op.run);
        rec = { status: "ok", candidates: list.map((s) => symOf(s.task.task_id)) };
        break;
      }
      case "reconcile": {
        const n = repo.reconcile(op.run);
        rec = { status: "ok", reconciled: n.reconciled ?? n };
        break;
      }
      case "check_record": {
        const dir = process.env.DIFF_FIXTURES ?? "scripts/node_migration_p3/records";
        const raw = JSON.parse(readFileSync(join(dir, op.file), "utf8"));
        try {
          const c = RC.check(raw);
          rec = { status: "ok", version_id: c.version.version_id };
        } catch (e) {
          rec = {
            status: "error",
            code: e.code ?? "internal",
            message: String(e.message),
            unchanged: true,
          };
        }
        break;
      }
      case "checkpoint": {
        const s = repo._readBoundTaskValidated(op.task);
        if (s) refreshRev(op.task, s.task.state_revision);
        rec = { status: "ok", task: projection(s) };
        break;
      }
      default:
        throw new Error(`unknown op ${name}`);
    }
  } catch (e) {
    rec = {
      status: "error", code: errCode(e), message: String(e.message),
      unchanged: stateDigest() === before,
    };
  }
  rec.seq = seq++;
  rec.op = name;
  results.push(rec);
}
repo.close();
writeFileSync(outPath, results.map((r) => JSON.stringify(r)).join("\n") + "\n");
