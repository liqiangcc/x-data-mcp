// NM-049 — Node side of the differential harness. Reads the same JSONL
// scenario vocabulary as the Rust oracle (src/diff_harness.rs) and emits
// identical {seq,op,status,code,message,task,unchanged} JSONL rows.
//
// Usage: node diff_runner.mjs <scenario.jsonl> <db> <out.jsonl>
import { createHash } from "node:crypto";
import { copyFileSync, chmodSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [, , scenarioPath, dbPath, outPath] = process.argv;
const { TaskAdmission } = await import("../../src_node/storage/task_repository.mjs");
const { openDatabase, StorageError } = await import("../../src_node/storage/sqlite_adapter.mjs");
const VST = await import("../../src_node/storage/verification_storage.mjs");
const { ser } = VST;
const RC = await import("../../src_node/market/record_codec.mjs");
const MR = await import("../../src_node/market/market_read.mjs");
const MW = await import("../../src_node/market/market_write.mjs");
const MC = await import("../../src_node/market/market_commit.mjs");
const CMR = await import("../../src_node/market/compact_read.mjs");
const CMM = await import("../../src_node/market/compact_market.mjs");
const CW = await import("../../src_node/market/compact_write.mjs");
const GAPS = await import("../../src_node/market/gaps.mjs");
const YR = await import("../../src_node/market/yearly_results.mjs");
const COLL = await import("../../src_node/market/collection.mjs");
const VF = await import("../../src_node/market/verification_facts.mjs");
const PC = await import("../../src_node/storage/publication_checks.mjs");
const BE = await import("../../src_node/market/backend.mjs");
const { DatabaseLease } = await import("../../src_node/storage/database_lease.mjs");
const isCompact = () => MR.isCompactSchema(db);
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
const NORM_TS = process.env.DIFF_NORM_TS === "1";
const normSet = () => new Set(db.query(
  "SELECT DISTINCT v FROM (SELECT created_at AS v FROM data_versions UNION ALL SELECT published_at AS v FROM data_versions UNION ALL SELECT hex(integrity_digest) AS v FROM data_versions WHERE integrity_digest IS NOT NULL)")
  .map((r) => r.v));
const normDeep = (v, set) => {
  if (typeof v === "string" && set.has(v)) return "<norm>";
  if (Array.isArray(v)) return v.map((x) => normDeep(x, set));
  if (v !== null && typeof v === "object") {
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, normDeep(x, set)]));
  }
  return v;
};

