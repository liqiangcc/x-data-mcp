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
const MR = await import("../../src_node/market/market_read.mjs");
const MW = await import("../../src_node/market/market_write.mjs");
const MC = await import("../../src_node/market/market_commit.mjs");
const marketDigest = () => {
  const tables = [
    ["data_versions", "version_id"],
    ["payloads", "payload_id"],
    ["bar_segments", "segment_id"],
    ["segment_bars", "segment_id, trade_date"],
    ["manifest_nodes", "node_id"],
    ["yearly_bars", "version_id, year"],
    ["yearly_lineage", "version_id"],
    ["effective_heads", "symbol, period, adjustment"],
    ["publication_batches", "publication_id"],
    ["task_publications", "task_id, version_id"],
    ["market_update_results", "result_id"],
  ];
  let acc = "";
  for (const [table, pk] of tables) {
    acc += table;
    for (const r of db.query(`SELECT * FROM ${table} ORDER BY ${pk}`)) {
      const vals = Object.values(r).map((v) =>
        v === null ? null : typeof v === "bigint"
          ? (v >= -(2n ** 53n) && v <= 2n ** 53n ? Number(v) : v.toString())
          : v instanceof Uint8Array
            ? [...v].map((x) => x.toString(16).padStart(2, "0")).join("")
            : v);
      acc += JSON.stringify(vals) + "\n";
    }
  }
  for (const [k, real] of syms) {
    if (!k.startsWith("rev:")) acc = acc.split(real).join(`$${k}`);
  }
  return sha(acc);
};

const preseeded = process.env.DIFF_NOINIT === "1";

