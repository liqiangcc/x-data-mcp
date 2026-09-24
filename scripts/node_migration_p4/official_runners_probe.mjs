// NM-101 official-runner decision differential probe.
//
// Compares the Node port (src_node/official/runner.mjs) against the live
// Rust implementation (official_runners_oracle.rs, compiled to
// target/debug/nm101_official_runners_oracle).
//
//   cargo build --lib && rustc --edition 2021 \
//     scripts/node_migration_p4/official_runners_oracle.rs \
//     --extern x_data_mcp=target/debug/libx_data_mcp.rlib \
//     --extern chrono=$(ls target/debug/deps/libchrono-*.rlib | head -1) \
//     --extern serde_json=$(ls target/debug/deps/libserde_json-*.rlib | head -1) \
//     --extern serde=$(ls target/debug/deps/libserde-*.rlib | head -1) \
//     -L dependency=target/debug/deps \
//     -o target/debug/nm101_official_runners_oracle
//
//   node scripts/node_migration_p4/official_runners_probe.mjs             -> exit 0/1
//   node scripts/node_migration_p4/official_runners_probe.mjs --selftest  -> exit 1
//   node scripts/node_migration_p4/official_runners_probe.mjs --out DIR   -> evidence
//
// Surfaces under test (all against the real gen-3 snapshot):
//   acquire / acquire_typed / acquire_merged (backfill + republish):
//     unchanged / append / unverified continuity dispatch, range-empty
//     failures, metadata drift fallbacks, combined candidate bytes.
//   gap_verify: all three conclusions + every BasisNotVerified gate.
//   run_item / run_items: scripted-repository decision order — the op
//     trace (call sequence + change tags + candidate content digests)
//     is compared verbatim against the Rust oracle's scripted repo.
//
// Continuity itself is a private Rust enum; probe/probe_facts/
// probe_bound dispatch is compared through the acquire surfaces'
// evidence.continuity and outcome. The scripted repository is a
// scenario-driven harness on both sides — decision ORDER is what's
// compared (decide_head semantics live in task_control, out of lane).

import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import {
  MaterializeSession, SourceError, TdxMarketSource, securityEventsDigest,
  sha256hex,
} from "../../src_node/official/official_market.mjs";
import {
  TdxBackfillProvider, TdxDailyRunner, TdxGapVerifier, TdxRangeProvider,
  TdxRepublishProvider, combinedVersionRecord,
} from "../../src_node/official/runner.mjs";
import { MarketError } from "../../src_node/market/record_codec.mjs";
import * as RC from "../../src_node/market/record_codec.mjs";
import { parseTs } from "../../src_node/governance/ts.mjs";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const GEN3_MANIFEST =
  join(REPO, "evidence/issue-4-update-gen3-acceptance/snapshot-manifest-gen3.json");
const GEN3_ROOT = "/home/box/retained-inputs/gen3";
const NOW = "2026-09-24T00:00:00Z";
const PUBLISHED = "2026-09-24T01:23:45.987654321Z";
const OBSERVED = "2026-09-24T02:00:00Z";
const ORACLE =
  process.env.NM101_ORACLE ?? join(REPO, "target/debug/nm101_official_runners_oracle");
const args = process.argv.slice(2);
const SELFTEST = args.includes("--selftest");
const OUT = args.includes("--out") ? args[args.indexOf("--out") + 1] : null;
const SCRATCH = mkdtempSync(join(tmpdir(), "nm101-"));
const J = JSON.stringify;

// ------------------------------------------------------------ canonicalize
// serde_json::Value canonical form: sorted object keys, NaiveDate/DateTime
// as their string forms; Node-side {y,mo,d,s} date objects -> "YYYY-MM-DD".
const isDate = (v) => v && typeof v === "object" &&
  "y" in v && "mo" in v && "d" in v && "s" in v &&
  Object.keys(v).every((k) => ["y", "mo", "d", "s"].includes(k));
const toWire = (v) => {
  if (v === null || v === undefined) return null;
  if (v instanceof Date) return parseTs(v.toISOString()).out;
  if (isDate(v)) return v.s;
  if (Array.isArray(v)) return v.map(toWire);
  if (typeof v === "object") {
    const o = {};
    for (const [k, x] of Object.entries(v)) {
      // serde skip_serializing_if on ItemOutcome.evidence and
      // FrozenWork.shared_evidence (None -> key absent).
      if (x === null &&
          ((k === "evidence" && "success" in v) ||
           (k === "shared_evidence" && "items" in v))) continue;
      o[k] = toWire(x);
    }
    return o;
  }
  return v;
};
const sortDeep = (v) => {
  if (Array.isArray(v)) return v.map(sortDeep);
  if (v !== null && typeof v === "object") {
    const o = {};
    for (const k of Object.keys(v).sort()) o[k] = sortDeep(v[k]);
    return o;
  }
  return v;
};
const canon = (v) => J(sortDeep(toWire(v)));

// ------------------------------------------------------------ source setup
const source = TdxMarketSource.inspect(
  GEN3_ROOT, readFileSync(GEN3_MANIFEST), new Date(NOW));
const session = source.materializeSession();

// Build a base (already-published) record: a real materialized batch with
// caller-controlled snapshot-input digests + bar subset, serialized into
// the EffectiveVersion+raw layout via combinedVersionRecord.
const SYM = "sh.600000";
const SYM2 = "sz.000002";
const dayInfo = (sym) => {
  const dot = sym.indexOf(".");
  const [market, code] = [sym.slice(0, dot), sym.slice(dot + 1)];
  const names = MaterializeSession.dayNames(market, code);
  const [member, bytes] = session.memberBytes(source.day, names);
  return { market, code, member, bytes, sha: sha256hex(bytes) };
};
const fullBatch = session.materializeDaily(SYM);   // canonical full batch
const info = dayInfo(SYM);
const N_BARS = fullBatch.bars.length;
const gbbqSha = session.decodedGbbq().sha256;
const eventsSha = (() => {
  // securityEventsDigest over the same events assembleDaily used.
  const [events, excluded] = session.securityEvents(info.market, info.code);
  return securityEventsDigest(events, excluded);
})();

