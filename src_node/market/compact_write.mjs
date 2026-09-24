// NM-055 — compact (schema-19) write path: stage_ops_delta,
// validate_staged, commit_ops_update / commit_ops_no_change,
// update_base_facts, base_bars_in_range, and the compact_results
// result-row surface (commit_market_update_tx + readers).
// Ports src/sqlite_market/compact_repo.rs (ops entry points) and
// src/sqlite_market/compact_results.rs verbatim.
import { MarketError, legacyDigest as _legacyDigest, __decNorm } from "./record_codec.mjs";
import * as CM from "./compact_market.mjs";
import * as CR from "./compact_read.mjs";
import { __readItemBinding as readItemBinding } from "./market_read.mjs";
import * as TR from "../storage/task_repository.mjs";
import { StorageError } from "../storage/sqlite_adapter.mjs";
import { canonEq } from "../storage/task_repository.mjs";

const integrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const notFound = () => new MarketError("not_found", "MARKET_NOT_FOUND");
const conflict = (m) => new MarketError("conflict", `PUBLISH_PRECONDITION: ${m}`);
const baseErr = (m) => new MarketError("base", `BASE_UNAVAILABLE: ${m}`);
const taskErr = (code, message) => Object.assign(new Error(message), { code });
const invalidCmd = (m) => new MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);

// .map_err(super::integrity) on compact_market::Error — same wrap as
// compact_read (full "compact market data invalid: ..." carried over).
const wrap = (f) => {
  try { return f(); } catch (e) {
    if (e instanceof MarketError && e.kind === "invalid" &&
        e.message.startsWith("compact market data invalid: ")) {
      throw integrity(e.message);
    }
    throw e;
  }
};

const UPDATE_ALGORITHM = "local-range-update-v1";
const COLLECTION_ALGORITHM = "local-collection-v1";
const OUTCOME_LIMIT = 192;
const MAX_DEPTH = 256;

const TERMINAL = new Set(
  ["succeeded", "partially_failed", "failed", "cancelled", "interrupted"]);
// task_control::visible_status — non-terminal + foreign run/owner
const visibleStatus = (head, run) =>
  !TERMINAL.has(head.status) &&
    (head.accepted_run_id !== run ||
     (head.owner_run_id !== null && head.owner_run_id !== run))
    ? "needs_verification" : head.status;

// rusqlite constraint-violation -> Conflict("result uniqueness violated")
const conflictOrStorage = (e) => {
  const m = String(e.message ?? e);
  if (/constraint/i.test(m) || /unique/i.test(m) || e.code === "ERR_SQLITE_ERROR" &&
      /constraint|unique/i.test(m)) {
    return conflict("result uniqueness violated");
  }
  return new MarketError("storage", `MARKET_STORAGE: ${m}`);
};

const storageOf = (e) => new MarketError("storage", `MARKET_STORAGE: ${e.message ?? e}`);

// ---------- helpers ----------
const registryKind = (db, resultId) => {
  const row = db.get("SELECT result_kind FROM result_registry WHERE result_id=?", resultId);
  return row ? row.result_kind : null;
};

const periodCode = (p) => p === "Day" ? 1 : 2;

const baseCount = (db, key) => {
  const row = db.get("SELECT record_count FROM data_versions WHERE version_key=?", key);
  if (!row) throw new MarketError("storage", "MARKET_STORAGE: query returned no rows");
  return row.record_count;
};

// ops_continuity_json — verbatim key set: mode/base_version_id/
// source_binding_digest, then proof/basis_ref/revision_ref copied from
// the continuity evidence, then optional prefix_bytes.
const opsContinuityJson = (mode, baseVersionId, sourceBindingDigest, prefixBytes, evidence) => {
  const continuity = {
    mode,
    base_version_id: baseVersionId,
    source_binding_digest: CM.digestHex(sourceBindingDigest),
  };
  if (evidence !== null && evidence !== undefined) {
    if (evidence.continuity !== undefined) continuity.proof = evidence.continuity;
    if (evidence.same_qfq_basis_ref !== undefined) {
      continuity.basis_ref = evidence.same_qfq_basis_ref;
    }
    if (evidence.revision_ref !== undefined) {
      continuity.revision_ref = evidence.revision_ref;
    }
  }
  if (prefixBytes !== null && prefixBytes !== undefined) {
    continuity.prefix_bytes = Number(prefixBytes); // json!(u64) — integer value
  }
  return continuity;
};

// result_outcome_json — JCS canonical, <=192 bytes
const resultOutcomeJson = (resultId, resultKind) => {
  const canonical = CM.canonicalJson({
    error: null,
    evidence: { result_id: resultId, result_kind: resultKind },
    success: true,
  });
  if (canonical.length > OUTCOME_LIMIT) {
    throw integrity("outcome_json exceeds byte limit");
  }
  return canonical;
};

// ---------- plan_market_update ----------
const requireValidatedResult = (command, result, base) => {
  if (result.key === base.key) throw integrity("published update reuses base version");
  if (result.state !== "validated") {
    throw integrity("update result version is not validated");
  }
  if (result.entity !== base.entity || result.period !== base.period) {
    throw integrity("update result scope mismatch");
  }
  if (!Buffer.from(result.source_binding_digest).equals(command.source_binding_digest)) {
    throw integrity("update source binding mismatch");
  }
  if (command.published_at === null || command.published_at === undefined) {
    throw integrity("published update lacks published_at");
  }
};

const codecOps = (ops) => ops.map((op) => ({
  range_start_day: op.range_start_day, range_end_day: op.range_end_day,
  kind: op.kind.kind === "upsert" ? { kind: "upsert", clip: op.kind.clip }
    : { kind: "remove" },
}));

const planMarketUpdate = (db, command, base) => {
  const change = command.change;
  if (change.kind === "no_change") {
    if (command.result_version_key !== base.key) {
      throw integrity("no-change update result differs from base");
    }
    if (command.published_at !== null && command.published_at !== undefined ||
        command.head.kind !== "keep") {
      throw integrity("no-change update carries publish effects");
    }
    return {
      result: base, outcome: 1 /* NoChange */, continuity: 1 /* Unchanged */,
      prefix_bytes: change.prefix_bytes, delta_digest: null, append_ops: [],
    };
  }
  if (change.kind === "append") {
    const ops = change.ops;
    if (ops.length === 0) throw integrity("append update requires ops");
    for (const op of ops) {
      if (op.kind.kind !== "upsert") throw integrity("append update op is not an upsert");
      if (op.range_start_day <= base.last_day) {
        throw integrity("append update op is not tail-only");
      }
    }
    const delta = wrap(() => CM.deltaDigest(codecOps(ops)));
    const result = CR.resultFacts(db, command.result_version_key);
    requireValidatedResult(command, result, base);
    if (result.manifest_mode === "delta") {
      if (result.parent_version_key !== base.key) {
        throw integrity("append delta parent mismatch");
      }
      const [persisted, storedDelta, projection] =
        readDeltaOps(db, result.key, base.projection_digest);
      if (!deltaOpsEq(persisted, ops) ||
          !Buffer.from(storedDelta).equals(delta) ||
          !Buffer.from(projection).equals(result.projection_digest)) {
        throw integrity("append delta ops mismatch persisted");
      }
    } else {
      // checkpoint
      if (base.delta_depth !== MAX_DEPTH) {
        throw integrity("append checkpoint rollover requires depth-256 base");
      }
    }
    return {
      result, outcome: 2 /* Published */, continuity: 2 /* Append */,
      prefix_bytes: change.prefix_bytes, delta_digest: delta,
      append_ops: ops,
    };
  }
  // full
  const result = CR.resultFacts(db, command.result_version_key);
  requireValidatedResult(command, result, base);
  if (result.manifest_mode !== "checkpoint" || result.parent_version_key !== null ||
      result.delta_depth !== 0) {
    throw integrity("full revision result is not a root checkpoint");
  }
  const delta = CM.fullRevisionDigest(base.projection_digest, result.projection_digest);
  return {
    result, outcome: 2, continuity: 3 /* Full */, prefix_bytes: null,
    delta_digest: delta, append_ops: [],
  };
};

// StoredDelta equality — ops are {range_start_day, range_end_day,
// kind:{kind,segment_key?,clip?}} on both sides.
const deltaOpsEq = (a, b) => {
  if (a.length !== b.length) return false;
  return a.every((x, i) => {
    const y = b[i];
    if (x.range_start_day !== y.range_start_day || x.range_end_day !== y.range_end_day ||
        x.kind.kind !== y.kind.kind) return false;
    if (x.kind.kind === "upsert") {
      const xa = x.kind.clip, ya = y.kind.clip;
      return Number(x.kind.segment_key) === Number(y.kind.segment_key) &&
        Buffer.from(xa.segment_digest).equals(ya.segment_digest) &&
        xa.clip_start_day === ya.clip_start_day &&
        xa.clip_end_day === ya.clip_end_day && xa.row_count === ya.row_count;
    }
    return true;
  });
};

// read_delta via compact_read's internal — re-exported for reuse here.
const readDeltaOps = (db, key, parentProjection) => CR.__readDelta(db, key, parentProjection);

const registerResult = (db, resultId, kind) => {
  if (resultId === "") throw integrity("result id is empty");
  const exists = db.get(
    "SELECT EXISTS(SELECT 1 FROM result_registry WHERE result_id=?) AS x", resultId).x === 1n;
  if (exists) throw conflict("result id already consumed");
  try {
    db.run("INSERT INTO result_registry(result_id,result_kind) VALUES(?,?)", resultId, kind);
  } catch (e) { throw conflictOrStorage(e); }
};

const requireItemUnused = (db, taskId, itemId) => {
  const exists = db.get(
    "SELECT EXISTS(SELECT 1 FROM market_update_results WHERE task_id=? AND item_id=?) OR EXISTS(SELECT 1 FROM publication_results WHERE task_id=? AND item_id=?) AS x",
    taskId, itemId, taskId, itemId).x === 1n;
  if (exists) throw conflict("task item already has a result");
};

const checkRequiredHeads = (db, heads) => {
  for (const h of heads) {
    const row = db.get(
      "SELECT version_key FROM effective_heads WHERE entity=? AND period=? AND adjustment=1",
      h.entity, periodCode(h.period));
    const current = row ? row.version_key : null;
    const expected = h.expected === null || h.expected === undefined
      ? null : BigInt(h.expected);
    if (current !== expected) throw conflict("effective head changed");
  }
};

const checkBinding = (db, taskId, itemId, check) => {
  let binding;
  try { binding = readItemBinding(db, taskId, itemId); }
  catch (e) { throw taskErr(e.code ?? "storage_unavailable", e.message); }
  if (!binding) throw taskErr("task_not_found", "TASK_NOT_FOUND");
  if (visibleStatus(binding.head, check.run_id) !== "running" ||
      binding.head.owner_run_id !== check.run_id) {
    throw conflict("task owner/state");
  }
  // serde Value equality on the stored target — key order agnostic
  const t = binding.item.spec.target;
  const targetsEqual = t !== null && typeof t === "object" && !Array.isArray(t) &&
    t.symbol === check.entity &&
    t.period === (check.period === "Day" ? "day" : "year");
  const spec = binding.item.spec;
  if (binding.item.stage !== "running" || spec.item_id !== itemId || !targetsEqual ||
      !spec.scope_keys.includes(check.occupancy_key) || !binding.scopes_bound) {
    throw conflict("task target/occupancy");
  }
};

const applyHead = (db, head, entity, period, baseKey, resultKey, allowMissing) => {
  const pc = periodCode(period);
  if (head.kind === "keep") return;
  if (head.kind === "missing") {
    if (!allowMissing) throw conflict("update cannot create a head");
    const changed = db.run(
      "INSERT INTO effective_heads(entity,period,adjustment,version_key) SELECT ?,?,1,? WHERE NOT EXISTS(SELECT 1 FROM effective_heads WHERE entity=? AND period=? AND adjustment=1)",
      entity, pc, resultKey, entity, pc).changes;
    if (changed !== 1) throw conflict("effective head already exists");
    return;
  }
  // HeadExpectation::Version
  if (baseKey !== head.version) {
    throw conflict("head expectation does not match base");
  }
  const changed = db.run(
    "UPDATE effective_heads SET version_key=? WHERE entity=? AND period=? AND adjustment=1 AND version_key=?",
    resultKey, entity, pc, head.version).changes;
  if (changed !== 1) throw conflict("effective head does not match expectation");
};

