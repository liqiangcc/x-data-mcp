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
const { MarketError } = RC;
const MR = await import("../../src_node/market/market_read.mjs");
const MW = await import("../../src_node/market/market_write.mjs");
const MC = await import("../../src_node/market/market_commit.mjs");
const CMR = await import("../../src_node/market/compact_read.mjs");
const CMM = await import("../../src_node/market/compact_market.mjs");
const { _dec: DEC } = await import("../../src_node/algorithms/qfq_affine.mjs");
const CW = await import("../../src_node/market/compact_write.mjs");
const GAPS = await import("../../src_node/market/gaps.mjs");
const YR = await import("../../src_node/market/yearly_results.mjs");
const COLL = await import("../../src_node/market/collection.mjs");
const VF = await import("../../src_node/market/verification_facts.mjs");
const PC = await import("../../src_node/storage/publication_checks.mjs");
const VI = await import("../../src_node/storage/verification_inspection.mjs");
const VRT = await import("../../src_node/storage/verification_runtime.mjs");
const { canonEq } = await import("../../src_node/storage/task_repository.mjs");
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
  const symText = (v) => {
    for (let i = 0; i < v.length; i++) {
      if (typeof v[i] === "string") {
        for (const [k, real] of syms) {
          if (!k.startsWith("rev:")) v[i] = v[i].split(real).join(`$${k}`);
        }
      }
    }
  };
  let acc = "";
  for (const [table, pk] of tables) {
    acc += table + "\n";
    for (const r of db.query(`SELECT * FROM ${table} ORDER BY ${pk}`)) {
      const vals = Object.values(r).map((v) =>
        v === null ? null : typeof v === "bigint"
          ? (v >= -(2n ** 53n) && v <= 2n ** 53n ? Number(v) : v.toString())
          : v instanceof Uint8Array
            ? [...v].map((x) => x.toString(16).padStart(2, "0")).join("")
            : v);
      // market_update_results.result_sha256 binds the audit bytes
      // including the random task id — recompute over the symbolized
      // audit so the digest input is runner-stable.
      if (table === "market_update_results" && vals.length === 9) {
        let symmed = vals[6];
        for (const [k, real] of syms) {
          if (!k.startsWith("rev:")) symmed = symmed.split(real).join(`$${k}`);
        }
        vals[7] = sha(symmed);
      }
      symText(vals);
      acc += JSON.stringify(vals) + "\n";
    }
  }
  // wall-clock columns (created_at/published_at inside rows and
  // audit/receipt JSON) differ per runner — normalize RFC3339 text.
  acc = acc.replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(\+00:00|Z)/g, "<TS>");
  // order-stable digest input — task ids are random per-runner;
  // the trailing newline leaves no empty line (Rust's .lines() drops it).
  return sha(acc.split("\n").filter((x) => x !== "").sort().join("\n"));
};

const preseeded = process.env.DIFF_NOINIT === "1";
const NORM_TS = process.env.DIFF_NORM_TS === "1";
const normSet = () => {
  // Volatile columns only — the union is built over tables that actually
  // exist (result tables are compact-only).
  const has = (t) => db.get(
    "SELECT count(*) AS n FROM sqlite_master WHERE type='table' AND name=?", t).n > 0n;
  const col = (t, k) => db.get(
    "SELECT count(*) AS n FROM pragma_table_info(?) WHERE name=?", t, k).n > 0n;
  const parts = [];
  if (col("data_versions", "created_at")) {
    parts.push("SELECT created_at AS v FROM data_versions");
  }
  if (col("data_versions", "published_at")) {
    parts.push("SELECT published_at AS v FROM data_versions");
  }
  if (col("data_versions", "integrity_digest")) {
    parts.push("SELECT hex(integrity_digest) AS v FROM data_versions");
  }
  if (has("gap_verification_decisions")) {
    parts.push("SELECT decided_at AS v FROM gap_verification_decisions",
      "SELECT decision_sha256 AS v FROM gap_verification_decisions");
  }
  if (has("proven_gaps")) {
    parts.push("SELECT observed_at AS v FROM proven_gaps",
      "SELECT evidence_digest AS v FROM proven_gaps");
  }
  if (has("gap_coverings")) parts.push("SELECT covered_at AS v FROM gap_coverings");
  if (has("market_update_results")) {
    // legacy column: result_sha256; compact column: result_digest
    parts.push(`SELECT hex(${col("market_update_results", "result_digest")
      ? "result_digest" : "result_sha256"}) AS v FROM market_update_results`);
  }
  if (has("publication_results")) {
    parts.push("SELECT hex(result_digest) AS v FROM publication_results");
  }
  if (has("tasks")) {
    parts.push("SELECT created_at AS v FROM tasks",
      "SELECT last_progress_at AS v FROM tasks");
  }
  if (parts.length === 0) parts.push("SELECT CAST(NULL AS TEXT) AS v WHERE 0");
  const out = new Set();
  for (const r of db.query(
    `SELECT DISTINCT v FROM (${parts.join(" UNION ALL ")}) WHERE v IS NOT NULL AND v != ''`)) {
    // serde DateTime emits "Z" where the stored text is "+00:00" —
    // cover both forms.
    if (typeof r.v === "string" && r.v.endsWith("+00:00")) {
      out.add(`${r.v.slice(0, -6)}Z`);
    }
    out.add(r.v);
  }
  return out;
};
const normDeep = (v, set) => {
  if (typeof v === "string" && set.has(v)) return "<norm>";
  if (Array.isArray(v)) return v.map((x) => normDeep(x, set));
  if (v !== null && typeof v === "object") {
    // Digests computed over task-bound/wall-clock content can't be
    // string-matched — norm by field name; content parity is proven by
    // the normalized state digest and the remaining emitted fields.
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k,
      ((k === "evidence_sha256" || k === "audit_sha256") &&
       typeof x === "string" && /^[0-9a-f]{64}$/.test(x))
        ? "<norm>" : normDeep(x, set)]));
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
// Descriptor cells bind runner-local paths/inodes — normalize so the
// cross-runner digest verifies the descriptor pipeline, not the path.
const normDesc = (s) => String(s)
  .replace(/"(database_path|lock_path)":"[^"]*"/g, '"$1":"<p>"')
  .replace(/"(database_identity|lock_identity)":\{[^}]*\}/g,
    '"$1":{"device":0,"inode":0}');