// snapshot_inputs bindings for a base that pins `daySha`/`evSha`.
const bindings = (daySha, evSha) => [
  { role: "day", member: info.member,
    archive_sha256: source.day.sha256, member_sha256: daySha },
  { role: "gbbq", member: "gbbq",
    archive_sha256: source.gbbq.sha256, member_sha256: gbbqSha },
  { role: "security_events", member: "gbbq",
    archive_sha256: source.gbbq.sha256, member_sha256: evSha },
];
const baseRecord = ({ bars = fullBatch.bars, daySha = info.sha,
                      evSha = eventsSha, tamper = null } = {}) => {
  const batch = {
    ...fullBatch, bars,
    provenance: {
      ...fullBatch.provenance,
      snapshot_inputs: bindings(daySha, evSha),
    },
  };
  const raw = combinedVersionRecord(batch, "base-version-1", parseTs(PUBLISHED).out);
  if (tamper === "bad-integrity") delete raw.integrity_sha256;
  if (tamper === "strange-inputs") {
    raw.batch.provenance.snapshot_inputs = null;
    raw.integrity_sha256 = rehash(raw);
  }
  if (tamper === "conflict") {
    // Flip r7_raw on the earliest bar — the subject's stored value then
    // contradicts the source's known value on an overlapping date.
    const b = { ...raw.batch.bars[0], r7_raw: (raw.batch.bars[0].r7_raw ?? 0) + 7 };
    raw.batch.bars = [b, ...raw.batch.bars.slice(1)];
    raw.records = raw.batch.bars;
    raw.integrity_sha256 = rehash(raw);
  }
  return raw;
};
// Re-digest a hand-edited raw record (fixture authoring only).
const rehash = (raw) => {
  const { integrity_sha256, ...rest } = raw;
  const sorted = (v) => {
    if (Array.isArray(v)) return v.map(sorted);
    if (v !== null && typeof v === "object") {
      const o = {};
      for (const k of Object.keys(v).sort()) o[k] = sorted(v[k]);
      return o;
    }
    return v;
  };
  return createHash("sha256").update(J(sorted(rest)), "utf8").digest("hex");
};

const APPEND_K = N_BARS - 3; // base covers all but the last 3 records
const appendBase = baseRecord({
  bars: fullBatch.bars.slice(0, APPEND_K),
  daySha: sha256hex(info.bytes.subarray(0, APPEND_K * 32)),
});
const unchangedBase = baseRecord();
const unverifiedBase = baseRecord({ daySha: "0".repeat(64) });
const driftedBase = baseRecord();
{
  // Same records, conflicting confirmed unit -> combined candidate.
  const raw = driftedBase;
  const pc = raw.unit_currency.price_currency;
  raw.unit_currency = { ...raw.unit_currency,
    price_currency: { ...pc, value: "EUR" } };
  raw.batch.unit_currency = raw.unit_currency;
  raw.integrity_sha256 = rehash(raw);
}

const RANGE_FULL = { start: "1900-01-01", end: "2100-01-01" };
const RANGE_NARROW = {
  start: fullBatch.bars[APPEND_K - 1].date.s,   // one base bar only
  end: fullBatch.bars[APPEND_K - 1].date.s,
};
const RANGE_TAIL = {
  start: fullBatch.bars[APPEND_K].date.s,       // only appended records
  end: fullBatch.bars[N_BARS - 1].date.s,
};

const factsOf = (raw, { daySha = null, evSha = null, coverage = null,
                        unit = null, count = null } = {}) => ({
  version_id: raw.version_id,
  symbol: raw.symbol,
  record_count: count ?? raw.records.length,
  first_day: raw.records[0].date,
  last_day: raw.records[raw.records.length - 1].date,
  integrity_sha256: raw.integrity_sha256,
  coverage: coverage ?? raw.batch.coverage,
  unit_currency: unit ?? raw.unit_currency,
  source: raw.batch.provenance.source,
  generation_id: "gen3",
  generation_manifest_sha256: source.manifestSha256(),
  day_member_sha256: daySha ?? raw.batch.provenance.snapshot_inputs[0].member_sha256,
  security_event_sha256:
    evSha ?? raw.batch.provenance.snapshot_inputs[2].member_sha256,
});
const unitDrifted = (u) => ({
  ...u,
  price_currency: { ...u.price_currency, value: null, reason: "x" },
});

// ------------------------------------------------------------ repo script
// Scripted repository — mirrors the oracle's ScriptedRepo semantics:
// queued bindings/receipts, queued transition effects, canned
// heads/tasks/scopes/records, stage->candidate via the staged map.
// publisher_prepare mirrors market_storage::Publisher::prepare's call
// order + check_task_binding guards over the same scripted surface.
const TERMINAL = new Set(["succeeded", "partially_failed", "failed",
  "cancelled", "interrupted"]);
const visStatus = (status, accepted, owner, run) =>
  !TERMINAL.has(status) && (accepted !== run || (owner !== null && owner !== run))
    ? "needs_verification" : status;