copyFileSync(preseeded ? process.env.DIFF_PRESEED : "/dev/shm/nm006/db18.sqlite", dbPath);
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
      case "seed": {
        rec = { status: "ok", seeded: true };
        break;
      }
      case "lookup_task": {
        const ids = db.query("SELECT task_id FROM tasks ORDER BY rowid").map((r) => r.task_id);
        if (op.sym) ids.forEach((id, i) => syms.set(i === 0 ? op.sym : `${op.sym}_${i + 1}`, id));
        rec = { status: "ok", task_id: ids[0] ?? null };
        break;
      }
      case "stage": {
        try {
          const dir = process.env.DIFF_FIXTURES || "scripts/node_migration_p3/records";
          const raw = JSON.parse(readFileSync(`${dir}/${op.record}`, "utf8"));
          MW.stage(db, raw);
          rec = { status: "ok", digest: marketDigest() };
        } catch (e) { rec = { status: "error", code: e.code ?? "internal", message: String(e.message), unchanged: true }; }
        break;
      }
      case "read_result":
      case "range_receipt": {
        try {
          const r = MR.readResult(db, resolve(op.result_id));
          rec = r === null ? { status: "ok", found: false }
            : { status: "ok", found: true,
                audit_sha256: sha(RC.__ser(r[0])),
                receipt: r[1] };
        } catch (e) { rec = { status: "error", code: e.code ?? "internal", message: String(e.message), unchanged: true }; }
        break;
      }
      case "item_receipt": {
        try {
          const r = MR.itemReceipt(db, resolve(op.task_id), op.item_id);
          rec = r === null ? { status: "ok", found: false }
            : { status: "ok", found: true, receipt: r };
        } catch (e) { rec = { status: "error", code: e.code ?? "internal", message: String(e.message), unchanged: true }; }
        break;
      }
      case "commit_range": {
        try {
          const p = op.plan;
          const dir = process.env.DIFF_FIXTURES || "scripts/node_migration_p3/records";
          const load = (n) => RC.check(JSON.parse(readFileSync(`${dir}/${n}`, "utf8")));
          const command = { ...p.command, task_id: resolve(p.command.task_id) };
          command.expected_revision = Number(command.expected_revision);
          if (command.intent?.Daily) {
            command.intent = { Daily: { expected_head: resolve(command.intent.Daily.expected_head) } };
          } else if (command.intent?.CurrentHead) {
            command.intent = { CurrentHead: {
              expected_daily_head: resolve(command.intent.CurrentHead.expected_daily_head),
              expected_yearly_head: resolve(command.intent.CurrentHead.expected_yearly_head) } };
          }
          const per = (x) => x === "day" ? "Day" : "Year";
          const deepResolve = (v) => {
            if (typeof v === "bigint") return Number(v);
            if (typeof v === "string" && v.startsWith("$")) {
              const r = resolve(v);
              return typeof r === "bigint" ? Number(r) : r;
            }
            if (Array.isArray(v)) return v.map(deepResolve);
            if (v !== null && typeof v === "object") {
              return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, deepResolve(x)]));
            }
            return v;
          };
          const audit = deepResolve(p.audit);

          const plan = {
            publication: {
              command,
              record: load(p.record),
              required_heads: (p.required_heads ?? []).map((h) =>
                [{ symbol: h[0], period: per(h[1]) }, h[2] == null ? null : resolve(h[2])]),
              promote: p.promote ? { symbol: p.promote.symbol, period: per(p.promote.period) } : null,
              base_digest: p.base_digest ?? null,
            },
            base: load(p.base),
            no_change: !!p.no_change,
            audit,
            audit_payloads: p.audit_payloads ?? [],
          };
          const r = MC.commitRangeTx(db, plan, op.now ?? "2026-09-24T00:00:00Z");
          rec = { status: "ok", receipt: r, digest: marketDigest() };
        } catch (e) { rec = { status: "error", code: e.code ?? "internal", message: String(e.message), unchanged: true }; }
        break;
      }
      case "exec_sql": {
        try {
          for (const stmt of op.sql.split(";").map((x) => x.trim()).filter(Boolean)) {
            db.run(stmt);
          }
          rec = { status: "ok" };
        } catch (e) {
          rec = { status: "error", code: "Storage", message: String(e.message), unchanged: true };
        }
        break;
      }
      case "head": {
        try {
          const scope = { symbol: op.symbol, period: op.period === "day" ? "Day" : "Year" };
          const v = MR.head(db, scope);
          rec = { status: "ok", version_id: v };
        } catch (e) { rec = { status: "error", code: e.code ?? "internal", message: String(e.message), unchanged: true }; }
        break;
      }
      case "read_record": {
        try {
          const r = op.mode === "fixed"
            ? MR.readRecord(db, op.version_id, true, true)
            : MR.readRecord(db, op.version_id, false, true);
          rec = { status: "ok", record: {
            version_id: r.version.version_id,
            integrity_sha256: r.raw.integrity_sha256,
            period: r.version.period,
            records: r.version.records.length,
            first: r.version.records[0]?.date.s ?? null,
            last: r.version.records.at(-1)?.date.s ?? null,
          }};
        } catch (e) { rec = { status: "error", code: e.code ?? "internal", message: String(e.message), unchanged: true }; }
        break;
      }
      case "range": {
        try {
          const scope = { symbol: op.symbol, period: op.period === "day" ? "Day" : "Year" };
          const p = MR.range(db, scope, op.version_id ?? null, {
            start: op.start, end: op.end,
            after: op.after ?? null, limit: op.limit,
          });
          rec = { status: "ok", page: {
            integrity_sha256: p.integrity_sha256,
            version_id: p.version.version_id,
            next_after: p.next_after ?? null,
            rows: p.rows.map((b) => ({
              date: typeof b.date === "string" ? b.date : b.date.s ?? b.date,
              open: MR.__decStr(b.open), high: MR.__decStr(b.high),
              low: MR.__decStr(b.low), close: MR.__decStr(b.close),
              volume: b.volume === null || b.volume === undefined ? null : MR.__decStr(b.volume),
              amount: b.amount === null || b.amount === undefined ? null : MR.__decStr(b.amount),
            })),
          }};
        } catch (e) { rec = { status: "error", code: e.code ?? "internal", message: String(e.message), unchanged: true }; }
        break;
      }
      case "versions": {
        try {
          const scope = { symbol: op.symbol, period: op.period === "day" ? "Day" : "Year" };
          const list = MR.versions(db, scope);
          rec = { status: "ok", versions: list.map((v) => ({
            version_id: v.version_id, published_at: MR.__toRfc3339(v.published_at),
            records: v.records.length,
          }))};
        } catch (e) { rec = { status: "error", code: e.code ?? "internal", message: String(e.message), unchanged: true }; }
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