// finish_item_and_task — verbatim SQL: item finish CAS, task counters +
// terminal status CAS, terminal result_json + occupancy release.
const finishItemAndTask = (db, taskId, itemId, expectedRevision, newRevision, outcomeJson) => {
  let changed = db.run(
    "UPDATE task_items SET stage='finished',outcome_json=? WHERE task_id=? AND item_id=? AND stage='running' AND outcome_json IS NULL",
    outcomeJson, taskId, itemId).changes;
  if (changed !== 1) throw conflict("task item is not running");
  changed = db.run(
    "UPDATE tasks SET state_revision=?,items_running=items_running-1,items_finished=items_finished+1,items_succeeded=items_succeeded+1,status=CASE WHEN items_finished+1=item_count THEN CASE WHEN items_succeeded+1=item_count THEN 'succeeded' ELSE 'partially_failed' END ELSE status END WHERE task_id=? AND status='running' AND state_revision=? AND items_running>0",
    newRevision, taskId, expectedRevision).changes;
  if (changed !== 1) throw conflict("task state does not match expectation");
  const t = db.get(
    "SELECT status IN ('succeeded','partially_failed','failed','cancelled','interrupted') AS term, items_finished AS f, items_succeeded AS s FROM tasks WHERE task_id=?",
    taskId);
  if (t.term === 1n) {
    const result = JSON.stringify(
      { finished_items: Number(t.f), successful_items: Number(t.s) });
    const w = db.run(
      "UPDATE tasks SET result_json=? WHERE task_id=? AND result_json IS NULL",
      result, taskId).changes;
    if (w !== 1) throw integrity("terminal task already carries a result");
    db.run("DELETE FROM scope_occupancy WHERE task_id=?", taskId);
  }
};

// ---------- commit_market_update_tx ----------
export const commitMarketUpdateTx = (db, command) => {
  wrap(() => CM.epochDate(command.range_start_day));
  wrap(() => CM.epochDate(command.range_end_day));
  if (command.range_start_day > command.range_end_day) {
    throw integrity("update range inverted");
  }
  const expectedRevisionI64 = BigInt(command.expected_task_revision);
  if (expectedRevisionI64 > 9223372036854775807n || expectedRevisionI64 < 0n) {
    throw integrity("task revision out of range");
  }
  const taskRevision = expectedRevisionI64 + 1n; // checked_add
  if (taskRevision > 9223372036854775807n) throw integrity("task revision out of range");
  const outcomeJson = resultOutcomeJson(command.result_id, "market_update");
  let prefixI64 = null;
  if (command.change.kind !== "full") {
    prefixI64 = BigInt(command.change.prefix_bytes);
    if (prefixI64 > 9223372036854775807n || prefixI64 < 0n) {
      throw integrity("prefix_bytes out of range");
    }
  }
  const base = CR.resultFacts(db, command.base_version_key);
  if (base.state !== "published") throw integrity("update base version is not published");
  CR.verifiedSourceBinding(db, command.source_binding_digest);
  if (CR.verifiedEvidenceKind(db, command.evidence_digest) !== "continuity") {
    throw integrity("market update evidence is not continuity");
  }
  const plan = planMarketUpdate(db, command, base);
  registerResult(db, command.result_id, 1);
  requireItemUnused(db, command.task_id, command.item_id);
  checkRequiredHeads(db, command.required_heads);
  checkBinding(db, command.task_id, command.item_id, command.binding);
  if (plan.outcome === 2 /* Published */) {
    if (command.published_at === null || command.published_at === undefined) {
      throw integrity("published update lacks published_at");
    }
    if (CR.publishDelta(db, plan.result.key, command.published_at) === null) {
      CR.publish(db, plan.result.key, command.published_at);
      CR.verifyLeafProjection(db, plan.result.key);
      if (plan.continuity === 2 && plan.result.manifest_mode === "checkpoint") {
        const baseCompact = CR.leafBars(db, base.key);
        const applied = CR.applyStoredDelta(
          db, baseCompact, plan.append_ops, plan.result.entity, plan.result.period);
        const actual = CR.leafBars(db, plan.result.key);
        if (!compactBarsEq(applied, actual)) {
          throw integrity("checkpoint append does not match base plus ops");
        }
      }
    }
  }
  const identity = {
    result_id: command.result_id, task_id: command.task_id, item_id: command.item_id,
    base_version_id: base.version_id, result_version_id: plan.result.version_id,
    outcome: plan.outcome,
    range_start_day: command.range_start_day, range_end_day: command.range_end_day,
    algorithm_version: command.algorithm_version,
    source_binding_digest: command.source_binding_digest,
    continuity_mode: plan.continuity, prefix_bytes: plan.prefix_bytes === null ||
      plan.prefix_bytes === undefined ? null : BigInt(plan.prefix_bytes),
    base_projection_digest: base.projection_digest,
    result_projection_digest: plan.result.projection_digest,
    delta_digest: plan.delta_digest,
    evidence_digest: command.evidence_digest,
    task_revision: taskRevision,
  };
  const resultDigest = wrap(() => CM.marketUpdateResultDigest(identity));
  let r;
  try {
    r = db.run(
      "INSERT INTO market_update_results(result_id,task_id,item_id,base_version_key,result_version_key,outcome,range_start_day,range_end_day,algorithm_version,source_binding_digest,continuity_mode,prefix_bytes,base_projection_digest,result_projection_digest,delta_digest,evidence_digest,task_revision,result_digest) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
      command.result_id, command.task_id, command.item_id, base.key, plan.result.key,
      plan.outcome, command.range_start_day, command.range_end_day, command.algorithm_version,
      command.source_binding_digest, plan.continuity, prefixI64, base.projection_digest,
      plan.result.projection_digest,
      plan.delta_digest === null ? null : plan.delta_digest,
      command.evidence_digest, taskRevision, resultDigest);
  } catch (e) { throw conflictOrStorage(e); }
  const resultKey = r.lastInsertRowid;
  plan.append_ops.forEach((op, seq) => {
    if (op.kind.kind !== "upsert") throw integrity("append result op is not an upsert");
    const stored = op.kind;
    db.run(
      "INSERT INTO market_update_delta_clips(result_key,seq,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count) VALUES(?,?,?,?,?,?,?,?)",
      resultKey, BigInt(seq), op.range_start_day, op.range_end_day, stored.segment_key,
      stored.clip.clip_start_day, stored.clip.clip_end_day, BigInt(stored.clip.row_count));
  });
  if (plan.outcome === 2) {
    try {
      db.run(
        "INSERT INTO task_publications(task_id,version_key,result_id) VALUES(?,?,?)",
        command.task_id, plan.result.key, command.result_id);
    } catch (e) { throw conflictOrStorage(e); }
    applyHead(db, command.head, plan.result.entity, plan.result.period, base.key,
      plan.result.key, false);
  }
  finishItemAndTask(db, command.task_id, command.item_id,
    expectedRevisionI64, taskRevision, outcomeJson);
  return {
    result_key: resultKey, base_version_key: base.key,
    result_version_key: plan.result.key, identity, result_digest: resultDigest,
  };
};

const compactBarsEq = (a, b) => a.length === b.length && a.every((x, i) => {
  const y = b[i];
  return x.trade_day === y.trade_day && x.open_cents === y.open_cents &&
    x.high_cents === y.high_cents && x.low_cents === y.low_cents &&
    x.close_cents === y.close_cents &&
    (x.volume === null ? y.volume === null : y.volume !== null && x.volume === y.volume) &&
    (x.amount === null ? y.amount === null : y.amount !== null && x.amount === y.amount) &&
    ((x.r7_raw === null || x.r7_raw === undefined)
      ? (y.r7_raw === null || y.r7_raw === undefined)
      : y.r7_raw !== null && y.r7_raw !== undefined && BigInt(x.r7_raw) === BigInt(y.r7_raw));
});

// ---------- compact_v19 / compact_versions write helpers ----------

const rejectExistingVersionRows = (db, table, key) => {
  const count = db.get(`SELECT count(*) AS c FROM ${table} WHERE version_key=?`, key).c;
  if (count !== 0n) throw integrity("version clip rows already exist");
};

// insert_segment — content-addressed dedup + collision check
export const insertSegment = (db, entity, period, bars) => {
  const digest = wrap(() => CM.segmentDigest(entity, period, bars));
  const existing = db.get(
    "SELECT segment_key FROM bar_segments WHERE segment_digest=?", digest);
  if (existing) {
    const stored = CR.__readSegment(db, existing.segment_key);
    if (stored.entity !== entity || stored.period !== period ||
        !compactBarsEq(stored.bars, bars)) {
      throw integrity("segment digest collision with different content");
    }
    return stored;
  }
  const r = db.run(
    "INSERT INTO bar_segments(segment_digest,entity,period,first_day,last_day,row_count) VALUES(?,?,?,?,?,?)",
    digest, entity, periodCode(period), bars[0].trade_day,
    bars[bars.length - 1].trade_day, BigInt(bars.length));
  const key = r.lastInsertRowid;
  const stmt = "INSERT INTO segment_bars(segment_key,trade_day,open_cents,high_cents,low_cents,close_cents,volume,amount,r7_raw) VALUES(?,?,?,?,?,?,?,?,?)";
  for (const b of bars) {
    db.run(stmt, key, b.trade_day, b.open_cents, b.high_cents, b.low_cents,
      b.close_cents, b.volume, b.amount,
      b.r7_raw === null || b.r7_raw === undefined ? null : BigInt(b.r7_raw));
  }
  return { key, digest, entity, period, bars: bars.slice() };
};

// resolve_clips — re-verify each clip against its stored segment
const resolveClips = (db, entity, period, clips) => {
  const resolved = [];
  for (const stored of clips) {
    const segment = CR.__readSegment(db, stored.segment_key);
    if (!Buffer.from(stored.clip.segment_digest).equals(segment.digest)) {
      throw integrity("clip segment digest mismatch");
    }
    CR.__checkClipAgainstSegment(segment, stored.clip, entity, period);
    resolved.push(stored.clip);
  }
  return resolved;
};

const writeCheckpoint = (db, key, entity, period, clips) => {
  const resolved = resolveClips(db, entity, period, clips);
  const digest = wrap(() => CM.checkpointProjectionDigest(entity, period, resolved));
  rejectExistingVersionRows(db, "version_checkpoint_clips", key);
  clips.forEach((stored, seq) => {
    db.run(
      "INSERT INTO version_checkpoint_clips(version_key,seq,segment_key,clip_start_day,clip_end_day,row_count) VALUES(?,?,?,?,?,?)",
      key, BigInt(seq), stored.segment_key, stored.clip.clip_start_day,
      stored.clip.clip_end_day, BigInt(stored.clip.row_count));
  });
  return digest;
};

const deltaOpsPlain = (ops) => ops.map((op) => ({
  range_start_day: op.range_start_day, range_end_day: op.range_end_day,
  kind: op.kind.kind === "upsert" ? { kind: "upsert", clip: op.kind.clip }
    : { kind: "remove" },
}));