const mInvalid = (m) => new MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);
const mConflict = (m) => new MarketError("conflict", `PUBLISH_PRECONDITION: ${m}`);
const mIntegrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const mStorage = (m) => new MarketError("storage", `MARKET_STORAGE: ${m}`);
const mNotFound = () => new MarketError("not_found", "PUBLISH_TARGET_NOT_FOUND");
const scriptErr = (v) => {
  const m = v.err;
  switch (v.err_kind ?? "storage") {
    case "invalid": return mInvalid(m);
    case "integrity": return mIntegrity(m);
    case "conflict": return mConflict(m);
    case "outcome_unknown":
      return new MarketError("outcome_unknown",
        `PUBLISH_OUTCOME_UNKNOWN: ${m}; inspect the same publication_id`);
    case "not_found": return mNotFound();
    default: return mStorage(m);
  }
};
const scopeId = (symbol) => J(["market", symbol, "day"]);

class ScriptedRepo {
  constructor(script) {
    this.ops = [];
    const q = (m) => new Map(
      Object.entries(m ?? {}).map(([k, a]) => [k, [...a]]));
    this._bindings = q(script.bindings);
    this._receipts = q(script.receipts);
    this._transitions = [...(script.transitions ?? [])];
    this._heads = script.heads ?? {};
    this._tasks = script.tasks ?? {};
    this._scopes = script.scopes ?? {};
    this._records = script.records ?? {};
    this._staged = new Map();
    this._markErr = script.mark_validated_err ?? null;
    this._stageErr = script.stage_err ?? null;
    this._commit = script.commit ?? { ok: {} };
    this._batch = script.commit_collection_batch ?? { ok: [] };
  }
  _pop(map, key) {
    const a = map.get(key);
    if (!a || a.length === 0) return undefined;
    // Clone: the Rust fake re-parses the scripted JSON each call, so
    // transition mutations must not leak back into the script.
    return structuredClone(a.length > 1 ? a.shift() : a[0]);
  }
  _effect(kind) {
    const v = this._transitions.length > 1
      ? this._transitions.shift() : this._transitions[0];
    if (v === undefined) return;
    if (v.err !== undefined) throw scriptErr(v);
    return v;
  }
  stage(raw) {
    this.ops.push(`stage:${raw.integrity_sha256}`);
    if (this._stageErr) throw mStorage(this._stageErr);
    this._staged.set(raw.version_id, raw);
  }
  candidate(id) {
    this.ops.push(`candidate:${id}`);
    const raw = this._staged.get(id) ?? this._records[id];
    if (raw === undefined) throw mIntegrity("record must be object");
    return RC.check(raw);
  }
  mark_validated(record) {
    this.ops.push(`mark_validated:${record.version.version_id}`);
    if (this._markErr) throw mStorage(this._markErr);
  }
  fixed(id) {
    this.ops.push(`fixed:${id}`);
    const raw = this._records[id];
    if (raw === undefined) throw mNotFound();
    return RC.check(raw);
  }
  task(id) {
    this.ops.push(`task:${id}`);
    const v = this._tasks[id];
    if (v === undefined) throw scriptErr({ err_kind: "not_found", err: "" });
    return v;
  }
  task_head(id) {
    this.ops.push(`task_head:${id}`);
    const v = this._heads[id];
    if (v === undefined) throw scriptErr({ err_kind: "not_found", err: "" });
    return v;
  }
  task_item_binding(task, item) {
    this.ops.push(`task_item_binding:${task}:${item}`);
    const v = this._pop(this._bindings, item);
    if (v === undefined) return null;
    if (v.err !== undefined) throw scriptErr(v);
    return v;
  }
  transition_task_head(head, item, run, change) {
    this.ops.push(`transition_task_head:${run}:${changeTag(change)}`);
    const v = this._effect();
    if (!v) return;
    if (v.head) {
      const { counts, ...rest } = v.head;
      Object.assign(head, rest);
      if (counts) head.counts = { ...head.counts, ...counts };
    }
    if (item && v.item) Object.assign(item, v.item);
  }
  commit(plan) {
    this.ops.push(`commit:${plan.publication.publication_id}`);
    const c = this._commit;
    if (c.err !== undefined) throw scriptErr(c);
    return c.ok;
  }
  collection_receipt(task, item) {
    this.ops.push(`collection_receipt:${task}:${item}`);
    const v = this._pop(this._receipts, item);
    if (v === undefined || v === null) return null;
    if (v.err !== undefined) throw scriptErr(v);
    return v;
  }
  effective_head(scope) {
    const k = scopeId(scope.symbol);
    this.ops.push(`effective_head:${k}`);
    const v = this._scopes[k];
    if (v === undefined || v === null) return null;
    if (typeof v === "object" && v.err !== undefined) throw scriptErr(v);
    return v;
  }
  commit_collection_batch(taskId, items) {
    this.ops.push(`commit_collection_batch:${taskId}:${items.length}`);
    for (const it of items) {
      this.ops.push(`cand:${createHash("sha256").update(canon(it), "utf8").digest("hex")}`);
    }
    const c = this._batch;
    if (c.err !== undefined) throw scriptErr(c);
    if (c.none) return null;
    return c.ok.map((r) =>
      r.err !== undefined ? { err: scriptErr(r) } : { ok: r.ok });
  }
  // Seam wrapper for market_storage::Publisher::prepare: same call order
  // and check_task_binding guards over this scripted surface.
  publisher_prepare(publication) {
    const record = this.candidate(publication.version_id);
    if (publication.publication_id.trim() === "") {
      throw mInvalid("empty publication_id");
    }
    // Daily intent: required_heads=[(scope,null)], promote=scope.
    const scope = {
      symbol: record.version.symbol, period: record.version.period };
    const binding = this.task_item_binding(
      publication.task_id, publication.item_id);
    if (!binding) throw mConflict("missing task item");
    const head = binding.head, item = binding.item;
    if (BigInt(head.state_revision) !== BigInt(publication.expected_revision) ||
        visStatus(head.status, head.accepted_run_id, head.owner_run_id,
                  publication.run_id) !== "running" ||
        head.owner_run_id !== publication.run_id) {
      throw mConflict("task revision/owner/state");
    }
    if (item.stage !== "running" || item.spec.item_id !== publication.item_id ||
        canon(item.spec.target) !== canon({ symbol: scope.symbol, period: "day" }) ||
        !item.spec.scope_keys.includes(scopeId(scope.symbol)) ||
        !binding.scopes_bound) {
      throw mConflict("task target/occupancy");
    }
    this.mark_validated(record);
    return { publication: { publication_id: publication.publication_id,
                            intent: "daily" } };
  }
}
const changeTag = (c) => {
  switch (c.kind) {
    case "claim": return "claim";
    case "requestCancel": return "requestCancel";
    case "startItem": return `startItem:${c.item_id}`;
    case "finishItem":
      return `finishItem:${c.item_id}:${c.outcome.success}:${c.outcome.error ?? ""}`;
    case "finishExecution": return "finishExecution";
    case "finishCancellation": return "finishCancellation";
    case "reconcile": return "reconcile";
    case "requireVerification":
      return `requireVerification:${c.item_id}:${c.reason}`;
    default: return `?${c.kind}`;
  }
};