const syms = new Map(); // symbol -> real task_id; "rev:<sym>" -> revision
// $CAP: captured serde values (bound snapshots / inspection facts) —
// audits and bind plans embed full snapshots whose volatile fields
// (task ids, wall-clock stamps) must come from this runner's own reads.
const caps = new Map();
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
    if (v.startsWith("$") && syms.has(v.slice(1))) return syms.get(v.slice(1));
    if (v.includes("$")) {
      // Embedded "$SYM" inside a longer string (e.g. the
      // verification:task:<id> scope key). Longest name first.
      let out = v;
      for (const k of [...syms.keys()].sort((a, b) => b.length - a.length)) {
        if (k.startsWith("rev:")) continue;
        out = out.split(`$${k}`).join(syms.get(k));
      }
      return out;
    }
    return v;
  }
  if (Array.isArray(v)) return v.map(resolve);
  if (v !== null && typeof v === "object") {
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, resolve(x)]));
  }
  return v;
};

const sortedValue = (v) => JSON.parse(ser(v)); // canonical Value (sorted keys)

// verification_execution::prepare_claim — the Claim-plan builder used
// by the runtime-guard claim op (claim_with_runtime_guard is ported).
const prepareClaim = (snap, descriptor, now) => {
  const isSupported = (key) =>
    canonEq(key, VST.VERIFICATION_KEY) || canonEq(key, VST.PUBLICATION_KEY);
  if (!isSupported(snap.task.key)) {
    throw new StorageError("unsupported_contract", "UNSUPPORTED_CONTRACT");
  }
  if (descriptor.protocol !== VRT.PROTOCOL ||
      descriptor.task_id !== snap.task.task_id ||
      descriptor.run_id !== snap.task.accepted_run_id ||
      snap.task.status !== "pending" ||
      snap.task.owner_run_id !== null) {
    throw new StorageError("revision_conflict", "STATE_REVISION_CONFLICT");
  }
  const after = JSON.parse(JSON.stringify(snap));
  // task_control::decide(Claim) — status running, owner=run, rev+1,
  // last_progress_at=now, claims stamped with the new revision.
  after.task.status = "running";
  after.task.owner_run_id = descriptor.run_id;
  after.task.state_revision += 1;
  after.task.last_progress_at = now;
  for (const c of after.claims) c.state_revision = after.task.state_revision;
  return { before: snap, after, descriptor };
};

// serde_json::to_value emits: sorted keys at every level, DateTime<Utc> as
// "Z", NaiveDate as "YYYY-MM-DD". Stored rows carry chrono "+00:00".
const deepZ = (v) =>
  typeof v === "string" && /T\d{2}:\d{2}:\d{2}(\.\d+)?\+00:00$/.test(v)
    ? CMM.serdeDateTime(v)
    : Array.isArray(v) ? v.map(deepZ)
    : v !== null && typeof v === "object"
      ? Object.fromEntries(Object.entries(v).map(([k, x]) => [k, deepZ(x)]))
      : v;