const writeDelta = (db, key, parentProjection, ops) => {
  const plain = [];
  for (const op of ops) {
    if (op.kind.kind === "upsert") {
      const segment = CR.__readSegment(db, op.kind.segment_key);
      CR.__checkClipAgainstSegment(segment, op.kind.clip, segment.entity, segment.period);
      plain.push({ range_start_day: op.range_start_day, range_end_day: op.range_end_day,
        kind: { kind: "upsert", clip: op.kind.clip } });
    } else {
      plain.push({ range_start_day: op.range_start_day, range_end_day: op.range_end_day,
        kind: { kind: "remove" } });
    }
  }
  const [delta, projection] = wrap(() => CM.deltaProjectionDigest(parentProjection, plain));
  rejectExistingVersionRows(db, "version_delta_clips", key);
  ops.forEach((op, seq) => {
    const isUpsert = op.kind.kind === "upsert";
    db.run(
      "INSERT INTO version_delta_clips(version_key,seq,delta_kind,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count) VALUES(?,?,?,?,?,?,?,?,?)",
      key, BigInt(seq), isUpsert ? 1 : 2, op.range_start_day, op.range_end_day,
      isUpsert ? op.kind.segment_key : null,
      isUpsert ? op.kind.clip.clip_start_day : null,
      isUpsert ? op.kind.clip.clip_end_day : null,
      isUpsert ? BigInt(op.kind.clip.row_count) : 0n);
  });
  return [delta, projection];
};

// put_evidence — canonical-evidence insert or collision check
export const putEvidence = (db, kind, value) => {
  const bundle = wrap(() => CM.canonicalEvidence(kind, value));
  const existing = db.get(
    "SELECT evidence_kind,canonical_json FROM evidence_bundles WHERE evidence_digest=?",
    bundle.digest);
  if (existing) {
    const storedKind = CR.__wrap(() => CM.parseEvidenceKind(existing.evidence_kind));
    wrap(() => CM.verifyEvidence(storedKind, existing.canonical_json, bundle.digest));
    if (storedKind !== kind || existing.canonical_json !== bundle.canonical_json) {
      throw integrity("evidence digest collision with different content");
    }
    return bundle.digest;
  }
  db.run(
    "INSERT INTO evidence_bundles(evidence_digest,evidence_kind,canonical_json) VALUES(?,?,?)",
    bundle.digest, kind, bundle.canonical_json);
  return bundle.digest;
};

export const putSourceBinding = (db, binding) => {
  const digest = wrap(() => CM.sourceBindingDigest(binding));
  const [kind] = CR.__readEvidence(db, binding.evidence_digest);
  if (kind !== "generation") throw integrity("source binding evidence is not generation");
  const existing = db.get(
    "SELECT generation_id,generation_manifest_sha256,day_member_sha256,security_event_sha256,evidence_digest FROM source_bindings WHERE source_binding_digest=?",
    digest);
  if (existing) {
    const stored = {
      generation_id: existing.generation_id,
      generation_manifest_sha256: CR.__fixedDigest(existing.generation_manifest_sha256),
      day_member_sha256: CR.__fixedDigest(existing.day_member_sha256),
      security_event_sha256: CR.__fixedDigest(existing.security_event_sha256),
      evidence_digest: CR.__fixedDigest(existing.evidence_digest),
    };
    if (!wrap(() => CM.sourceBindingDigest(stored)).equals(digest)) {
      throw integrity("stored source binding digest mismatch");
    }
    if (stored.generation_id !== binding.generation_id ||
        !stored.generation_manifest_sha256.equals(binding.generation_manifest_sha256) ||
        !stored.day_member_sha256.equals(binding.day_member_sha256) ||
        !stored.security_event_sha256.equals(binding.security_event_sha256) ||
        !stored.evidence_digest.equals(binding.evidence_digest)) {
      throw integrity("source binding digest collision with different content");
    }
    return digest;
  }
  db.run(
    "INSERT INTO source_bindings(source_binding_digest,generation_id,generation_manifest_sha256,day_member_sha256,security_event_sha256,evidence_digest) VALUES(?,?,?,?,?,?)",
    digest, binding.generation_id, binding.generation_manifest_sha256,
    binding.day_member_sha256, binding.security_event_sha256, binding.evidence_digest);
  return digest;
};

// ---------- draft checks + row writers ----------
const checkDraftStatic = (db, draft) => {
  if (draft.version_id === "") throw integrity("version_id is empty");
  CR.__wrap(() => CM.canonicalEntity(draft.entity));
  try { CM.canonicalRfc3339(draft.created_at); }
  catch { throw integrity("created_at is not canonical UTC RFC 3339"); }
  const yearly = draft.period === "Year";
  if ((draft.period === "Day" && draft.yearly_lineage !== null &&
       draft.yearly_lineage !== undefined) ||
      (yearly && (draft.yearly_lineage === null || draft.yearly_lineage === undefined))) {
    throw integrity("draft period/lineage mismatch");
  }
  const exists = db.get(
    "SELECT EXISTS(SELECT 1 FROM data_versions WHERE version_id=?) AS x",
    draft.version_id).x === 1n;
  if (exists) throw conflict("version_id already exists");
};

const checkDraftEvidence = (db, draft, bars) => {
  const evidence = CR.__verifyVersionEvidence(db, draft.evidence_digest);
  CR.__readSourceBinding(db, draft.source_binding_digest);
  CR.__checkBarsUnits(evidence, bars);
  return evidence;
};

const checkDraftLineage = (db, draft, bars) => {
  if (draft.period !== "Year") return;
  const lineage = draft.yearly_lineage;
  if (lineage === null || lineage === undefined) {
    throw integrity("yearly draft lacks lineage");
  }
  CR.__verifyLineage(db, draft.entity, draft.period, draft.source_binding_digest, bars, lineage);
};

const insertVersionRowStats = (db, draft, parentKey, manifestMode, deltaDepth,
                               recordCount, firstDay, lastDay, projection) => {
  const coverage = draft.coverage_status === "Complete" ? 1
    : draft.coverage_status === "Incomplete" ? 2 : 3;
  const r = db.run(
    "INSERT INTO data_versions(version_id,entity,period,adjustment,state,source_binding_digest,parent_version_key,manifest_mode,delta_depth,record_count,first_day,last_day,coverage_status,evidence_digest,projection_digest,integrity_digest,digest_format,created_at,published_at) VALUES(?,?,?,1,1,?,?,?,?,?,?,?,?,?,?,NULL,?,?,NULL)",
    draft.version_id, draft.entity, periodCode(draft.period),
    draft.source_binding_digest, parentKey, manifestMode, deltaDepth,
    recordCount, firstDay, lastDay, coverage, draft.evidence_digest,
    projection, "compact-market-version-v1", draft.created_at);
  return r.lastInsertRowid;
};

const insertVersionRow = (db, draft, parentKey, manifestMode, deltaDepth, bars, projection) =>
  insertVersionRowStats(db, draft, parentKey, manifestMode, deltaDepth,
    BigInt(bars.length), bars.length ? bars[0].trade_day : 0n,
    bars.length ? bars[bars.length - 1].trade_day : 0n, projection);

const insertLineageRow = (db, key, lineage) => {
  db.run(
    "INSERT INTO yearly_lineage(version_key,base_daily_version_key,algorithm_version,first_trade_day,last_trade_day,as_of_day) VALUES(?,?,?,?,?,?)",
    key, lineage.base_daily_version_key, lineage.algorithm_version,
    lineage.first_trade_day, lineage.last_trade_day, lineage.as_of_day);
};

// stage_checkpoint — flatten + optional basis verify + row + clips
export const stageCheckpoint = (db, draft, clips, basis) => {
  checkDraftStatic(db, draft);
  const bars = CR.__flattenClips(db, draft.entity, draft.period, clips);
  const plain = clips.map((s) => s.clip);
  const projection = wrap(() => CM.checkpointProjectionDigest(draft.entity, draft.period, plain));
  if (basis) {
    if (basis.delta_ops.length === 0) {
      throw integrity("checkpoint basis requires at least one delta op");
    }
    const [parent, parentBars] = CR.__reconstructLeaf(db, basis.parent_version_key);
    CR.__verifyPublishedNode(db, parent, parentBars);
    if (parent.entity !== draft.entity || parent.period !== draft.period) {
      throw integrity("checkpoint basis parent entity/period mismatch");
    }
    if (parent.delta_depth !== BigInt(MAX_DEPTH)) {
      throw integrity("checkpoint basis parent is not at delta depth bound");
    }
    const applied = CR.__applyDelta(db, parentBars, basis.delta_ops, draft.entity, draft.period);
    if (!compactBarsEq(applied, bars)) {
      throw integrity("checkpoint does not equal parent plus basis delta");
    }
  }
  checkDraftLineage(db, draft, bars);
  checkDraftEvidence(db, draft, bars);
  const key = insertVersionRow(db, draft, null, 1, 0n, bars, projection);
  if (draft.yearly_lineage) insertLineageRow(db, key, draft.yearly_lineage);
  const written = writeCheckpoint(db, key, draft.entity, draft.period, clips);
  if (!Buffer.from(written).equals(projection)) {
    throw integrity("persisted checkpoint digest mismatch");
  }
  return key;
};

// stage_delta — full projection path
export const stageDelta = (db, draft, parentKey, ops) => {
  checkDraftStatic(db, draft);
  if (ops.length === 0) throw integrity("delta version requires at least one op");
  const [parent, parentBars] = CR.__reconstructLeaf(db, parentKey);
  CR.__verifyPublishedNode(db, parent, parentBars);
  if (parent.entity !== draft.entity || parent.period !== draft.period) {
    throw integrity("delta parent entity/period mismatch");
  }
  const parentDepth = BigInt(parent.delta_depth);
  if (parentDepth >= BigInt(MAX_DEPTH)) {
    throw integrity("delta depth bound reached");
  }
  const bars = CR.__applyDelta(db, parentBars, ops, draft.entity, draft.period);
  if (bars.length === 0) throw integrity("delta projection is empty");
  const plain = deltaOpsPlain(ops);
  const [delta, projection] = wrap(() =>
    CM.deltaProjectionDigest(parent.projection_digest, plain));
  checkDraftLineage(db, draft, bars);
  checkDraftEvidence(db, draft, bars);
  const key = insertVersionRow(db, draft, parentKey, 2, parentDepth + 1n, bars, projection);
  if (draft.yearly_lineage) insertLineageRow(db, key, draft.yearly_lineage);
  const [writtenDelta, writtenProjection] =
    writeDelta(db, key, parent.projection_digest, ops);
  if (!Buffer.from(writtenDelta).equals(delta) ||
      !Buffer.from(writtenProjection).equals(projection)) {
    throw integrity("persisted delta digest mismatch");
  }
  return key;
};

// stage_delta_append — O(ops) append-only path, no parent leaf replay
export const stageDeltaAppend = (db, draft, parentKey, ops, newBars) => {
  checkDraftStatic(db, draft);
  if (ops.length === 0) throw integrity("delta version requires at least one op");
  if (draft.period !== "Day") throw integrity("append fast path is daily-only");
  const parent = CR.__readVersionRow(db, parentKey);
  if (parent === null) throw integrity("delta parent version row missing");
  CR.__verifyPublishedFields(db, parent);
  if (parent.entity !== draft.entity || parent.period !== draft.period) {
    throw integrity("delta parent entity/period mismatch");
  }
  const parentDepth = BigInt(parent.delta_depth);
  if (parentDepth >= BigInt(MAX_DEPTH)) {
    throw integrity("delta depth bound reached");
  }
  let added = 0n;
  let prevEnd = parent.last_day;
  for (const op of ops) {
    if (op.kind.kind !== "upsert") {
      throw integrity("append delta carries a non-upsert op");
    }
    const stored = op.kind;
    if (op.range_start_day <= prevEnd || op.range_start_day > op.range_end_day ||
        op.range_start_day !== stored.clip.clip_start_day ||
        op.range_end_day !== stored.clip.clip_end_day) {
      throw integrity("append ops overlap the base or each other");
    }
    const segment = CR.__readSegment(db, stored.segment_key);
    CR.__checkClipAgainstSegment(segment, stored.clip, draft.entity, draft.period);
    added += BigInt(stored.clip.row_count);
    prevEnd = op.range_end_day;
  }
  const plain = deltaOpsPlain(ops);
  const [delta, projection] = wrap(() =>
    CM.deltaProjectionDigest(parent.projection_digest, plain));
  checkDraftEvidence(db, draft, newBars);
  const key = insertVersionRowStats(db, draft, parentKey, 2, parentDepth + 1n,
    parent.record_count + added, parent.first_day, prevEnd, projection);
  const [writtenDelta, writtenProjection] =
    writeDelta(db, key, parent.projection_digest, ops);
  if (!Buffer.from(writtenDelta).equals(delta) ||
      !Buffer.from(writtenProjection).equals(projection)) {
    throw integrity("persisted delta digest mismatch");
  }
  return key;
};

