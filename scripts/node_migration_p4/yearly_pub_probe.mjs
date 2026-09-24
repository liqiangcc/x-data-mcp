#!/usr/bin/env node
// NM-089 probe — yearly_publication pure layer.
// Golden vectors over constructed fixtures + a scripted in-memory
// repository: contract normalize/resolve, derive_yearly, bound
// evidence coverage edges, prepare_* decision paths, runner guards,
// typed-seam refusals. Emits JSONL {"check","status","detail"}.
import * as RC from "../../src_node/market/record_codec.mjs";
import { aggregate } from "../../src_node/algorithms/yearly.mjs";
import {
  boundEvidence, boundYearlyVerifier, CONTRACT_VERSION, deriveYearly,
  officialYearlyVerifier, parseIntent, prepareDerivedYearly,
  prepareYearlyResult, SourceNotVerified, validateBinding,
  YearlyContract, YearlyRunner, YearlyRepositoryAdapter, YearlySeamError,
} from "../../src_node/yearly_publication/index.mjs";

const results = [];
const check = (name, ok, detail = "") =>
  results.push({ check: name, status: ok ? "PASS" : "FAIL", detail });
const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const throws = (fn, msgOrCode) => {
  try { fn(); return { thrown: null }; }
  catch (e) {
    return { thrown: e, match: typeof msgOrCode === "string"
      ? e.message.includes(msgOrCode) || e.kind === msgOrCode || e.code === msgOrCode
      : true };
  }
};

// ---------- fixtures ----------
const UC = {
  price_currency: { status: "Confirmed", value: "CNY", reason: "t" },
  price_unit: { status: "Confirmed", value: "yuan", reason: "t" },
  volume_unit: { status: "Confirmed", value: "share", reason: "t" },
  amount_currency: { status: "Confirmed", value: "CNY", reason: "t" },
  amount_unit: { status: "Confirmed", value: "yuan", reason: "t" },
};
const ADJ_EV = { status: "Confirmed", value: "qfq", reason: "t" };
const bar = (date, o, h, l, c, vol = "1000", amt = "5000") =>
  ({ date, open: o, high: h, low: l, close: c, volume: vol, amount: amt, r7_raw: null });

const mkDailyRaw = (id, bars) => {
  const raw = {
    version_id: id, symbol: "sh.600006", period: "Day",
    adjustment: "Forward", source: "fixture:yearly",
    records: bars,
    published_at: "2024-06-01T00:00:00Z",
    validation: {
      record_count: bars.length, unique_record_count: bars.length,
      duplicate_count: 0,
    },
    unit_currency: UC, adjustment_evidence: ADJ_EV,
    batch: {
      symbol: "sh.600006", period: "Day", adjustment: "Forward",
      bars,
      provenance: {
        source: "fixture:yearly", source_period: "day",
        source_adjustment: "qfq", symbol: "sh.600006",
        acquired_at: "2024-06-01T00:00:00Z", adapter_version: "fixture",
        yearly_derivation: null, snapshot_inputs: null,
      },
      coverage: { status: "Complete", reason: "fixture" },
      unit_currency: UC, adjustment_evidence: ADJ_EV,
    },
  };
  raw.integrity_sha256 = RC.legacyDigest(raw);
  return raw;
};
// 2 days in 2023, 3 in 2024 — strictly increasing.
const DAILY_BARS = [
  bar("2023-01-05", "10", "11", "9", "10.5"),
  bar("2023-12-29", "12", "13", "11", "12.5"),
  bar("2024-03-01", "20", "22", "19", "21"),
  bar("2024-06-03", "21", "23", "20", "22"),
  bar("2024-12-31", "22", "24", "21", "23"),
];
const DAILY_RAW = mkDailyRaw("daily-base-1", DAILY_BARS);
const BASE = RC.check(DAILY_RAW); // CheckedRecord {raw, version, batch}
const AS_OF = "2025-01-05"; // derived_at -> Shanghai date (past last bar)
const DERIVED_AT = new Date("2025-01-05T02:00:00Z");
const YEARS = [2023, 2024];