// ------------------------------------------------------------ scenarios
const T0 = "2026-09-20T09:00:00Z";
const KEY = { operation: "start_collection", dataset: "market.daily",
              contract_version: "v1" };
const head = (o = {}) => ({
  task_id: "t1", key: KEY, status: "pending", state_revision: 1,
  accepted_run_id: "run-1", owner_run_id: null, last_progress_at: T0,
  result: null,
  counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 },
  evidence_digest: null, ...o,
});
const item = (sym = SYM, id = "i1", o = {}) => ({
  spec: {
    item_id: id,
    target: { symbol: sym, period: "day" },
    progress_unit: "symbol",
    frozen_inputs: {},
    scope_keys: [scopeId(sym)],
  },
  stage: "pending", outcome: null, ...o,
});
const binding = (sym = SYM, id = "i1", ho = {}, io = {}) =>
  ({ head: head(ho), item: item(sym, id, io), scopes_bound: true });
const receipt = (task = "t1", rev = 4) => ({
  publication_id: `collection-result-${sha256hex(Buffer.from(J([task, "i1"]), "utf8"))}`,
  version_id: `collection-version-${sha256hex(Buffer.from(J([task, "i1"]), "utf8"))}`,
  task_id: task, task_revision: rev,
});
const taskSnapshot = (o = {}) => ({
  task: {
    task_id: "t1", key: KEY, status: "running", state_revision: 3,
    accepted_run_id: "run-1", owner_run_id: "run-1",
    created_at: T0, last_progress_at: T0,
    work: { items: [item().spec], scopes: [scopeId(SYM)] },
    result: null, retry_of: null, ...o,
  },
  items: [item(SYM, "i1", { stage: "finished",
    outcome: { success: false, error: "x", evidence: null } })],
  claims: [{ scope_key: scopeId(SYM), task_id: "t1", state_revision: 1 }],
});

const SCEN = { published_at: PUBLISHED, ops: [] };
const op = (o) => SCEN.ops.push(o);

// --- acquire (envelope) ---
op({ op: "acquire", base: unchangedBase, range: RANGE_FULL,
     candidate_id: "cand-unchanged" });                        // unchanged
op({ op: "acquire", base: appendBase, range: RANGE_FULL,
     candidate_id: "cand-append" });                          // append+combined
op({ op: "acquire", base: appendBase, range: RANGE_NARROW,
     candidate_id: "cand-append-narrow" });                   // merge==base -> no-change
op({ op: "acquire", base: unverifiedBase, range: RANGE_FULL,
     candidate_id: "cand-unverified" });                      // full materialize
op({ op: "acquire", base: baseRecord({ evSha: "0".repeat(64) }),
     range: RANGE_FULL, candidate_id: "cand-evdrift" });      // events drift
op({ op: "acquire", base: driftedBase, range: RANGE_FULL,
     candidate_id: "cand-units" });                           // unchanged+drift -> combined
op({ op: "acquire", base: appendBase, range: { start: "2100-01-01", end: "2100-12-31" },
     candidate_id: "cand-empty" });                           // range-empty -> invalid_response
op({ op: "acquire", base: baseRecord({ tamper: "strange-inputs" }),
     range: RANGE_FULL, candidate_id: "cand-noinputs" });     // no snapshot_inputs
op({ op: "acquire", base: baseRecord({ tamper: "bad-integrity" }),
     range: RANGE_FULL, candidate_id: "cand-nointegrity" });  // missing digest -> codec integrity error
{
  // Unsupported symbol reaches split_symbol (record must still pass
  // compat::check — symbol consistent across version/batch/provenance).
  const badSym = combinedVersionRecord(
    { ...fullBatch, symbol: "bj.600000",
      provenance: { ...fullBatch.provenance, symbol: "bj.600000",
                    snapshot_inputs: bindings(info.sha, eventsSha) } },
    "base-version-bad-sym", parseTs(PUBLISHED).out);
  op({ op: "acquire", base: badSym, range: RANGE_FULL,
       candidate_id: "cand-sym" });
}
{
  // Prefix digest matches but base claims records BEYOND the prefix —
  // tail[0] <= last base date -> overlap -> Unverified (fail closed).
  const r = baseRecord({
    bars: fullBatch.bars.slice(APPEND_K),
    daySha: sha256hex(info.bytes.subarray(0, APPEND_K * 32)),
  });
  op({ op: "acquire", base: r, range: RANGE_FULL,
       candidate_id: "cand-overlap" });
}