// ---------- ops-first entry points (compact_repo) ----------

// stage_ops_delta_tx body (caller owns the transaction)
export const stageOpsDeltaTx = (db, entity, baseVersionId, versionId, upserts, removes) => {
  const baseKey = CR.versionKey(db, baseVersionId);
  const baseFacts = CR.resultFacts(db, baseKey);
  if (baseFacts.state !== "published") {
    throw integrity("ops-delta base version is not published");
  }
  if (baseFacts.entity !== entity || baseFacts.period !== "Day") {
    throw integrity("ops-delta base entity/period mismatch");
  }
  const cov = db.get(
    "SELECT coverage_status, evidence_digest FROM data_versions WHERE version_key=?", baseKey);
  const coverageStatus = cov.coverage_status === 1n ? "Complete"
    : cov.coverage_status === 2n ? "Incomplete" : "Unknown";
  const bars = upserts.map((b) => wrap(() => CM.compactBarFromBar(b, "Day")));
  bars.sort((a, b) => a.trade_day < b.trade_day ? -1 : a.trade_day > b.trade_day ? 1 : 0);
  for (let i = 0; i + 1 < bars.length; i++) {
    if (bars[i].trade_day >= bars[i + 1].trade_day) {
      throw integrity("ops-delta upserts are not strictly increasing");
    }
  }
  const ops = [];
  for (let i = 0; i < bars.length; i += CM.MAX_SEGMENT_BARS) {
    const chunk = bars.slice(i, i + CM.MAX_SEGMENT_BARS);
    const segment = insertSegment(db, entity, "Day", chunk);
    ops.push({
      range_start_day: chunk[0].trade_day,
      range_end_day: chunk[chunk.length - 1].trade_day,
      kind: { kind: "upsert", segment_key: segment.key,
        clip: { segment_digest: segment.digest,
          clip_start_day: chunk[0].trade_day,
          clip_end_day: chunk[chunk.length - 1].trade_day,
          row_count: chunk.length } },
    });
  }
  for (const day of removes) {
    const tradeDay = wrap(() => CM.epochDay(day));
    ops.push({ range_start_day: tradeDay, range_end_day: tradeDay, kind: { kind: "remove" } });
  }
  ops.sort((a, b) => a.range_start_day < b.range_start_day ? -1
    : a.range_start_day > b.range_start_day ? 1 : 0);
  const draft = {
    version_id: versionId, entity, period: "Day",
    source_binding_digest: baseFacts.source_binding_digest,
    coverage_status: coverageStatus,
    evidence_digest: cov.evidence_digest,
    created_at: CM.nowRfc3339(),
    yearly_lineage: null,
  };
  const appendOnly = bars.length > 0 && removes.length === 0 &&
    bars[0].trade_day > baseFacts.last_day;
  return appendOnly
    ? stageDeltaAppend(db, draft, baseKey, ops, bars)
    : stageDelta(db, draft, baseKey, ops);
};

// stage_ops_delta — BEGIN IMMEDIATE transaction
export const stageOpsDelta = (db, entity, baseVersionId, versionId, upserts, removes) => {
  db.exec("BEGIN IMMEDIATE");
  try {
    const key = stageOpsDeltaTx(db, entity, baseVersionId, versionId, upserts, removes);
    db.exec("COMMIT");
    return key;
  } catch (e) {
    try { db.exec("ROLLBACK"); } catch {}
    throw e;
  }
};

// validate_staged — mark_validated_delta
export const validateStaged = (db, versionId) => {
  db.exec("BEGIN IMMEDIATE");
  try {
    CR.markValidatedDelta(db, CR.versionKey(db, versionId));
    db.exec("COMMIT");
  } catch (e) {
    try { db.exec("ROLLBACK"); } catch {}
    throw e;
  }
};

// commit_ops_update_tx
export const commitOpsUpdateTx = (db, { result_id, task_id, item_id,
    base_version_id, result_version_id, expected_revision, run_id,
    occupancy_key, continuity_evidence }) => {
  const kind = registryKind(db, result_id);
  if (kind !== null) {
    if (kind !== 1n) throw conflict("result identity conflict");
    const stored = readMarketUpdate(db, result_id);
    if (stored.identity.task_id !== task_id || stored.identity.item_id !== item_id ||
        stored.identity.result_version_id !== result_version_id) {
      throw conflict("result identity conflict");
    }
    return {
      publication_id: result_id, version_id: stored.identity.result_version_id,
      task_id, task_revision: stored.identity.task_revision,
    };
  }
  const baseKey = CR.versionKey(db, base_version_id);
  const resultKey = CR.versionKey(db, result_version_id);
  const facts = CR.resultFacts(db, resultKey);
  if (facts.state !== "validated") throw conflict("candidate is not validated");
  let change, rangeStart, rangeEnd;
  if (facts.manifest_mode === "delta") {
    if (facts.parent_version_key !== baseKey) {
      throw conflict("update result delta is not based on the expected head");
    }
    const parent = CR.resultFacts(db, baseKey);
    const [ops] = CR.__readDelta(db, resultKey, parent.projection_digest);
    change = { kind: "append", prefix_bytes: baseCount(db, baseKey) * 32n, ops };
    rangeStart = ops.length ? ops[0].range_start_day : 0n;
    rangeEnd = ops.length ? ops[ops.length - 1].range_end_day : 0n;
  } else {
    change = { kind: "full" };
    rangeStart = facts.first_day;
    rangeEnd = facts.last_day;
  }
  const continuity = opsContinuityJson(
    change.kind === "append" ? "append" : "full", base_version_id,
    facts.source_binding_digest,
    change.kind === "append" ? change.prefix_bytes : null,
    continuity_evidence);
  const evidence = putEvidence(db, "continuity", continuity);
  const entity = facts.entity;
  const stored = commitMarketUpdateTx(db, {
    result_id, task_id, item_id,
    base_version_key: baseKey, result_version_key: resultKey,
    range_start_day: rangeStart, range_end_day: rangeEnd,
    algorithm_version: UPDATE_ALGORITHM,
    source_binding_digest: facts.source_binding_digest,
    evidence_digest: evidence,
    expected_task_revision: BigInt(expected_revision),
    published_at: CM.nowRfc3339(),
    head: { kind: "version", version: baseKey },
    required_heads: [{ entity, period: "Day", expected: baseKey }],
    binding: { run_id, entity, period: "Day", occupancy_key },
    change,
  });
  return {
    publication_id: result_id, version_id: stored.identity.result_version_id,
    task_id, task_revision: stored.identity.task_revision,
  };
};

// commit_ops_no_change_tx
export const commitOpsNoChangeTx = (db, { result_id, task_id, item_id,
    base_version_id, expected_revision, run_id, occupancy_key,
    range_start_day, range_end_day, continuity_evidence }) => {
  const kind = registryKind(db, result_id);
  if (kind !== null) {
    if (kind !== 1n) throw conflict("result identity conflict");
    const stored = readMarketUpdate(db, result_id);
    if (stored.identity.task_id !== task_id || stored.identity.item_id !== item_id ||
        stored.identity.result_version_id !== base_version_id) {
      throw conflict("result identity conflict");
    }
    return {
      publication_id: result_id, version_id: stored.identity.result_version_id,
      task_id, task_revision: stored.identity.task_revision,
    };
  }
  const baseKey = CR.versionKey(db, base_version_id);
  const facts = CR.resultFacts(db, baseKey);
  if (facts.state !== "published") throw integrity("no-change base is not published");
  const prefixBytes = facts.record_count * 32n;
  const continuity = opsContinuityJson(
    "unchanged", base_version_id, facts.source_binding_digest, prefixBytes,
    continuity_evidence);
  const evidence = putEvidence(db, "continuity", continuity);
  const stored = commitMarketUpdateTx(db, {
    result_id, task_id, item_id,
    base_version_key: baseKey, result_version_key: baseKey,
    range_start_day: BigInt(range_start_day), range_end_day: BigInt(range_end_day),
    algorithm_version: UPDATE_ALGORITHM,
    source_binding_digest: facts.source_binding_digest,
    evidence_digest: evidence,
    expected_task_revision: BigInt(expected_revision),
    published_at: null,
    head: { kind: "keep" },
    required_heads: [{ entity: facts.entity, period: "Day", expected: baseKey }],
    binding: { run_id, entity: facts.entity, period: "Day", occupancy_key },
    change: { kind: "no_change", prefix_bytes: prefixBytes },
  });
  return {
    publication_id: result_id, version_id: stored.identity.result_version_id,
    task_id, task_revision: stored.identity.task_revision,
  };
};

// commit_ops_update/commit_ops_no_change run under self.write() in Rust —
// the coordinator's write() executes the closure in AUTOCOMMIT (no enclosing
// transaction): mid-failure partial writes persist (evidence bundles, the
// registry row), matching the oracle's semantics verbatim. The batch path
// supplies its own atomicity boundary; these ops do not.
export const commitOpsUpdate = (db, args) => commitOpsUpdateTx(db, args);
export const commitOpsNoChange = (db, args) => commitOpsNoChangeTx(db, args);

// update_base_facts — typed facts, no bar materialization
export const updateBaseFacts = (db, baseVersionId) => {
  const key = CR.versionKey(db, baseVersionId);
  const facts = CR.resultFacts(db, key);
  if (facts.state !== "published") throw integrity("ops base version is not published");
  if (facts.period !== "Day") throw integrity("ops base is not a daily version");
  const binding = CR.verifiedSourceBinding(db, facts.source_binding_digest);
  const [kind, evidenceJson] = CR.__readEvidence(db, facts.evidence_digest);
  if (kind !== "version") throw integrity("base version evidence kind mismatch");
  let evidence;
  try { evidence = JSON.parse(evidenceJson); }
  catch (e) { throw integrity(`version evidence malformed: ${e.message}`); }
  if (facts.integrity_digest === null) {
    throw integrity("published base lacks integrity digest");
  }
  const coverageStatus = facts.coverage_status === 1 ? "Complete"
    : facts.coverage_status === 2 ? "Incomplete" : "Unknown";
  const recordCount = Number(facts.record_count);
  if (recordCount < 0 || recordCount > 4294967295 || !Number.isSafeInteger(recordCount)) {
    throw integrity("base record_count out of range");
  }
  return {
    version_id: facts.version_id, symbol: facts.entity, record_count: recordCount,
    first_day: wrap(() => CM.epochDate(facts.first_day)),
    last_day: wrap(() => CM.epochDate(facts.last_day)),
    integrity_sha256: CM.digestHex(facts.integrity_digest),
    coverage: { status: coverageStatus, reason: evidence.coverage_reason },
    unit_currency: evidence.unit_currency,
    source: evidence.source,
    generation_id: binding.generation_id,
    generation_manifest_sha256: CM.digestHex(binding.generation_manifest_sha256),
    day_member_sha256: CM.digestHex(binding.day_member_sha256),
    security_event_sha256: CM.digestHex(binding.security_event_sha256),
  };
};

// base_bars_in_range — windowed leaf read → serde Bar shape
export const baseBarsInRange = (db, baseVersionId, start, end) => {
  const key = CR.versionKey(db, baseVersionId);
  const bars = CR.leafBarsInRange(db, key, CM.epochDay(start), CM.epochDay(end));
  return bars.map((b) => wrap(() => CM.compactBarToBar(b, "Day")));
};

// ---------- compact_results readers ----------
const OUTCOME_CODES = { 1: "no_change", 2: "published" };
const CONTINUITY_CODES = { 1: "unchanged", 2: "append", 3: "full" };

const fixedDigestBytes = (raw) => {
  if (raw === null || raw === undefined || Buffer.from(raw).length !== 32) {
    throw integrity("stored digest length mismatch");
  }
  return Buffer.from(raw);
};

