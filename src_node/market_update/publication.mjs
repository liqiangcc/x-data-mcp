// NM-082 — src/market_update/publication.rs port: CombinedEvidence,
// Preparation/RangePlan decision surface, Publisher::prepare_range
// decision gates, check_frozen_inputs, plus the PublishPlan helpers it
// calls (plan_record/check_task_binding/finish_task, Publisher::prepare).
// Every repository access goes through the injected `repository` object —
// an unwired call throws a typed seam error, never a silent success
// (NM-063 RepositorySeam pattern).
import { decideHead } from "../storage/task_repository.mjs";
import {
  MarketError, invalid, integrity, prepare, sha256Hex,
  covEq, ucEq, evEq, barsEq, rangeEq, rangeValue, tcNotFound,
} from "./mod.mjs";
import {
  parseDefaultFrozen, frozen, scopeId, union,
  CONTRACT_VERSION, DEFAULT_CONTRACT_VERSION,
} from "./tasks.mjs";
import { serValue, batchValue, evidenceValue } from "./serde.mjs";

const TASK_TERMINAL = ["succeeded", "partially_failed", "failed", "cancelled", "interrupted"];
// task_control::visible_status(status, accepted, owner, run)
export const visibleStatus = (status, accepted, owner, run) =>
  !TASK_TERMINAL.includes(status) && (accepted !== run || (owner !== null && owner !== undefined && owner !== run))
    ? "needs_verification" : status;
const headVisibleStatus = (head, run) =>
  visibleStatus(head.status, head.accepted_run_id, head.owner_run_id ?? null, run);
const conflict = (m) => new MarketError("conflict", `PUBLISH_PRECONDITION: ${m}`);
const base = (m) => new MarketError("base", `BASE_UNAVAILABLE: ${m}`);
const baseError = (e) => base(e.message);

// market_storage::Scope — {symbol, period:"Day"|"Year"}; occupancy_key()
// = legacy::scope_id.
const scopeOf = (v) => ({ symbol: v.symbol, period: v.period });
const occupancyKey = (scope) => scopeId(scope.symbol, scope.period === "Day" ? "day" : "year");
const scopeValue = (s) => ({ period: s.period === "Day" ? "day" : "year", symbol: s.symbol });

// canon equality on JSON Values (serde_json Value eq = BTreeMap eq —
// key order-insensitive; emitted sorted by serValue).
const canonEq = (a, b) => serValue(a) === serValue(b);

// ----------------------------------------------------------- PublishPlan
// PublishPlan::check_task_binding — revision/owner/state + item
// target/occupancy binding.
const checkTaskBinding = (plan, binding) => {
  if (BigInt(binding.head.state_revision) !== BigInt(plan.command.expected_revision) ||
      headVisibleStatus(binding.head, plan.command.run_id) !== "running" ||
      (binding.head.owner_run_id ?? null) !== plan.command.run_id) {
    throw conflict("task revision/owner/state");
  }
  const scope = scopeOf(plan.record.version);
  const item = binding.item;
  if (item.stage !== "running" ||
      item.spec.item_id !== plan.command.item_id ||
      !canonEq(item.spec.target, scopeValue(scope)) ||
      !item.spec.scope_keys.includes(occupancyKey(scope)) ||
      !binding.scopes_bound) {
    throw conflict("task target/occupancy");
  }
};

// PublishPlan::finish_task — binding guards + FinishItem (+FinishExecution
// when all items finished). `now` is a timestamp string passed to
// decide_head's last_progress_at (Utc::now() in Rust — not observable in
// the returned plan).
const finishTask = (plan, binding, now) => {
  checkTaskBinding(plan, binding);
  decideHead(binding.head, binding.item, plan.command.run_id, {
    kind: "finishItem", item_id: plan.command.item_id,
    outcome: { success: true, error: null },
  }, now);
  if (binding.head.counts.finished === binding.head.counts.total) {
    decideHead(binding.head, null, plan.command.run_id, { kind: "finishExecution" }, now);
  }
};

// Publisher::plan_record — eligibility for a staged candidate/verified
// reuse: publication_id, intent/period match, required heads, optional
// yearly lineage validation, task binding.
const planRecord = (repo, command, record, now) => {
  if (command.publication_id.trim() === "") throw invalid("empty publication_id");
  const v = record.version;
  const scope = scopeOf(v);
  let required_heads, promote;
  const intent = command.intent;
  if (intent !== null && typeof intent === "object" && "Daily" in intent && v.period === "Day") {
    required_heads = [[scope, intent.Daily.expected_head ?? null]];
    promote = scope;
  } else if (intent === "HistoricalFixedBase" && v.period === "Year") {
    required_heads = [];
    promote = null;
  } else if (intent !== null && typeof intent === "object" && "CurrentHead" in intent && v.period === "Year") {
    const ch = intent.CurrentHead;
    if ((v.yearly_derivation?.base_daily_version_id ?? null) !== ch.expected_daily_head) {
      throw conflict("candidate must bind expected daily head");
    }
    required_heads = [
      [{ symbol: v.symbol, period: "Day" }, ch.expected_daily_head],
      [scope, ch.expected_yearly_head ?? null],
    ];
    promote = scope;
  } else {
    throw invalid("intent/period mismatch");
  }
  const plan = { command, record, required_heads, promote, base_digest: null };
  if (plan.record.version.yearly_derivation != null) {
    const lineage = plan.record.version.yearly_derivation;
    let facts;
    try { facts = repo.fixed_facts(lineage.base_daily_version_id); } catch (e) { throw baseError(e); }
    validateBaseFacts(plan.record, repo, facts);
    plan.base_digest = facts.integrity_sha256;
  }
  const binding = repo.task_item_binding(plan.command.task_id, plan.command.item_id);
  if (binding == null) throw conflict("missing task item");
  checkTaskBinding(plan, binding);
  return plan;
};