// --- acquire_typed (ops path) ---
const unchangedFacts = factsOf(unchangedBase);
const appendFacts = factsOf(appendBase, { count: APPEND_K });
op({ op: "acquire_typed", facts: unchangedFacts,
     base_window: unchangedBase.records, range: RANGE_FULL });       // Unchanged
op({ op: "acquire_typed", facts: appendFacts,
     base_window: appendBase.records, range: RANGE_FULL });          // Append upserts
op({ op: "acquire_typed", facts: appendFacts,
     base_window: appendBase.records, range: RANGE_NARROW });        // narrow -> Unchanged
op({ op: "acquire_typed", facts: factsOf(unverifiedBase),
     base_window: unverifiedBase.records, range: RANGE_FULL });      // unverified -> null
op({ op: "acquire_typed", facts: unchangedFacts, base_window: [],
     range: RANGE_FULL });                                           // empty window -> err
op({ op: "acquire_typed",
     facts: factsOf(unchangedBase, { unit: unitDrifted(unchangedBase.unit_currency) }),
     base_window: unchangedBase.records, range: RANGE_FULL });       // unit drift -> null
op({ op: "acquire_typed",
     facts: factsOf(unchangedBase,
       { coverage: { status: "Incomplete", reason: "x" } }),
     base_window: unchangedBase.records, range: RANGE_FULL });       // non-complete -> null
op({ op: "acquire_typed",
     facts: factsOf(appendBase, {
       coverage: { status: "Incomplete", reason: "drifted" }, count: APPEND_K }),
     base_window: appendBase.records, range: RANGE_FULL });          // append coverage drift -> null
op({ op: "acquire_typed", facts: { ...unchangedFacts, symbol: "bj.600000" },
     base_window: unchangedBase.records, range: RANGE_FULL });       // bad symbol -> unsupported

// --- acquire_merged (backfill/republish) ---
op({ op: "acquire_merged", provider: "backfill", base: unchangedBase,
     range: RANGE_NARROW, candidate_id: "bf-1" });
op({ op: "acquire_merged", provider: "republish", base: unchangedBase,
     range: RANGE_NARROW, candidate_id: "rp-1" });
op({ op: "acquire_merged", provider: "backfill", base: unchangedBase,
     range: { start: "2100-01-01", end: "2100-12-31" },
     candidate_id: "bf-empty" });
op({ op: "acquire_merged", provider: "backfill",
     base: baseRecord({ tamper: "strange-inputs" }),
     range: RANGE_NARROW, candidate_id: "bf-noinputs" });
op({ op: "acquire_merged", provider: "republish",
     base: baseRecord({ tamper: "conflict" }),
     range: RANGE_FULL, candidate_id: "rp-conflict" });

// --- gap_verify ---
const gapSubject = { version_id: unchangedBase.version_id,
                     integrity_sha256: unchangedBase.integrity_sha256 };
const gapTargetDay = fullBatch.bars[N_BARS - 1].date.s;   // present in source
const missingSubjectBase = baseRecord({
  bars: fullBatch.bars.filter((b) => b.date.s !== gapTargetDay),
});
const missingSubject = { version_id: missingSubjectBase.version_id,
                         integrity_sha256: missingSubjectBase.integrity_sha256 };
op({ op: "gap_verify",
     target: { symbol: SYM, start_date: gapTargetDay, end_date: gapTargetDay },
     subject: missingSubject,
     records: { [missingSubject.version_id]: missingSubjectBase },
     observed_at: OBSERVED });                                        // confirmed_missing_range
op({ op: "gap_verify",
     target: { symbol: SYM, start_date: gapTargetDay, end_date: gapTargetDay },
     subject: gapSubject,
     records: { [gapSubject.version_id]: unchangedBase },
     observed_at: OBSERVED });                                        // no_local_gap
op({ op: "gap_verify",
     target: { symbol: SYM, start_date: "1990-01-01", end_date: "1990-01-01" },
     subject: gapSubject,
     records: { [gapSubject.version_id]: unchangedBase },
     observed_at: OBSERVED });                                        // source_limitation
op({ op: "gap_verify",
     target: { symbol: SYM, start_date: gapTargetDay,
               end_date: fullBatch.bars[N_BARS - 2].date.s },
     subject: gapSubject,
     records: { [gapSubject.version_id]: unchangedBase },
     observed_at: OBSERVED });                                        // multi-day -> basis error
op({ op: "gap_verify",
     target: { symbol: SYM, start_date: gapTargetDay, end_date: gapTargetDay },
     subject: { version_id: unchangedBase.version_id,
                integrity_sha256: "0".repeat(64) },
     records: { [gapSubject.version_id]: unchangedBase },
     observed_at: OBSERVED });                                        // digest mismatch
op({ op: "gap_verify",
     target: { symbol: SYM, start_date: gapTargetDay, end_date: gapTargetDay },
     subject: gapSubject,
     records: { [gapSubject.version_id]: (() => {
       const r = baseRecord({ tamper: "strange-inputs" });
       return { ...r, version_id: unchangedBase.version_id };
     })() },
     observed_at: OBSERVED });                                        // inputs mismatch
op({ op: "gap_verify",
     target: { symbol: SYM,
               start_date: fullBatch.bars[0].date.s,
               end_date: fullBatch.bars[0].date.s },
     subject: gapSubject,
     records: { [gapSubject.version_id]: baseRecord({ tamper: "conflict" }) },
     observed_at: OBSERVED });                                        // overlap conflict