const verifyTaskItem = (db, taskId, itemId, resultId, resultKind, resultRevision) => {
  const item = db.get(
    "SELECT stage,outcome_json FROM task_items WHERE task_id=? AND item_id=?",
    taskId, itemId);
  if (!item) throw integrity("task item missing");
  const expected = resultOutcomeJson(resultId, resultKind);
  if (item.stage !== "finished" || item.outcome_json !== expected) {
    throw integrity("task item outcome mismatch");
  }
  const row = db.get("SELECT state_revision FROM tasks WHERE task_id=?", taskId);
  if (!row) throw integrity("task missing");
  if (row.state_revision < resultRevision) {
    throw integrity("task revision regressed");
  }
};

const verifyTaskPublication = (db, taskId, versionKey, resultId, published) => {
  if (published) {
    const c = db.get(
      "SELECT count(*) AS c FROM task_publications WHERE task_id=? AND version_key=? AND result_id=?",
      taskId, versionKey, resultId).c;
    if (c !== 1n) throw integrity("task publication row missing");
  } else {
    const c = db.get(
      "SELECT count(*) AS c FROM task_publications WHERE result_id=?", resultId).c;
    if (c !== 0n) throw integrity("no-change result carries task publication");
  }
};

const readUpdateDeltaClips = (db, resultKey) => {
  const rows = db.query(
    "SELECT seq,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count FROM market_update_delta_clips WHERE result_key=? ORDER BY seq",
    resultKey);
  return rows.map((r) => ({
    range_start_day: r.range_start_day, range_end_day: r.range_end_day,
    kind: { kind: "upsert", segment_key: r.segment_key,
      clip: { segment_digest: CR.__readSegment(db, r.segment_key).digest,
        clip_start_day: r.clip_start_day, clip_end_day: r.clip_end_day,
        row_count: Number(r.row_count) } },
  }));
};

// read_market_update — stored row verified verbatim
export const readMarketUpdate = (db, resultId) => {
  const kind = registryKind(db, resultId);
  if (kind === null) throw notFound();
  if (kind !== 1n) throw integrity("registry kind mismatch");
  const row = db.get(
    "SELECT result_key,task_id,item_id,base_version_key,result_version_key,outcome,range_start_day,range_end_day,algorithm_version,source_binding_digest,continuity_mode,prefix_bytes,base_projection_digest,result_projection_digest,delta_digest,evidence_digest,task_revision,result_digest FROM market_update_results WHERE result_id=?",
    resultId);
  if (!row) throw integrity("market update result row missing");
  const outcome = OUTCOME_CODES[Number(row.outcome)];
  if (!outcome) throw integrity("unknown outcome");
  const continuity = CONTINUITY_CODES[Number(row.continuity_mode)];
  if (!continuity) throw integrity("unknown continuity_mode");
  const prefixBytes = row.prefix_bytes === null ? null
    : (row.prefix_bytes >= 0n ? row.prefix_bytes
      : (() => { throw integrity("negative prefix_bytes"); })());
  const baseProjection = fixedDigestBytes(row.base_projection_digest);
  const resultProjection = fixedDigestBytes(row.result_projection_digest);
  const storedDelta = row.delta_digest === null ? null
    : fixedDigestBytes(row.delta_digest);
  const taskRevision = row.task_revision;
  const epochOk = (d) => {
    try { CM.epochDate(d); return true; } catch { return false; }
  };
  if (taskRevision <= 0n || row.range_start_day > row.range_end_day ||
      !epochOk(row.range_start_day) || !epochOk(row.range_end_day)) {
    throw integrity("market update result metadata invalid");
  }
  const outcomeCode = outcome === "no_change" ? 1 : 2;
  const continuityCode = { unchanged: 1, append: 2, full: 3 }[continuity];
  const noChangeCombo = outcomeCode === 1 && continuityCode === 1 &&
    row.base_version_key === row.result_version_key &&
    storedDelta === null && prefixBytes !== null;
  const publishedCombo = outcomeCode === 2 && (continuityCode === 2 || continuityCode === 3) &&
    row.base_version_key !== row.result_version_key && storedDelta !== null;
  if (!noChangeCombo && !publishedCombo) {
    throw integrity("market update result combination invalid");
  }
  if (continuityCode === 3 && prefixBytes !== null) {
    throw integrity("full update result carries prefix_bytes");
  }
  const [baseFacts, baseFixed] = CR.requirePublishedFacts(db, row.base_version_key);
  const [resultFacts, resultFixed] = CR.requirePublishedFacts(db, row.result_version_key);
  if (!baseProjection.equals(baseFacts.projection_digest) ||
      !resultProjection.equals(resultFacts.projection_digest)) {
    throw integrity("update result projection linkage mismatch");
  }
  if (baseFacts.entity !== resultFacts.entity || baseFacts.period !== resultFacts.period) {
    throw integrity("update result scope mismatch");
  }
  const sourceBinding = fixedDigestBytes(row.source_binding_digest);
  CR.verifiedSourceBinding(db, sourceBinding);
  if (outcomeCode === 2 &&
      !resultFacts.source_binding_digest.equals(sourceBinding)) {
    throw integrity("update result source binding mismatch");
  }
  const evidenceDigest = fixedDigestBytes(row.evidence_digest);
  if (CR.verifiedEvidenceKind(db, evidenceDigest) !== "continuity") {
    throw integrity("market update evidence kind mismatch");
  }
  const ops = readUpdateDeltaClips(db, row.result_key);
  let deltaDigest;
  if (continuityCode === 1) {
    if (ops.length !== 0) throw integrity("no-change update carries delta clips");
    deltaDigest = null;
  } else if (continuityCode === 2) {
    if (ops.length === 0) throw integrity("append update lacks delta clips");
    for (const op of ops) {
      if (op.range_start_day <= baseFacts.last_day) {
        throw integrity("append update op is not tail-only");
      }
    }
    const opsDigest = wrap(() => CM.deltaDigest(codecOps(ops)));
    if (resultFacts.manifest_mode === "delta") {
      if (resultFacts.parent_version_key !== baseFacts.key) {
        throw integrity("append delta parent mismatch");
      }
      const [persisted, persistedDelta, projection] =
        CR.__readDelta(db, resultFacts.key, baseFacts.projection_digest);
      if (!deltaOpsEq(persisted, ops) || !persistedDelta.equals(opsDigest) ||
          !projection.equals(resultFacts.projection_digest)) {
        throw integrity("append delta ops mismatch persisted");
      }
    } else {
      if (baseFacts.delta_depth !== MAX_DEPTH) {
        throw integrity("append checkpoint rollover requires depth-256 base");
      }
      const applied = CR.applyStoredDelta(
        db, fixedToCompactBars(baseFixed), ops, resultFacts.entity, resultFacts.period);
      if (!compactBarsEq(applied, fixedToCompactBars(resultFixed))) {
        throw integrity("checkpoint append does not match base plus ops");
      }
    }
    deltaDigest = opsDigest;
  } else {
    // full
    if (ops.length !== 0) throw integrity("full update carries delta clips");
    if (resultFacts.manifest_mode !== "checkpoint" ||
        resultFacts.parent_version_key !== null || resultFacts.delta_depth !== 0) {
      throw integrity("full revision result is not a root checkpoint");
    }
    deltaDigest = CM.fullRevisionDigest(baseFacts.projection_digest, resultFacts.projection_digest);
  }
  if ((deltaDigest === null) !== (storedDelta === null) ||
      (deltaDigest !== null && !deltaDigest.equals(storedDelta))) {
    throw integrity("update delta digest mismatch");
  }
  const identity = {
    result_id: resultId, task_id: row.task_id, item_id: row.item_id,
    base_version_id: baseFacts.version_id, result_version_id: resultFacts.version_id,
    outcome: outcomeCode,
    range_start_day: row.range_start_day, range_end_day: row.range_end_day,
    algorithm_version: row.algorithm_version,
    source_binding_digest: sourceBinding,
    continuity_mode: continuityCode, prefix_bytes: prefixBytes,
    base_projection_digest: baseProjection, result_projection_digest: resultProjection,
    delta_digest: deltaDigest,
    evidence_digest: evidenceDigest,
    task_revision: taskRevision,
  };
  if (!wrap(() => CM.marketUpdateResultDigest(identity)).equals(fixedDigestBytes(row.result_digest))) {
    throw integrity("update result digest mismatch");
  }
  verifyTaskPublication(db, row.task_id, resultFacts.key, resultId, outcomeCode === 2);
  verifyTaskItem(db, row.task_id, row.item_id, resultId, "market_update", taskRevision);
  return {
    result_key: row.result_key, base_version_key: row.base_version_key,
    result_version_key: row.result_version_key, identity,
    result_digest: fixedDigestBytes(row.result_digest),
  };
};

// verified FixedVersion -> CompactBar[] (compact_bars)
const fixedToCompactBars = (verified) => verified.version.records.map((b) =>
  wrap(() => CM.compactBarFromBar(b, verified.version.period)));

// read_publication — publication_results read side (yearly/collection)
export const readPublication = (db, publicationId) => {
  const kind = registryKind(db, publicationId);
  if (kind === null) throw notFound();
  if (kind !== 2n) throw integrity("registry kind mismatch");
  const row = db.get(
    "SELECT result_key,task_id,item_id,publication_kind,base_version_key,result_version_key,outcome,algorithm_version,source_binding_digest,evidence_digest,task_revision,result_digest FROM publication_results WHERE publication_id=?",
    publicationId);
  if (!row) throw integrity("publication result row missing");
  const publicationKind = { 1: "collection", 2: "yearly", 3: "republish" }[Number(row.publication_kind)];
  if (!publicationKind) throw integrity("unknown publication_kind");
  const outcome = OUTCOME_CODES[Number(row.outcome)];
  if (!outcome) throw integrity("unknown outcome");
  const taskRevision = row.task_revision;
  if (taskRevision <= 0n) throw integrity("publication task revision invalid");
  const outcomeCode = outcome === "no_change" ? 1 : 2;
  const baseKey = row.base_version_key;
  const comboOk = (outcomeCode === 1 && baseKey !== null && baseKey === row.result_version_key) ||
    (outcomeCode === 2 && (baseKey === null || baseKey !== row.result_version_key));
  if (!comboOk) throw integrity("publication result combination invalid");
  const [resultFacts] = CR.requirePublishedFacts(db, row.result_version_key);
  if (publicationKind === "collection" && resultFacts.period !== "Day") {
    throw integrity("collection publication result is not daily");
  }
  if (publicationKind === "yearly" && resultFacts.period !== "Year") {
    throw integrity("yearly publication result is not yearly");
  }
  const expectedEvidence = publicationKind === "yearly" ? "yearly" : "version";
  const evidenceDigest = fixedDigestBytes(row.evidence_digest);
  if (CR.verifiedEvidenceKind(db, evidenceDigest) !== expectedEvidence) {
    throw integrity("publication evidence kind mismatch");
  }
  const sourceBinding = fixedDigestBytes(row.source_binding_digest);
  CR.verifiedSourceBinding(db, sourceBinding);
  if (outcomeCode === 2 && !resultFacts.source_binding_digest.equals(sourceBinding)) {
    throw integrity("publication result source binding mismatch");
  }
  let baseFacts = null;
  if (baseKey !== null) {
    const [facts] = CR.requirePublishedFacts(db, baseKey);
    if (facts.entity !== resultFacts.entity || facts.period !== resultFacts.period) {
      throw integrity("publication base scope mismatch");
    }
    baseFacts = facts;
  }
  const identity = {
    publication_id: publicationId, task_id: row.task_id, item_id: row.item_id,
    publication_kind: { collection: 1, yearly: 2, republish: 3 }[publicationKind],
    base_version_id: baseFacts === null ? null : baseFacts.version_id,
    result_version_id: resultFacts.version_id,
    outcome: outcomeCode, algorithm_version: row.algorithm_version,
    source_binding_digest: sourceBinding, evidence_digest: evidenceDigest,
    task_revision: taskRevision,
  };
  if (!wrap(() => CM.publicationResultDigest(identity)).equals(fixedDigestBytes(row.result_digest))) {
    throw integrity("publication result digest mismatch");
  }
  verifyTaskPublication(db, row.task_id, resultFacts.key, publicationId, outcomeCode === 2);
  verifyTaskItem(db, row.task_id, row.item_id, publicationId, "publication", taskRevision);
  return {
    result_key: row.result_key, base_version_key: baseKey,
    result_version_key: row.result_version_key, identity,
    result_digest: fixedDigestBytes(row.result_digest),
  };
};

