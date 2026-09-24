// yearly_publication.rs (mod level) + market_storage::Publisher — the
// derivation/decision half: derive_yearly + prepare_derived_yearly +
// prepare + plan_record + check_task_binding. Pure layer: all storage
// goes through the injected `repository` object; the commit arm
// (`commit_yearly`) stays a typed seam in repository.mjs.
import * as RC from "../market/record_codec.mjs";
import * as MR from "../market/market_read.mjs";
import { aggregate } from "../algorithms/yearly.mjs";
import { _dec } from "../algorithms/qfq_affine.mjs";
import { aggregateKey, aggregateMemoized } from "./memo.mjs";
import {
  CONTRACT_VERSION, DATASET, intentBody, intentVariant, mBase,
  mConflict, mIntegrity, mInvalid, repositoryError, scopeId,
  visibleStatus, YearlySeamError,
} from "./types.mjs";
import { validateBinding } from "./tasks.mjs";

const decStr = MR.__decStr;
const integrity = mIntegrity;
// Decimal-normalized comparison across storage text and compact {m,s}.
const cmpDec = (a, b) => RC.__cmpDec(decStr(a), decStr(b));
const decEq = (a, b) => cmpDec(a, b) === 0;

/** Serde bar Value: date "YYYY-MM-DD", decimals as canonical text,
 *  r7_raw always present (null when absent). */
const serBar = (b) => ({
  date: typeof b.date === "string" ? b.date : b.date.s,
  open: decStr(b.open), high: decStr(b.high),
  low: decStr(b.low), close: decStr(b.close),
  volume: b.volume === null || b.volume === undefined ? null : decStr(b.volume),
  amount: b.amount === null || b.amount === undefined ? null : decStr(b.amount),
  r7_raw: b.r7_raw ?? null,
});
// Typed bar for validateAndDeduplicate/check-time comparisons.
const typedBar = (b) => ({
  date: typeof b.date === "string" ? RC.__parseDate(b.date) : b.date,
  open: b.open, high: b.high, low: b.low, close: b.close,
  volume: b.volume ?? null, amount: b.amount ?? null,
  r7_raw: b.r7_raw ?? null,
});
// For validation comparisons the {m,s} Decs must go through canonical
// text — decNorm cannot read compact Decs.
const normBar = (b) => {
  const s = serBar(b);
  return { date: RC.__parseDate(s.date), open: s.open, high: s.high,
    low: s.low, close: s.close, volume: s.volume, amount: s.amount,
    r7_raw: s.r7_raw };
};

/** chrono serde DateTime<Utc>: to_rfc3339_opts(AutoSi, use_z=true) —
 *  "Z" suffix, subsecond only when non-zero (ms precision here). */
const serUtc = (v) => {
  const d = v instanceof Date ? v : new Date(v);
  const iso = d.toISOString(); // YYYY-MM-DDTHH:MM:SS.mmmZ
  const ms = d.getMilliseconds();
  return `${iso.slice(0, 19)}${ms === 0 ? "" : `.${String(ms).padStart(3, "0")}`}Z`;
};
const SH = 8 * 3600 * 1000;
const shanghaiDate = (v) => {
  const d = v instanceof Date ? v : new Date(v);
  return new Date(d.getTime() + SH).toISOString().slice(0, 10);
};
const serNaiveDate = (d) => (typeof d === "string" ? d : d.s);

