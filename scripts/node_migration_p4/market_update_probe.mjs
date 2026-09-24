// NM-082 Node probe: replays the scenario JSONL through the ported
// decision layer and diffs against the Rust oracle's golden JSONL.
//   node market_update_probe.mjs <scenarios.jsonl> <golden.jsonl>
// The StubRepo/StubProvider/runItem here are scenario scaffolding that
// mirror the oracle's stub semantics (call log + write-back state
// machine + the run_item decision path, which stays unported in the
// lane pending S4/S5 storage commits). Every function under test is the
// real ported code.
import { readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import * as MU from "../../src_node/market_update/mod.mjs";
import * as SER from "../../src_node/market_update/serde.mjs";
import * as TASKS from "../../src_node/market_update/tasks.mjs";
import * as PUB from "../../src_node/market_update/publication.mjs";
import * as RUN from "../../src_node/market_update/runner.mjs";
import { check } from "../../src_node/market/record_codec.mjs";
import { decideHead } from "../../src_node/storage/task_repository.mjs";
import { serValue, serF64, F64 } from "../../src_node/mcp/serde_json.mjs";

const sha = (s) => createHash("sha256").update(s).digest("hex");
const NOW = "2024-01-01T00:00:00Z";

// ------------------------------------------------------- rust fmt bits
const dbgStr = (s) => '"' + s.replace(/[\\"\n\r\t]|[\x00-\x1f\x7f]/g, (c) =>
  c === '"' ? '\\"' : c === "\\" ? "\\\\" : c === "\n" ? "\\n" : c === "\r" ? "\\r"
    : c === "\t" ? "\\t" : `\\u{${c.codePointAt(0).toString(16)}}`) + '"';
const dbgNum = (v) =>
  typeof v === "bigint" ? String(v) : Number.isInteger(v) ? String(v) : serF64(v);
const dbgValue = (v) =>
  v === null || v === undefined ? "Null" :
  typeof v === "boolean" ? `Bool(${v})` :
  typeof v === "number" || typeof v === "bigint" ? `Number(${dbgNum(v)})` :
  typeof v === "string" ? `String(${dbgStr(v)})` :
  Array.isArray(v) ? `Array [${v.map(dbgValue).join(", ")}]` :
  `Object {${Object.keys(v).sort().map((k) => `${dbgStr(k)}: ${dbgValue(v[k])}`).join(", ")}}`;
const dbgOpt = (v, f) => (v == null ? "None" : `Some(${f(v)})`);
const dbgDec = (d) => SER.decCanon(d);
const dbgBar = (b) =>
  `Bar { date: ${b.date.s}, open: ${dbgDec(b.open)}, high: ${dbgDec(b.high)}, low: ${dbgDec(b.low)}, ` +
  `close: ${dbgDec(b.close)}, volume: ${dbgOpt(b.volume, dbgDec)}, amount: ${dbgOpt(b.amount, dbgDec)}, ` +
  `r7_raw: ${dbgOpt(b.r7_raw, dbgNum)} }`;
const dbgEv = (e) =>
  `EvidenceValue { status: ${e.status}, value: ${dbgOpt(e.value, dbgStr)}, reason: ${dbgStr(e.reason)} }`;
const dbgAdj = (e) =>
  `AdjustmentEvidence { status: ${e.status}, value: ${dbgOpt(e.value, dbgStr)}, reason: ${dbgStr(e.reason)} }`;
const dbgCov = (c) => `CoverageEvidence { status: ${c.status}, reason: ${dbgStr(c.reason)} }`;
const dbgUC = (u) =>
  `UnitCurrencyEvidence { price_currency: ${dbgEv(u.price_currency)}, price_unit: ${dbgEv(u.price_unit)}, ` +
  `volume_unit: ${dbgEv(u.volume_unit)}, amount_currency: ${dbgEv(u.amount_currency)}, amount_unit: ${dbgEv(u.amount_unit)} }`;
const dbgDeriv = (d) => dbgOpt(d, (x) =>
  `YearlyDerivation { base_daily_version_id: ${dbgStr(x.base_daily_version_id)}, ` +
  `algorithm_version: ${dbgStr(x.algorithm_version)}, first_trading_date: ${x.first_trading_date.s ?? x.first_trading_date}, ` +
  `last_trading_date: ${x.last_trading_date.s ?? x.last_trading_date} }`);
const dbgSnap = (s) =>
  `SnapshotInputBinding { role: ${dbgStr(s.role)}, member: ${dbgStr(s.member)}, ` +
  `archive_sha256: ${dbgStr(s.archive_sha256)}, member_sha256: ${dbgStr(s.member_sha256)} }`;
const dbgProv = (p) =>
  `Provenance { source: ${dbgStr(p.source)}, source_period: ${dbgStr(p.source_period)}, ` +
  `source_adjustment: ${dbgStr(p.source_adjustment)}, symbol: ${dbgStr(p.symbol)}, acquired_at: ${p.acquired_at}, ` +
  `adapter_version: ${dbgStr(p.adapter_version)}, yearly_derivation: ${dbgDeriv(p.yearly_derivation)}, ` +
  `snapshot_inputs: ${dbgOpt(p.snapshot_inputs, (l) => `[${l.map(dbgSnap).join(", ")}]`)} }`;
const dbgBatch = (b) =>
  `MarketDataBatch { symbol: ${dbgStr(b.symbol)}, period: ${b.period}, adjustment: ${b.adjustment}, ` +
  `bars: [${b.bars.map(dbgBar).join(", ")}], provenance: ${dbgProv(b.provenance)}, coverage: ${dbgCov(b.coverage)}, ` +
  `unit_currency: ${dbgUC(b.unit_currency)}, adjustment_evidence: ${dbgAdj(b.adjustment_evidence)} }`;
const dbgValidation = (v) =>
  `ValidationSummary { record_count: ${v.record_count}, unique_record_count: ${v.unique_record_count}, ` +
  `duplicate_count: ${v.duplicate_count} }`;
const dbgVersion = (v) =>
  `EffectiveVersion { version_id: ${dbgStr(v.version_id)}, symbol: ${dbgStr(v.symbol)}, period: ${v.period}, ` +
  `adjustment: ${v.adjustment}, source: ${dbgStr(v.source)}, records: [${v.records.map(dbgBar).join(", ")}], ` +
  `published_at: ${v.published_at}, validation: ${dbgValidation(v.validation)}, ` +
  `unit_currency: ${dbgUC(v.unit_currency)}, adjustment_evidence: ${dbgAdj(v.adjustment_evidence)}, ` +
  `yearly_derivation: ${dbgDeriv(v.yearly_derivation)} }`;
const dbgChecked = (r) =>
  `CheckedRecord { raw: ${dbgValue(r.raw)}, version: ${dbgVersion(r.version)}, batch: ${dbgBatch(r.batch)} }`;
const dbgScope = (s) => `Scope { symbol: ${dbgStr(s.symbol)}, period: ${s.period} }`;
const dbgIntent = (i) =>
  i === "HistoricalFixedBase" ? "HistoricalFixedBase" :
  "Daily" in i ? `Daily { expected_head: ${dbgOpt(i.Daily.expected_head, dbgStr)} }` :
  `CurrentHead { expected_daily_head: ${dbgStr(i.CurrentHead.expected_daily_head)}, expected_yearly_head: ${dbgOpt(i.CurrentHead.expected_yearly_head, dbgStr)} }`;
const dbgPublication = (p) =>
  `Publication { publication_id: ${dbgStr(p.publication_id)}, version_id: ${dbgStr(p.version_id)}, ` +
  `task_id: ${dbgStr(p.task_id)}, item_id: ${dbgStr(p.item_id)}, run_id: ${dbgStr(p.run_id)}, ` +
  `expected_revision: ${p.expected_revision}, intent: ${dbgIntent(p.intent)} }`;
const dbgPlan = (plan) =>
  `PublishPlan { command: ${dbgPublication(plan.command)}, record: ${dbgChecked(plan.record)}, ` +
  `required_heads: [${plan.required_heads.map(([s, h]) => `(${dbgScope(s)}, ${dbgOpt(h, dbgStr)})`).join(", ")}], ` +
  `promote: ${dbgOpt(plan.promote, dbgScope)}, base_digest: ${dbgOpt(plan.base_digest, dbgStr)} }`;
const dbgPayload = (p) => `Payload { id: ${dbgStr(p.id)}, json: ${dbgStr(p.json)} }`;
const dbgRangePlan = (p) =>
  `RangePlan { publication: ${dbgPlan(p.publication)}, base: ${dbgChecked(p.base)}, no_change: ${p.no_change}, ` +
  `audit: ${dbgValue(p.audit)}, audit_payloads: [${p.audit_payloads.map(dbgPayload).join(", ")}] }`;
const dbgOutcome = (o) =>
  `ItemOutcome { success: ${o.success}, error: ${dbgOpt(o.error, dbgStr)}, evidence: ${dbgOpt(o.evidence, dbgValue)} }`;
const dbgChange = (c) =>
  c.kind === "claim" ? "Claim" :
  c.kind === "requestCancel" ? "RequestCancel" :
  c.kind === "reconcile" ? "Reconcile" :
  c.kind === "startItem" ? `StartItem(${dbgStr(c.item_id)})` :
  c.kind === "finishItem" ? `FinishItem(${dbgStr(c.item_id)}, ${dbgOutcome(c.outcome)})` :
  c.kind === "finishExecution" ? "FinishExecution" :
  c.kind === "finishCancellation" ? "FinishCancellation" :
  c.kind === "requireVerification" ?
    `RequireVerification { item_id: ${dbgStr(c.item_id)}, reason: ${dbgStr(c.reason)} }` : "?";
const ds = (d) => d.s ?? d;
const reasonDebug = (r) =>
  r.kind === "coverage_unknown" ? "CoverageUnknown" :
  r.kind === "basis_unknown" ? "BasisUnknown" :
  r.kind === "units_incompatible" ? "UnitsIncompatible" :
  r.kind === "missing_old_dates" ? `MissingOldDates([${r.dates.map(ds).join(", ")}])` :
  r.kind === "known_value_lost" ? `KnownValueLost([${r.dates.map(ds).join(", ")}])` :
  `RevisionUnverified([${r.dates.map(ds).join(", ")}])`;

// ------------------------------------------------------------- errors
const DISPLAY = {
  storage: "MARKET_STORAGE", integrity: "MARKET_INTEGRITY", base: "BASE_UNAVAILABLE",
  conflict: "PUBLISH_PRECONDITION", not_found: "MARKET_NOT_FOUND",
  invalid: "INVALID_MARKET_COMMAND", outcome_unknown: "PUBLISH_OUTCOME_UNKNOWN",
  basis_not_verified: "GAP_BASIS_NOT_VERIFIED",
};
const TC_KIND_CODES = new Set([
  "invalid_input", "request_conflict", "range_conflict", "needs_verification",
  "unavailable", "outcome_unknown", "revision_conflict", "unsupported_contract",
  "task_not_found", "invalid_retry_selection",
]);
const newSourceError = (kind, detail) => new MU.SourceError(kind, detail);
const mkErr = (kind, msg) =>
  new MU.MarketError(kind, kind === "not_found" ? "MARKET_NOT_FOUND"
    : kind === "outcome_unknown" ? `PUBLISH_OUTCOME_UNKNOWN: ${msg}; inspect the same publication_id`
    : `${DISPLAY[kind]}: ${msg}`);
const rowErr = (e) => {
  if (e instanceof MU.MarketError) {
    if (e.isTaskError) return { code: e.code, message: e.message };
    return { code: e.kind, message: e.message };
  }
  if (e.taskControl === true) return { code: e.code, message: e.message };
  if (typeof e?.code === "string" && TC_KIND_CODES.has(e.code)) {
    return { code: e.code, message: e.message };
  }
  if (e instanceof SER.Decode) return { message: e.message };
  throw e;
};

// ------------------------------------------------------------ stub repo
const TERMINAL = ["succeeded", "failed", "partially_failed", "cancelled"];
const clone = (v) => (v === undefined ? v : JSON.parse(JSON.stringify(v, (_k, x) =>
  typeof x === "bigint" ? { $bigint: String(x) } : x), (_k, x) =>
  x && x.$bigint !== undefined ? BigInt(x.$bigint) : x));
const mkHead = (c) => ({
  task_id: c.task_id,
  key: { operation: c.operation, dataset: c.dataset ?? "market.daily",
    contract_version: c.contract_version },
  status: c.status, state_revision: BigInt(c.state_revision),
  accepted_run_id: c.accepted_run_id, owner_run_id: c.owner_run_id ?? null,
  last_progress_at: NOW,
  result: c.result ?? null,
  counts: { total: BigInt(c.counts.total), pending: BigInt(c.counts.pending),
    running: BigInt(c.counts.running), finished: BigInt(c.counts.finished),
    succeeded: BigInt(c.counts.succeeded) },
  evidence_digest: c.evidence_digest ?? null,
});
const mkItem = (c) => ({
  spec: { item_id: c.item_id, target: c.target,
    progress_unit: c.progress_unit ?? "security_period",
    frozen_inputs: c.frozen_inputs ?? null, scope_keys: c.scope_keys ?? [] },
  stage: c.stage,
  outcome: c.outcome == null ? null : { success: c.outcome.success,
    error: c.outcome.error ?? null,
    ...(c.outcome.evidence !== undefined && c.outcome.evidence !== null
      ? { evidence: c.outcome.evidence } : {}) },
});
class StubRepo {
  constructor(cfg) {
    this.cfg = cfg;
    this.head = cfg.head != null ? mkHead(cfg.head) : null;
    this.item = cfg.item != null ? mkItem(cfg.item) : null;
    this.calls = [];
  }
  log(v) { this.calls.push(v); }
  errAt(key) {
    const e = this.cfg.errors?.[key];
    return e ? mkErr(e.kind, e.message ?? "") : null;
  }
  stage() { this.log({ call: "stage" }); throw mkErr("storage", "unimplemented stage"); }
  candidate(id) {
    this.log({ call: "candidate", id });
    const e = this.errAt("candidate"); if (e) throw e;
    if (this.cfg.candidate == null) throw mkErr("storage", "UNEXPECTED candidate");
    return check(this.cfg.candidate);
  }
  mark_validated(record) {
    this.log({ call: "mark_validated", version_id: record.version().version_id });
    const e = this.errAt("mark_validated"); if (e) throw e;
  }
  fixed(id) {
    this.log({ call: "fixed", id });
    const e = this.errAt("fixed"); if (e) throw e;
    if (this.cfg.fixed == null) throw mkErr("storage", "UNEXPECTED fixed");
    return check(this.cfg.fixed);
  }
  fixed_many(ids) {
    this.log({ call: "fixed_many", ids });
    throw mkErr("storage", "UNEXPECTED fixed_many");
  }
  fixed_facts(id) {
    this.log({ call: "fixed_facts", id });
    const e = this.errAt("fixed_facts"); if (e) throw e;
    const f = this.cfg.facts_by_id?.[id] ?? this.cfg.facts;
    if (f == null) throw mkErr("storage", "UNEXPECTED fixed_facts");
    return {
      version_id: f.version_id, symbol: f.symbol, period: f.period,
      integrity_sha256: f.integrity_sha256,
      first_day: SER.tDate(f.first_day), last_day: SER.tDate(f.last_day),
    };
  }
  fixed_window(id, a, b) {
    this.log({ call: "fixed_window", id, a: a.s ?? a, b: b.s ?? b });
    const e = this.errAt("fixed_window"); if (e) throw e;
    if (this.cfg.window == null) throw mkErr("storage", "UNEXPECTED fixed_window");
    return check(this.cfg.window);
  }
  fixed_window_years() { throw mkErr("storage", "UNEXPECTED fixed_window_years"); }
  task(id) {
    this.log({ call: "task", id });
    const e = this.errAt("task"); if (e) throw e;
    if (!this.head) throw mkErr("storage", "UNEXPECTED task");
    const h = this.head;
    return {
      task: {
        task_id: h.task_id, key: { operation: h.key.operation, dataset: h.key.dataset,
          contract_version: h.key.contract_version },
        status: h.status, state_revision: Number(h.state_revision),
        accepted_run_id: h.accepted_run_id, owner_run_id: h.owner_run_id,
        created_at: NOW, last_progress_at: NOW,
        work: this.cfg.work ?? { items: [], scopes: [] },
        result: h.result ?? null, retry_of: null,
      },
      items: this.item ? [{ spec: this.item.spec, stage: this.item.stage,
        outcome: this.item.outcome == null ? null
          : { success: this.item.outcome.success, error: this.item.outcome.error,
              ...(this.item.outcome.evidence != null ? { evidence: this.item.outcome.evidence } : {}) } }] : [],
      claims: [],
    };
  }
  task_head(id) {
    this.log({ call: "task_head", id });
    const e = this.errAt("task_head"); if (e) throw e;
    if (!this.head) throw MU.tcError("task_not_found", "TASK_NOT_FOUND");
    return clone(this.head);
  }
  task_item_binding(task, item) {
    this.log({ call: "task_item_binding", task, item });
    const e = this.errAt("task_item_binding"); if (e) throw e;
    if (this.head && this.item && this.head.task_id === task && this.item.spec.item_id === item) {
      return { head: clone(this.head), item: clone(this.item),
        scopes_bound: this.cfg.scopes_bound ?? true };
    }
    return null;
  }
  transition_task_head(head, item, run, change) {
    this.log({ call: "transition_task_head", change: dbgChange(change) });
    decideHead(head, item ?? null, run, change, NOW);
    this.head = clone(head);
    if (item) this.item = clone(item);
  }
  commit() { throw mkErr("storage", "UNEXPECTED commit"); }
  receipt(id) { this.log({ call: "receipt", id }); throw mkErr("storage", "UNEXPECTED receipt"); }
  range() { throw mkErr("storage", "UNEXPECTED range"); }
  versions() { throw mkErr("storage", "UNEXPECTED versions"); }
  commit_range() { throw mkErr("storage", "UNEXPECTED commit_range"); }
  commit_range_ops() { return null; }
  commit_range_ops_typed() { return null; }
  commit_range_no_change_typed() { return null; }
  update_base_facts(id) {
    this.log({ call: "update_base_facts", id });
    const e = this.errAt("update_base_facts"); if (e) throw e;
    const f = this.cfg.base_facts;
    if (f == null) return null;
    return {
      version_id: f.version_id, symbol: f.symbol, record_count: f.record_count,
      first_day: SER.tDate(f.first_day), last_day: SER.tDate(f.last_day),
      integrity_sha256: f.integrity_sha256,
      coverage: MU.tCoverage(f.coverage),
      unit_currency: MU.tUnitCurrency(f.unit_currency),
      source: f.source, generation_id: f.generation_id ?? "g",
      generation_manifest_sha256: f.generation_manifest_sha256 ?? "m",
      day_member_sha256: f.day_member_sha256 ?? "d",
      security_event_sha256: f.security_event_sha256 ?? "s",
    };
  }
  base_bars_in_range(id, a, b) {
    this.log({ call: "base_bars_in_range", id, a: a.s ?? a, b: b.s ?? b });
    const e = this.errAt("base_bars_in_range"); if (e) throw e;
    if (this.cfg.base_bars == null) return null;
    return this.cfg.base_bars.map(MU.tBar);
  }
  commit_update_batch(task_id, items) {
    this.log({ call: "commit_update_batch", task_id,
      items: items.map((i) => ({
        task_id: i.task_id, item_id: i.item_id, publication_id: i.publication_id,
        run_id: i.run_id, symbol: i.symbol, occupancy_key: i.occupancy_key,
        prepared: i.prepared.kind === "no_change"
          ? { kind: "no_change", base_version_id: i.prepared.base_version_id,
              range_start_day: i.prepared.range_start_day, range_end_day: i.prepared.range_end_day,
              continuity: i.prepared.continuity ?? null }
          : { kind: "append", base_version_id: i.prepared.base_version_id,
              result_version_id: i.prepared.result_version_id,
              upserts: i.prepared.upserts.map(SER.barValue),
              continuity: i.prepared.continuity ?? null } })) });
    const e = this.errAt("commit_update_batch"); if (e) throw e;
    if (this.cfg.commit === "none") return null;
    return (this.cfg.commit_results ?? []).map((r) =>
      r.receipt ? { ok: { ...r.receipt } }
        : { err: mkErr(r.error.kind, r.error.message ?? "") });
  }
  range_receipt(id) { this.log({ call: "range_receipt", id }); throw mkErr("storage", "UNEXPECTED range_receipt"); }
  item_receipt(task, item) {
    this.log({ call: "item_receipt", task, item });
    const e = this.errAt("item_receipt"); if (e) throw e;
    return this.cfg.item_receipt ?? null;
  }
  transition_task(task, rev, run, change) {
    this.log({ call: "transition_task", task, rev, run, change: dbgChange(change) });
    throw mkErr("storage", "UNEXPECTED transition_task");
  }
  cover_gap(c) {
    this.log({ call: "cover_gap", gap_id: c.gap_id, version_id: c.version_id });
    const e = this.errAt("cover_gap"); if (e) throw e;
    return true;
  }
  gap_coverings(gap_id) {
    this.log({ call: "gap_coverings", gap_id });
    const e = this.errAt("gap_coverings"); if (e) throw e;
    return (this.cfg.coverings?.[gap_id] ?? []).map((c) => ({
      gap_id: c.gap_id, version_id: c.version_id,
      covered_at: c.covered_at ?? NOW }));
  }
}

// ------------------------------------------------------- stub provider
class StubProvider {
  constructor(cfg) { this.cfg = cfg; }
  acquire(base, range, candidate_id) {
    const a = this.cfg.acquire;
    if (a?.error) throw newSourceError(a.error.kind, a.error.detail ?? "");
    return {
      batch: MU.parseBatch(a.batch),
      evidence: MU.normalizeEvidence(a.evidence),
      combined: a.combined == null ? null : {
        record: a.combined.record, evidence: {
          base_integrity_sha256: a.combined.evidence.base_integrity_sha256,
          segment_sha256: a.combined.evidence.segment_sha256,
          combined_integrity_sha256: a.combined.evidence.combined_integrity_sha256,
          complete_coverage_ref: a.combined.evidence.complete_coverage_ref } },
    };
  }
  acquire_typed(facts, window, range) {
    const a = this.cfg.typed;
    if (a == null) return null;
    if (a.error) throw newSourceError(a.error.kind, a.error.detail ?? "");
    return {
      decision: a.decision.kind === "unchanged" ? { kind: "unchanged" }
        : { kind: "append", upserts: a.decision.upserts.map(MU.tBar) },
      evidence: MU.normalizeEvidence(a.evidence),
    };
  }
}

// ------------------------------------------------- probe-local run_item
// Faithful replica of runner.rs run_item's observable decision path —
// used only to drive run_items fallback vectors. The ported lane keeps
// run_item unported (storage-commit lane S4/S5); the oracle runs the
// real Rust, so divergence surfaces as a diff.
const visibleStatus = PUB.visibleStatus;
const runItem = (repo, runId, taskId, itemId, provider) => {
  const runner = new RUN.RangeRunner({ repository: repo, run_id: runId });
  const bindingOf = () => runner.binding(taskId, itemId, "range");
  const binding = bindingOf();
  const head = binding.head;
  const operation = head.key.operation;
  const defaultContract = head.key.contract_version === TASKS.DEFAULT_CONTRACT_VERSION;
  const supported = (operation === "update_data" || operation === "backfill_data" ||
      operation === "republish_data") &&
    head.key.dataset === "market.daily" &&
    (head.key.contract_version === TASKS.CONTRACT_VERSION || defaultContract ||
     head.key.contract_version === TASKS.REPUBLISH_CONTRACT_VERSION);
  if (!supported) throw MU.invalid("unsupported range execution contract");
  const receipt0 = repo.item_receipt(taskId, itemId);
  if (receipt0) {
    if (defaultContract) {
      runner.ensureDefaultCoverings(binding.item.spec.frozen_inputs, receipt0.version_id);
    }
    return { kind: "published", receipt: receipt0 };
  }
  if (visibleStatus(head.status, head.accepted_run_id, head.owner_run_id ?? null, runId)
      === "needs_verification") {
    throw MU.taskErr(MU.tcNeedsVerification("old run or unresolved execution"));
  }
  if (binding.item.stage === "running") {
    throw MU.outcomeUnknown("running item has no verified result; inspect before any retry");
  }
  if (TERMINAL.includes(head.status) || binding.item.stage === "finished") {
    if (binding.item.outcome != null && binding.item.outcome.success === true) {
      throw MU.integrity("successful range item has no receipt");
    }
    return { kind: "not_published", snapshot: repo.task(taskId) };
  }
  if (head.status === "cancel_requested") {
    repo.transition_task_head(binding.head, null, runId, { kind: "finishExecution" });
    return { kind: "not_published", snapshot: repo.task(taskId) };
  }
  if (head.status === "pending") {
    repo.transition_task_head(binding.head, null, runId, { kind: "claim" });
  }
  repo.transition_task_head(binding.head, binding.item, runId,
    { kind: "startItem", item_id: itemId });
  const commitAcquired = (base_id, base_digest, range) => {
    // commit_typed_range — typed ops-first (runner.rs:697+)
    const facts = repo.update_base_facts(base_id);
    if (facts !== null) {
      const window = repo.base_bars_in_range(base_id, range.start, range.end);
      if (window !== null) {
        let typed;
        try { typed = provider.acquire_typed(facts, window, range); }
        catch (e) { throw MU.invalid(e.message); }
        if (typed !== null) {
          // Same post-acquisition task guard as the envelope path.
          const current = repo.task_head(binding.head.task_id);
          if (current.status !== "running" ||
              current.state_revision !== binding.head.state_revision) {
            throw MU.conflict("task changed or cancelled during acquisition");
          }
          const id = sha(`["${binding.head.task_id}","${itemId}"]`);
          const command = {
            publication_id: `range-result-${id}`,
            version_id: typed.decision.kind === "append" ? `range-version-${id}` : base_id,
            task_id: binding.head.task_id, item_id: itemId, run_id: runId,
            expected_revision: binding.head.state_revision,
            intent: { Daily: { expected_head: base_id } },
          };
          const e0 = typed.evidence;
          if (e0.base_version_id !== base_id ||
              e0.base_integrity_sha256 !== facts.integrity_sha256 ||
              !SER.dateEq(e0.range.start, range.start) ||
              !SER.dateEq(e0.range.end, range.end)) {
            throw MU.invalid(`NEEDS_VERIFICATION: ${reasonDebug({ kind: "basis_unknown" })}`);
          }
          const scope = { symbol: facts.symbol, period: "Day" };
          const cont = SER.evidenceValue(e0);
          if (typed.decision.kind === "unchanged") {
            const r = repo.commit_range_no_change_typed(command, base_id, range, cont);
            if (r !== null) return r;
          }
          const r = repo.commit_range_ops_typed(command, scope, base_id,
            typed.decision.upserts, [], cont);
          if (r !== null) return r;
        }
      }
    }
    // envelope path (runner.rs:596+)
    const base = repo.fixed(base_id);
    if (base.raw.integrity_sha256 !== base_digest) {
      throw MU.integrity("fixed base differs from accepted digest");
    }
    const id = sha(`["${binding.head.task_id}","${itemId}"]`);
    const candidate_id = `range-version-${id}`;
    let acquired;
    try { acquired = provider.acquire(base, range, candidate_id); }
    catch (e) { throw MU.invalid(e.message); }
    const current = repo.task_head(binding.head.task_id);
    if (current.status !== "running" || current.state_revision !== binding.head.state_revision) {
      throw MU.conflict("task changed or cancelled during acquisition");
    }
    const decision = MU.prepare(base, acquired.batch, range, acquired.evidence);
    if (decision.kind === "needs_verification") {
      throw MU.invalid(`NEEDS_VERIFICATION: ${reasonDebug(decision.reason)}`);
    }
    const prepared = decision.kind === "candidate" ? decision : null;
    if (prepared && !prepared.metadata_changed) {
      // try_ops_commit (runner.rs:804+): frozen-inputs binding, combined
      // gate, upserts = merged bars whose base twin differs/absent.
      const command = {
        publication_id: `range-result-${id}`, version_id: candidate_id,
        task_id: binding.head.task_id, item_id: itemId, run_id: runId,
        expected_revision: binding.head.state_revision,
        intent: { kind: "Daily", expected_head: base_id } };
      PUB.checkFrozenInputs(repo, command, acquired.evidence, range);
      const whole = acquired.combined;
      if (!whole) throw MU.invalid("NEEDS_VERIFICATION: combined coverage absent");
      if (whole.record.version_id !== candidate_id ||
          whole.evidence.base_integrity_sha256 !== prepared.base_integrity_sha256 ||
          whole.evidence.segment_sha256 !== prepared.candidate_sha256 ||
          !whole.evidence.complete_coverage_ref.trim()) {
        throw MU.invalid(`NEEDS_VERIFICATION: ${reasonDebug({ kind: "coverage_unknown" })}`);
      }
      const baseDates = new Map(base.version.records.map((b) => [b.date.s, b]));
      const preparedDates = new Set(prepared.records.map((b) => b.date.s));
      const upserts = prepared.records.filter((b) => {
        const p = baseDates.get(b.date.s);
        return !p || !MU.barEq(p, b);
      });
      const removes = base.version.records
        .filter((b) => !preparedDates.has(b.date.s)).map((b) => b.date);
      const scope = { symbol: base.version.symbol, period: "Day" };
      const r = repo.commit_range_ops(command, scope, base_id, upserts, removes,
        prepared.records);
      if (r !== null) return r;
    }
    const changed = prepared !== null;
    if (changed) {
      const whole = acquired.combined;
      if (!whole) throw MU.invalid("NEEDS_VERIFICATION: combined coverage absent");
      if (whole.record.version_id !== candidate_id) {
        throw MU.invalid("unexpected candidate version identity");
      }
      repo.stage(whole.record);
    }
    const plan = PUB.prepareRange(repo, {
      publication_id: `range-result-${id}`,
      version_id: changed ? candidate_id : base_id,
      task_id: binding.head.task_id, item_id: itemId, run_id: runId,
      expected_revision: binding.head.state_revision,
      intent: { Daily: { expected_head: base_id } } },
      acquired.batch, range, acquired.evidence,
      acquired.combined ? acquired.combined.evidence : null);
    if (plan.kind === "needs_verification") {
      throw MU.invalid(`NEEDS_VERIFICATION: ${reasonDebug(plan.reason)}`);
    }
    return repo.commit_range(plan);
  };
  const tryAcquire = () => {
    const frozen = binding.item.spec.frozen_inputs;
    if (defaultContract) {
      // acquire_and_commit_default (runner.rs:502-534)
      const f = TASKS.parseDefaultFrozen(frozen);
      if (operation === "update_data" && f.refresh_range == null) {
        throw MU.integrity("default update item missing frozen refresh range");
      }
      if (operation === "backfill_data" && f.refresh_range != null) {
        throw MU.integrity("default backfill item carries a refresh range");
      }
      const range = TASKS.union(f);
      const receipt = commitAcquired(f.base_version_id, f.base_integrity_sha256, range);
      runner.ensureDefaultCoverings(frozen, receipt.version_id);
      return receipt;
    }
    // acquire_and_commit (runner.rs:475-493)
    const base_id = typeof frozen?.base_version_id === "string" && frozen.base_version_id !== ""
      ? frozen.base_version_id : null;
    if (base_id === null) throw MU.integrity("frozen base missing");
    const base_digest = typeof frozen?.base_integrity_sha256 === "string" ? frozen.base_integrity_sha256 : null;
    if (base_digest === null) throw MU.integrity("frozen base digest missing");
    let rs, re;
    try { rs = SER.tDate(frozen?.range?.start); } catch (e) { throw MU.integrity(e.message); }
    try { re = SER.tDate(frozen?.range?.end); } catch (e) { throw MU.integrity(e.message); }
    return commitAcquired(base_id, base_digest, { start: rs, end: re });
  };
  let outcome;
  try { outcome = { ok: tryAcquire() }; } catch (e) { outcome = { err: e }; }
  if (outcome.ok !== undefined) return { kind: "published", receipt: outcome.ok };
  const error = outcome.err;
  // Err arm (runner.rs:167-238) verbatim.
  const r2 = repo.item_receipt(taskId, itemId);
  if (r2) {
    if (defaultContract) {
      runner.ensureDefaultCoverings(binding.item.spec.frozen_inputs, r2.version_id);
    }
    return { kind: "published", receipt: r2 };
  }
  const nv = error instanceof MU.MarketError &&
    (error.kind === "outcome_unknown" || error.kind === "storage" ||
     (error.isTaskError && (error.code === "unavailable" || error.code === "outcome_unknown")));
  if (nv) {
    const reb = bindingOf();
    try {
      repo.transition_task_head(reb.head, reb.item, runId,
        { kind: "requireVerification", item_id: itemId,
          reason: "item execution result not confirmed after acquisition/derivation/publication; committed receipt not yet established" });
    } catch (recording) {
      const r3 = repo.item_receipt(taskId, itemId);
      if (r3) return { kind: "published", receipt: r3 };
      throw recording;
    }
    throw error;
  }
  const reb = bindingOf();
  const cancelled = error.kind === "conflict" && reb.head.status === "cancel_requested";
  repo.transition_task_head(reb.head, reb.item, runId,
    { kind: "finishItem", item_id: itemId,
      outcome: { success: false,
        error: cancelled ? `cancelled before publication: ${error.message}` : error.message,
        evidence: null } });
  if (reb.head.counts.finished === reb.head.counts.total ||
      reb.head.status === "cancel_requested") {
    repo.transition_task_head(reb.head, null, runId,
      { kind: cancelled ? "finishCancellation" : "finishExecution" });
  }
  return { kind: "not_published", snapshot: repo.task(taskId) };
};

// -------------------------------------------------------- emit helpers
const evidenceOfCase = (v) => MU.normalizeEvidence(v);
const reasonJson = (r) =>
  r.kind === "missing_old_dates" || r.kind === "known_value_lost" || r.kind === "revision_unverified"
    ? { kind: r.kind, dates: r.dates.map((d) => d.s ?? d) }
    : { kind: r.kind };
const decisionJson = (d) => {
  if (d.kind === "no_change") return { kind: "no_change", version_id: d.version_id };
  if (d.kind === "needs_verification") {
    return { kind: "needs_verification", reason: reasonJson(d.reason), debug: reasonDebug(d.reason) };
  }
  return {
    kind: "candidate",
    records: `[${d.records.map(SER.serBar).join(",")}]`,
    changed_dates: d.changed_dates.map((x) => x.s),
    metadata_changed: d.metadata_changed,
    affected_years: d.affected_years,
    base_version_id: d.base_version_id,
    base_integrity_sha256: d.base_integrity_sha256,
    candidate_sha256: d.candidate_sha256,
    requested_range: { start: d.requested_range.start.s, end: d.requested_range.end.s },
    same_qfq_basis_ref: d.same_qfq_basis_ref,
    revision_ref: d.revision_ref ?? null,
  };
};
const serPublication = (p) =>
  `{"publication_id":${SER.J(p.publication_id)},"version_id":${SER.J(p.version_id)},` +
  `"task_id":${SER.J(p.task_id)},"item_id":${SER.J(p.item_id)},"run_id":${SER.J(p.run_id)},` +
  `"expected_revision":${Number(p.expected_revision)},"intent":${serIntent(p.intent)}}`;
const serIntent = (i) =>
  i === "HistoricalFixedBase" ? `"HistoricalFixedBase"` :
  "Daily" in i ? `{"Daily":{"expected_head":${i.Daily.expected_head == null ? "null" : SER.J(i.Daily.expected_head)}}}` :
  `{"CurrentHead":{"expected_daily_head":${SER.J(i.CurrentHead.expected_daily_head)},"expected_yearly_head":${i.CurrentHead.expected_yearly_head == null ? "null" : SER.J(i.CurrentHead.expected_yearly_head)}}}`;

const scrub = (v) => {
  if (Array.isArray(v)) { v.forEach(scrub); return v; }
  if (v && typeof v === "object") {
    for (const k of Object.keys(v)) {
      if (k === "last_progress_at" || k === "created_at" || k === "covered_at") v[k] = NOW;
      else scrub(v[k]);
    }
  }
  return v;
};

// -------------------------------------------------------------- driver
const [scenPath, goldenPath] = process.argv.slice(2);
const scenarios = readFileSync(scenPath, "utf8").split("\n").filter(Boolean).map(JSON.parse);
const golden = readFileSync(goldenPath, "utf8").split("\n").filter(Boolean).map(JSON.parse);

const runCase = (seq, c) => {
  const op = c.op;
  const ok = (detail, calls) => ({ seq, op, status: "ok", detail, ...(calls ? { calls } : {}) });
  const err = (e) => ({ seq, op, status: "err", ...rowErr(e) });
  switch (op) {
    case "dec": {
      try { const s = SER.decCanon(c.value); return ok({ canon: s, serde: SER.J(s) }); }
      catch (e) { return err(e); }
    }
    case "date": {
      try { const d = SER.tDate(c.value); return ok({ canon: d.s, serde: SER.J(d.s) }); }
      catch (e) { return err(e); }
    }
    case "datetime": {
      try { return ok({ canon: SER.J(SER.tDateTime(c.value)) }); }
      catch (e) { return err(e); }
    }
    case "parse_batch": {
      try { return ok({ serde: SER.serBatch(MU.parseBatch(c.batch)) }); }
      catch (e) { return err(e); }
    }
    case "digest": {
      try {
        const b = MU.parseBatch(c.batch);
        return ok({ digest: MU.candidateDigest(b), bytes: SER.serBatch(b) });
      } catch (e) { return err(e); }
    }
    case "prepare": {
      let existing;
      try { existing = check(c.existing); }
      catch (e) { return { seq, op, status: "err", message: `CHECK:${e.message}` }; }
      let batch;
      try { batch = MU.parseBatch(c.batch); }
      catch (e) { return { seq, op, status: "err", message: `PARSE:${e.message}` }; }
      const range = { start: SER.tDate(c.range[0]), end: SER.tDate(c.range[1]) };
      const evidence = c.evidence == null ? null : evidenceOfCase(c.evidence);
      try { return ok(decisionJson(MU.prepare(existing, batch, range, evidence))); }
      catch (e) { return err(e); }
    }
    case "key": {
      const k = TASKS.operationKey(c.operation);
      return ok({ contract_version: k.contract_version, dataset: k.dataset, operation: k.operation });
    }
    case "normalize": {
      try {
        const contract = TASKS.rangeContract({ operation: c.operation,
          repository: {}, today: () => SER.tDate("2024-06-30") });
        return ok(contract.normalize(c.raw));
      } catch (e) { return err(e); }
    }
    case "resolve": {
      try {
        const stub = new StubRepo(c.stub ?? {});
        const contract = TASKS.rangeContract({ operation: c.operation,
          repository: stub, today: () => SER.tDate(c.today ?? "2024-06-30") });
        return ok(contract.resolve(c.raw));
      } catch (e) { return err(e); }
    }
    case "prepare_range": {
      const stub = new StubRepo(c.stub);
      try {
        const command = {
          publication_id: c.command.publication_id, version_id: c.command.version_id,
          task_id: c.command.task_id, item_id: c.command.item_id, run_id: c.command.run_id,
          expected_revision: BigInt(c.command.expected_revision),
          intent: c.command.intent === "HistoricalFixedBase" ? "HistoricalFixedBase"
            : c.command.intent.Daily !== undefined ? { Daily: { expected_head: c.command.intent.Daily.expected_head ?? null } }
            : { CurrentHead: { expected_daily_head: c.command.intent.CurrentHead.expected_daily_head,
                expected_yearly_head: c.command.intent.CurrentHead.expected_yearly_head ?? null } },
        };
        const batch = MU.parseBatch(c.batch);
        const range = { start: SER.tDate(c.range[0]), end: SER.tDate(c.range[1]) };
        const evidence = evidenceOfCase(c.evidence);
        const combined = c.combined == null ? null : {
          base_integrity_sha256: c.combined.base_integrity_sha256,
          segment_sha256: c.combined.segment_sha256,
          combined_integrity_sha256: c.combined.combined_integrity_sha256,
          complete_coverage_ref: c.combined.complete_coverage_ref };
        const prep = PUB.prepareRange(stub, command, batch, range, evidence, combined);
        if (prep.kind === "needs_verification") {
          return ok({ kind: "needs_verification", reason: reasonJson(prep.reason),
            debug: reasonDebug(prep.reason) }, stub.calls);
        }
        const plan = prep.plan;
        return ok({ kind: "ready", no_change: plan.no_change,
          command: serPublication(plan.publication.command), debug: dbgRangePlan(plan) }, stub.calls);
      } catch (e) {
        return { seq, op, status: "err", ...rowErr(e), calls: stub.calls };
      }
    }
    case "run_items": {
      const stub = new StubRepo(c.stub);
      const runner = new RUN.RangeRunner({ repository: stub, run_id: c.run_id,
        runItem: (t, i, p) => runItem(stub, c.run_id, t, i, p) });
      const provider = new StubProvider(c.provider ?? {});
      try {
        const r = runner.runItems(c.task_id, c.item_ids, provider);
        return ok({ result: r !== null ? "some" : "none" }, stub.calls);
      } catch (e) {
        return { seq, op, status: "err", ...rowErr(e), calls: stub.calls };
      }
    }
    case "run_item": {
      const stub = new StubRepo(c.stub);
      const provider = new StubProvider(c.provider ?? {});
      try {
        const r = runItem(stub, c.run_id, c.task_id, c.item_id, provider);
        const detail = r.kind === "published" ? { kind: "published", receipt: r.receipt }
          : { kind: "not_published", snapshot: scrub(r.snapshot) };
        return ok(detail, stub.calls);
      } catch (e) {
        return { seq, op, status: "err", ...rowErr(e), calls: stub.calls };
      }
    }
    default: return { seq, op, status: "err", message: `unknown op ${op}` };
  }
};

// --------------------------------------------------------------- diff
const eq = (a, b) => {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return a === b;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a).filter((k) => a[k] !== undefined);
  const kb = Object.keys(b).filter((k) => b[k] !== undefined);
  if (ka.length !== kb.length) return false;
  return ka.every((k) => k in b && eq(a[k], b[k]));
};
let fail = 0;
const fails = [];
const rows = [];
scenarios.forEach((c, i) => {
  let got;
  try { got = runCase(i, c); }
  catch (e) { got = { seq: i, op: c.op, status: "probe_panic", message: e.stack?.split("\n")[0] ?? String(e) }; }
  rows.push(got);
  const want = golden[i];
  if (!eq(got, want)) { fail++; fails.push({ i, op: c.op, got, want }); }
});
if (process.env.RESULTS_PATH) {
  writeFileSync(process.env.RESULTS_PATH,
    rows.map((r) => JSON.stringify(r, (_k, v) => typeof v === "bigint" ? Number(v) : v)).join("\n") + "\n");
}
console.log(`NM-082 probe: ${scenarios.length - fail}/${scenarios.length} match, ${fail} diffs`);
for (const f of fails.slice(0, 20)) {
  console.log(`\n=== seq ${f.i} op ${f.op}`);
  console.log("GOT :", JSON.stringify(f.got, (_k,v)=>typeof v==="bigint"?String(v):v).slice(0, 2000));
  console.log("WANT:", JSON.stringify(f.want).slice(0, 2000));
}
process.exit(fail ? 1 : 0);