// read_update_audit — RangeExecutionResult audit projection
export const readUpdateAudit = (db, resultId) => {
  const exists = db.get(
    "SELECT EXISTS(SELECT 1 FROM market_update_results WHERE result_id=?) AS x",
    resultId).x === 1n;
  if (!exists) return null;
  const stored = readMarketUpdate(db, resultId);
  const id = stored.identity;
  const base = CR.materializeChecked(db, id.base_version_id, false);
  const result = CR.materializeChecked(db, id.result_version_id, false);
  const binding = readItemBinding(db, id.task_id, id.item_id);
  if (!binding) throw integrity("range result task missing");
  if (binding.head.state_revision < id.task_revision ||
      binding.item.stage !== "finished" ||
      !(binding.item.outcome && binding.item.outcome.success === true)) {
    throw integrity("range result lacks successful task item");
  }
  const noChange = id.outcome === 1;
  if (!noChange) {
    const linked = db.get(
      "SELECT EXISTS(SELECT 1 FROM task_publications WHERE result_id=?) AS x",
      resultId).x === 1n;
    if (!linked) throw integrity("range publication missing");
  }
  const audit = {
    no_change: noChange,
    command: {
      publication_id: id.result_id, task_id: id.task_id, item_id: id.item_id,
      version_id: id.result_version_id,
    },
    evidence: { base_version_id: id.base_version_id },
    segment: { provenance: { acquired_at: result.raw.batch.provenance.acquired_at } },
    algorithm_version: id.algorithm_version,
    base_integrity_sha256: base.raw.integrity_sha256,
    result_integrity_sha256: result.raw.integrity_sha256,
  };
  return {
    audit,
    receipt: {
      publication_id: id.result_id, version_id: id.result_version_id,
      task_id: id.task_id, task_revision: Number(id.task_revision),
    },
  };
};

// read_yearly_audit — publication_results kind=2 audit projection
export const readYearlyAudit = (db, resultId) => {
  const exists = db.get(
    "SELECT EXISTS(SELECT 1 FROM publication_results WHERE publication_id=? AND publication_kind=2) AS x",
    resultId).x === 1n;
  if (!exists) return null;
  const stored = readPublication(db, resultId);
  const id = stored.identity;
  const resultKey = CR.versionKey(db, id.result_version_id);
  const baseId = CR.dailyBaseVersionId(db, resultKey);
  if (baseId === null) throw integrity("yearly result lacks daily base");
  CR.materializeChecked(db, baseId, false);
  const result = CR.materializeChecked(db, id.result_version_id, false);
  const binding = readItemBinding(db, id.task_id, id.item_id);
  if (!binding) throw integrity("yearly result task missing");
  if (binding.head.state_revision < id.task_revision ||
      binding.item.stage !== "finished" ||
      !(binding.item.outcome && binding.item.outcome.success === true)) {
    throw integrity("yearly result lacks successful task item");
  }
  const noChange = id.outcome === 1;
  if (!noChange) {
    const linked = db.get(
      "SELECT EXISTS(SELECT 1 FROM task_publications WHERE result_id=?) AS x",
      resultId).x === 1n;
    if (!linked) throw integrity("yearly publication missing");
  }
  const audit = {
    no_change: noChange,
    base_daily_version_id: baseId,
    command: { item_id: id.item_id },
    candidate: { batch: { provenance: { acquired_at: result.raw.batch.provenance.acquired_at } } },
  };
  return {
    audit,
    receipt: {
      publication_id: id.publication_id, version_id: id.result_version_id,
      task_id: id.task_id, task_revision: Number(id.task_revision),
    },
  };
};

// ---------- commit_publication (publication_results rows) ----------
// commit_publication_tx — shared by the per-item path and collection batch.
export const commitPublicationTx = (db, command, verifyStored) => {
  const expectedRevisionI64 = BigInt(command.expected_task_revision);
  if (expectedRevisionI64 > 9223372036854775807n || expectedRevisionI64 < 0n) {
    throw integrity("task revision out of range");
  }
  const taskRevision = expectedRevisionI64 + 1n;
  if (taskRevision > 9223372036854775807n) throw integrity("task revision out of range");
  const outcomeJson = resultOutcomeJson(command.publication_id, "publication");

  const result = CR.resultFacts(db, command.result_version_key);
  const expectedEvidence =
    command.publication_kind === "yearly" ? "yearly" : "version";
  if (CR.verifiedEvidenceKind(db, command.evidence_digest) !== expectedEvidence) {
    throw integrity("publication evidence kind mismatch");
  }
  CR.verifiedSourceBinding(db, command.source_binding_digest);
  if (command.publication_kind === "collection" && result.period !== "Day") {
    throw integrity("collection publication requires daily result");
  }
  if (command.publication_kind === "yearly" && result.period !== "Year") {
    throw integrity("yearly publication requires yearly result");
  }
  let base = null;
  if (command.base_version_key !== null && command.base_version_key !== undefined) {
    const facts = CR.resultFacts(db, command.base_version_key);
    if (facts.state !== "published") {
      throw integrity("publication base version is not published");
    }
    if (facts.entity !== result.entity || facts.period !== result.period) {
      throw integrity("publication base scope mismatch");
    }
    CR.requirePublishedFacts(db, facts.key);
    base = facts;
  }
  if (command.outcome === "no_change") {
    if (base === null) throw integrity("no-change publication lacks base");
    if (base.key !== result.key) {
      throw integrity("no-change publication base differs from result");
    }
    if (result.state !== "published") {
      throw integrity("no-change publication result is not published");
    }
    if (command.published_at !== null && command.published_at !== undefined ||
        command.head.kind !== "keep") {
      throw integrity("no-change publication carries publish effects");
    }
    CR.requirePublishedFacts(db, result.key);
  } else {
    if (result.state !== "validated") {
      throw integrity("publication result version is not validated");
    }
    if (!result.source_binding_digest.equals(command.source_binding_digest)) {
      throw integrity("publication source binding mismatch");
    }
    if (command.published_at === null || command.published_at === undefined) {
      throw integrity("published publication lacks published_at");
    }
    if (base !== null && base.key === result.key) {
      throw integrity("published publication reuses base version");
    }
  }
  registerResult(db, command.publication_id, 2);
  requireItemUnused(db, command.task_id, command.item_id);
  checkRequiredHeads(db, command.required_heads);
  checkBinding(db, command.task_id, command.item_id, command.binding);
  if (command.outcome === "published") {
    if (verifyStored) {
      CR.publish(db, result.key, command.published_at);
      CR.requirePublishedFacts(db, result.key);
    } else {
      CR.publishPresumed(db, result.key, command.published_at);
    }
  }
  const identity = {
    publication_id: command.publication_id, task_id: command.task_id,
    item_id: command.item_id,
    publication_kind: { collection: 1, yearly: 2, republish: 3 }[command.publication_kind],
    base_version_id: base === null ? null : base.version_id,
    result_version_id: result.version_id,
    outcome: command.outcome === "no_change" ? 1 : 2,
    algorithm_version: command.algorithm_version,
    source_binding_digest: command.source_binding_digest,
    evidence_digest: command.evidence_digest,
    task_revision: taskRevision,
  };
  const resultDigest = wrap(() => CM.publicationResultDigest(identity));
  let r;
  try {
    r = db.run(
      "INSERT INTO publication_results(publication_id,task_id,item_id,publication_kind,base_version_key,result_version_key,outcome,algorithm_version,source_binding_digest,evidence_digest,task_revision,result_digest) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
      command.publication_id, command.task_id, command.item_id,
      identity.publication_kind,
      command.base_version_key ?? null, result.key,
      identity.outcome, command.algorithm_version,
      command.source_binding_digest, command.evidence_digest,
      taskRevision, resultDigest);
  } catch (e) { throw conflictOrStorage(e); }
  const resultKey = r.lastInsertRowid;
  if (command.outcome === "published") {
    try {
      db.run(
        "INSERT INTO task_publications(task_id,version_key,result_id) VALUES(?,?,?)",
        command.task_id, result.key, command.publication_id);
    } catch (e) { throw conflictOrStorage(e); }
    applyHead(db, command.head, result.entity, result.period,
      command.base_version_key ?? null, result.key, true);
  }
  finishItemAndTask(db, command.task_id, command.item_id,
    expectedRevisionI64, taskRevision, outcomeJson);
  return {
    result_key: resultKey, base_version_key: command.base_version_key ?? null,
    result_version_key: result.key, identity, result_digest: resultDigest,
  };
};

// commit_publication — owns its IMMEDIATE transaction
export const commitPublication = (db, command) => {
  db.exec("BEGIN IMMEDIATE");
  try {
    const r = commitPublicationTx(db, command, true);
    db.exec("COMMIT");
    return r;
  } catch (e) { try { db.exec("ROLLBACK"); } catch {} throw e; }
};

// commit_market_update — self-transacting variant (used by commit_tx paths)
export const commitMarketUpdate = (db, command) => {
  db.exec("BEGIN IMMEDIATE");
  try {
    const r = commitMarketUpdateTx(db, command);
    db.exec("COMMIT");
    return r;
  } catch (e) { try { db.exec("ROLLBACK"); } catch {} throw e; }
};

// ---------- helpers used by the plan-level commits ----------
const headKey = (db, entity, period) =>
  db.get("SELECT version_key FROM effective_heads WHERE entity=? AND period=? AND adjustment=1",
    entity, periodCode(period))?.version_key ?? null;

const requiredHeads = (db, heads) => heads.map(([scope, expected]) => ({
  entity: scope.symbol,
  period: scope.period === "Day" || scope.period === "day" ? "Day" : "Year",
  expected: expected === null || expected === undefined
    ? null : CR.versionKey(db, expected),
}));

const occupancyKey = (symbol, period) =>
  JSON.stringify(["market", symbol, period === "Day" || period === "day" ? "day" : "year"]);

const bindingCheck = (plan) => ({
  run_id: plan.command.run_id,
  entity: plan.record.version.symbol,
  period: plan.record.version.period,
  occupancy_key: occupancyKey(plan.record.version.symbol, plan.record.version.period),
});

const publicationReceipt = (stored) => ({
  publication_id: stored.identity.publication_id,
  version_id: stored.identity.result_version_id,
  task_id: stored.identity.task_id,
  task_revision: stored.identity.task_revision,
});

const replayReceipt = (db, kind, command) => {
  if (kind === 1n || kind === 1) {
    const stored = readMarketUpdate(db, command.publication_id);
    if (stored.identity.task_id !== command.task_id ||
        stored.identity.item_id !== command.item_id ||
        stored.identity.result_version_id !== command.version_id) {
      throw conflict("result identity conflict");
    }
    return {
      publication_id: command.publication_id,
      version_id: stored.identity.result_version_id,
      task_id: command.task_id,
      task_revision: stored.identity.task_revision,
    };
  }
  if (kind === 2n || kind === 2) {
    const stored = readPublication(db, command.publication_id);
    if (stored.identity.task_id !== command.task_id ||
        stored.identity.item_id !== command.item_id ||
        stored.identity.result_version_id !== command.version_id) {
      throw conflict("result identity conflict");
    }
    return {
      publication_id: command.publication_id,
      version_id: stored.identity.result_version_id,
      task_id: command.task_id,
      task_revision: stored.identity.task_revision,
    };
  }
  throw integrity("unknown result kind");
};