// --- run_item ---
const runItem = (id, repo, { task = "t1", it = "i1", run = "run-1" } = {}) =>
  op({ op: "run_item", task_id: task, item_id: it, run_id: run, repo });

// replay: receipt already stored
runItem("replay", {
  bindings: { i1: [binding()] },
  receipts: { i1: [receipt()] },
});
// unsupported contract
{
  const b = binding();
  b.head.key = { ...KEY, operation: "update_data" };
  runItem("bad-contract", { bindings: { i1: [b] }, tasks: {} });
}
// old-run visible -> needs_verification
runItem("old-run", {
  bindings: { i1: [binding(SYM, "i1", { accepted_run_id: "run-0" })] },
  receipts: { i1: [null] },
});
// running item -> outcome_unknown
runItem("running-item", {
  bindings: { i1: [binding(SYM, "i1", { status: "running", state_revision: 3,
                                       owner_run_id: "run-1" },
                           { stage: "running" })] },
  receipts: { i1: [null] },
});
// terminal + success outcome, no receipt -> integrity
runItem("terminal-success", {
  bindings: { i1: [binding(SYM, "i1", { status: "succeeded" },
                           { stage: "finished",
                             outcome: { success: true, error: null,
                                        evidence: null } })] },
  receipts: { i1: [null] },
});
// terminal + failed outcome -> not_published
runItem("terminal-failed", {
  bindings: { i1: [binding(SYM, "i1", { status: "failed" },
                           { stage: "finished",
                             outcome: { success: false, error: "x",
                                        evidence: null } })] },
  receipts: { i1: [null] },
  tasks: { t1: taskSnapshot({ status: "failed" }) },
});
// cancel_requested -> finishExecution -> not_published
runItem("cancel-req", {
  bindings: { i1: [binding(SYM, "i1", { status: "cancel_requested",
                                       owner_run_id: "run-1" })] },
  receipts: { i1: [null] },
  transitions: [{ head: { status: "cancelled", state_revision: 2 } }],
  tasks: { t1: taskSnapshot({ status: "cancelled" }) },
});
// happy path: pending -> claim -> startItem -> materialize -> stage ->
// prepare -> commit -> Published
runItem("publish", {
  bindings: { i1: [binding(),
                  binding(SYM, "i1", { status: "running", state_revision: 3,
                                       owner_run_id: "run-1" },
                          { stage: "running" })] },
  receipts: { i1: [null] },
  transitions: [
    { head: { status: "running", owner_run_id: "run-1", state_revision: 2,
              counts: { pending: 0 } } },
    { head: { state_revision: 3, counts: { running: 1 } },
      item: { stage: "running" } },
  ],
  heads: { t1: head({ status: "running", owner_run_id: "run-1",
                     state_revision: 3 }) },
  scopes: { [scopeId(SYM)]: null },
  commit: { ok: receipt() },
});
// effective head exists -> Invalid -> finishItem + finishExecution
runItem("existing-head", {
  bindings: { i1: [binding(),
                  binding(SYM, "i1", { status: "running", state_revision: 3,
                                       owner_run_id: "run-1" },
                          { stage: "running" })] },
  receipts: { i1: [null] },
  transitions: [
    { head: { status: "running", owner_run_id: "run-1", state_revision: 2,
              counts: { pending: 0 } } },
    { head: { state_revision: 3, counts: { running: 1 } },
      item: { stage: "running" } },
    { head: { state_revision: 4, counts: { running: 0, finished: 1 } },
      item: { stage: "finished",
              outcome: { success: false,
                         error: "INVALID_MARKET_COMMAND: existing daily head requires the update_data range path",
                         evidence: null } } },
    { head: { status: "failed", state_revision: 5 } },
  ],
  heads: { t1: head({ status: "running", owner_run_id: "run-1",
                     state_revision: 3 }) },
  scopes: { [scopeId(SYM)]: "existing-version" },
  tasks: { t1: taskSnapshot({ status: "failed" }) },
});
// commit error (storage) -> requireVerification -> rethrow
runItem("commit-storage-err", {
  bindings: { i1: [binding(),
                  binding(SYM, "i1", { status: "running", state_revision: 3,
                                       owner_run_id: "run-1" },
                          { stage: "running" }),
                  binding(SYM, "i1", { status: "needs_verification",
                                       state_revision: 4 },
                          { stage: "running" })] },
  receipts: { i1: [null, null] },
  transitions: [
    { head: { status: "running", owner_run_id: "run-1", state_revision: 2,
              counts: { pending: 0 } } },
    { head: { state_revision: 3, counts: { running: 1 } },
      item: { stage: "running" } },
    { head: { status: "needs_verification", state_revision: 4 } },
  ],
  heads: { t1: head({ status: "running", owner_run_id: "run-1",
                     state_revision: 3 }) },
  scopes: { [scopeId(SYM)]: null },
  commit: { err_kind: "storage", err: "disk full" },
});
// commit error then receipt appears -> Published (replay win)
runItem("commit-lost", {
  bindings: { i1: [binding()] },
  receipts: { i1: [null, receipt()] },
  transitions: [
    { head: { status: "running", owner_run_id: "run-1", state_revision: 2,
              counts: { pending: 0 } } },
    { head: { state_revision: 3, counts: { running: 1 } },
      item: { stage: "running" } },
  ],
  heads: { t1: head({ status: "running", owner_run_id: "run-1",
                     state_revision: 3 }) },
  scopes: { [scopeId(SYM)]: null },
  commit: { err_kind: "storage", err: "disk full" },
});
// task_head conflict mid-materialization -> cancelled before publication
runItem("cancel-during", {
  bindings: { i1: [binding(),
                  binding(SYM, "i1", { status: "cancel_requested",
                                       owner_run_id: "run-1",
                                       state_revision: 4 },
                          { stage: "running" })] },
  receipts: { i1: [null] },
  transitions: [
    { head: { status: "running", owner_run_id: "run-1", state_revision: 2,
              counts: { pending: 0 } } },
    { head: { state_revision: 3, counts: { running: 1 } },
      item: { stage: "running" } },
    { head: { state_revision: 5, counts: { running: 0, finished: 1 } },
      item: { stage: "finished",
              outcome: { success: false,
                         error: "cancelled before publication: PUBLISH_PRECONDITION: task changed or cancelled during materialization",
                         evidence: null } } },
    { head: { status: "cancelled", state_revision: 6 } },
  ],
  heads: { t1: head({ status: "cancel_requested", owner_run_id: "run-1",
                     state_revision: 4 }) },
  scopes: { [scopeId(SYM)]: null },
  tasks: { t1: taskSnapshot({ status: "cancelled" }) },
});
// missing binding -> Invalid + task_head probe
runItem("missing-binding", {
  bindings: { i1: [] },
  heads: { t1: head() },
});