const serdeEmit = (v) => sortedValue(deepZ(v));
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
  {
    // command_digest binds the random task id inside command_json —
    // recompute over the symbolized text like the result digests.
    const normCell = (x) => {
      if (x === null || x === undefined) return "";
      let o = normDesc(String(x)
        .replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(\+00:00|Z)/g, "<TS>"));
      for (const [k, real] of syms) {
        if (!k.startsWith("rev:")) o = o.split(real).join(`$${k}`);
      }
      return o;
    };
    for (const r of db.query("SELECT request_id,command_json,outcome_kind,task_id,terminal_json FROM requests ORDER BY 1")) {
      lines.push(`R:${symOf(r.request_id)}|${sha(normCell(r.command_json))}|${r.outcome_kind ?? ""}|${r.task_id === null ? "" : symOf(r.task_id)}|${r.terminal_json ?? ""}`);
    }
    // Verification registries: *_sha256 cells hash text that binds task
    // ids/timestamps/descriptor paths — emit the sha recomputed over the
    // normalized text so the digest proves the pipeline, not runner bytes.
    for (const [tag, sql] of [
      ["PC", "SELECT verification_task_id AS a,target_task_id AS b,audit_json AS t FROM publication_checks ORDER BY 1"],
      ["SC", "SELECT verification_task_id AS a,target_task_id AS b,audit_json AS t FROM stopped_write_checks ORDER BY 1"],
      ["VI", "SELECT request_id AS a,target_task_id AS b,audit_json AS t FROM verification_inspections ORDER BY 1"],
      ["VG", "SELECT task_id AS a,claim_revision AS b,descriptor_json AS t FROM verification_runtime_guards ORDER BY 1"],
    ]) {
      for (const r of db.query(sql)) {
        const text = normCell(r.t);
        lines.push(`${tag}:${symOf(r.a)}|${normCell(String(r.b ?? ""))}|${text}|${sha(text)}`);
      }
    }
  }
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
    // result_digest binds the runner-local task id — recompute it with
    // the task id symbolized so the digest verifies deterministically.
    const vid = (k) => db.get("SELECT version_id FROM data_versions WHERE version_key=?", k)?.version_id ?? "";
    const hx = (x) => Buffer.from(x, "hex");
    for (const r of db.query("SELECT result_key,result_id,task_id,item_id,base_version_key,result_version_key,outcome,range_start_day,range_end_day,algorithm_version,hex(source_binding_digest) AS sbd,continuity_mode,prefix_bytes,hex(base_projection_digest) AS bpd,hex(result_projection_digest) AS rpd,hex(delta_digest) AS dd,hex(evidence_digest) AS ed,task_revision,hex(result_digest) AS rd0 FROM market_update_results ORDER BY result_key")) {
      // NULL digests (hex() -> "") can't recompute — emit the stored
      // digest verbatim like the Rust arm's fallback.
      let rd = (r.sbd && r.bpd && r.rpd && r.ed) ? null : r.rd0;
      if (rd === null) try {
        rd = Buffer.from(CMM.marketUpdateResultDigest({
          result_id: r.result_id, task_id: symOf(r.task_id), item_id: r.item_id,
          base_version_id: vid(r.base_version_key),
          result_version_id: vid(r.result_version_key),
          outcome: Number(r.outcome),
          range_start_day: Number(r.range_start_day),
          range_end_day: Number(r.range_end_day),
          algorithm_version: r.algorithm_version,
          source_binding_digest: hx(r.sbd), continuity_mode: Number(r.continuity_mode),
          prefix_bytes: r.prefix_bytes === null ? null : Number(r.prefix_bytes),
          base_projection_digest: hx(r.bpd), result_projection_digest: hx(r.rpd),
          delta_digest: (r.dd === null || r.dd === "") ? null : hx(r.dd),
          evidence_digest: hx(r.ed), task_revision: r.task_revision,
        })).toString("hex");
      } catch { rd = "<ERR>"; }
      lines.push(`CMU:${[cvs(r.result_key), cvs(r.result_id), symOf(r.task_id), cvs(r.item_id),
        cvs(r.base_version_key), cvs(r.result_version_key), cvs(r.outcome), cvs(r.range_start_day),
        cvs(r.range_end_day), cvs(r.algorithm_version), cvs(r.sbd), cvs(r.continuity_mode),
        cvs(r.prefix_bytes), cvs(r.bpd), cvs(r.rpd), cvs(r.dd), cvs(r.ed), cvs(r.task_revision),
        rd].join("|")}`);
    }
    cdump("SELECT result_key,seq,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count FROM market_update_delta_clips ORDER BY result_key,seq", "CMD");
    for (const r of db.query("SELECT result_key,publication_id,task_id,item_id,publication_kind,base_version_key,result_version_key,outcome,algorithm_version,hex(source_binding_digest) AS sbd,hex(evidence_digest) AS ed,task_revision,hex(result_digest) AS rd0 FROM publication_results ORDER BY result_key")) {
      let rd = (r.sbd && r.ed) ? null : r.rd0;
      if (rd === null) try {
        rd = Buffer.from(CMM.publicationResultDigest({
          publication_id: r.publication_id, task_id: symOf(r.task_id), item_id: r.item_id,
          publication_kind: Number(r.publication_kind),
          base_version_id: r.base_version_key === null ? null : vid(r.base_version_key),
          result_version_id: vid(r.result_version_key),
          outcome: Number(r.outcome), algorithm_version: r.algorithm_version,
          source_binding_digest: hx(r.sbd), evidence_digest: hx(r.ed),
          task_revision: r.task_revision,
        })).toString("hex");
      } catch { rd = "<ERR>"; }
      lines.push(`CPR:${[cvs(r.result_key), cvs(r.publication_id), symOf(r.task_id), cvs(r.item_id),
        cvs(r.publication_kind), cvs(r.base_version_key), cvs(r.result_version_key), cvs(r.outcome),
        cvs(r.algorithm_version), cvs(r.sbd), cvs(r.ed), cvs(r.task_revision), rd].join("|")}`);
    }
  }
  if (!isCompact()) {
    dump("SELECT result_id,task_id,item_id,base_version_id,result_version_id,no_change,audit_json,result_sha256,receipt_json FROM market_update_results ORDER BY result_id", "MU");
    dump("SELECT result_id,task_id,item_id,base_daily_version_id,result_version_id,no_change,audit_json,result_sha256,receipt_json FROM yearly_execution_results ORDER BY result_id", "YR");
  }
  // Wall-clock columns + the digests binding them are normalized like the
  // compact integrity column: fixed stamp + recompute.
  const TS_FIX = "0000-01-01T00:00:00+00:00";
  const decisionRow = (r) => ({
    decision_id: r.decision_id, symbol: r.symbol,
    period: r.period === "day" ? "Day" : "Year",
    start_date: r.start_date, end_date: r.end_date,
    subject_version_id: r.subject_version_id,
    subject_version_digest: r.subject_version_digest,
    conclusion: r.conclusion, basis_kind: r.basis_kind, basis_ref: r.basis_ref,
    basis_sha256: r.basis_sha256, verifier_id: r.verifier_id,
    decided_at: TS_FIX, decision_sha256: r.decision_sha256 });
  for (const r of db.query("SELECT decision_id,symbol,period,start_date,end_date,subject_version_id,subject_version_digest,conclusion,basis_kind,basis_ref,basis_sha256,verifier_id,decided_at,decision_sha256 FROM gap_verification_decisions ORDER BY decision_id")) {
    lines.push(`GD:${[r.decision_id, r.symbol, r.period, r.start_date, r.end_date,
      r.subject_version_id, r.subject_version_digest, r.conclusion, r.basis_kind,
      r.basis_ref, r.basis_sha256, r.verifier_id, TS_FIX,
      GAPS.decisionContentDigest(decisionRow(r))].join("|")}`);
  }
  for (const r of db.query("SELECT gap_id,symbol,period,start_date,end_date,evidence_kind,evidence_id,evidence_digest,observed_at FROM proven_gaps ORDER BY gap_id")) {
    const d = db.get("SELECT decision_id,symbol,period,start_date,end_date,subject_version_id,subject_version_digest,conclusion,basis_kind,basis_ref,basis_sha256,verifier_id,decided_at,decision_sha256 FROM gap_verification_decisions WHERE decision_id=?", r.evidence_id);
    const evd = d ? GAPS.decisionContentDigest(decisionRow(d)) : r.evidence_digest;
    lines.push(`PG:${[r.gap_id, r.symbol, r.period, r.start_date, r.end_date,
      r.evidence_kind, r.evidence_id, evd, TS_FIX].join("|")}`);
  }
  dump("SELECT gap_id,version_id,'0000-01-01T00:00:00+00:00' FROM gap_coverings ORDER BY gap_id", "GC");
  // (verification registry rows emitted above with recomputed digests)
  // Embedded RFC3339 stamps inside audit/result/terminal blobs bind
  // wall-clock values — normalize before symbolization.
  let joined = lines.join("\n")
    .replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(\+00:00|Z)/g, "<TS>");
  for (const [k, real] of syms) {
    if (!k.startsWith("rev:")) joined = joined.split(real).join(`$${k}`);
  }
  // task ids are random per-runner — sort the symbolized lines so the
  // digest input is order-stable.
  joined = joined.split("\n").sort().join("\n");
  if (process.env.DIFF_DIGEST_DUMP) writeFileSync(process.env.DIFF_DIGEST_DUMP, joined);
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
// market_code + err_code PascalCase for batch per-item errors
const codeMap = (e) => {
  if (e instanceof MarketError) {
    return { integrity: "Integrity", conflict: "Conflict", invalid: "Invalid",
      not_found: "NotFound", base: "Base", storage: "Storage",
      outcome_unknown: "OutcomeUnknown", basis_not_verified: "BasisNotVerified" }[e.code]
      ?? "Storage";
  }
  if (e instanceof StorageError) {
    return { revision_conflict: "RevisionConflict",
      needs_verification: "NeedsVerification",
      invalid_retry_selection: "InvalidRetrySelection",
      conflict: "RequestConflict", range_conflict: "RangeConflict",
      invalid_input: "InvalidCommand", unavailable: "StorageUnavailable",
      task_not_found: "TaskNotFound", outcome_unknown: "OutcomeUnknown",
      unsupported_contract: "UnsupportedContract",
      inconsistent: "Internal" }[e.code] ?? "StorageUnavailable";
  }
  return "Storage";
};
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
          // serde struct order: items then scopes; nested Values
          // (target/frozen_inputs) are BTreeMap-sorted by serde.
          workJson: JSON.stringify({
            items: work.items.map((i) => ({
              item_id: i.item_id, target: sortedValue(i.target),
              progress_unit: i.progress_unit,
              frozen_inputs: sortedValue(i.frozen_inputs),
              scope_keys: sortedArr(i.scope_keys) })),
            scopes: sortedArr(work.scopes),
            ...(work.shared_evidence !== undefined ? { shared_evidence: sortedValue(work.shared_evidence) } : {}),
          }),
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
                // audit embeds wall-clock integrity digests + the random
                // task id — normalize both before hashing the canonical form.
                audit_sha256: sha(ser(normDeep(normalizeIds(r[0]), normSet()))),
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
          const load = (n) => {
            if (n === "@candidate") return CMR.materializeChecked(db, command.version_id, true);
            if (n === "@base") return CMR.materializeChecked(db, p.base_version_id, false);
            return RC.check(JSON.parse(readFileSync(`${dir}/${n}`, "utf8")));
          };
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
          const r = isCompact()
            ? CW.commitRangeTx(db, plan)
            : MC.commitRangeTx(db, plan, op.now ?? "2026-09-24T00:00:00Z");
          rec = { status: "ok",
            receipt: { ...r, task_revision: Number(r.task_revision) },
            digest: isCompact() ? sha(stateDigest()) : marketDigest() };
        } catch (e) { rec = { status: "error", code: e.code ?? "internal", message: String(e.message), unchanged: true }; }
        break;
      }
      // NM-057 — publish/result commit entry points:
      //   commit_publication -> MarketRepository::commit (commit_tx)
      //   commit_yearly      -> YearlyRepository::commit_yearly
      //     (yearly_no_change_tx / commit_tx -> commit_publication)
      case "commit_publication":
      case "commit_yearly": {
        try {
          const p = op.plan;
          const dir = process.env.DIFF_FIXTURES || "scripts/node_migration_p3/records";
          const load = (n, ver) => {
            if (n === "@candidate") return CMR.materializeChecked(db, ver, true);
            if (n === "@base" || n === "@fixed") return CMR.materializeChecked(db, ver, false);
            return RC.check(JSON.parse(readFileSync(`${dir}/${n}`, "utf8")));
          };
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
          const publication = {
            command,
            record: load(p.record, command.version_id),
            required_heads: (p.required_heads ?? []).map((h) =>
              [{ symbol: h[0], period: per(h[1]) }, h[2] == null ? null : resolve(h[2])]),
            promote: p.promote ? { symbol: p.promote.symbol, period: per(p.promote.period) } : null,
            base_digest: p.base_digest ?? null,
          };
          let r;
          if (op.op === "commit_publication") {
            r = isCompact()
              ? CW.commitTx(db, publication)
              : MC.commitPublication(db, publication, op.now ?? "2026-09-24T00:00:00Z");
          } else {
            const base = p.base == null ? null : load(p.base, p.base_version_id);
            if (base === null) throw Object.assign(new Error("MARKET_INTEGRITY: yearly plan base missing"), { code: "inconsistent" });
            const plan = { publication, base, prior: null,
              no_change: !!p.no_change, audit: p.audit ?? {}, audit_payloads: [] };
            if (!isCompact()) throw Object.assign(new Error("commit_yearly not ported on legacy"), { code: "unsupported_contract" });
            r = CW.commitYearly(db, plan);
          }
          rec = { status: "ok",
            receipt: { ...r, task_revision: Number(r.task_revision) },
            digest: isCompact() ? sha(stateDigest()) : marketDigest() };
        } catch (e) { rec = { status: "error", code: e.code ?? "internal", message: String(e.message), unchanged: true }; }
        break;
      }
      case "yearly_receipt":
      case "yearly_item_receipt":
      case "receipt": {
        try {
          let r;
          if (op.op === "yearly_receipt") {
            r = isCompact() ? CW.yearlyReceipt(db, resolve(op.id))
              : YR.yearlyReceipt(db, resolve(op.id));
          } else if (op.op === "yearly_item_receipt") {
            r = isCompact() ? CW.yearlyItemReceipt(db, resolve(op.task_id), op.item_id)
              : YR.yearlyItemReceipt(db, resolve(op.task_id), op.item_id);
          } else {
            r = isCompact() ? CW.receipt(db, resolve(op.id))
              : MC.receipt?.(db, resolve(op.id)) ?? null;
          }
          rec = r === null || r === undefined
            ? { status: "ok", found: false }
            : { status: "ok", found: true,
                receipt: { ...r, task_revision: Number(r.task_revision) } };
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
      case "base_bars_in_range":
      case "commit_range_ops":
      case "commit_range_ops_typed":
      case "commit_range_no_change_typed":
      case "commit_update_batch": {
        try {
          let out;
          if (!isCompact()) {
            // trait/inherent absent on legacy — Ok(None) per op shape
            out = name === "stage_ops_delta" ? { version_key: null }
              : name === "update_base_facts" ? { facts: null }
              : name === "base_bars_in_range" ? { bars: null }
              : name === "commit_update_batch" ? { items: null }
              : { receipt: null };
            rec = { status: "ok", digest: sha(stateDigest()), ...out };
            break;
          }
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
          } else if (name === "commit_range_ops" || name === "commit_range_ops_typed") {
            {
              const command = op.command;
              const scope = { symbol: op.scope.symbol,
                period: op.scope.period === "year" ? "Year" : "Day" };
              const r = name === "commit_range_ops"
                ? CW.commitRangeOps(db, command, scope, op.base_version_id,
                    op.upserts ?? [], op.removes ?? [], op.expected ?? [])
                : CW.commitRangeOpsTyped(db, command, scope, op.base_version_id,
                    op.upserts ?? [], op.removes ?? [],
                    op.continuity_evidence ?? null);
              out = { receipt: r === null ? null : {
                publication_id: r.publication_id, version_id: r.version_id,
                task_id: r.task_id, task_revision: Number(r.task_revision) } };
            }
          } else if (name === "commit_range_no_change_typed") {
            {
              const r = CW.commitRangeNoChangeTyped(db, op.command,
                op.base_version_id, op.range, op.continuity_evidence ?? null);
              out = { receipt: r === null ? null : {
                publication_id: r.publication_id, version_id: r.version_id,
                task_id: r.task_id, task_revision: Number(r.task_revision) } };
            }
          } else if (name === "commit_update_batch") {
            {
              const outs = CW.commitUpdateBatch(db, op.task_id, op.items);
              out = { items: outs.map((x) => x.ok
                ? { ok: { publication_id: x.receipt.publication_id,
                    version_id: x.receipt.version_id, task_id: x.receipt.task_id,
                    task_revision: Number(x.receipt.task_revision) } }
                : { err: `${codeMap(x.error)}:${x.error.message}` }) };
            }
          } else {
            const bars = CW.baseBarsInRange(db, op.base_version_id, op.start, op.end);
            // serde Bar: sorted keys, Decimal keeps its scale ("10.60").
            const decWire = (v) => v !== null && typeof v === "object" && typeof v.m === "bigint"
              ? DEC.toString(v) : v;
            out = { bars: bars.map((b) => ({
              amount: b.amount === null ? null : decWire(b.amount),
              close: decWire(b.close), date: b.date,
              high: decWire(b.high), low: decWire(b.low),
              open: decWire(b.open),
              r7_raw: b.r7_raw === null || b.r7_raw === undefined ? null : Number(b.r7_raw),
              volume: b.volume === null ? null : decWire(b.volume),
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
            decision: serdeEmit(GAPS.gapVerificationDecision(db, op.decision_id)) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "proven_gap": {
        try {
          rec = { status: "ok", gap: serdeEmit(GAPS.provenGap(db, op.gap_id)) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "open_proven_gaps": {
        try {
          rec = { status: "ok", gaps: serdeEmit(GAPS.openProvenGaps(
            db, op.symbol, op.period === "year" ? "Year" : "Day")) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "gap_coverings": {
        try {
          rec = { status: "ok", coverings: serdeEmit(GAPS.gapCoverings(db, op.gap_id)) };
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
      case "capture": {
        try {
          const name = op.as;
          const kind = op.kind ?? "snapshot";
          const id = resolve(op.task);
          if (kind === "snapshot") {
            const s = repo._readBoundTaskValidated(id);
            if (!s) throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" });
            caps.set(name, VST.snapshotCanon(s));
          } else if (kind === "inspection_facts") {
            caps.set(name, VI.inspectionFacts(db, id,
              (x) => repo._readBoundTaskValidated(x)));
          } else if (kind === "stop_plan") {
            const readBound = (x) => repo._readBoundTaskValidated(x);
            const v = readBound(resolve(op.verification));
            if (!v) throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" });
            const t = readBound(resolve(op.target));
            if (!t) throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" });
            const o = op.observation;
            const obs = o === undefined ? null
              : o.kind === "insufficient" ? { kind: "insufficient", reason: o.reason }
              : { kind: "stopped",
                  snapshot_sha256: VST.snapshotDigest(t),
                  evidence_ref: o.evidence_ref };
            const plan = VST.prepareStop(VST.snapshotCanon(v), VST.snapshotCanon(t),
              op.run, op.actor, op.now, obs);
            caps.set(name, plan.audit);
          } else if (kind === "check_plan") {
            const readBound = (x) => repo._readBoundTaskValidated(x);
            const check = readBound(resolve(op.verification));
            if (!check) throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" });
            const facts = VF.readFacts(db, resolve(op.target), readBound);
            facts.task = VST.snapshotCanon(facts.task);
            const plan = PC.prepareCheck(VST.snapshotCanon(check), facts,
              op.run, op.actor, op.now);
            caps.set(name, plan.audit);
          } else throw new Error(`capture kind ${kind}`);
          rec = { status: "ok" };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "commit_stop_check": {
        try {
          const readBound = (x) => repo._readBoundTaskValidated(x);
          if (op.audit_cap !== undefined) {
            const a = JSON.parse(JSON.stringify(caps.get(op.audit_cap)));
            for (const [path, val] of Object.entries(op.tweak ?? {})) {
              const parts = path.split(".");
              let cur = a;
              for (const pp of parts.slice(0, -1)) cur = cur[pp];
              cur[parts[parts.length - 1]] = val;
            }
            const receipt = VST.commitStopCheck(db, { audit: a }, readBound);
            rec = { status: "ok", receipt: sortedValue(receipt), digest: stateDigest() };
            break;
          }
          const vid = resolve(op.verification);
          const tid = resolve(op.target);
          const v = readBound(vid);
          if (!v) throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" });
          const t = readBound(tid);
          if (!t) throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" });
          const o = op.observation;
          const obs = o === undefined ? null
            : o.kind === "insufficient" ? { kind: "insufficient", reason: o.reason }
            : { kind: "stopped",
                snapshot_sha256: VST.snapshotDigest(t),
                evidence_ref: o.evidence_ref };
          const plan = VST.prepareStop(VST.snapshotCanon(v), VST.snapshotCanon(t),
            op.run, op.actor, op.now, obs);
          const receipt = VST.commitStopCheck(db, plan, readBound);
          rec = { status: "ok", receipt: sortedValue(receipt), digest: stateDigest() };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "stop_receipt": {
        try {
          const r = VST.stopReceipt(db, resolve(op.id),
            (x) => repo._readBoundTaskValidated(x));
          rec = { status: "ok", receipt: r === null ? null : sortedValue(r) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "bind_existing_stop": {
        try {
          const readBound = (x) => repo._readBoundTaskValidated(x);
          const target = op.target.cap !== undefined
            ? caps.get(op.target.cap)
            : VST.snapshotCanon(readBound(resolve(op.target.task)) ??
              (() => { throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" }); })());
          const acc = VST.bindExistingStop(db, {
            request_id: op.request_id, command: op.command,
            target, result: op.result,
          }, readBound,
            (x) => VF.readFacts(db, resolve(x),
              (y) => repo._readBoundTaskValidated(resolve(y))));
          rec = { status: "ok", reused: acc.reused,
            outcome_kind: "Task" in acc.record.outcome ? "task" : "terminal",
            digest: stateDigest() };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "commit_check": {
        try {
          const readBound = (x) => repo._readBoundTaskValidated(x);
          const readFactsFn = (x) => VF.readFacts(db, x, readBound);
          if (op.audit_cap !== undefined) {
            const a = JSON.parse(JSON.stringify(caps.get(op.audit_cap)));
            for (const [path, val] of Object.entries(op.tweak ?? {})) {
              const parts = path.split(".");
              let cur = a;
              for (const pp of parts.slice(0, -1)) cur = cur[pp];
              cur[parts[parts.length - 1]] = val;
            }
            const receipt = PC.commitCheck(db, { audit: a }, readBound, readFactsFn);
            rec = { status: "ok", receipt: sortedValue(receipt), digest: stateDigest() };
            break;
          }
          const vid = resolve(op.verification);
          const tid = resolve(op.target);
          const check = readBound(vid);
          if (!check) throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" });
          const facts = readFactsFn(tid);
          facts.task = VST.snapshotCanon(facts.task);
          const plan = PC.prepareCheck(VST.snapshotCanon(check), facts,
            op.run, op.actor, op.now);
          const receipt = PC.commitCheck(db, plan, readBound, readFactsFn);
          rec = { status: "ok", receipt: sortedValue(receipt), digest: stateDigest() };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "known_publication": {
        try {
          const readBound = (x) => repo._readBoundTaskValidated(x);
          const r = PC.knownPublication(db, resolve(op.target), readBound,
            (x) => VF.readFacts(db, x, readBound));
          rec = { status: "ok", receipt: r === null ? null : sortedValue(r) };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "bind_known_publication": {
        try {
          const readBound = (x) => repo._readBoundTaskValidated(x);
          const readFactsFn = (x) => VF.readFacts(db, x, readBound);
          const target = op.target.cap !== undefined
            ? caps.get(op.target.cap)
            : VST.snapshotCanon(readBound(resolve(op.target.task)) ??
              (() => { throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" }); })());
          const acc = PC.bindKnownPublication(db, {
            request_id: op.request_id, command: op.command,
            target, result: op.result,
          }, readBound, readFactsFn);
          rec = { status: "ok", reused: acc.reused,
            outcome_kind: "Task" in acc.record.outcome ? "task" : "terminal",
            digest: stateDigest() };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "inspection_facts": {
        try {
          const f = VI.inspectionFacts(db, resolve(op.id),
            (x) => repo._readBoundTaskValidated(x));
          const fv = JSON.parse(JSON.stringify(f));
          if (typeof fv.completion_digest === "string") {
            fv.completion_digest = "<digest>";
          }
          if (fv.registration?.descriptor) {
            fv.registration.descriptor.database_path = "<db>";
            fv.registration.descriptor.database_identity = "<id>";
            fv.registration.descriptor.lock_identity = "<id>";
            fv.registration.descriptor.lock_path = "<lock>";
          }
          rec = { status: "ok", facts: fv };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "probe_execution": {
        try {
          const f = VI.inspectionFacts(db, resolve(op.id),
            (x) => repo._readBoundTaskValidated(x));
          const p = VI.probeExecution(dbPath, f);
          const g = p.guard;
          if (g) g.close(); // Rust drops the probe guard at op end
          rec = { status: "ok", observation: p.observation,
            guard: g !== null && g !== undefined };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "commit_inspection": {
        try {
          const readBound = (x) => repo._readBoundTaskValidated(x);
          const readFactsFn = (x) => VF.readFacts(db, x, readBound);
          const tid = resolve(op.target);
          const facts = op.facts_cap !== undefined
            ? caps.get(op.facts_cap)
            : VI.inspectionFacts(db, tid, readBound);
          const probe = op.observation === "@probe"
            ? VI.probeExecution(dbPath, facts)
            : { observation: op.observation, guard: null };
          const command = { key: VST.INSPECTION_KEY,
            intent: { target_task_id: tid } };
          const plan = op.audit_cap !== undefined
            ? { audit: caps.get(op.audit_cap) }
            : VI.prepareInspection(op.request_id, command, facts,
              probe.observation, op.run, op.actor, op.now);
          const acc = VI.commitInspection(db, dbPath, plan, probe,
            readBound, readFactsFn);
          if (probe.guard) probe.guard.close();
          rec = { status: "ok", reused: acc.reused,
            outcome_kind: "Task" in acc.record.outcome ? "task" : "terminal",
            digest: stateDigest() };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "claim_runtime": {
        try {
          const id = resolve(op.task);
          const t = repo._readBoundTaskValidated(id);
          if (!t) throw Object.assign(new Error("TASK_NOT_FOUND"), { code: "task_not_found" });
          const guard = VRT.create(dbPath, id, op.run);
          try {
            const plan = prepareClaim(VST.snapshotCanon(t), guard.descriptor, op.now);
            const snap = VST.claimWithRuntimeGuard(db, repo, plan, guard, dbPath);
            rec = { status: "ok", snapshot: VST.snapshotCanon(snap) };
          } finally { guard.close(); }
        } catch (e) {
          const rtMap = (x) => x instanceof VRT.RuntimeGuardError
            ? (x.kind === "io"
              ? { code: "unavailable", message: `STORAGE_UNAVAILABLE: ${x.message}` }
              : { code: "needs_verification",
                  message: `ACCEPTANCE_NEEDS_VERIFICATION: ${x.message}` })
            : null;
          const mapped = rtMap(e);
          rec = mapped === null
            ? { status: "error", code: e.code ?? "internal",
                message: String(e.message), unchanged: stateDigest() === before }
            : { status: "error", code: mapped.code,
                message: mapped.message, unchanged: stateDigest() === before };
        }
        break;
      }
      case "never_started_facts": {
        try {
          const f = VST.neverStartedFacts(db, resolve(op.id),
            (x) => repo._readBoundTaskValidated(x));
          rec = { status: "ok", facts: f };
        } catch (e) {
          rec = { status: "error", code: e.code ?? "internal",
            message: String(e.message), unchanged: stateDigest() === before };
        }
        break;
      }
      case "determinate_facts": {
        try {
          const f = VST.determinateFacts(db, resolve(op.id),
            (x) => repo._readBoundTaskValidated(x));
          rec = { status: "ok", facts: f };
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
          rec = { status: "ok", receipt: r === null ? null : sortedValue(r[1]) };
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
  let out2 = JSON.stringify(NORM_TS ? normDeep(rec, normSet()) : rec);
  // symbolize emitted task ids — task-<hex> differs across runners and
  // the comparator byte-compares every emitted field.
  for (const [k, real] of syms) {
    if (!k.startsWith("rev:")) out2 = out2.split(real).join(`$${k}`);
  }
  results.push(JSON.parse(out2));
}
repo.close();
writeFileSync(outPath, results.map((r) => JSON.stringify(r)).join("\n") + "\n");