// ---------- commit_tx — compact PublishPlan commit ----------
export const commitTx = (db, plan) => {
  const command = plan.command;
  const kind0 = registryKind(db, command.publication_id);
  if (kind0 !== null) return replayReceipt(db, kind0, command);
  const resultKey = CR.versionKey(db, command.version_id);
  const facts = CR.resultFacts(db, resultKey);
  if (facts.state !== "validated") throw conflict("candidate is not validated");
  const stored = CR.materializeChecked(db, command.version_id, true);
  let same;
  if (plan.record.version.period === "Year") {
    const sv = stored.version, pv = plan.record.version;
    same = sv.version_id === pv.version_id && sv.symbol === pv.symbol &&
      canonEq(sv.records, pv.records) &&
      canonEq(sv.yearly_derivation ?? null, pv.yearly_derivation ?? null) &&
      sv.source === pv.source &&
      canonEq(sv.unit_currency, pv.unit_currency) &&
      canonEq(stored.batch.coverage, plan.record.batch.coverage);
  } else {
    same = canonEq(stored.raw, plan.record.raw);
  }
  if (!same) throw integrity("validated candidate changed since plan");
  if (plan.base_digest !== null && plan.base_digest !== undefined) {
    const lineage = plan.record.version.yearly_derivation;
    if (!lineage) throw integrity("base digest without yearly lineage");
    const baseKey = CR.versionKey(db, lineage.base_daily_version_id);
    const base = CR.resultFacts(db, baseKey);
    if (base.state !== "published" ||
        base.integrity_digest === null ||
        CM.digestHex(base.integrity_digest) !== plan.base_digest) {
      throw baseErr("fixed base changed since validation");
    }
  }
  const scope = { symbol: plan.record.version.symbol, period: plan.record.version.period };
  const binding = bindingCheck(plan);
  const required = requiredHeads(db, plan.required_heads ?? []);
  const intent = command.intent ?? {};
  if (intent.Daily !== undefined && intent.Daily.expected_head !== null &&
      intent.Daily.expected_head !== undefined) {
    const expected = intent.Daily.expected_head;
    const baseKey = CR.versionKey(db, expected);
    const base = CR.resultFacts(db, baseKey);
    if (base.state !== "published") throw integrity("update base is not published");
    let change, rangeStart, rangeEnd;
    if (facts.manifest_mode === "delta") {
      if (facts.parent_version_key !== baseKey) {
        throw conflict("update result delta is not based on the expected head");
      }
      const parent = CR.resultFacts(db, facts.parent_version_key);
      const [ops] = CR.__readDelta(db, resultKey, parent.projection_digest);
      change = { kind: "append", prefix_bytes: baseCount(db, baseKey) * 32n, ops };
      rangeStart = ops.length ? ops[0].range_start_day : 0n;
      rangeEnd = ops.length ? ops[ops.length - 1].range_end_day : 0n;
    } else {
      change = { kind: "full" };
      rangeStart = facts.first_day; rangeEnd = facts.last_day;
    }
    const evidence = putEvidence(db, "continuity", {
      mode: change.kind === "append" ? "append" : "full",
      base_version_id: expected,
      source_binding_digest: CM.digestHex(facts.source_binding_digest),
    });
    const storedR = commitMarketUpdate(db, {
      result_id: command.publication_id, task_id: command.task_id,
      item_id: command.item_id,
      base_version_key: baseKey, result_version_key: resultKey,
      range_start_day: rangeStart, range_end_day: rangeEnd,
      algorithm_version: UPDATE_ALGORITHM,
      source_binding_digest: facts.source_binding_digest,
      evidence_digest: evidence,
      expected_task_revision: BigInt(command.expected_revision),
      published_at: CM.nowRfc3339(),
      head: { kind: "version", version: baseKey },
      required_heads: required,
      binding,
      change,
    });
    return {
      publication_id: command.publication_id,
      version_id: storedR.identity.result_version_id,
      task_id: command.task_id,
      task_revision: storedR.identity.task_revision,
    };
  }
  // Daily{None} | HistoricalFixedBase | CurrentHead
  // serde unit variant: "HistoricalFixedBase" arrives as a bare string.
  let kind, head, baseKey, requiredHeads2;
  if (intent.Daily !== undefined) {
    kind = "collection"; head = { kind: "missing" }; baseKey = null;
    requiredHeads2 = required;
  } else if (intent === "HistoricalFixedBase" || intent.HistoricalFixedBase !== undefined) {
    kind = "yearly"; head = { kind: "keep" };
    baseKey = headKey(db, scope.symbol, scope.period);
    requiredHeads2 = required;
  } else {
    // CurrentHead { expected_daily_head, expected_yearly_head }
    const yearlyKey = intent.CurrentHead.expected_yearly_head === null ||
      intent.CurrentHead.expected_yearly_head === undefined
      ? null : CR.versionKey(db, intent.CurrentHead.expected_yearly_head);
    kind = "yearly";
    head = yearlyKey === null ? { kind: "missing" } : { kind: "version", version: yearlyKey };
    baseKey = yearlyKey; requiredHeads2 = required;
  }
  let evidence;
  if (kind === "yearly") {
    const lineage = stored.version.yearly_derivation;
    if (!lineage) throw integrity("yearly result lacks derivation");
    evidence = putEvidence(db, "yearly", {
      base_daily_version_id: lineage.base_daily_version_id,
      algorithm_version: lineage.algorithm_version,
    });
  } else {
    const row = db.get(
      "SELECT evidence_digest FROM data_versions WHERE version_key=?", resultKey);
    evidence = CR.__fixedDigest(row.evidence_digest);
  }
  const algorithm = kind === "yearly"
    ? (stored.version.yearly_derivation?.algorithm_version ?? COLLECTION_ALGORITHM)
    : COLLECTION_ALGORITHM;
  const storedR = commitPublication(db, {
    publication_id: command.publication_id, task_id: command.task_id,
    item_id: command.item_id, publication_kind: kind,
    base_version_key: baseKey, result_version_key: resultKey,
    outcome: "published", algorithm_version: algorithm,
    source_binding_digest: facts.source_binding_digest,
    evidence_digest: evidence,
    expected_task_revision: BigInt(command.expected_revision),
    published_at: CM.nowRfc3339(),
    head, required_heads: requiredHeads2, binding,
  });
  return {
    publication_id: command.publication_id,
    version_id: storedR.identity.result_version_id,
    task_id: command.task_id,
    task_revision: storedR.identity.task_revision,
  };
};

// ---------- yearly_results ----------
// yearly_no_change_tx — typed publication_results(kind=2) no-change receipt
export const yearlyNoChangeTx = (db, plan) => {
  const command = plan.publication.command;
  const kind = registryKind(db, command.publication_id);
  if (kind !== null) return replayReceipt(db, kind, command);
  const resultKey = CR.versionKey(db, command.version_id);
  const facts = CR.resultFacts(db, resultKey);
  if (facts.period !== "Year") {
    throw integrity("yearly no-change result is not a yearly version");
  }
  const lineage = plan.publication.record.version.yearly_derivation;
  if (!lineage) throw integrity("yearly no-change result lacks lineage");
  const evidence = putEvidence(db, "yearly", {
    base_daily_version_id: lineage.base_daily_version_id,
    algorithm_version: lineage.algorithm_version,
    mode: "no_change",
  });
  const stored = commitPublication(db, {
    publication_id: command.publication_id, task_id: command.task_id,
    item_id: command.item_id, publication_kind: "yearly",
    base_version_key: resultKey, result_version_key: resultKey,
    outcome: "no_change", algorithm_version: lineage.algorithm_version,
    source_binding_digest: facts.source_binding_digest,
    evidence_digest: evidence,
    expected_task_revision: BigInt(command.expected_revision),
    published_at: null, head: { kind: "keep" },
    required_heads: requiredHeads(db, plan.publication.required_heads ?? []),
    binding: bindingCheck(plan.publication),
  });
  return publicationReceipt(stored);
};

// commit_yearly — self.write() autocommit, like the oracle
export const commitYearly = (db, plan) =>
  plan.no_change ? yearlyNoChangeTx(db, plan) : commitTx(db, plan.publication);

// yearly_receipt — publication_results kind=2 receipt lookup
export const yearlyReceipt = (db, id) => {
  const kind = db.get(
    "SELECT publication_kind FROM publication_results WHERE publication_id=?", id)
    ?.publication_kind;
  if (kind === 2n) {
    return publicationReceipt(readPublication(db, id));
  }
  return null;
};

export const yearlyItemReceipt = (db, task, item) => {
  const id = db.get(
    "SELECT publication_id FROM publication_results WHERE task_id=? AND item_id=? AND publication_kind=2",
    task, item)?.publication_id;
  if (id === undefined) return null;
  return publicationReceipt(readPublication(db, id));
};

// receipt — registry-dispatched result receipt (MarketRepository::receipt)
export const receipt = (db, id) => {
  const kind = registryKind(db, id);
  if (kind === 1n || kind === 1) {
    const stored = readMarketUpdate(db, id);
    return {
      publication_id: stored.identity.result_id,
      version_id: stored.identity.result_version_id,
      task_id: stored.identity.task_id,
      task_revision: stored.identity.task_revision,
    };
  }
  if (kind === 2n || kind === 2) {
    const stored = readPublication(db, id);
    return publicationReceipt(stored);
  }
  if (kind !== null) throw integrity("unknown result kind");
  return null;
};

// ===== NM-057 — RangeRepository commit surface =====
const SEGMENT_ROWS = 512;

// typed_replay_receipt — compact_repo.rs:452
export const typedReplayReceipt = (db, command) => {
  const kind = registryKind(db, command.publication_id);
  if (kind === null) return null;
  if (kind !== 1n) throw conflict("result identity conflict");
  const stored = readMarketUpdate(db, command.publication_id);
  if (stored.identity.task_id !== command.task_id ||
      stored.identity.item_id !== command.item_id ||
      stored.identity.result_version_id !== command.version_id) {
    throw conflict("result identity conflict");
  }
  return {
    publication_id: command.publication_id,
    version_id: stored.identity.result_version_id,
    task_id: command.task_id,
    task_revision: stored.identity.task_revision,
  };
};

// ops delta stage-if-absent shared by commit_range_ops{,_typed}
const opsStageIfAbsent = (db, scope, baseVersionId, versionId, upserts, removes) => {
  const exists = db.get(
    "SELECT EXISTS(SELECT 1 FROM data_versions WHERE version_id=?) x",
    versionId).x !== 0n;
  if (!exists) {
    stageOpsDelta(db, scope.symbol, baseVersionId, versionId, upserts, removes);
  }
};

// intent == Daily{expected_head: base} gate — verbatim arm order
const opsIntentGate = (command, baseVersionId) => {
  const i = command.intent;
  const ok = i !== null && typeof i === "object" && !Array.isArray(i) &&
    Object.keys(i).length === 1 && "Daily" in i &&
    i.Daily !== null && typeof i.Daily === "object" &&
    i.Daily.expected_head === baseVersionId;
  if (!ok) throw invalidCmd("ops commit must bind the expected daily head");
};

// commit_range_ops — leaf-equality content binding (compact_repo.rs:1774)
export const commitRangeOps = (db, command, scope, baseVersionId,
    upserts, removes, expected) => {
  opsIntentGate(command, baseVersionId);
  const versionId = command.version_id;
  opsStageIfAbsent(db, scope, baseVersionId, versionId, upserts, removes);
  {
    const key = CR.versionKey(db, versionId);
    const facts = CR.resultFacts(db, key);
    if (facts.parent_version_key !== CR.versionKey(db, baseVersionId) ||
        facts.state === "published") {
      throw conflict("staged ops version is not a delta on the expected base");
    }
    const leaf = CR.leafBars(db, key);
    const approved = expected.map((b) => CM.compactBarFromBar(b, "Day"));
    if (!canonEq(leaf, approved)) {
      throw integrity("staged ops projection differs from approved candidate");
    }
  }
  validateStaged(db, versionId);
  return commitOpsUpdate(db, {
    result_id: command.publication_id, task_id: command.task_id,
    item_id: command.item_id, base_version_id: baseVersionId,
    result_version_id: versionId,
    expected_revision: command.expected_revision, run_id: command.run_id,
    occupancy_key: occupancyKey(scope.symbol, scope.period),
    continuity_evidence: null,
  });
};