// ---------------------------------------------------------------------
// Publisher::derive_yearly — yearly_publication.rs:60 verbatim order.
export const deriveYearly = (repository, request, evidence, base, annual) => {
  if (typeof request.symbol !== "string" || request.symbol.trim() === "" ||
      typeof request.version_id !== "string" || request.version_id.trim() === "" ||
      typeof request.base_daily_version_id !== "string" ||
      request.base_daily_version_id.trim() === "" ||
      request.version_id === request.base_daily_version_id ||
      !Array.isArray(request.years) || request.years.length === 0) {
    throw mInvalid("explicit yearly identity/selection");
  }
  const daily = base.version;
  if (daily.version_id !== request.base_daily_version_id) {
    throw mBase("fixed daily identity binding");
  }
  if (daily.symbol !== request.symbol || daily.period !== "Day") {
    throw mBase("fixed daily identity/period");
  }
  const asOf = shanghaiDate(request.derived_at);
  if (base.raw.integrity_sha256 !== evidence.base_integrity_sha256 ||
      !sameYears(evidence.years, request.years) ||
      serNaiveDate(evidence.as_of) !== asOf ||
      evidence.coverage.status !== "Complete" ||
      (typeof evidence.coverage.reason !== "string" ||
       evidence.coverage.reason.trim() === "")) {
    throw mBase("selected range coverage not verified for fixed daily/years/as_of");
  }
  // `annual` was aggregated once by the caller over the same windowed
  // base — verify it really is that series before trusting it.
  const annualYears = annual.records.map((r) => yearOfBar(r.bar));
  const reqYears = [...new Set(request.years)].sort((a, b) => a - b);
  if (annual.base_daily_version_id !== daily.version_id ||
      annual.as_of !== asOf ||
      annual.records.length !== reqYears.length ||
      !annualYears.every((y, i) => y === reqYears[i])) {
    throw mBase("aggregated annual series does not match the request");
  }
  const lineage = {
    base_daily_version_id: daily.version_id,
    algorithm_version: "local-natural-year-v1",
    first_trading_date: annual.records[0].lineage.first_trading_date,
    last_trading_date: annual.records[annual.records.length - 1].lineage.last_trading_date,
  };
  const provenance = {
    ...base.batch.provenance,
    acquired_at: serUtc(request.derived_at),
    adapter_version: "local-yearly-publication-v1",
    yearly_derivation: lineage,
  };
  const batch = {
    symbol: base.batch.symbol,
    period: "Year",
    adjustment: base.batch.adjustment,
    bars: annual.records.map((r) => normBar(r.bar)),
    provenance,
    coverage: { ...evidence.coverage },
    unit_currency: base.batch.unit_currency,
    adjustment_evidence: base.batch.adjustment_evidence,
  };
  let records, validation;
  try {
    ({ bars: records, validation } = RC.validateAndDeduplicate(batch));
  } catch (e) {
    if (e instanceof RC.MarketError) throw mBase(stripPrefix(e.message));
    throw e;
  }
  const version = {
    version_id: request.version_id,
    symbol: daily.symbol,
    period: "Year",
    adjustment: daily.adjustment,
    source: daily.source,
    records,
    published_at: serUtc(request.derived_at),
    validation,
    unit_currency: daily.unit_currency,
    adjustment_evidence: daily.adjustment_evidence,
    yearly_derivation: lineage,
  };
  const raw = serVersion(version);
  raw.batch = serBatch(batch);
  // Preserve per-year participation and unfinished flags.
  raw.local_yearly_derivation_v1 = {
    annual: serAnnual(annual),
    evidence: serEvidence(evidence),
    base_acquired_at: base.batch.provenance.acquired_at,
  };
  raw.integrity_sha256 = RC.legacyDigest(raw);
  let checked;
  try { checked = RC.check(raw); }
  catch (e) { if (e instanceof RC.MarketError) throw e; throw e; }
  return {
    raw,
    checked,
    base_daily_version_id: daily.version_id,
    base_integrity_sha256: evidence.base_integrity_sha256,
  };
};

const sameYears = (a, b) => {
  const x = [...new Set(a)].sort((m, n) => m - n);
  const y = [...new Set(b)].sort((m, n) => m - n);
  return x.length === y.length && x.every((v, i) => v === y[i]);
};
const yearOfBar = (bar) =>
  typeof bar.date === "string" ? Number(bar.date.slice(0, 4)) : bar.date.y;
const stripPrefix = (m) => m.replace(/^[A-Z_]+: /, "");