// market_storage::validate_base_facts — yearly lineage validation against
// facts + windowed metadata. Day records return early (unreachable from
// prepare_range: Daily intent requires a Day candidate, and check()
// forbids Day+yearly_derivation).
const validateBaseFacts = (record, repo, facts) => {
  const v = record.version;
  if (v.period === "Day") return;
  const lineage = v.yearly_derivation;
  if (lineage == null) throw base("yearly lineage missing");
  if (facts.version_id !== lineage.base_daily_version_id ||
      facts.symbol !== v.symbol || facts.period !== "Day") {
    throw base("fixed base identity/period/adjustment/source mismatch");
  }
  let meta;
  try { meta = repo.fixed_window(facts.version_id, facts.first_day, facts.first_day); }
  catch (e) { throw baseError(e); }
  const daily = meta.version;
  if (daily.version_id !== lineage.base_daily_version_id ||
      daily.symbol !== v.symbol || daily.period !== "Day" ||
      daily.adjustment !== v.adjustment || daily.source !== v.source ||
      !ucEq(daily.unit_currency, v.unit_currency) ||
      !isConf(daily.adjustment_evidence) || daily.adjustment_evidence.value !== "qfq") {
    throw base("fixed base identity/period/adjustment/source mismatch");
  }
  // Yearly re-aggregation (memoized in Rust on the base digest; the memo
  // is transparent so the call computes directly). Unreachable on the
  // range-update path — the aggregation backend is a typed seam.
  throw new MarketError("storage",
    "MARKET_STORAGE: market_update repository seam not wired (S8): yearly aggregate_memoized");
};
const isConf = (e) => e.status === "Confirmed" && typeof e.value === "string" && e.value !== "";

// Publisher::prepare — candidate read + plan + mark_validated.
export const publisherPrepare = (repo, command, now) => {
  const record = repo.candidate(command.version_id);
  const plan = planRecord(repo, command, record, now);
  repo.mark_validated(plan.record);
  return plan;
};

// envelope::pack_batch — bars array becomes a content-addressed payload;
// shared evidence blocks (unit_currency/coverage/adjustment_evidence) are
// $ref'd into payloads. Payload json = serde_json::to_string(Value) =
// sorted-key bytes.
export const packBatch = (batchVal) => {
  const out = { ...batchVal };
  const payloads = [];
  if (!Array.isArray(out.bars)) throw integrity("envelope payload is not an array");
  const barsJson = serValue(out.bars);
  const barsId = sha256Hex(Buffer.from(barsJson, "utf8"));
  out.bars = { $ref: `payload:${barsId}` };
  payloads.push({ id: barsId, json: barsJson });
  for (const key of ["unit_currency", "coverage", "adjustment_evidence"]) {
    const v = out[key];
    if (v === null || v === undefined ||
        (typeof v === "object" && !Array.isArray(v) && "$ref" in v)) continue;
    const json = serValue(v);
    const id = sha256Hex(Buffer.from(json, "utf8"));
    out[key] = { $ref: `payload:${id}` };
    payloads.push({ id, json });
  }
  return [out, payloads];
};