// The typed base in aggregate()'s shape.
const baseDailyView = (v) => ({
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
    date: r.date.s,
    open: dec(r.open), high: dec(r.high), low: dec(r.low),
    close: dec(r.close),
    volume: r.volume === null ? null : dec(r.volume),
    amount: r.amount === null ? null : dec(r.amount),
  })),
});
import { _dec } from "../../src_node/algorithms/qfq_affine.mjs";
const dec = (v) => _dec.fromStr(String(v));
const ANNUAL = aggregate(baseDailyView(BASE.version), YEARS, AS_OF);

const fakeFacts = (record, firstDay, lastDay) => ({
  version_id: record.version.version_id, symbol: record.version.symbol,
  period: record.version.period,
  integrity_sha256: record.raw.integrity_sha256,
  first_day: firstDay ?? record.version.records[0].date.s,
  last_day: lastDay ?? record.version.records[record.version.records.length - 1].date.s,
});

// Scripted repository — records calls; returns canned data. Staged
// raws become visible to candidate() — the prepare path re-reads them.
const fakeRepo = (over = {}) => {
  const calls = [];
  const staged = {};
  const repo = {
    calls,
    fixed_facts: (id) => {
      calls.push(["fixed_facts", id]);
      if (over.facts && over.facts[id]) return over.facts[id];
      if (id === BASE.raw.version_id) return fakeFacts(BASE);
      if (over.prior) return fakeFacts(over.prior);
      throw new RC.MarketError("not_found", "MARKET_NOT_FOUND");
    },
    fixed_window_years: (id, f, l) => {
      calls.push(["fixed_window_years", id, f, l]);
      const set = new Set();
      for (const b of BASE.version.records) {
        if (b.date.s >= f && b.date.s <= l) set.add(b.date.y);
      }
      return [...set].sort((a, b) => a - b);
    },
    fixed: (id) => {
      calls.push(["fixed", id]);
      if (id === BASE.raw.version_id) return BASE;
      if (over.prior && id === over.prior.version.version_id) return over.prior;
      throw new RC.MarketError("not_found", "MARKET_NOT_FOUND");
    },
    fixed_window: (id, f, l) => {
      calls.push(["fixed_window", id, f, l]);
      if (id === BASE.raw.version_id) return BASE;
      if (over.prior && id === over.prior.version.version_id) return over.prior;
      throw new RC.MarketError("not_found", "MARKET_NOT_FOUND");
    },
    candidate: (id) => {
      calls.push(["candidate", id]);
      if (staged[id]) return staged[id];
      if (over.staged) return over.staged;
      throw new RC.MarketError("not_found", "MARKET_NOT_FOUND");
    },
    task_item_binding: (t, i) => (calls.push(["task_item_binding", t, i]), over.binding ?? null),
    task_head: (t) => {
      calls.push(["task_head", t]);
      if (over.head) return over.head;
      throw new RC.MarketError("task", "task not found");
    },
    task: (t) => ({ head: over.head ?? {}, items: [] }),
    yearly_item_receipt: (t, i) => over.receipt ?? null,
    yearly_receipt: (id) => over.receipt ?? null,
    stage: (raw) => {
      calls.push(["stage", raw.version_id]);
      staged[raw.version_id] = RC.check(raw);
    },
    mark_validated: (r) => calls.push(["mark_validated", r.version.version_id]),
    transition_task_head: (h, i, run, change) => {
      calls.push(["transition", change.kind, change.item_id ?? null]);
      // decideHead's observable effects the decision paths rely on.
      if (change.kind === "claim") { h.status = "running"; h.owner_run_id = run; }
      if (change.kind === "startItem" && i) i.stage = "running";
      if (change.kind === "finishItem" && i) {
        i.stage = "finished"; i.outcome = change.outcome;
        h.counts.finished += 1n; h.counts.running -= 1n;
      }
      if (change.kind === "finishExecution") h.status = "succeeded";
      if (change.kind === "finishCancellation") h.status = "cancelled";
      h.state_revision += 1n;
    },
    commit_yearly: over.commit_yearly ?? ((plan) => {
      calls.push(["commit_yearly", plan.publication.command.version_id]);
      return { publication_id: plan.publication.command.publication_id,
        version_id: plan.publication.command.version_id,
        task_id: plan.publication.command.task_id,
        task_revision: 3 };
    }),
  };
  return repo;
};