// ---- serde Value shapes for EffectiveVersion/MarketDataBatch/AnnualSeries ----
const serDec = (v) => (v === null ? null : decStr(v));
const serVersion = (v) => ({
  version_id: v.version_id, symbol: v.symbol, period: v.period,
  adjustment: v.adjustment, source: v.source,
  records: v.records.map(serBar),
  published_at: v.published_at,
  validation: v.validation,
  unit_currency: v.unit_currency,
  adjustment_evidence: v.adjustment_evidence,
  yearly_derivation: v.yearly_derivation,
});
const serBatch = (b) => ({
  symbol: b.symbol, period: b.period, adjustment: b.adjustment,
  bars: b.bars.map(serBar),
  provenance: b.provenance,
  coverage: b.coverage,
  unit_currency: b.unit_currency,
  adjustment_evidence: b.adjustment_evidence,
});
const serAnnual = (a) => ({
  symbol: a.symbol, source: a.source,
  base_daily_version_id: a.base_daily_version_id,
  as_of: a.as_of,
  unit_currency: a.unit_currency,
  records: a.records.map((r) => ({
    bar: serBar(r.bar), lineage: r.lineage,
    year_unfinished: r.year_unfinished,
  })),
});
const serEvidence = (e) => ({
  base_integrity_sha256: e.base_integrity_sha256,
  years: [...new Set(e.years)].sort((a, b) => a - b),
  as_of: serNaiveDate(e.as_of),
  coverage: { status: e.coverage.status, reason: e.coverage.reason },
});

// ---------------------------------------------------------------------
// market_storage::Publisher::plan_record / prepare / check_task_binding
// — verbatim decision layer over the injected repository.
const scopeOf = (v) => ({ symbol: v.symbol, period: v.period });
const occupancyKey = (s) =>
  `["market",${JSON.stringify(s.symbol)},${JSON.stringify(s.period === "Day" ? "day" : "year")}]`;

const checkTaskBinding = (command, scope, binding) => {
  if (BigInt(binding.head.state_revision) !== BigInt(command.expected_revision) ||
      visibleStatus(binding.head.status, binding.head.accepted_run_id,
        binding.head.owner_run_id, command.run_id) !== "running" ||
      binding.head.owner_run_id !== command.run_id) {
    throw mConflict("task revision/owner/state");
  }
  const item = binding.item;
  if (item.stage !== "running" ||
      item.spec.item_id !== command.item_id ||
      !RC.__jsonEq(item.spec.target,
        { symbol: scope.symbol, period: scope.period === "Day" ? "day" : "year" }) ||
      !item.spec.scope_keys.includes(occupancyKey(scope)) ||
      !binding.scopes_bound) {
    throw mConflict("task target/occupancy");
  }
};

export const planRecord = (repository, command, record) => {
  if (command.publication_id.trim() === "") {
    throw mInvalid("empty publication_id");
  }
  const v = record.version;
  const scope = scopeOf(v);
  let requiredHeads, promote;
  const iv = intentVariant(command.intent), ib = intentBody(command.intent);
  if (iv === "Daily" && v.period === "Day") {
    requiredHeads = [[{ ...scope }, ib.expected_head ?? null]];
    promote = scope;
  } else if (iv === "HistoricalFixedBase" && v.period === "Year") {
    requiredHeads = [];
    promote = null;
  } else if (iv === "CurrentHead" && v.period === "Year") {
    if ((v.yearly_derivation?.base_daily_version_id ?? null) !==
        ib.expected_daily_head) {
      throw mConflict("candidate must bind expected daily head");
    }
    requiredHeads = [
      [{ symbol: v.symbol, period: "Day" }, ib.expected_daily_head],
      [{ ...scope }, ib.expected_yearly_head ?? null],
    ];
    promote = scope;
  } else {
    throw mInvalid("intent/period mismatch");
  }
  const plan = { command, record, required_heads: requiredHeads, promote,
    base_digest: null };
  if (plan.record.version.yearly_derivation !== null) {
    // Facts + memoized/windowed validation.
    const lineage = plan.record.version.yearly_derivation;
    let facts;
    try { facts = repository.fixed_facts(lineage.base_daily_version_id); }
    catch (e) { throw baseError(e); }
    validateBaseFacts(repository, plan.record, facts);
    plan.base_digest = facts.integrity_sha256;
  }
  const binding = repository.task_item_binding(
    plan.command.task_id, plan.command.item_id);
  if (binding === null) throw mConflict("missing task item");
  checkTaskBinding(plan.command, scope, binding);
  return plan;
};

export const prepare = (repository, command) => {
  const record = repository.candidate(command.version_id);
  const plan = planRecord(repository, command, record);
  repository.mark_validated(plan.record);
  return plan;
};

// market_storage::base_error — Error::Base(e.to_string()): any error's
// Display text wrapped under BASE_UNAVAILABLE.
const baseError = (e) => mBase(e instanceof Error ? e.message : String(e));