// --- run_items ---
const candItem2 = item(SYM2, "i2");
const runItems = (id, repo, ids = ["i1"], gen = "gen3") =>
  op({ op: "run_items", task_id: "t1", item_ids: ids,
       generation_id: gen, run_id: "run-1", repo });
// all candidates -> batch commit ok
runItems("batch-ok", {
  bindings: { i1: [binding()], i2: [binding(SYM2, "i2")] },
  scopes: { [scopeId(SYM)]: null, [scopeId(SYM2)]: null },
  commit_collection_batch: { ok: [{ ok: receipt() }, { ok: receipt("t1", 5) }] },
}, ["i1", "i2"]);
// backend lacks batch surface -> per-item fallback
runItems("batch-none", {
  bindings: { i1: [binding(), binding(SYM, "i1",
                     { status: "running", state_revision: 3,
                       owner_run_id: "run-1" }, { stage: "running" })] },
  receipts: { i1: [null] },
  transitions: [
    { head: { status: "running", owner_run_id: "run-1", state_revision: 2,
              counts: { pending: 0 } } },
    { head: { state_revision: 3, counts: { running: 1 } },
      item: { stage: "running" } },
  ],
  heads: { t1: head({ status: "running", owner_run_id: "run-1",
                     state_revision: 3 }) },
  scopes: { [scopeId(SYM)]: null },
  commit_collection_batch: { none: true },
  commit: { ok: receipt() },
});
// per-item error -> that item falls back to run_item
runItems("batch-itemerr", {
  bindings: { i1: [binding(), binding(SYM, "i1",
                     { status: "running", state_revision: 3,
                       owner_run_id: "run-1" }, { stage: "running" })],
              i2: [binding(SYM2, "i2")] },
  receipts: { i1: [null] },
  transitions: [
    { head: { status: "running", owner_run_id: "run-1", state_revision: 2,
              counts: { pending: 0 } } },
    { head: { state_revision: 3, counts: { running: 1 } },
      item: { stage: "running" } },
  ],
  heads: { t1: head({ status: "running", owner_run_id: "run-1",
                     state_revision: 3 }) },
  scopes: { [scopeId(SYM)]: null, [scopeId(SYM2)]: null },
  commit_collection_batch: {
    ok: [{ ok: receipt() }, { err_kind: "conflict", err: "revision" }] },
  commit: { ok: receipt() },
});
// structural batch error -> all items fall back
runItems("batch-err", {
  bindings: { i1: [binding(), binding(SYM, "i1",
                     { status: "running", state_revision: 3,
                       owner_run_id: "run-1" }, { stage: "running" })] },
  receipts: { i1: [null] },
  transitions: [
    { head: { status: "running", owner_run_id: "run-1", state_revision: 2,
              counts: { pending: 0 } } },
    { head: { state_revision: 3, counts: { running: 1 } },
      item: { stage: "running" } },
  ],
  heads: { t1: head({ status: "running", owner_run_id: "run-1",
                     state_revision: 3 }) },
  scopes: { [scopeId(SYM)]: null },
  commit_collection_batch: { err_kind: "storage", err: "tx rollback" },
  commit: { ok: receipt() },
});
// candidate-null path (terminal head) -> run_item -> replay
runItems("batch-fallback-candidate-null", {
  bindings: { i1: [binding(SYM, "i1", { status: "succeeded" },
                   { stage: "finished",
                     outcome: { success: false, error: "x",
                                evidence: null } }),
                  binding(SYM, "i1", { status: "failed" },
                   { stage: "finished",
                     outcome: { success: false, error: "x",
                                evidence: null } })] },
  receipts: { i1: [null] },
  tasks: { t1: taskSnapshot({ status: "failed" }) },
  commit_collection_batch: { ok: [] },
});

writeFileSync(join(SCRATCH, "scenario.json"), J(SCEN));

// ------------------------------------------------------------ oracle run
const oracleOut = {};
{
  const r = spawnSync(ORACLE,
    [GEN3_MANIFEST, GEN3_ROOT, NOW, join(SCRATCH, "scenario.json")],
    { maxBuffer: 1 << 29, encoding: "utf8" });
  if (r.status !== 0) {
    throw new Error(`oracle failed (${r.status}): ${r.stderr?.slice(0, 600)}`);
  }
  for (const line of r.stdout.trim().split("\n")) {
    const row = JSON.parse(line);
    oracleOut[row.id] = row.observed ?? row;
  }
}