// ---------- 1. contract key ----------
{
  const c = new YearlyContract("update", fakeRepo(), () => "2025-01-05");
  check("contract_key", eq(c.key(), {
    operation: "update_data", dataset: "market.yearly",
    contract_version: CONTRACT_VERSION,
  }));
}

// ---------- 2. normalize ----------
const GOOD_ITEM = {
  symbol: "sh.600006", base_daily_version_id: "daily-base-1",
  years: [2023, 2024], intent: "HistoricalFixedBase",
  comparison_version_id: null,
};
{
  const c = new YearlyContract("update", fakeRepo(), () => "2025-01-05");
  const n = c.normalize({ items: [GOOD_ITEM] });
  check("normalize_ok", eq(n, { items: [GOOD_ITEM] }));
}
{
  const c = new YearlyContract("update", fakeRepo(), () => "2025-01-05");
  const cases = [
    ["sym_no_dot", { ...GOOD_ITEM, symbol: "sh600006" }],
    ["sym_bad_exch", { ...GOOD_ITEM, symbol: "bj.600006" }],
    ["sym_short_code", { ...GOOD_ITEM, symbol: "sh.60006" }],
    ["sym_alpha_code", { ...GOOD_ITEM, symbol: "sh.60000a" }],
    ["empty_base", { ...GOOD_ITEM, base_daily_version_id: "  " }],
    ["empty_comparison", { ...GOOD_ITEM, comparison_version_id: "" }],
    ["empty_years", { ...GOOD_ITEM, years: [] }],
    ["year_zero", { ...GOOD_ITEM, years: [0] }],
    ["year_10000", { ...GOOD_ITEM, years: [10000] }],
    ["unknown_field", { ...GOOD_ITEM, extra: 1 }],
    ["missing_intent", (() => { const x = { ...GOOD_ITEM }; delete x.intent; return x; })()],
    ["ch_mismatch", { ...GOOD_ITEM, intent: { CurrentHead: {
      expected_daily_head: "other", expected_yearly_head: null } } }],
    ["ch_cmp_mismatch", { ...GOOD_ITEM, comparison_version_id: "y-x",
      intent: { CurrentHead: { expected_daily_head: "daily-base-1",
        expected_yearly_head: "y-other" } } }],
    ["ch_empty_yh", { ...GOOD_ITEM, intent: { CurrentHead: {
      expected_daily_head: "daily-base-1", expected_yearly_head: "  " } } }],
    ["daily_intent", { ...GOOD_ITEM, intent: { Daily: { expected_head: null } } }],
  ];
  let pass = 0;
  for (const [name, item] of cases) {
    const { thrown, match } = throws(() => c.normalize({ items: [item] }), "INVALID_COMMAND");
    if (thrown && match) pass++;
    else check(`normalize_reject_${name}`, false, thrown?.message ?? "no throw");
  }
  check("normalize_rejects", pass === cases.length, `${pass}/${cases.length}`);
}
{
  // sort + dedup + budget + same-symbol conflict
  const c = new YearlyContract("update", fakeRepo(), () => "2025-01-05");
  const dup = c.normalize({ items: [
    { ...GOOD_ITEM, symbol: "sz.000001" },
    { ...GOOD_ITEM, symbol: "sh.600006" },
    { ...GOOD_ITEM, symbol: "sz.000001" }, // identical -> dedup'd
  ] });
  check("normalize_sort_dedup",
    dup.items.length === 2 && dup.items[0].symbol === "sh.600006");
  const conflict = throws(() => c.normalize({ items: [
    GOOD_ITEM,
    { ...GOOD_ITEM, base_daily_version_id: "other-base" },
  ] }), "conflicting yearly bindings");
  check("normalize_same_symbol_conflict", conflict.thrown && conflict.match);
  const over = throws(() => c.normalize({
    items: Array.from({ length: 101 }, (_, i) => ({
      ...GOOD_ITEM, symbol: `sh.${String(600000 + i)}`,
    })),
  }), "explicit yearly budget");
  check("normalize_budget", over.thrown && over.match);
  // BTreeSet year dedup on parse
  const yrs = c.normalize({ items: [{ ...GOOD_ITEM, years: [2024, 2023, 2024] }] });
  check("normalize_years_btreeset", eq(yrs.items[0].years, [2023, 2024]));
}