// --------------------------------------------------------- prepare_range
// Publisher::prepare_range (publication.rs:145-269) — verbatim.
// Returns {kind:"needs_verification",reason} | {kind:"ready",plan} where
// plan = RangePlan {publication, base, no_change, audit, audit_payloads}.
export const prepareRange = (repo, command, batch, range, evidence, combined, now) => {
  const intent = command.intent;
  const intentIsDailyHead = intent !== null && typeof intent === "object" &&
    "Daily" in intent && Object.keys(intent).length === 1 &&
    (intent.Daily?.expected_head ?? null) === evidence.base_version_id;
  if (command.publication_id.trim() === "" || !intentIsDailyHead) {
    throw invalid("range update must bind its expected daily head");
  }
  const baseRec = repo.fixed(evidence.base_version_id);
  checkFrozenInputs(repo, command, evidence, range);
  const decision = prepare(baseRec, batch, range, evidence);
  let publication, noChange;
  if (decision.kind === "needs_verification") {
    return { kind: "needs_verification", reason: decision.reason };
  } else if (decision.kind === "no_change") {
    if (command.version_id !== decision.version_id || combined != null) {
      throw invalid("no-change must reuse the fixed version");
    }
    publication = {
      command,
      record: baseRec,
      required_heads: [[scopeOf(baseRec.version), decision.version_id]],
      promote: null,
      base_digest: null,
    };
    const binding = repo.task_item_binding(publication.command.task_id, publication.command.item_id);
    if (binding == null) throw tcNotFound();
    finishTask(publication, binding, now);
    noChange = true;
  } else {
    const prepared = decision;
    if (command.version_id === baseRec.version.version_id) {
      throw invalid("changed data needs a new immutable version");
    }
    const candidate = repo.candidate(command.version_id);
    if (combined == null) {
      return { kind: "needs_verification", reason: { kind: "coverage_unknown" } };
    }
    if (combined.base_integrity_sha256 !== prepared.base_integrity_sha256 ||
        combined.segment_sha256 !== prepared.candidate_sha256 ||
        combined.combined_integrity_sha256 !== candidate.raw["integrity_sha256"] ||
        combined.complete_coverage_ref.trim() === "") {
      return { kind: "needs_verification", reason: { kind: "coverage_unknown" } };
    }
    const v = candidate.version;
    if (prepared.metadata_changed &&
        covEq(candidate.batch.coverage, baseRec.batch.coverage) &&
        ucEq(candidate.version.unit_currency, baseRec.version.unit_currency)) {
      throw invalid("combined candidate discards changed metadata");
    }
    if (!barsEq(v.records, prepared.records) ||
        v.symbol !== batch.symbol ||
        v.period !== batch.period ||
        v.adjustment !== batch.adjustment ||
        v.source !== batch.provenance.source ||
        !evEq(v.adjustment_evidence, batch.adjustment_evidence)) {
      throw invalid("combined candidate differs from approved range merge");
    }
    // A full candidate may enrich units but may not lose/change any
    // confirmed unit from either the preserved data or the segment.
    for (const units of [baseRec.version.unit_currency, batch.unit_currency]) {
      const target = v.unit_currency;
      for (const u of ["price_currency", "price_unit", "volume_unit", "amount_currency", "amount_unit"]) {
        const oldU = units[u], newU = target[u];
        if (isConf(oldU) && (!isConf(newU) || oldU.value !== newU.value)) {
          return { kind: "needs_verification", reason: { kind: "units_incompatible" } };
        }
      }
    }
    publication = publisherPrepare(repo, command, now);
    if (!canonEq(publication.record.raw, candidate.raw)) {
      throw integrity("combined candidate changed during approval");
    }
    noChange = false;
  }
  const [segment, audit_payloads] = packBatch(batchValue(batch));
  const audit = {
    algorithm_version: "local-range-update-v1",
    command: publication.command,
    base_integrity_sha256: baseRec.raw["integrity_sha256"],
    result_integrity_sha256: publication.record.raw["integrity_sha256"],
    segment,
    range: rangeValue(range),
    evidence: evidenceValue(evidence),
    combined: combined ?? null,
    no_change: noChange,
  };
  return {
    kind: "ready",
    plan: { publication, base: baseRec, no_change: noChange, audit, audit_payloads },
  };
};

// Publisher::check_frozen_inputs (publication.rs:273-326) — executed
// base+range must equal the item's frozen inputs for accepted
// update/backfill tasks; other operations skip the check.
export const checkFrozenInputs = (repo, command, evidence, range) => {
  const head = repo.task_head(command.task_id);
  const rangeTask = head.key.operation === "update_data" || head.key.operation === "backfill_data";
  const binding = repo.task_item_binding(command.task_id, command.item_id);
  const itemFrozen = binding == null ? null : binding.item.spec.frozen_inputs;
  let frozenMatches;
  if (head.key.contract_version === CONTRACT_VERSION && head.key.dataset === "market.daily") {
    // Generation-mode stamp is carried through verbatim.
    const expected = frozen(evidence.base_version_id, evidence.base_integrity_sha256, range);
    const stamped = itemFrozen != null && typeof itemFrozen === "object" && !Array.isArray(itemFrozen)
      ? itemFrozen.source_generation_id : undefined;
    if (stamped !== undefined) expected.source_generation_id = stamped;
    frozenMatches = itemFrozen != null && canonEq(itemFrozen, expected);
  } else if (head.key.contract_version === DEFAULT_CONTRACT_VERSION && head.key.dataset === "market.daily") {
    // Executed range must equal the union of frozen required ranges on
    // the frozen base identity.
    let parsed = null;
    if (itemFrozen != null) {
      try { parsed = parseDefaultFrozen(itemFrozen); } catch { parsed = null; }
    }
    let unionMatches = false;
    if (parsed != null) {
      try { unionMatches = rangeEq(union(parsed), range); } catch { unionMatches = false; }
    }
    frozenMatches = parsed != null &&
      parsed.base_version_id === evidence.base_version_id &&
      parsed.base_integrity_sha256 === evidence.base_integrity_sha256 &&
      unionMatches;
  } else {
    frozenMatches = false;
  }
  if (rangeTask && !frozenMatches) {
    throw conflict("range publication differs from accepted frozen inputs");
  }
};