/** Storage-parsed version -> aggregate()'s EffectiveVersion shape —
 *  the same projection market_read's validateBase uses (lowercase
 *  period, confirmed-evidence view, compact Dec {m,s}). */
export const dailyView = (v) => ({
  period: "day", yearly_derivation: null, version_id: v.version_id,
  symbol: v.symbol, source: v.source,
  adjustment_evidence: {
    confirmed: v.adjustment_evidence.status === "Confirmed",
    value: v.adjustment_evidence.value,
  },
  unit_currency: Object.fromEntries(
    ["price_currency", "price_unit", "volume_unit", "amount_currency",
      "amount_unit"].map((k) => [k, {
      confirmed: v.unit_currency[k].status === "Confirmed",
      value: v.unit_currency[k].value,
    }])),
  records: v.records.map((r) => ({
    date: typeof r.date === "string" ? r.date : r.date.s,
    open: toDec(r.open), high: toDec(r.high), low: toDec(r.low),
    close: toDec(r.close),
    volume: r.volume === null || r.volume === undefined ? null : toDec(r.volume),
    amount: r.amount === null || r.amount === undefined ? null : toDec(r.amount),
  })),
});
const toDec = (v) =>
  (v !== null && typeof v === "object" && typeof v.m === "bigint")
    ? v : _dec.fromStr(String(v));

// market_storage::validate_base_facts — the live re-check of the
// derivation base: identity/facts + memoized re-aggregation of the
// fixed window compared bar-for-bar with the candidate records.
export const validateBaseFacts = (repository, record, facts) => {
  const v = record.version;
  if (v.period === "Day") return;
  const lineage = v.yearly_derivation;
  if (lineage === null) throw mBase("yearly lineage missing");
  if (facts.version_id !== lineage.base_daily_version_id ||
      facts.symbol !== v.symbol || facts.period !== "Day") {
    throw mBase("fixed base identity/period/adjustment/source mismatch");
  }
  let meta;
  try { meta = repository.fixed_window(facts.version_id, facts.first_day, facts.first_day); }
  catch (e) { throw baseError(e); }
  const daily = meta.version;
  const ae = daily.adjustment_evidence;
  if (daily.version_id !== lineage.base_daily_version_id ||
      daily.symbol !== v.symbol || daily.period !== "Day" ||
      daily.adjustment !== v.adjustment || daily.source !== v.source ||
      !RC.__ucEq(daily.unit_currency, v.unit_currency) ||
      !(ae.status === "Confirmed" && ae.value === "qfq")) {
    throw mBase("fixed base identity/period/adjustment/source mismatch");
  }
  const years = [...new Set(v.records.map((b) => yearOfBar(b)))].sort((a, b) => a - b);
  const asOf = shanghaiDate(v.published_at);
  const key = aggregateKey(facts.integrity_sha256, years, asOf);
  const annual = aggregateMemoized(key, () => {
    let window;
    try {
      window = repository.fixed_window(
        facts.version_id,
        serNaiveDate(lineage.first_trading_date),
        serNaiveDate(lineage.last_trading_date));
    } catch (e) { throw baseError(e); }
    try { return aggregate(dailyView(window.version), years, asOf); }
    catch (e) { throw mBase(e.message); }
  });
  const recBars = annual.records.map((r) => r.bar);
  if (recBars.length !== v.records.length ||
      !recBars.every((b, i) => barEq(b, v.records[i])) ||
      lineage.algorithm_version !== "local-natural-year-v1" ||
      annual.records[0].lineage.first_trading_date !==
        serNaiveDate(lineage.first_trading_date) ||
      annual.records[annual.records.length - 1].lineage.last_trading_date !==
        serNaiveDate(lineage.last_trading_date)) {
    throw mBase("yearly content/lineage does not match fixed daily");
  }
};