// ---------- 3. resolve ----------
{
  const c = new YearlyContract("update", fakeRepo(), () => "2025-01-05");
  const normalized = c.normalize({ items: [GOOD_ITEM] });
  const work = c.resolve(normalized);
  const scope = '["market","sh.600006","year"]';
  check("resolve_shape", eq(work, {
    items: [{
      item_id: scope, scope_keys: [scope],
      target: { symbol: "sh.600006", period: "year" },
      progress_unit: "security_period",
      frozen_inputs: {
        base_daily_version_id: "daily-base-1",
        base_integrity_sha256: BASE.raw.integrity_sha256,
        years: [2023, 2024], intent: "HistoricalFixedBase",
        comparison: null,
      },
    }],
    scopes: [scope], shared_evidence: null,
  }));
  const cFut = new YearlyContract("update", fakeRepo(), () => "2024-06-15");
  const fut = throws(() => cFut.resolve({ items: [{ ...GOOD_ITEM, years: [2025] }] }),
    "future yearly write rejected");
  check("resolve_future_reject", fut.thrown && fut.match);
  const absent = throws(() => c.resolve({ items: [{ ...GOOD_ITEM, years: [2020] }] }),
    "needs_verification");
  check("resolve_absent_year", absent.thrown && absent.match,
    absent.thrown?.message);
  // comparison identity mismatch
  const badCmp = fakeRepo({
    facts: { "daily-base-1": fakeFacts(BASE),
      "cmp-1": { ...fakeFacts(BASE), version_id: "cmp-1", period: "Day" } },
  });
  const c2 = new YearlyContract("update", badCmp, () => "2025-01-05");
  const mis = throws(() => c2.resolve({ items: [{
    ...GOOD_ITEM, comparison_version_id: "cmp-1",
  }] }), "comparison yearly identity/period mismatch");
  check("resolve_cmp_mismatch", mis.thrown && mis.match);
  // CurrentHead intent binds expected_yearly_head as comparison
  const c3 = new YearlyContract("update", fakeRepo({ prior: null }), () => "2025-01-05");
  const w2 = c3.resolve({ items: [{
    ...GOOD_ITEM,
    intent: { CurrentHead: { expected_daily_head: "daily-base-1",
      expected_yearly_head: null } },
    comparison_version_id: null,
  }] });
  check("resolve_currenthead", w2.items[0].frozen_inputs.comparison === null);
}