copyFileSync(preseeded ? process.env.DIFF_PRESEED : "/dev/shm/nm006/db18.sqlite", dbPath);
const repo = TaskAdmission.open(dbPath, { mode: "readwrite" });
const db = repo._db;
// Production posture: a compact (schema-19) market is always opened under
// the DatabaseLease flock — mirror it so open/open_shared lock semantics
// differential against the Rust coordinator lease.
let heldLease = null;
if (MR.isCompactSchema(db)) {
  // match the harness's chmod-to-0600 before the lease gate.
  chmodSync(dbPath, 0o600);
  const { DatabaseLease } = await import("../../src_node/storage/database_lease.mjs");
  heldLease = DatabaseLease.acquire(dbPath);
}

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
  if (isCompact()) {
    const cvs = (x) => x === null ? "" : x instanceof Uint8Array
      ? Buffer.from(x).toString("hex") : String(x);
    const cdump = (sql, tag) => {
      for (const r of db.query(sql)) {
        lines.push(`${tag}:${Object.values(r).map(cvs).join("|")}`);
      }
    };
    cdump("SELECT version_key,version_id,entity,period,adjustment,state,hex(source_binding_digest),parent_version_key,manifest_mode,delta_depth,record_count,first_day,last_day,coverage_status,hex(evidence_digest),hex(projection_digest),digest_format FROM data_versions ORDER BY version_key", "CV");
    for (const r of db.query("SELECT version_key,published_at IS NOT NULL AS p FROM data_versions ORDER BY version_key")) {
      let idhex = "";
      if (r.p === 1n) {
        try {
          const row = CMR.__readVersionRow(db, r.version_key);
          idhex = Buffer.from(CMM.versionIntegrityDigest(
            CMR.__buildIdentity(db, row, "0000-01-01T00:00:00+00:00"))).toString("hex");
        } catch { idhex = "<ERR>"; }
      }
      lines.push(`CVI:${r.version_key}:${idhex}`);
    }
    cdump("SELECT segment_key,hex(segment_digest),entity,period,first_day,last_day,row_count FROM bar_segments ORDER BY segment_key", "CS");
    cdump("SELECT segment_key,trade_day,open_cents,high_cents,low_cents,close_cents,volume,amount,r7_raw FROM segment_bars ORDER BY segment_key,trade_day", "CB");
    cdump("SELECT version_key,seq,segment_key,clip_start_day,clip_end_day,row_count FROM version_checkpoint_clips ORDER BY version_key,seq", "CC");
    cdump("SELECT version_key,seq,delta_kind,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count FROM version_delta_clips ORDER BY version_key,seq", "CD");
    cdump("SELECT version_key,base_daily_version_key,algorithm_version,first_trade_day,last_trade_day,as_of_day FROM yearly_lineage ORDER BY version_key", "CY");
    cdump("SELECT entity,period,adjustment,version_key FROM effective_heads ORDER BY 1,2,3", "CH");
    cdump("SELECT hex(evidence_digest),evidence_kind,canonical_json FROM evidence_bundles ORDER BY evidence_digest", "CE");
    cdump("SELECT hex(source_binding_digest),generation_id,hex(generation_manifest_sha256),hex(day_member_sha256),hex(security_event_sha256),hex(evidence_digest) FROM source_bindings ORDER BY source_binding_digest", "CSB");
    cdump("SELECT task_id,version_key,result_id FROM task_publications ORDER BY 1,2", "CTP");
    cdump("SELECT result_id,result_kind FROM result_registry ORDER BY result_id", "CRR");
    cdump("SELECT result_key,result_id,task_id,item_id,base_version_key,result_version_key,outcome,range_start_day,range_end_day,algorithm_version,hex(source_binding_digest),continuity_mode,prefix_bytes,hex(base_projection_digest),hex(result_projection_digest),hex(delta_digest),hex(evidence_digest),task_revision,hex(result_digest) FROM market_update_results ORDER BY result_key", "CMU");
    cdump("SELECT result_key,seq,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count FROM market_update_delta_clips ORDER BY result_key,seq", "CMD");
    cdump("SELECT result_key,publication_id,task_id,item_id,publication_kind,base_version_key,result_version_key,outcome,algorithm_version,hex(source_binding_digest),hex(evidence_digest),task_revision,hex(result_digest) FROM publication_results ORDER BY result_key", "CPR");
  }
  if (!isCompact()) {
    dump("SELECT result_id,task_id,item_id,base_version_id,result_version_id,no_change,audit_json,result_sha256,receipt_json FROM market_update_results ORDER BY result_id", "MU");
    dump("SELECT result_id,task_id,item_id,base_daily_version_id,result_version_id,no_change,audit_json,result_sha256,receipt_json FROM yearly_execution_results ORDER BY result_id", "YR");
  }
  dump("SELECT decision_id,symbol,period,start_date,end_date,subject_version_id,subject_version_digest,conclusion,basis_kind,basis_ref,basis_sha256,verifier_id,decided_at,decision_sha256 FROM gap_verification_decisions ORDER BY decision_id", "GD");
  dump("SELECT gap_id,symbol,period,start_date,end_date,evidence_kind,evidence_id,evidence_digest,observed_at FROM proven_gaps ORDER BY gap_id", "PG");
  dump("SELECT gap_id,version_id,covered_at FROM gap_coverings ORDER BY gap_id", "GC");
  dump("SELECT verification_task_id,target_task_id,audit_json,audit_sha256 FROM publication_checks ORDER BY 1", "PC");
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
          rec = { status: "error", code: "unavailable", message: String(e.message), unchanged: true };
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
            first: r.version.records[0]
              ? (typeof r.version.records[0].date === "string"
                  ? r.version.records[0].date : r.version.records[0].date.s)
              : null,
            last: r.version.records.at(-1)
              ? (typeof r.version.records.at(-1).date === "string"
                  ? r.version.records.at(-1).date : r.version.records.at(-1).date.s)
              : null,
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
      case "stage_ops_delta":
      case "validate_staged":
      case "commit_ops_update":
      case "commit_ops_no_change":
      case "update_base_facts":
      case "base_bars_in_range": {
        try {
          let out;
          if (name === "stage_ops_delta") {
            out = { version_key: Number(CW.stageOpsDelta(
              db, op.entity, op.base_version_id, op.version_id,
              op.upserts ?? [], op.removes ?? [])) };
          } else if (name === "validate_staged") {
            CW.validateStaged(db, op.version_id);
            out = {};
          } else if (name === "commit_ops_update") {
            const r = CW.commitOpsUpdate(db, {
              result_id: op.result_id, task_id: op.task_id, item_id: op.item_id,
              base_version_id: op.base_version_id,
              result_version_id: op.result_version_id,
              expected_revision: op.expected_revision,
              run_id: op.run_id, occupancy_key: op.occupancy_key,
              continuity_evidence: op.continuity_evidence ?? null,
            });
            out = { receipt: {
              publication_id: r.publication_id, version_id: r.version_id,
              task_id: r.task_id, task_revision: Number(r.task_revision) } };
          } else if (name === "commit_ops_no_change") {
            const r = CW.commitOpsNoChange(db, {
              result_id: op.result_id, task_id: op.task_id, item_id: op.item_id,
              base_version_id: op.base_version_id,
              expected_revision: op.expected_revision,
              run_id: op.run_id, occupancy_key: op.occupancy_key,
              range_start_day: op.range_start_day, range_end_day: op.range_end_day,
              continuity_evidence: op.continuity_evidence ?? null,
            });
            out = { receipt: {
              publication_id: r.publication_id, version_id: r.version_id,
              task_id: r.task_id, task_revision: Number(r.task_revision) } };
          } else if (name === "update_base_facts") {
            out = CW.updateBaseFacts(db, op.base_version_id);
          } else {
            const bars = CW.baseBarsInRange(db, op.base_version_id, op.start, op.end);
            out = { bars: bars.map((b) => ({
              date: b.date, open: MR.__decStr(b.open), high: MR.__decStr(b.high),
              low: MR.__decStr(b.low), close: MR.__decStr(b.close),
              volume: b.volume === null ? null : MR.__decStr(b.volume),
              amount: b.amount === null ? null : MR.__decStr(b.amount),
              r7_raw: b.r7_raw === null || b.r7_raw === undefined ? null : Number(b.r7_raw),
            })) };
          }
          rec = { status: "ok", digest: sha(stateDigest()), ...out };
        } catch (e) {
          // commit_ops_* run autocommit like the oracle — partial writes
          // on rejection are real state; report unchanged honestly.
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "checkpoint": {
        const s = repo._readBoundTaskValidated(op.task);
        if (s) refreshRev(op.task, s.task.state_revision);
        rec = { status: "ok", task: projection(s) };
        break;
      }

      // ---- NM-056 — market tail ops ----
      case "record_gap_decision":
      case "record_gap_decision_guarded": {
        try {
          const inserted = name.endsWith("guarded")
            ? GAPS.recordGapDecisionGuarded(db, op.decision)
            : GAPS.recordGapDecision(db, op.decision);
          rec = { status: "ok", inserted };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "record_proven_gap": {
        try {
          rec = { status: "ok", inserted: GAPS.recordProvenGap(db, op.gap) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "cover_gap": {
        try {
          rec = { status: "ok",
            inserted: GAPS.coverGap(db, op.covering, isCompact()) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "gap_verification_decision": {
        try {
          rec = { status: "ok",
            decision: GAPS.gapVerificationDecision(db, op.decision_id) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "proven_gap": {
        try {
          rec = { status: "ok", gap: GAPS.provenGap(db, op.gap_id) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "open_proven_gaps": {
        try {
          rec = { status: "ok", gaps: GAPS.openProvenGaps(
            db, op.symbol, op.period === "year" ? "Year" : "Day") };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "gap_coverings": {
        try {
          rec = { status: "ok", coverings: GAPS.gapCoverings(db, op.gap_id) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "yearly_read_result": {
        try {
          const r = YR.readResult(db, op.result_id);
          rec = r === null
            ? { status: "ok", audit_sha256: null, receipt: null }
            : { status: "ok",
                audit_sha256: sha(CMM.canonicalJson(r[0])),
                receipt: r[1] };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "collection_receipt": {
        try {
          rec = { status: "ok",
            receipt: COLL.collectionReceipt(db, op.task, op.item) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "effective_head": {
        try {
          rec = { status: "ok", version_id: COLL.effectiveHead(
            db, { symbol: op.symbol,
                  period: op.period === "year" ? "Year" : "Day" }) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "read_facts": {
        try {
          const facts = VF.readFacts(db, resolve(op.task),
            (x) => repo._readBoundTaskValidated(resolve(x)));
          rec = { status: "ok", facts: {
            task: VST.snapshotCanon(facts.task),
            items: facts.items,
          } };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "check_receipt": {
        try {
          const r = PC.readCheckReceipt(db, resolve(op.id),
            (x) => repo._readBoundTaskValidated(resolve(x)),
            (x) => VF.readFacts(db, resolve(x),
              (y) => repo._readBoundTaskValidated(resolve(y))));
          rec = { status: "ok", receipt: r === null ? null : r[1] };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "backend_open_shared": {
        const target = op.target === "copy"
          ? (() => {
              const dst = `${dbPath}.backend-copy`;
              copyFileSync(dbPath, dst);
              chmodSync(dst, 0o600);
              return dst;
            })()
          : op.target === "missing" ? `${dbPath}.backend-missing` : op.target;
        try {
          const leasePath = target === `${dbPath}.backend-missing` ? dbPath : target;
          let lease;
          try { lease = DatabaseLease.acquire(leasePath); }
          catch (e) {
            rec = { status: "error", code: "base_unavailable",
              message: `BASE_UNAVAILABLE: database lease: ${e.message}`,
              unchanged: stateDigest() === before };
            break;
          }
          try {
            const b = BE.backendOpenShared(target, lease);
            rec = { status: "ok", kind: b.kind };
            if (b.lease) b.lease.close();
            lease.close();
          } catch (e) {
            try { lease.close(); } catch { /* */ }
            throw e;
          }
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "backend_open": {
        const target = op.target === "copy"
          ? (() => {
              const dst = `${dbPath}.backend-copy`;
              copyFileSync(dbPath, dst);
              chmodSync(dst, 0o600);
              return dst;
            })()
          : op.target === "missing" ? `${dbPath}.backend-missing` : op.target;
        try {
          const b = BE.backendOpen(target);
          rec = { status: "ok", kind: b.kind };
          if (b.lease) b.lease.close();
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
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
  results.push(NORM_TS ? normDeep(rec, normSet()) : rec);
}
repo.close();
writeFileSync(outPath, results.map((r) => JSON.stringify(r)).join("\n") + "\n");