// ---------------------------------------------------------------------
// Publisher::prepare_derived_yearly — yearly_publication.rs:171.
export const prepareDerivedYearly = (repository, command, candidate) => {
  if (candidate.raw.version_id !== command.version_id) {
    throw mInvalid("yearly candidate/version binding");
  }
  const binding0 = repository.task_item_binding(command.task_id, command.item_id);
  if (binding0 !== null) {
    validateBinding(binding0, command, candidate);
  } else {
    const head = repository.task_head(command.task_id);
    if (head.key.operation === "update_data" || head.key.operation === "backfill_data") {
      throw mConflict("yearly publication differs from accepted frozen inputs");
    }
  }
  let base;
  try { base = repository.fixed_facts(candidate.base_daily_version_id); }
  catch (e) { throw baseError(e); }
  if (base.integrity_sha256 !== candidate.base_integrity_sha256) {
    throw mBase("fixed daily changed after derivation");
  }
  if (intentVariant(command.intent) === "CurrentHead" &&
      intentBody(command.intent).expected_yearly_head !== null) {
    const id = intentBody(command.intent).expected_yearly_head;
    let old;
    try { old = repository.fixed(id); }
    catch (e) { throw baseError(e); }
    const next = candidate.checked;
    if (old.version.symbol !== next.version.symbol || old.version.period !== "Year") {
      throw mBase("expected yearly identity/period");
    }
    const selected = new Set(next.version.records.map((r) => r.date.y));
    if (old.version.records.some((r) => !selected.has(r.date.y))) {
      throw mConflict("selected years cannot replace a head containing other years");
    }
  }
  let existing;
  try { existing = repository.candidate(command.version_id); }
  catch (e) {
    if (e instanceof RC.MarketError && e.kind === "not_found") {
      repository.stage(candidate.raw);
      existing = undefined;
    } else { throw e; }
  }
  if (existing !== undefined) {
    if (RC.__jsonEq(existing.raw, candidate.raw)) {
      // identical staged candidate — allowed
    } else if (existing.raw.compact_source !== undefined &&
               versionEq(existing.version, candidate.checked.version)) {
      // compact backends synthesize staged raw from typed facts
    } else {
      throw mConflict("yearly candidate id has different content");
    }
  }
  const plan = prepare(repository, command);
  const next = candidate.checked;
  const sameRecord = RC.__jsonEq(plan.record.raw, candidate.raw) ||
    (plan.record.raw.compact_source !== undefined &&
     versionEq(plan.record.version, next.version));
  if (!sameRecord ||
      (plan.base_digest ?? null) !== candidate.base_integrity_sha256) {
    throw mBase("yearly candidate/base changed during approval");
  }
  return plan;
};

// EffectiveVersion PartialEq — serde value equality on the typed fields.
const versionEq = (a, b) =>
  a.version_id === b.version_id && a.symbol === b.symbol &&
  a.period === b.period && a.adjustment === b.adjustment &&
  a.source === b.source &&
  a.records.length === b.records.length &&
  a.records.every((r, i) => barEq(r, b.records[i])) &&
  a.published_at === b.published_at &&
  a.validation.record_count === b.validation.record_count &&
  a.validation.unique_record_count === b.validation.unique_record_count &&
  a.validation.duplicate_count === b.validation.duplicate_count &&
  RC.__ucEq(a.unit_currency, b.unit_currency) &&
  evEq(a.adjustment_evidence, b.adjustment_evidence) &&
  derivEq(a.yearly_derivation, b.yearly_derivation);
export const barEq = (a, b) =>
  (typeof a.date === "string" ? a.date : a.date.s) ===
    (typeof b.date === "string" ? b.date : b.date.s) &&
  decEq(a.open, b.open) && decEq(a.high, b.high) &&
  decEq(a.low, b.low) && decEq(a.close, b.close) &&
  (a.volume === null || a.volume === undefined ? b.volume === null || b.volume === undefined
    : b.volume !== null && b.volume !== undefined && decEq(a.volume, b.volume)) &&
  (a.amount === null || a.amount === undefined ? b.amount === null || b.amount === undefined
    : b.amount !== null && b.amount !== undefined && decEq(a.amount, b.amount)) &&
  (a.r7_raw ?? null) === (b.r7_raw ?? null);
const evEq = (a, b) =>
  a.status === b.status && a.value === b.value && a.reason === b.reason;
const derivEq = (a, b) =>
  a === null ? b === null : b !== null &&
  a.base_daily_version_id === b.base_daily_version_id &&
  a.algorithm_version === b.algorithm_version &&
  serNaiveDate(a.first_trading_date) === serNaiveDate(b.first_trading_date) &&
  serNaiveDate(a.last_trading_date) === serNaiveDate(b.last_trading_date);

export { checkTaskBinding, scopeId };