// ---------- 4. boundEvidence coverage edges ----------
{
  const ev = boundEvidence(BASE, YEARS, ANNUAL, DERIVED_AT, "test-source");
  check("evidence_shape", eq(ev, {
    base_integrity_sha256: BASE.raw.integrity_sha256,
    years: [2023, 2024], as_of: AS_OF,
    coverage: { status: "Complete",
      reason: `selected years verified by re-aggregating fixed daily daily-base-1 against test-source as of ${AS_OF}; not a listing-history claim` },
  }));
  const missing = throws(() =>
    boundEvidence(BASE, [2023], ANNUAL, DERIVED_AT, "t"),
    "does not cover");
  check("evidence_year_mismatch", missing.thrown instanceof SourceNotVerified);
  const noDigest = throws(() => boundEvidence(
    { ...BASE, raw: { ...BASE.raw, integrity_sha256: 42 } },
    YEARS, ANNUAL, DERIVED_AT, "t"), "no integrity digest");
  check("evidence_no_digest", noDigest.thrown instanceof SourceNotVerified);
  const single = boundEvidence(
    BASE, [2023], { ...ANNUAL, records: ANNUAL.records.filter((r) => r.bar.date.startsWith("2023")) },
    DERIVED_AT, "t");
  check("evidence_single_year", eq(single.years, [2023]));
  const off = officialYearlyVerifier({ installed: false });
  const inst = throws(() => off.verify(BASE, YEARS, ANNUAL, DERIVED_AT), "not installed");
  check("verifier_official_absent", inst.thrown instanceof SourceNotVerified);
  const unbound = boundYearlyVerifier({ bound: null });
  const nb = throws(() => unbound.verify(BASE, YEARS, ANNUAL, DERIVED_AT), "no bound generation");
  check("verifier_unbound", nb.thrown instanceof SourceNotVerified);
  const bound = boundYearlyVerifier({ bound: { generation_id: "g-1" } });
  check("verifier_bound",
    bound.verify(BASE, YEARS, ANNUAL, DERIVED_AT).coverage.reason.includes("bound generation g-1"));
}

// ---------- 5. derive_yearly ----------
let CANDIDATE;
{
  const repo = fakeRepo();
  const request = {
    symbol: "sh.600006", base_daily_version_id: "daily-base-1",
    version_id: "yearly-v1", years: YEARS, derived_at: DERIVED_AT,
  };
  const evidence = boundEvidence(BASE, YEARS, ANNUAL, DERIVED_AT, "test");
  CANDIDATE = deriveYearly(repo, request, evidence, BASE, ANNUAL);
  check("derive_candidate", CANDIDATE.raw.version_id === "yearly-v1" &&
    CANDIDATE.raw.period === "Year" &&
    CANDIDATE.raw.records.length === 2 &&
    CANDIDATE.raw.yearly_derivation.base_daily_version_id === "daily-base-1" &&
    typeof CANDIDATE.raw.integrity_sha256 === "string" &&
    CANDIDATE.checked.version.period === "Year");
  check("derive_bar_content", eq(CANDIDATE.raw.records[0], {
    date: "2023-12-29", open: "10", high: "13", low: "9", close: "12.5",
    volume: "2000", amount: "10000", r7_raw: null,
  }));
  check("derive_raw_layout", CANDIDATE.raw.local_yearly_derivation_v1.annual.records.length === 2 &&
    CANDIDATE.raw.local_yearly_derivation_v1.evidence.years.length === 2 &&
    CANDIDATE.raw.local_yearly_derivation_v1.base_acquired_at === "2024-06-01T00:00:00Z");
  // rejects
  const badId = throws(() => deriveYearly(repo, { ...request, version_id: "daily-base-1" },
    evidence, BASE, ANNUAL), "explicit yearly identity/selection");
  check("derive_id_reject", badId.thrown && badId.thrown.kind === "invalid");
  const badBase = throws(() => deriveYearly(repo, request, {
    ...evidence, base_integrity_sha256: "00" }, BASE, ANNUAL),
    "coverage not verified");
  check("derive_evidence_reject", badBase.thrown && badBase.thrown.kind === "base");
  const badAnnual = throws(() => deriveYearly(repo, request, evidence, BASE,
    { ...ANNUAL, base_daily_version_id: "other" }), "does not match");
  check("derive_annual_reject", badAnnual.thrown && badAnnual.thrown.kind === "base");
}