// ------------------------------------------------------------ node mirror
const errObserved = (e) => ({
  status: "error",
  kind: e instanceof SourceError ? e.kind
      : (e?.kind ?? e?.code) === "needs_verification"
        ? "task:NeedsVerification"
      : e instanceof MarketError ? e.kind
      : "other",
  error: String(e.message ?? e),
});
const okObserved = (v) => ({ status: "ok", value: v });
const acquisitionJson = (a) => ({
  batch: toWire(a.batch),
  evidence: toWire(a.evidence),
  combined: a.combined === null ? null : {
    record: toWire(a.combined.record),
    evidence: toWire(a.combined.evidence),
  },
});
const typedJson = (t) => t === null ? null : ({
  decision: t.decision === "Unchanged" ? "Unchanged"
    : { Append: { upserts: toWire(t.decision.Append.upserts) } },
  evidence: toWire(t.evidence),
});
const nd = (s) => s;
const rng = (v) => ({ start: v.start, end: v.end });
const nodeResults = {};
{
  const source2 = source; // same inspected source
  const session2 = session;
  SCEN.ops.forEach((o, i) => {
    const id = `${i}:${o.op}`;
    // run_item/run_items get their own block so the repo trace survives
    // thrown outcomes.
    if (o.op === "run_item" || o.op === "run_items") {
      const repo = new ScriptedRepo(o.repo);
      const runner = new TdxDailyRunner({
        source: source2, session: session2, repository: repo,
        runId: o.run_id ?? "run-1", publishedAt: parseTs(PUBLISHED).out,
      });
      try {
        if (o.op === "run_item") {
          const r = runner.runItem(o.task_id, o.item_id);
          nodeResults[id] = okObserved(
            r.kind === "published"
              ? { published: toWire(r.receipt) }
              : { not_published: toWire(r.task) });
        } else {
          const r = runner.runItems(o.task_id, o.item_ids, o.generation_id);
          nodeResults[id] = okObserved(r === null ? null : "some");
        }
      } catch (e) {
        nodeResults[id] = errObserved(e);
      }
      nodeResults[`${id}.trace`] = { ops: repo.ops };
      return;
    }
    try {
      switch (o.op) {
        case "acquire": {
          const base = RC.check(o.base);
          const p = new TdxRangeProvider(session2, parseTs(PUBLISHED).out);
          const r = p.acquire(base, rng(o.range), o.candidate_id);
          nodeResults[id] = okObserved(acquisitionJson(r));
          break;
        }
        case "acquire_typed": {
          const p = new TdxRangeProvider(session2, parseTs(PUBLISHED).out);
          // base_window arrives wire-form; the typed surface takes
          // parsed Bars (base_bars_in_range returns parsed records).
          const window = o.base_window.map(
            (b) => ({ ...b, date: RC.__parseDate(b.date) }));
          const r = p.acquireTyped(o.facts, window, rng(o.range));
          nodeResults[id] = okObserved(typedJson(r));
          break;
        }
        case "acquire_merged": {
          const base = RC.check(o.base);
          const P = o.provider === "backfill"
            ? TdxBackfillProvider : TdxRepublishProvider;
          const r = new P(session2, parseTs(PUBLISHED).out)
            .acquire(base, rng(o.range), o.candidate_id);
          nodeResults[id] = okObserved(acquisitionJson(r));
          break;
        }
        case "gap_verify": {
          const repo = new ScriptedRepo({ records: o.records });
          const v = new TdxGapVerifier(source2, repo, parseTs(OBSERVED).out);
          const b = v.verify(o.target, o.subject);
          nodeResults[id] = okObserved(toWire(b));
          break;
        }
        default:
          nodeResults[id] = { status: "error", kind: "bad_op" };
      }
    } catch (e) {
      nodeResults[id] = errObserved(e);
    }
  });
}

// ------------------------------------------------------------ compare
const rows = [];
let fails = 0;
for (const [id, expected] of Object.entries(oracleOut)) {
  const actual = nodeResults[id];
  if (actual === undefined) {
    rows.push({ id, verdict: "MISSING" });
    fails++;
    continue;
  }
  const same = canon(expected) === canon(actual);
  rows.push({
    id, verdict: same ? "PASS" : "FAIL",
    ...(same ? {} : { expected, actual }),
  });
  if (!same) fails++;
}
for (const id of Object.keys(nodeResults)) {
  if (!(id in oracleOut)) { rows.push({ id, verdict: "EXTRA" }); fails++; }
}
const resultsJsonl =
  rows.map((r) => J(r)).join("\n") + "\n";
const summary = {
  task: "NM-101", total: rows.length, pass: rows.length - fails, fail: fails,
};
if (OUT) {
  mkdirSync(OUT, { recursive: true });
  writeFileSync(join(OUT, "results.jsonl"), resultsJsonl);
  writeFileSync(join(OUT, "scenario.json"), J(SCEN, null, 2));
}
process.stdout.write(
  `NM-101 runner differential: ${summary.pass}/${summary.total} PASS\n`);
if (fails) {
  for (const r of rows.filter((r) => r.verdict !== "PASS").slice(0, 8)) {
    process.stdout.write(`  ${r.verdict} ${r.id}\n`);
    if (r.verdict === "FAIL") {
      process.stdout.write(`    expected: ${canon(r.expected).slice(0, 500)}\n`);
      process.stdout.write(`    actual:   ${canon(r.actual).slice(0, 500)}\n`);
    }
  }
}
rmSync(SCRATCH, { recursive: true, force: true });
process.exit(SELFTEST ? 1 : (fails ? 1 : 0));