// commit_range_ops_typed — O(ops) read-back binding (compact_repo.rs:1883)
export const commitRangeOpsTyped = (db, command, scope, baseVersionId,
    upserts, removes, continuity) => {
  opsIntentGate(command, baseVersionId);
  const replay = typedReplayReceipt(db, command);
  if (replay !== null) return replay;
  const versionId = command.version_id;
  opsStageIfAbsent(db, scope, baseVersionId, versionId, upserts, removes);
  {
    const key = CR.versionKey(db, versionId);
    const facts = CR.resultFacts(db, key);
    if (facts.parent_version_key !== CR.versionKey(db, baseVersionId) ||
        facts.state === "published") {
      throw conflict("staged ops version is not a delta on the expected base");
    }
    const base = CR.resultFacts(db, facts.parent_version_key);
    const [staged] = CR.__readDelta(db, key, base.projection_digest);
    const bars = upserts.map((b) => CM.compactBarFromBar(b, "Day"))
      .sort((a, b) => a.trade_day < b.trade_day ? -1 : a.trade_day > b.trade_day ? 1 : 0);
    for (let i = 1; i < bars.length; i++) {
      if (bars[i - 1].trade_day >= bars[i].trade_day) {
        throw integrity("typed ops upserts are not strictly increasing");
      }
    }
    const expected = [];
    for (let i = 0; i < bars.length; i += SEGMENT_ROWS) {
      const chunk = bars.slice(i, i + SEGMENT_ROWS);
      expected.push([chunk[0].trade_day, chunk[chunk.length - 1].trade_day, {
        segment_digest: CM.segmentDigest(scope.symbol, "Day", chunk),
        clip_start_day: chunk[0].trade_day,
        clip_end_day: chunk[chunk.length - 1].trade_day,
        row_count: chunk.length,
      }]);
    }
    for (const day of removes) {
      const d = CM.epochDay(typeof day === "string" ? day : day.s);
      expected.push([BigInt(d), BigInt(d), null]);
    }
    expected.sort((a, b) => a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0);
    const actual = staged.map((op) => [
      op.range_start_day, op.range_end_day,
      op.kind.kind === "upsert" ? {
        segment_digest: op.kind.clip.segment_digest,
        clip_start_day: op.kind.clip.clip_start_day,
        clip_end_day: op.kind.clip.clip_end_day,
        row_count: op.kind.clip.row_count,
      } : null,
    ]);
    if (!canonEq(actual, expected)) {
      throw integrity("staged ops differ from the requested delta");
    }
  }
  validateStaged(db, versionId);
  return commitOpsUpdate(db, {
    result_id: command.publication_id, task_id: command.task_id,
    item_id: command.item_id, base_version_id: baseVersionId,
    result_version_id: versionId,
    expected_revision: command.expected_revision, run_id: command.run_id,
    occupancy_key: occupancyKey(scope.symbol, scope.period),
    continuity_evidence: continuity ?? null,
  });
};

// commit_range_no_change_typed (compact_repo.rs:2002)
export const commitRangeNoChangeTyped = (db, command, baseVersionId,
    range, continuity) => {
  const i = command.intent;
  const ok = i !== null && typeof i === "object" && !Array.isArray(i) &&
    Object.keys(i).length === 1 && "Daily" in i &&
    i.Daily !== null && typeof i.Daily === "object" &&
    i.Daily.expected_head === baseVersionId;
  if (!ok) {
    throw invalidCmd("no-change commit must bind the expected daily head");
  }
  if (command.version_id !== baseVersionId) {
    throw invalidCmd("no-change result must reuse the base version");
  }
  const baseKey = CR.versionKey(db, baseVersionId);
  const entity = CR.resultFacts(db, baseKey).entity;
  return commitOpsNoChange(db, {
    result_id: command.publication_id, task_id: command.task_id,
    item_id: command.item_id, base_version_id: baseVersionId,
    expected_revision: command.expected_revision, run_id: command.run_id,
    occupancy_key: occupancyKey(entity, "Day"),
    range_start_day: CM.epochDay(typeof range.start === "string" ? range.start : range.start.s),
    range_end_day: CM.epochDay(typeof range.end === "string" ? range.end : range.end.s),
    continuity_evidence: continuity ?? null,
  });
};

// commit_range_tx (compact_repo.rs:1386) — the RangePlan commit inside a
// caller-supplied transaction.
export const commitRangeTx = (db, plan) => {
  const publication = plan.publication;
  const command = publication.command;
  const kind = registryKind(db, command.publication_id);
  if (kind !== null) return replayReceipt(db, kind, command);
  const baseKey = CR.versionKey(db, plan.base.version.version_id);
  const base = CR.resultFacts(db, baseKey);
  if (base.state !== "published") throw integrity("range base is not published");
  const binding = bindingCheck(publication);
  const required = requiredHeads(db, publication.required_heads);
  const rangeStart = CM.epochDay(plan.audit.range.start);
  const rangeEnd = CM.epochDay(plan.audit.range.end);
  const evidenceValue = plan.audit.evidence ?? {};
  let change, resultKey;
  if (plan.no_change) {
    const baseCount = Number(CR.resultFacts(db, baseKey).record_count);
    change = { kind: "no_change", prefix_bytes: BigInt(baseCount) * 32n };
    resultKey = baseKey;
  } else {
    const key = CR.versionKey(db, command.version_id);
    const facts = CR.resultFacts(db, key);
    if (facts.state !== "validated") throw conflict("candidate is not validated");
    const stored = CR.materializeChecked(db, command.version_id, true);
    if (!canonEq(stored.raw, publication.record.raw)) {
      throw integrity("validated candidate changed since plan");
    }
    if (facts.manifest_mode === "delta") {
      if (facts.parent_version_key !== baseKey) {
        throw conflict("update result delta is not based on the frozen base");
      }
      const [ops] = CR.__readDelta(db, key, base.projection_digest);
      const bc = CR.resultFacts(db, baseKey).record_count;
      change = { kind: "append", prefix_bytes: bc * 32n, ops };
    } else {
      change = { kind: "full" };
    }
    resultKey = key;
  }
  const continuity = {
    mode: change.kind === "no_change" ? "unchanged"
      : change.kind === "append" ? "append" : "full",
    base_version_id: plan.base.version.version_id,
    source_binding_digest: Buffer.from(
      CR.resultFacts(db, resultKey).source_binding_digest).toString("hex"),
  };
  if (evidenceValue.continuity !== undefined) continuity.proof = evidenceValue.continuity;
  if (evidenceValue.same_qfq_basis_ref !== undefined) continuity.basis_ref = evidenceValue.same_qfq_basis_ref;
  if (evidenceValue.revision_ref !== undefined) continuity.revision_ref = evidenceValue.revision_ref;
  if (change.kind === "append" || change.kind === "no_change") {
    continuity.prefix_bytes = Number(change.prefix_bytes);
  }
  const evidence = putEvidence(db, "continuity", continuity);
  const stored = commitMarketUpdateTx(db, {
    result_id: command.publication_id, task_id: command.task_id,
    item_id: command.item_id,
    base_version_key: baseKey, result_version_key: resultKey,
    range_start_day: rangeStart, range_end_day: rangeEnd,
    algorithm_version: plan.audit.algorithm_version ?? UPDATE_ALGORITHM,
    source_binding_digest: CR.resultFacts(db, resultKey).source_binding_digest,
    evidence_digest: evidence,
    expected_task_revision: BigInt(command.expected_revision),
    published_at: plan.no_change ? null : CM.nowRfc3339(),
    head: plan.no_change ? { kind: "keep" } : { kind: "version", version: baseKey },
    required_heads: required,
    binding, change,
  });
  return {
    publication_id: command.publication_id,
    version_id: stored.identity.result_version_id,
    task_id: command.task_id,
    task_revision: stored.identity.task_revision,
  };
};

// commit_update_batch (compact_repo.rs:2637) — one immediate transaction;
// per-item savepoint confines item failures.
export const commitUpdateBatch = (db, taskId, items) => {
  db.exec("BEGIN IMMEDIATE");
  try {
    const snap = TR.readBoundSnapshot(db, taskId);
    if (!snap) throw taskErr("task_not_found", "task not found");
    if (snap.head.status === "pending") {
      const runId = items.length ? items[0].run_id : null;
      if (runId === null) throw invalidCmd("update batch carries no items");
      TR.transitionLightTx(db, snap, runId, { kind: "claim" }, CM.nowRfc3339());
    }
    const out = items.map((item) => updateBatchItem(db, snap, item));
    db.exec("COMMIT");
    return out;
  } catch (e) {
    try { db.exec("ROLLBACK"); } catch {}
    throw e;
  }
};

// update_batch_item — replay → StartItem → SAVEPOINT commit → outcome map
const updateBatchItem = (db, snap, item) => {
  const kind = registryKind(db, item.publication_id);
  if (kind !== null) {
    const stored = readMarketUpdate(db, item.publication_id);
    if (kind !== 1n) throw conflict("update result id kind mismatch");
    const expectedVersion = item.prepared.kind === "no_change"
      ? item.prepared.base_version_id : item.prepared.result_version_id;
    if (stored.identity.task_id !== item.task_id ||
        stored.identity.item_id !== item.item_id ||
        stored.identity.result_version_id !== expectedVersion) {
      throw conflict("update result id replays a different commit");
    }
    return { ok: true, receipt: {
      publication_id: item.publication_id,
      version_id: stored.identity.result_version_id,
      task_id: item.task_id,
      task_revision: stored.identity.task_revision,
    } };
  }
  // transition errors are structural — abort the whole batch.
  TR.transitionLightTx(db, snap, item.run_id,
    { kind: "startItem", item_id: item.item_id }, CM.nowRfc3339());
  const revision = db.get(
    "SELECT state_revision FROM tasks WHERE task_id=?", item.task_id).state_revision;
  const expectedRevision = revision;
  let committed;
  db.run("SAVEPOINT update_batch_item");
  try {
    const receipt = item.prepared.kind === "no_change"
      ? commitOpsNoChangeTx(db, {
          result_id: item.publication_id, task_id: item.task_id,
          item_id: item.item_id,
          base_version_id: item.prepared.base_version_id,
          expected_revision: expectedRevision, run_id: item.run_id,
          occupancy_key: item.occupancy_key,
          range_start_day: item.prepared.range_start_day,
          range_end_day: item.prepared.range_end_day,
          continuity_evidence: item.prepared.continuity })
      : (stageOpsDeltaTx(db, item.symbol, item.prepared.base_version_id,
            item.prepared.result_version_id, item.prepared.upserts, []),
         CR.markValidatedDelta(db, CR.versionKey(db, item.prepared.result_version_id)),
         commitOpsUpdateTx(db, {
          result_id: item.publication_id, task_id: item.task_id,
          item_id: item.item_id,
          base_version_id: item.prepared.base_version_id,
          result_version_id: item.prepared.result_version_id,
          expected_revision: expectedRevision, run_id: item.run_id,
          occupancy_key: item.occupancy_key,
          continuity_evidence: item.prepared.continuity }));
    db.run("RELEASE update_batch_item");
    committed = { ok: true, receipt };
  } catch (error) {
    db.exec("ROLLBACK TO update_batch_item; RELEASE update_batch_item");
    committed = { ok: false, error };
  }
  if (committed.ok) return committed;
  const uncertain = committed.error instanceof StorageError &&
    (committed.error.code === "unavailable" || committed.error.code === "outcome_unknown");
  const change = uncertain
    ? { kind: "requireVerification", item_id: item.item_id,
        reason: "item execution result not confirmed inside update batch; committed receipt not yet established" }
    : { kind: "finishItem", item_id: item.item_id,
        outcome: { success: false, error: String(committed.error.message ?? committed.error) } };
  TR.transitionLightTx(db, snap, item.run_id, change, CM.nowRfc3339());
  return committed;
};