// ---------- 6. binding + prepare ----------
const mkBinding = (frozenInputs, over = {}) => ({
  head: {
    task_id: "t-1",
    key: { operation: "update_data", dataset: "market.yearly",
      contract_version: CONTRACT_VERSION },
    status: "running", state_revision: 3n, accepted_run_id: "r-1",
    owner_run_id: "r-1", last_progress_at: "2024-06-15T02:00:00Z",
    result: null,
    counts: { total: 1n, pending: 0n, running: 1n, finished: 0n, succeeded: 0n },
    evidence_digest: null,
    ...over.head,
  },
  item: {
    spec: {
      item_id: "i-1", target: { symbol: "sh.600006", period: "year" },
      progress_unit: "security_period", frozen_inputs: frozenInputs,
      scope_keys: ['["market","sh.600006","year"]'],
    },
    stage: "running", outcome: null,
    ...over.item,
  },
  scopes_bound: true, ...over.tail,
});
{
  const c = new YearlyContract("update", fakeRepo(), () => "2025-01-05");
  const fi = c.resolve(c.normalize({ items: [GOOD_ITEM] })).items[0].frozen_inputs;
  const command = {
    publication_id: "pub-1", version_id: "yearly-v1", task_id: "t-1",
    item_id: "i-1", run_id: "r-1", expected_revision: 3,
    intent: "HistoricalFixedBase",
  };
  // validateBinding: matching frozen -> ok; drifted -> conflict
  validateBinding(mkBinding(fi), command, CANDIDATE);
  check("binding_match", true);
  const drifted = throws(() => validateBinding(
    mkBinding({ ...fi, base_integrity_sha256: "dead" }), command, CANDIDATE),
    "differs from accepted frozen inputs");
  check("binding_drift", drifted.thrown && drifted.match);

  // prepare_yearly_result — fresh publish (no prior)
  const repo = fakeRepo({ binding: mkBinding(fi) });
  const plan = prepareYearlyResult(repo, command, CANDIDATE, null);
  check("prepare_new", plan.no_change === false &&
    plan.audit.format === "local-yearly-result-v1" &&
    plan.audit.no_change === false &&
    plan.audit.result_integrity_sha256 === CANDIDATE.raw.integrity_sha256 &&
    plan.audit.base_daily_version_id === "daily-base-1" &&
    plan.audit.prior_version_id === null);
  check("prepare_stage_called",
    repo.calls.some((c2) => c2[0] === "stage" && c2[1] === "yearly-v1"));
  check("prepare_mark_validated",
    repo.calls.some((c2) => c2[0] === "mark_validated"));

  // no_change reuse: prior = an identical-content yearly record
  const PRIOR = CANDIDATE.checked; // same facts subtree -> equivalent
  const fi2 = { ...fi, comparison: {
    version_id: "yearly-v1", integrity_sha256: CANDIDATE.raw.integrity_sha256 } };
  const repo2 = fakeRepo({ binding: mkBinding(fi2), prior: PRIOR });
  // staged candidate must exist identical for prepare_derived path —
  // here prior reuse short-circuits before staging.
  const plan2 = prepareYearlyResult(repo2, command, CANDIDATE, "yearly-v1");
  check("prepare_reuse", plan2.no_change === true &&
    plan2.publication.command.version_id === "yearly-v1" &&
    plan2.publication.promote === null);
  // CurrentHead with mismatched prior binding — the frozen intent must
  // match the command's for validateBinding to pass first.
  const chIntent = { CurrentHead: {
    expected_daily_head: "daily-base-1", expected_yearly_head: "y-other" } };
  const fiCh = { ...fi2, intent: chIntent };
  const repo3 = fakeRepo({ binding: mkBinding(fiCh), prior: PRIOR });
  const badCmd = { ...command, intent: chIntent };
  const misHead = throws(() => prepareYearlyResult(
    repo3, badCmd, CANDIDATE, null), "must bind expected yearly head");
  check("prepare_head_mismatch", misHead.thrown && misHead.match,
    misHead.thrown?.message ?? "no throw");
}

// ---------- 7. runner guards + seam ----------
{
  const fi = new YearlyContract("update", fakeRepo(), () => "2025-01-05")
    .resolve(new YearlyContract("update", fakeRepo(), () => "2025-01-05")
      .normalize({ items: [GOOD_ITEM] })).items[0].frozen_inputs;
  const binding = mkBinding(fi);
  const runner = new YearlyRunner(fakeRepo({ binding }), "r-1", DERIVED_AT);
  // wrong contract
  const badContract = fakeRepo({ binding: mkBinding(fi, {
    head: { key: { operation: "update_data", dataset: "market.daily",
      contract_version: "x" } } }) });
  const bc = throws(() => new YearlyRunner(badContract, "r-1", DERIVED_AT)
    .run_item("t-1", "i-1", {}), "unsupported yearly execution contract");
  check("runner_contract_gate", bc.thrown && bc.match);
  // replay: receipt exists
  const replay = new YearlyRunner(
    fakeRepo({ binding, receipt: { publication_id: "p", version_id: "v",
      task_id: "t-1", task_revision: 3 } }), "r-1", DERIVED_AT);
  const r1 = replay.run_item("t-1", "i-1", {});
  check("runner_receipt_replay", r1.kind === "published" &&
    r1.receipt.publication_id === "p");
  // running item -> outcome_unknown
  const running = throws(() => new YearlyRunner(fakeRepo({
    binding: mkBinding(fi, { item: { stage: "running" } }) }), "r-1", DERIVED_AT)
    .run_item("t-1", "i-1", {}), "outcome_unknown");
  check("runner_running_item", running.thrown && running.match);
  // terminal+success without receipt -> integrity
  const term = throws(() => new YearlyRunner(fakeRepo({
    binding: mkBinding(fi, {
      head: { status: "succeeded" },
      item: { stage: "finished", outcome: { success: true } } }) }),
    "r-1", DERIVED_AT).run_item("t-1", "i-1", {}),
    "successful yearly item has no receipt");
  check("runner_terminal_integrity", term.thrown && term.match);
  // happy path -> derive -> commit arm (scripted success): head
  // running + item pending; task_head re-read returns the SAME head
  // object so the post-verification revision check stays coherent.
  const happyBinding = mkBinding(fi, { item: { stage: "pending" } });
  happyBinding.head.status = "running";
  const happy = fakeRepo({ binding: happyBinding, head: happyBinding.head });
  const ok = new YearlyRunner(happy, "r-1", DERIVED_AT).run_item(
    "t-1", "i-1", officialYearlyVerifier({ installed: true }));
  check("runner_publish", ok.kind === "published" &&
    happy.calls.some((c2) => c2[0] === "commit_yearly") &&
    happy.calls.some((c2) => c2[0] === "transition" && c2[1] === "startItem"));
  // seam: real adapter's commit_yearly refuses
  const seam = throws(() =>
    new YearlyRepositoryAdapter({ query: () => [], get: () => ({ x: 0n }), run: () => ({ changes: 0 }) })
      .commit_yearly({}), "seam: storage commit arm");
  check("commit_yearly_seam", seam.thrown instanceof YearlySeamError);
}

// ---------- summary ----------
const pass = results.filter((r) => r.status === "PASS").length;
const fail = results.filter((r) => r.status === "FAIL").length;
for (const r of results) console.log(JSON.stringify(r));
console.log(JSON.stringify({ check: "summary", status: fail === 0 ? "PASS" : "FAIL",
  detail: `${pass} PASS / ${fail} FAIL` }));
process.exit(fail === 0 ? 0 : 1);
