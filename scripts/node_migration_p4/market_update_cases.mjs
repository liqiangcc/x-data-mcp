// NM-082 scenario generator: emits decision-matrix scenario JSONL for the
// Rust oracle (market_update_oracle.rs) and the Node probe
// (market_update_probe.mjs). Digests inside inputs are computed with the
// already-verified record_codec serialization — the oracle recomputes
// them internally, so any Node divergence surfaces as a golden
// needs_verification/conflict instead of a silent match.
import { createHash } from "node:crypto";
import { legacyDigest } from "../../src_node/market/record_codec.mjs";
import { parseBatch, candidateDigest } from "../../src_node/market_update/mod.mjs";

const sha = (s) => createHash("sha256").update(s).digest("hex");

// ----------------------------------------------------------- fixtures
const ev = (status, value, reason = "") => ({ status, value, reason });
const UC = {
  price_currency: ev("Confirmed", "cny", "vendor"),
  price_unit: ev("Confirmed", "share", "vendor"),
  volume_unit: ev("Confirmed", "share", "vendor"),
  amount_currency: ev("Confirmed", "cny", "vendor"),
  amount_unit: ev("Confirmed", "yuan", "vendor"),
};
const UC_UNKNOWN_AMOUNT = {
  ...UC,
  amount_currency: ev("Unknown", null, "no amount evidence"),
  amount_unit: ev("Unknown", null, "no amount evidence"),
};
const ADJ = ev("Confirmed", "qfq", "vendor adjustment table");
const COV = ev("Complete", null, "full window observed");

const bar = (date, o, h, l, c, extra = {}) => ({
  date, open: String(o), high: String(h), low: String(l), close: String(c),
  volume: null, amount: null, r7_raw: null, ...extra,
});
const BARS = [
  bar("2024-01-02", 10, 11, 9.5, 10.5, { volume: "1000", amount: "10500" }),
  bar("2024-01-03", 10.5, 11, 10, 10.8, { volume: "1200", amount: "12960" }),
  bar("2024-01-04", 10.8, 11.2, 10.6, 11.0, { volume: "900", amount: "9900" }),
  bar("2024-01-05", 11.0, 11.5, 10.9, 11.4, { volume: "1500", amount: "17100" }),
];
const BARS_2Y = [...BARS, bar("2025-01-06", 12, 12.6, 11.8, 12.4, { volume: "800", amount: "9920" })];

const prov = (symbol) => ({
  source: "official", source_period: "day", source_adjustment: "qfq",
  symbol, acquired_at: "2024-01-06T00:00:00Z", adapter_version: "probe-1",
  yearly_derivation: null, snapshot_inputs: null,
});
const batchOf = (symbol, bars, over = {}) => ({
  symbol, period: "Day", adjustment: "Forward", bars,
  provenance: prov(symbol), coverage: COV, unit_currency: UC,
  adjustment_evidence: ADJ, ...over,
});
const recOf = (versionId, symbol, bars, over = {}) => {
  // Stored record layout: EffectiveVersion fields are flat at the top
  // level; `batch` carries the publication evidence; integrity_sha256 is
  // the legacy digest over the whole raw object.
  const raw = {
    version_id: versionId, symbol, period: "Day", adjustment: "Forward",
    source: "official", records: bars, published_at: "2024-01-06T00:00:00Z",
    validation: { record_count: bars.length, unique_record_count: bars.length, duplicate_count: 0 },
    unit_currency: UC, adjustment_evidence: ADJ, yearly_derivation: null,
    batch: batchOf(symbol, bars, over.batch ?? {}), ...over.raw,
  };
  raw.integrity_sha256 = legacyDigest(raw);
  return raw;
};
const candSha = (batch) => candidateDigest(parseBatch(batch));
const evidenceFor = (baseRaw, batch, range, over = {}) => ({
  base_version_id: baseRaw.version_id,
  base_integrity_sha256: baseRaw.integrity_sha256,
  candidate_sha256: candSha(batch),
  range: { start: range[0], end: range[1] },
  same_qfq_basis_ref: "probe-basis-1",
  revision_ref: null,
  continuity: null,
  ...over,
});

// ---------------------------------------------------------- stub blocks
const scope = (sym) => `["market","${sym}","day"]`;
const head = (over = {}) => ({
  task_id: "T", operation: "update_data", dataset: "market.daily",
  contract_version: "explicit-range-task-v1",
  status: "running", state_revision: 7, accepted_run_id: "R", owner_run_id: "R",
  counts: { total: 1, pending: 0, running: 1, finished: 0, succeeded: 0 },
  ...over,
});
const item = (over = {}) => ({
  item_id: scope("sh.600000"),
  target: { period: "day", symbol: "sh.600000" },
  progress_unit: "security_period",
  frozen_inputs: null,
  scope_keys: [scope("sh.600000")],
  stage: "running", outcome: null,
  ...over,
});
const frozenIn = (baseRaw, range, over = {}) => ({
  base_integrity_sha256: baseRaw.integrity_sha256,
  base_version_id: baseRaw.version_id,
  range: { end: range[1], start: range[0] }, ...over,
});
const cmd = (over = {}) => ({
  publication_id: "PUB-1", version_id: "cand-v2", task_id: "T",
  item_id: scope("sh.600000"), run_id: "R", expected_revision: 7,
  intent: { Daily: { expected_head: "v1" } }, ...over,
});
const facts = (over = {}) => ({
  version_id: "v1", symbol: "sh.600000", period: "Day",
  integrity_sha256: "", first_day: "2024-01-02", last_day: "2024-01-05",
  ...over,
});
const baseFacts = (baseRaw, over = {}) => ({
  version_id: baseRaw.version_id, symbol: "sh.600000",
  record_count: baseRaw.records.length,
  first_day: "2024-01-02", last_day: "2024-01-05",
  integrity_sha256: "compact-digest-1", coverage: COV, unit_currency: UC,
  source: "official", generation_id: "gen-1", generation_manifest_sha256: "gm",
  day_member_sha256: "dm", security_event_sha256: "se", ...over,
});

const cases = [];
const c = (x) => cases.push(x);

// ------------------------------------------------------- scalar probes
for (const v of ["1.230", "1e2", "+1.5", "-0.00", "0.000", ".5", "5.",
    "007.500", "1.23456789012345678901234567890",
    "0.00000000000000000000000000005", "1e-7", "-0.5e-3"]) {
  c({ op: "dec", value: v });
}
for (const v of [1.230, -0.0, 1e-7, 0.1 + 0.2, 1e30, 9223372036854775807,
    18446744073709551615, -9223372036854775808, 9007199254740993]) {
  c({ op: "dec", value: v });
}
for (const v of ["abc", "NaN", "Infinity", "1.2.3", "", "1e100", true, null,
    {}, "79000000000000000000000000000", "-79000000000000000000000000001",
    "0.123456789012345678901234567895", "123456789012345678901234567890"]) {
  c({ op: "dec", value: v });
}
for (const v of ["2024-1-5", "+10000-01-01", "-0001-01-01", " 2024-01-01 ",
    "2024-02-30", "24-1-5", "2024-01-01Z", "2024/01/05", "0000-01-01",
    "-262143-01-01", "262142-12-31", "+262143-01-01", "10000-1-1", 123, null,
    "-262144-01-01", "+262142-12-31", "2024-13-01", "2024-00-01", "0001-1-1",
    "+262142-01-01", "262143-12-31", "9999-12-31", "10000-12-31"]) {
  c({ op: "date", value: v });
}
for (const v of ["2024-01-02T03:04:05.123456789Z", "2024-01-02T03:04:05+05:30",
    "2024-01-02T03:04:05.120000000Z", "2024-01-02", "2024-01-02 03:04:05Z",
    "bad", "2024-01-02T25:00:00Z", "2024-01-02T03:04:05.0000000001Z"]) {
  c({ op: "datetime", value: v });
}

// --------------------------------------------------------- parse_batch
c({ op: "parse_batch", batch: batchOf("sh.600000", BARS) });
c({ op: "parse_batch", batch: { symbol: "sh.600000", period: "Day", adjustment: "Forward",
    bars: BARS, provenance: prov("sh.600000") } }); // serde(default) evidence
c({ op: "parse_batch", batch: { ...batchOf("sh.600000", BARS), extra: 1,
    bars: BARS.map((b) => ({ ...b, mystery: true })) } }); // permissive
c({ op: "parse_batch", batch: { ...batchOf("sh.600000", BARS), bogus_vec: 9 } });
c({ op: "parse_batch", batch: { ...batchOf("sh.600000", BARS), period: "Month" } });
c({ op: "parse_batch", batch: { ...batchOf("sh.600000", BARS), bars: [{ date: "bad" }] } });
c({ op: "parse_batch", batch: { ...batchOf("sh.600000", BARS), bars: [{ date: "2024-01-02", open: 12.5, high: "12.6", low: "12", close: "12.4" }] } });

// -------------------------------------------------------------- digest
c({ op: "digest", batch: batchOf("sh.600000", BARS) });
c({ op: "digest", batch: { ...batchOf("sh.600000", BARS),
    bars: [bar("2024-01-02", "10.50", "11", "9.5", "10.50", { volume: 1000 })] } });
c({ op: "digest", batch: { ...batchOf("sh.600000", BARS),
    provenance: { ...prov("sh.600000"), acquired_at: "2024-01-06T00:00:00+08:00" } } });

// -------------------------------------------------------------- prepare
const BASE = recOf("v1", "sh.600000", BARS);
const RANGE = ["2024-01-02", "2024-01-05"];
const P = (over) => c({ op: "prepare", existing: BASE, range: RANGE, ...over });

// no_change: identical in-range bars + identical metadata
P({ batch: batchOf("sh.600000", BARS), evidence: evidenceFor(BASE, batchOf("sh.600000", BARS), RANGE) });
// candidate: revised close on 01-04 + new bar; revision_ref supplied
{
  const b = batchOf("sh.600000", [...BARS.slice(0, 3),
    bar("2024-01-04", 10.8, 11.2, 10.6, 11.1, { volume: "900", amount: "9900" }),
    BARS[3]]);
  P({ batch: b, evidence: evidenceFor(BASE, b, RANGE, { revision_ref: "rev-ticket-9" }) });
}
// candidate: brand-new bar inside a gap-free merge (extend last day)
{
  const b = batchOf("sh.600000", [...BARS, bar("2024-01-05", 11.0, 11.5, 10.9, 11.4, { volume: "1500", amount: "17100" })]);
  P({ batch: b, range: ["2024-01-02", "2024-01-06"],
      evidence: evidenceFor(BASE, b, ["2024-01-02", "2024-01-06"]) });
}
// missing_old_dates: in-range old bar absent from candidate
P({ batch: batchOf("sh.600000", [BARS[0], BARS[2], BARS[3]]),
    evidence: evidenceFor(BASE, batchOf("sh.600000", [BARS[0], BARS[2], BARS[3]]), RANGE) });
// coverage_unknown: missing/unknown coverage
{
  const b = batchOf("sh.600000", BARS, { coverage: ev("Unknown", null, "gap suspected") });
  P({ batch: b, evidence: evidenceFor(BASE, b, RANGE) });
}
// units_incompatible: confirmed conflict vs base
{
  const b = batchOf("sh.600000", BARS, { unit_currency: { ...UC, price_currency: ev("Confirmed", "usd", "vendor") } });
  P({ batch: b, evidence: evidenceFor(BASE, b, RANGE) });
}
// basis_unknown variants
P({ batch: batchOf("sh.600000", BARS), evidence: null });
P({ batch: batchOf("sh.600000", BARS),
    evidence: evidenceFor(BASE, batchOf("sh.600000", BARS), RANGE, { base_version_id: "vX" }) });
P({ batch: batchOf("sh.600000", BARS),
    evidence: evidenceFor(BASE, batchOf("sh.600000", BARS), RANGE, { base_integrity_sha256: "0".repeat(64) }) });
P({ batch: batchOf("sh.600000", BARS),
    evidence: evidenceFor(BASE, batchOf("sh.600000", BARS), RANGE, { candidate_sha256: "f".repeat(64) }) });
P({ batch: batchOf("sh.600000", BARS),
    evidence: evidenceFor(BASE, batchOf("sh.600000", BARS), RANGE, { range: { start: "2024-01-03", end: "2024-01-05" } }) });
P({ batch: batchOf("sh.600000", BARS),
    evidence: evidenceFor(BASE, batchOf("sh.600000", BARS), RANGE, { same_qfq_basis_ref: "  " }) });
// known_value_lost: volume dropped on one date
{
  const b = batchOf("sh.600000", [...BARS.slice(0, 2),
    bar("2024-01-04", 10.8, 11.2, 10.6, 11.0, { amount: "9900" }), BARS[3]]);
  P({ batch: b, evidence: evidenceFor(BASE, b, RANGE) });
}
// revision_unverified: price change without revision_ref
{
  const b = batchOf("sh.600000", [...BARS.slice(0, 1),
    bar("2024-01-03", 10.5, 11, 10, 10.9, { volume: "1200", amount: "12960" }),
    ...BARS.slice(2)]);
  P({ batch: b, evidence: evidenceFor(BASE, b, RANGE) });
}
// revision_ref supplied -> candidate
{
  const b = batchOf("sh.600000", [...BARS.slice(0, 1),
    bar("2024-01-03", 10.5, 11, 10, 10.9, { volume: "1200", amount: "12960" }),
    ...BARS.slice(2)]);
  P({ batch: b, evidence: evidenceFor(BASE, b, RANGE, { revision_ref: "rev-1" }) });
}
// metadata_changed via coverage reason drift only (still Complete)
{
  const b = batchOf("sh.600000", BARS, { coverage: ev("Complete", null, "vendor window v2") });
  P({ batch: b, evidence: evidenceFor(BASE, b, RANGE) });
}
// metadata_changed via unit enrichment (base unit unconfirmed -> confirmed)
{
  const baseUnknown = recOf("v1", "sh.600000", BARS, {
    raw: {}, batch: { unit_currency: UC_UNKNOWN_AMOUNT },
  });
  // also make the version's stored units unknown on the amount pair
  baseUnknown.unit_currency = UC_UNKNOWN_AMOUNT;
  baseUnknown.integrity_sha256 = legacyDigest(baseUnknown);
  const b = batchOf("sh.600000", BARS, { unit_currency: UC });
  c({ op: "prepare", existing: baseUnknown, batch: b, range: RANGE,
      evidence: evidenceFor(baseUnknown, b, RANGE) });
}
// invalid: inverted / out-of-calendar range
P({ batch: batchOf("sh.600000", BARS), range: ["2024-01-05", "2024-01-02"],
    evidence: null });
P({ batch: batchOf("sh.600000", BARS), range: ["-0001-01-01", "2024-01-05"],
    evidence: null });
P({ batch: batchOf("sh.600000", BARS), range: ["2024-01-02", "+10000-01-01"],
    evidence: null });
// invalid: yearly batch period
P({ batch: batchOf("sh.600000", BARS, { period: "Year" }), evidence: null });
// invalid: identity mismatches
P({ batch: batchOf("sz.000001", BARS), evidence: null });
P({ batch: { ...batchOf("sh.600000", BARS), provenance: { ...prov("sz.000001") } }, evidence: null });
P({ batch: { ...batchOf("sh.600000", BARS), provenance: { ...prov("sh.600000"), source: "other" } }, evidence: null });
P({ batch: { ...batchOf("sh.600000", BARS), provenance: { ...prov("sh.600000"), source_period: "week" } }, evidence: null });
P({ batch: { ...batchOf("sh.600000", BARS), provenance: { ...prov("sh.600000"), source_adjustment: "hfq" } }, evidence: null });
P({ batch: { ...batchOf("sh.600000", BARS), provenance: { ...prov("sh.600000"),
      yearly_derivation: { base_daily_version_id: "v1", algorithm_version: "local-natural-year-v1",
        first_trading_date: "2024-01-02", last_trading_date: "2024-01-05" } } }, evidence: null });
// invalid: candidate bar outside range
P({ batch: batchOf("sh.600000", [...BARS, bar("2024-01-08", 11, 12, 10, 11.5, { volume: "1", amount: "1" })]),
    evidence: null });
// publish validators -> Error::Invalid(publish display)
P({ batch: batchOf("sh.600000", [bar("2024-01-02", 12, 11, 9, 10, { volume: "1", amount: "1" })]),
    evidence: null }); // open > high
P({ batch: batchOf("sh.600000", [BARS[0], BARS[0],
    bar("2024-01-02", 10, 11, 9.5, 10.6, { volume: "1000", amount: "10500" }), BARS[1], BARS[2], BARS[3]]),
    evidence: null }); // duplicate conflict (all in-range dates covered)
P({ batch: batchOf("sh.600000", [BARS[0], BARS[0], ...BARS.slice(1)]),
    evidence: evidenceFor(BASE, batchOf("sh.600000", BARS), RANGE) }); // exact dup dedupes
P({ batch: batchOf("sh.600000", [bar("2024-01-02", 10, 11, 9, 10, { volume: "-1", amount: "1" })]),
    evidence: null }); // negative volume
P({ batch: batchOf("sh.600000", BARS, { unit_currency: { ...UC, price_currency: ev("Unknown", null, "not supplied") } }),
    evidence: null }); // unconfirmed required unit
P({ batch: batchOf("sh.600000", BARS, { adjustment_evidence: ev("Confirmed", "hfq", "wrong") }),
    evidence: null }); // wrong confirmed adjustment value
// yearly existing record -> "yearly must derive from one fixed daily version"
{
  const yrec = recOf("y1", "sh.600000", BARS, {
    raw: {}, batch: {},
  });
  yrec.period = "Year";
  yrec.yearly_derivation = { base_daily_version_id: "v1", algorithm_version: "local-natural-year-v1",
    first_trading_date: "2024-01-02", last_trading_date: "2024-01-05" };
  yrec.batch.provenance.yearly_derivation = yrec.yearly_derivation;
  yrec.integrity_sha256 = legacyDigest(yrec);
  c({ op: "prepare", existing: yrec, batch: batchOf("sh.600000", BARS), range: RANGE, evidence: null });
}
// metadata change + missing candidate — ordering: missing fires first
P({ batch: batchOf("sh.600000", [BARS[0], BARS[2], BARS[3]], { coverage: ev("Unknown", null, "x") }),
    evidence: null });

// ------------------------------------------------------------ normalize
const ER = (symbol, baseId, s, e) => ({ symbol, base_version_id: baseId, start_date: s, end_date: e });
const RAW1 = { items: [ER("sh.600000", "v1", "2024-01-02", "2024-01-05")] };
for (const op of ["update", "backfill", "republish"]) c({ op: "key", operation: op });
c({ op: "normalize", operation: "update", raw: RAW1 });
c({ op: "normalize", operation: "backfill", raw: RAW1 });
c({ op: "normalize", operation: "republish", raw: RAW1 });
// sort + dedup + conflict
c({ op: "normalize", operation: "update", raw: { items: [
    ER("sz.000002", "v9", "2024-01-01", "2024-01-31"),
    ER("sh.600000", "v1", "2024-01-02", "2024-01-05"),
    ER("sz.000001", "v3", "2024-02-01", "2024-02-29"),
    ER("sh.600000", "v1", "2024-01-02", "2024-01-05"),
  ] } });
c({ op: "normalize", operation: "update", raw: { items: [
    ER("sh.600000", "v1", "2024-01-02", "2024-01-05"),
    ER("sh.600000", "v2", "2024-02-01", "2024-02-29"),
  ] } }); // same symbol different base -> conflict
c({ op: "normalize", operation: "update", raw: { items: [
    ER("sh.600000", "v1", "2024-01-02", "2024-01-05"),
    ER("sh.600000", "v1", "2024-01-02", "2024-02-01"),
  ] } }); // same symbol different range -> conflict
// invalid item shapes
for (const raw of [
  { items: [] }, { items: Array(101).fill(0).map((_, i) => ER(`sz.${String(i).padStart(6, "0")}`, "v", "2024-01-01", "2024-01-02")) },
  { items: [ER("SZ.600000", "v1", "2024-01-02", "2024-01-05")] },
  { items: [ER("sh.60000", "v1", "2024-01-02", "2024-01-05")] },
  { items: [ER("sh.60000a", "v1", "2024-01-02", "2024-01-05")] },
  { items: [ER("sh.6000000", "v1", "2024-01-02", "2024-01-05")] },
  { items: [ER("xx.600000", "v1", "2024-01-02", "2024-01-05")] },
  { items: [ER("600000", "v1", "2024-01-02", "2024-01-05")] },
  { items: [ER("sh.600000", "  ", "2024-01-02", "2024-01-05")] },
  { items: [ER("sh.600000", "v1", "2024-01-05", "2024-01-02")] },
  { items: [ER("sh.600000", "v1", "-0001-01-01", "2024-01-05")] },
  { items: [ER("sh.600000", "v1", "2024-01-02", "+10000-01-01")] },
  { items: [{ ...ER("sh.600000", "v1", "2024-01-02", "2024-01-05"), extra: 1 }] },
  { items: [{ symbol: "sh.600000", base_version_id: "v1" }] },
  { items: "x" }, { bogus: [] }, "x", null,
]) {
  c({ op: "normalize", operation: "update", raw });
}
// serde unknown-field on ExplicitRanges itself
c({ op: "normalize", operation: "update", raw: { ...RAW1, junk: 1 } });
// seq-form decode
c({ op: "normalize", operation: "update", raw: [["sh.600000", "v1", "2024-01-02", "2024-01-05"]] });

// -------------------------------------------------------------- resolve
const RES_STUB = { facts: facts({ integrity_sha256: "base-digest-1" }) };
c({ op: "resolve", operation: "update", raw: RAW1, today: "2024-06-30", stub: RES_STUB });
c({ op: "resolve", operation: "backfill", raw: { items: [
    ER("sz.000001", "v3", "2024-02-01", "2024-02-29"), ER("sh.600000", "v1", "2024-01-02", "2024-01-05")] },
    today: "2024-06-30",
    stub: { facts_by_id: { v1: RES_STUB.facts,
      v3: { ...RES_STUB.facts, version_id: "v3", symbol: "sz.000001", integrity_sha256: "d3" } } } });
// future end_date rejected with verbatim list
c({ op: "resolve", operation: "update", raw: RAW1, today: "2024-01-03", stub: RES_STUB });
c({ op: "resolve", operation: "update", raw: { items: [ER("sh.600000", "v1", "2024-01-02", "2024-06-30"), ER("sz.000001", "v3", "2024-02-01", "2024-07-01")] }, today: "2024-06-30", stub: RES_STUB });
// identity mismatches
c({ op: "resolve", operation: "update", raw: RAW1, today: "2024-06-30",
    stub: { facts: { ...RES_STUB.facts, symbol: "sz.000001" } } });
c({ op: "resolve", operation: "update", raw: RAW1, today: "2024-06-30",
    stub: { facts: { ...RES_STUB.facts, period: "Year" } } });
c({ op: "resolve", operation: "update", raw: RAW1, today: "2024-06-30",
    stub: { facts: { ...RES_STUB.facts, version_id: "vX" } } });
// repository_error mapping matrix
for (const [kind, message] of [
    ["storage", "disk gone"], ["integrity", "bad row"], ["base", "no base"],
    ["conflict", "heads moved"], ["outcome_unknown", "pub-42"], ["basis_not_verified", "no proof"],
    ["invalid", "bad cmd"], ["not_found", ""]]) {
  c({ op: "resolve", operation: "update", raw: RAW1, today: "2024-06-30",
      stub: { errors: { fixed_facts: { kind, message } } } });
}
// resolve raw decode failure
c({ op: "resolve", operation: "update", raw: { items: [{ symbol: 7 }] }, today: "2024-06-30", stub: RES_STUB });

// ---------------------------------------------------------- prepare_range
const BASE_RAW = BASE;
const CAN_BARS = [...BARS.slice(0, 3),
  bar("2024-01-04", 10.8, 11.2, 10.6, 11.1, { volume: "900", amount: "9900" }),
  BARS[3]];
const CAN_BATCH = batchOf("sh.600000", CAN_BARS);
const CAN_REC = recOf("cand-v2", "sh.600000", CAN_BARS);
const EV_OK = evidenceFor(BASE_RAW, CAN_BATCH, RANGE, { revision_ref: "rev-1" });
const stubBase = (over = {}) => ({
  fixed: BASE_RAW,
  head: head(), item: item({ frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
  candidate: CAN_REC, scopes_bound: true, ...over,
});
const COMBINED = {
  base_integrity_sha256: BASE_RAW.integrity_sha256,
  segment_sha256: EV_OK.candidate_sha256,
  combined_integrity_sha256: CAN_REC.integrity_sha256,
  complete_coverage_ref: "cover-1",
};
const PR = (over) => c({ op: "prepare_range", command: cmd(), batch: CAN_BATCH,
  range: RANGE, evidence: EV_OK, combined: COMBINED, stub: stubBase(), ...over });

PR({}); // full happy path -> Ready
PR({ command: cmd({ publication_id: "  " }) });
PR({ command: cmd({ intent: { Daily: { expected_head: "vX" } } }) });
PR({ command: cmd({ intent: "HistoricalFixedBase" }) });
PR({ command: cmd({ intent: { CurrentHead: { expected_daily_head: "v1", expected_yearly_head: null } } }) });
PR({ stub: stubBase({ errors: { fixed: { kind: "not_found", message: "" } } }) });
PR({ stub: stubBase({ item: item({ frozen_inputs: frozenIn(BASE_RAW, RANGE, { base_version_id: "vX" }) }) }) }); // frozen mismatch -> conflict
PR({ stub: stubBase({ item: item({ frozen_inputs: { junk: 1 } }) }) }); // wrong shape -> conflict
PR({ stub: stubBase({ head: head({ operation: "republish_data", contract_version: "explicit-republish-task-v1" }), item: item({ frozen_inputs: { junk: 1 } }) }) }); // republish skips check -> proceeds to basis gates
PR({ combined: null }); // candidate w/o combined -> coverage_unknown
PR({ combined: { ...COMBINED, base_integrity_sha256: "0".repeat(64) } });
PR({ combined: { ...COMBINED, segment_sha256: "0".repeat(64) } });
PR({ combined: { ...COMBINED, combined_integrity_sha256: "0".repeat(64) } });
PR({ combined: { ...COMBINED, complete_coverage_ref: " " } });
PR({ command: cmd({ version_id: "v1" }) }); // candidate version == base id
// no_change path
{
  const same = batchOf("sh.600000", BARS);
  const ev0 = evidenceFor(BASE_RAW, same, RANGE);
  c({ op: "prepare_range", command: cmd({ version_id: "v1" }), batch: same,
      range: RANGE, evidence: ev0, combined: null, stub: stubBase() });
  c({ op: "prepare_range", command: cmd({ version_id: "v1" }), batch: same,
      range: RANGE, evidence: ev0, combined: COMBINED, stub: stubBase() }); // no-change + combined -> invalid
}
// needs_verification bubbles from prepare (missing dates)
{
  const b = batchOf("sh.600000", [BARS[0], BARS[2], BARS[3]]);
  c({ op: "prepare_range", command: cmd(), batch: b, range: RANGE,
      evidence: evidenceFor(BASE_RAW, b, RANGE), combined: COMBINED, stub: stubBase() });
}
// combined record differs from merge -> invalid
PR({ stub: stubBase({ candidate: recOf("cand-v2", "sh.600000", BARS) }) });
// metadata discard guard: prepared metadata_changed + candidate same meta
{
  const metaBatch = batchOf("sh.600000", BARS, { coverage: ev("Complete", null, "vendor window v2") });
  const ev0 = evidenceFor(BASE_RAW, metaBatch, RANGE);
  const sameMetaCand = recOf("cand-v2", "sh.600000", BARS); // unchanged meta
  c({ op: "prepare_range", command: cmd(), batch: metaBatch, range: RANGE,
      evidence: ev0, combined: { ...COMBINED, segment_sha256: ev0.candidate_sha256,
        combined_integrity_sha256: sameMetaCand.integrity_sha256 },
      stub: stubBase({ candidate: sameMetaCand }) });
}
// candidate unit loss vs base -> units_incompatible
{
  const weakCand = recOf("cand-v2", "sh.600000", CAN_BARS);
  weakCand.unit_currency = { ...UC, price_currency: ev("Confirmed", "usd", "drift") };
  weakCand.batch.unit_currency = weakCand.unit_currency;
  weakCand.integrity_sha256 = legacyDigest(weakCand);
  c({ op: "prepare_range", command: cmd(), batch: CAN_BATCH, range: RANGE,
      evidence: EV_OK,
      combined: { ...COMBINED, combined_integrity_sha256: weakCand.integrity_sha256 },
      stub: stubBase({ candidate: weakCand }) });
}
// binding guards inside plan_record
PR({ stub: stubBase({ head: head({ state_revision: 8 }) }) }); // revision mismatch -> conflict
PR({ stub: stubBase({ head: head({ status: "pending", owner_run_id: null, counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }) }) }); // not running
PR({ stub: stubBase({ head: head({ owner_run_id: "OTHER" }) }) }); // foreign owner
PR({ stub: stubBase({ item: item({ stage: "pending" }) }) });
PR({ stub: stubBase({ item: item({ target: { period: "day", symbol: "sz.000001" } }) }) });
PR({ stub: stubBase({ scopes_bound: false }) });
PR({ stub: stubBase({ head: null, item: null }) }); // binding absent -> missing task item
// no-change arm finish_task binding failure (revision moved)
{
  const same = batchOf("sh.600000", BARS);
  const ev0 = evidenceFor(BASE_RAW, same, RANGE);
  c({ op: "prepare_range", command: cmd({ version_id: "v1" }), batch: same,
      range: RANGE, evidence: ev0, combined: null,
      stub: stubBase({ head: head({ state_revision: 8 }) }) });
}
// frozen check on default contract: union mismatch -> conflict
{
  const df = {
    selection_evidence: { evidence_digest: "se-1" },
    base_version_id: "v1", base_integrity_sha256: BASE_RAW.integrity_sha256,
    refresh_range: { start_date: "2024-01-02", end_date: "2024-01-05" },
    proven_gap_ranges: [],
  };
  c({ op: "prepare_range", command: cmd(), batch: CAN_BATCH, range: RANGE,
      evidence: EV_OK, combined: COMBINED,
      stub: stubBase({ head: head({ contract_version: "default-range-task-v1" }),
        item: item({ frozen_inputs: df }) }) });
  // union = refresh only here; range matches -> proceeds (basis gates run)
  const df2 = { ...df, proven_gap_ranges: [{ gap_id: "g1",
      range: { start_date: "2024-01-08", end_date: "2024-01-09" }, evidence_digest: "ge-1" }] };
  c({ op: "prepare_range", command: cmd(), batch: CAN_BATCH, range: RANGE,
      evidence: EV_OK, combined: COMBINED,
      stub: stubBase({ head: head({ contract_version: "default-range-task-v1" }),
        item: item({ frozen_inputs: df2 }) }) }); // union != range -> conflict
}
// frozen check skipped for foreign contract
PR({ stub: stubBase({ head: head({ contract_version: "other-v1" }),
    item: item({ frozen_inputs: { junk: 1 } }) }) });

// -------------------------------------------------------------- run_items
const RI = (over) => c({ op: "run_items", task_id: "T", run_id: "R",
  item_ids: [scope("sh.600000")],
  stub: {
    head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
    base_facts: baseFacts(BASE_RAW), base_bars: BARS,
    item_receipt: null, commit: "ok",
    commit_results: [{ receipt: { publication_id: "x", version_id: "y", task_id: "T", task_revision: 8 } }],
    scopes_bound: true, ...over.stub },
  provider: { typed: { decision: { kind: "unchanged" }, evidence: {
      base_version_id: "v1", base_integrity_sha256: "compact-digest-1",
      candidate_sha256: "x", range: { start: RANGE[0], end: RANGE[1] },
      same_qfq_basis_ref: "b" } } },
  ...over });
RI({}); // unchanged -> commit no_change prepared item
RI({ provider: { typed: { decision: { kind: "append",
      upserts: [bar("2024-01-06", 11.4, 12, 11.2, 11.8, { volume: "700", amount: "8260" })] },
    evidence: { base_version_id: "v1", base_integrity_sha256: "compact-digest-1",
      candidate_sha256: "x", range: { start: "2024-01-02", end: "2024-01-05" },
      same_qfq_basis_ref: "b" } } },
    stub: { head: head({ status: "pending", owner_run_id: null,
        counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
      item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
      base_facts: baseFacts(BASE_RAW), base_bars: BARS, item_receipt: null,
      commit: "ok", commit_results: [{ receipt: { publication_id: "x", version_id: "y", task_id: "T", task_revision: 8 } }] } });
// deferral paths -> fallback -> run_item -> stub receipt -> published
RI({ stub: { head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
    base_facts: null, item_receipt: { publication_id: "p0", version_id: "v9", task_id: "T", task_revision: 9 } } });
RI({ stub: { head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
    base_facts: baseFacts(BASE_RAW), base_bars: null,
    item_receipt: { publication_id: "p0", version_id: "v9", task_id: "T", task_revision: 9 } } });
RI({ provider: { typed: null }, stub: { head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
    base_facts: baseFacts(BASE_RAW), base_bars: BARS,
    item_receipt: { publication_id: "p0", version_id: "v9", task_id: "T", task_revision: 9 } } });
// provider typed error -> update_candidate Err -> fallback -> published
RI({ provider: { typed: { error: { kind: "not_verified", detail: "no proof" } } },
    stub: { head: head({ status: "pending", owner_run_id: null,
        counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
      item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
      base_facts: baseFacts(BASE_RAW), base_bars: BARS,
      item_receipt: { publication_id: "p0", version_id: "v9", task_id: "T", task_revision: 9 } } });
// evidence binding mismatch -> Invalid -> fallback -> published
RI({ provider: { typed: { decision: { kind: "unchanged" }, evidence: {
      base_version_id: "vX", base_integrity_sha256: "compact-digest-1",
      candidate_sha256: "x", range: { start: RANGE[0], end: RANGE[1] },
      same_qfq_basis_ref: "b" } } },
    stub: { head: head({ status: "pending", owner_run_id: null,
        counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
      item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
      base_facts: baseFacts(BASE_RAW), base_bars: BARS,
      item_receipt: { publication_id: "p0", version_id: "v9", task_id: "T", task_revision: 9 } } });
// unsupported contract -> update_candidate Err -> fallback run_item Err -> run_items Err
RI({ stub: { head: head({ operation: "collect_data", status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
    item_receipt: null } });
// commit UpdateBatch "none" -> early Ok(None), deferred items dropped
RI({ stub: { commit: "none", head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
    base_facts: baseFacts(BASE_RAW), base_bars: BARS,
    item_receipt: { publication_id: "p0", version_id: "v9", task_id: "T", task_revision: 9 } } });
// structural commit failure -> batch items join fallback
RI({ stub: { errors: { commit_update_batch: { kind: "storage", message: "rolled back" } },
    head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
    base_facts: baseFacts(BASE_RAW), base_bars: BARS,
    item_receipt: { publication_id: "p0", version_id: "v9", task_id: "T", task_revision: 9 } } });
// per-item error result -> that item falls back
RI({ stub: { commit_results: [{ error: { kind: "conflict", message: "heads moved" } }],
    head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
    base_facts: baseFacts(BASE_RAW), base_bars: BARS,
    item_receipt: { publication_id: "p0", version_id: "v9", task_id: "T", task_revision: 9 } } });
// result-count mismatch -> integrity error
RI({ stub: { commit_results: [],
    head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
    base_facts: baseFacts(BASE_RAW), base_bars: BARS } });
// default-contract item: typed path + coverings appended post-commit
{
  const df = {
    selection_evidence: { evidence_digest: "se-1" },
    base_version_id: "v1", base_integrity_sha256: BASE_RAW.integrity_sha256,
    refresh_range: { start_date: "2024-01-02", end_date: "2024-01-05" },
    proven_gap_ranges: [{ gap_id: "g1",
      range: { start_date: "2024-01-03", end_date: "2024-01-04" }, evidence_digest: "ge-1" }],
  };
  c({ op: "run_items", task_id: "T", run_id: "R", item_ids: [scope("sh.600000")],
    stub: { head: head({ contract_version: "default-range-task-v1", status: "pending",
        owner_run_id: null, counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
      item: item({ stage: "pending", frozen_inputs: df }),
      base_facts: baseFacts(BASE_RAW), base_bars: BARS, item_receipt: null, commit: "ok",
      commit_results: [{ receipt: { publication_id: "x", version_id: "y", task_id: "T", task_revision: 8 } }] },
    provider: { typed: { decision: { kind: "unchanged" }, evidence: {
        base_version_id: "v1", base_integrity_sha256: "compact-digest-1",
        candidate_sha256: "x", range: { start: RANGE[0], end: RANGE[1] },
        same_qfq_basis_ref: "b" } } } });
  // same but covering already present -> skip
  c({ op: "run_items", task_id: "T", run_id: "R", item_ids: [scope("sh.600000")],
    stub: { head: head({ contract_version: "default-range-task-v1", status: "pending",
        owner_run_id: null, counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
      item: item({ stage: "pending", frozen_inputs: df }),
      base_facts: baseFacts(BASE_RAW), base_bars: BARS, item_receipt: null, commit: "ok",
      coverings: { g1: [{ gap_id: "g1", version_id: "y" }] },
      commit_results: [{ receipt: { publication_id: "x", version_id: "y", task_id: "T", task_revision: 8 } }] },
    provider: { typed: { decision: { kind: "unchanged" }, evidence: {
        base_version_id: "v1", base_integrity_sha256: "compact-digest-1",
        candidate_sha256: "x", range: { start: RANGE[0], end: RANGE[1] },
        same_qfq_basis_ref: "b" } } } });
  // gap outside committed union -> update_candidate ok, commit ok, covering -> integrity err
  const df3 = { ...df, proven_gap_ranges: [{ gap_id: "g2",
      range: { start_date: "2024-02-01", end_date: "2024-02-02" }, evidence_digest: "ge-2" }] };
  c({ op: "run_items", task_id: "T", run_id: "R", item_ids: [scope("sh.600000")],
    stub: { head: head({ contract_version: "default-range-task-v1", status: "pending",
        owner_run_id: null, counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
      item: item({ stage: "pending", frozen_inputs: df3 }),
      base_facts: baseFacts(BASE_RAW), base_bars: BARS, item_receipt: null, commit: "ok",
      commit_results: [{ receipt: { publication_id: "x", version_id: "y", task_id: "T", task_revision: 8 } }] },
    provider: { typed: { decision: { kind: "unchanged" }, evidence: {
        base_version_id: "v1", base_integrity_sha256: "compact-digest-1",
        candidate_sha256: "x", range: { start: RANGE[0], end: RANGE[1] },
        same_qfq_basis_ref: "b" } } } });
}
// frozen base missing on explicit item -> update_candidate Integrity err -> fallback -> published
RI({ stub: { head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: { nope: 1 } }),
    item_receipt: { publication_id: "p0", version_id: "v9", task_id: "T", task_revision: 9 } } });

// --------------------------------------------------------------- run_item
// verbatim error/outcome extraction through the real run_item path
const RITEM = (over) => c({ op: "run_item", task_id: "T", item_id: scope("sh.600000"),
  run_id: "R", stub: stubBase({ item_receipt: null }), provider: {}, ...over });
RITEM({ stub: stubBase({ item_receipt: { publication_id: "p1", version_id: "v1", task_id: "T", task_revision: 5 } }) }); // replay -> published
RITEM({ stub: stubBase({ head: head({ accepted_run_id: "OLD", owner_run_id: null }) }) }); // needs_verification visibility
RITEM({ stub: stubBase({ head: head({ status: "running" }), item: item({ stage: "running" }) }) }); // running item -> outcome_unknown
RITEM({ stub: stubBase({ head: head({ status: "succeeded" }),
    item: item({ stage: "finished", outcome: { success: true, error: null } }) }) }); // success without receipt -> integrity
RITEM({ stub: stubBase({ head: head({ status: "failed" }),
    item: item({ stage: "finished", outcome: { success: false, error: "x" } }) }) }); // -> not_published snapshot
RITEM({ stub: stubBase({ head: head({ status: "cancel_requested" }),
    item: item({ stage: "pending" }) }) }); // cancel -> finish_execution -> not_published
// pending head -> claim -> start_item -> acquire -> provider error recorded
RITEM({ stub: stubBase({ head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending" }), base_facts: null }),
  provider: { acquire: { error: { kind: "not_verified", detail: "no verifier" } } } });
// frozen shape violations inside acquire_and_commit_default
RITEM({ stub: stubBase({ head: head({ contract_version: "default-range-task-v1",
      status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: {
      selection_evidence: { evidence_digest: "se-1" }, base_version_id: "v1",
      base_integrity_sha256: BASE_RAW.integrity_sha256, refresh_range: null,
      proven_gap_ranges: [] } }), base_facts: null }),
  provider: { acquire: { error: { kind: "not_verified", detail: "x" } } } }); // update w/o refresh
RITEM({ stub: stubBase({ head: head({ contract_version: "default-range-task-v1",
      operation: "backfill_data", status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: {
      selection_evidence: { evidence_digest: "se-1" }, base_version_id: "v1",
      base_integrity_sha256: BASE_RAW.integrity_sha256,
      refresh_range: { start_date: "2024-01-02", end_date: "2024-01-05" },
      proven_gap_ranges: [] } }), base_facts: null }),
  provider: { acquire: { error: { kind: "not_verified", detail: "x" } } } }); // backfill w/ refresh
RITEM({ stub: stubBase({ head: head({ contract_version: "default-range-task-v1",
      status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: { bad: true } }), base_facts: null }),
  provider: { acquire: { error: { kind: "not_verified", detail: "x" } } } }); // malformed frozen
RITEM({ stub: stubBase({ head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending", frozen_inputs: { nope: 1 } }), base_facts: null }),
  provider: { acquire: { error: { kind: "not_verified", detail: "x" } } } }); // frozen base missing
RITEM({ stub: stubBase({ head: head({ status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending",
      frozen_inputs: { base_version_id: "v1", base_integrity_sha256: "d",
        range: { start: "not-a-date", end: "2024-01-05" } } }), base_facts: null }),
  provider: { acquire: { error: { kind: "not_verified", detail: "x" } } } }); // frozen date decode
RITEM({ stub: stubBase({ head: head({ operation: "collect_data" }) }) }); // unsupported contract
RITEM({ stub: stubBase({ head: head(), item: null }) }); // item absent -> invalid
RITEM({ stub: stubBase({ head: null, item: null }) }); // task absent -> task_head error

// ------------------------------------------------- serde seq decoding
// Struct seq form: fields consume slots in declaration order.
c({ op: "normalize", operation: "update", raw: { items: [
    ["sh.600000", "v1", "2024-01-02", "2024-01-05"]] } }); // EntityRange seq
c({ op: "normalize", operation: "update", raw: [["sh.600000", "v1",
    "2024-01-02", "2024-01-05"]] }); // ExplicitRanges seq-of-seq
c({ op: "normalize", operation: "update", raw: { items: [
    ["sh.600000", "v1", "2024-01-02"]] } }); // seq too short
c({ op: "normalize", operation: "update", raw: { items: [
    ["sh.600000", "v1", "2024-01-02", "2024-01-05", "extra"]] } }); // seq too long
c({ op: "normalize", operation: "update", raw: { items: "x" } });
c({ op: "normalize", operation: "update", raw: { items: [
    { symbol: "sh.600000", base_version_id: "v1", start_date: "2024-01-02",
      end_date: "2024-01-05", zbad: 1 }] } }); // unknown key last
c({ op: "normalize", operation: "update", raw: { items: [
    { zbad: 1, symbol: 5, base_version_id: "v1", start_date: "2024-01-02",
      end_date: "2024-01-05" }] } }); // unknown key first (order)
// Bar seq decode inside a batch.
c({ op: "parse_batch", batch: { ...batchOf("sh.600000", []), bars: [
    ["2024-01-02", "10", "11", "9.5", "10.5", "1000", "10500", null]] } });
c({ op: "parse_batch", batch: { ...batchOf("sh.600000", []), bars: [
    ["2024-01-02", "10", "11", "9.5", "10.5"]] } }); // optionals omitted
// DateTime leniency: +08:00 offset normalizes to UTC in Value output.
c({ op: "datetime", value: "2024-01-06T16:00:00+08:00" });
c({ op: "datetime", value: "2024-01-06T08:00:00.250Z" });

// ------------------------------------------ republish contract update_candidate
RI({ stub: stubBase({ head: head({
      operation: "republish_data", contract_version: "explicit-republish-task-v1",
      status: "pending", owner_run_id: null,
      counts: { total: 1, pending: 1, running: 0, finished: 0, succeeded: 0 } }),
    item: item({ stage: "pending",
      frozen_inputs: frozenIn(BASE_RAW, RANGE) }),
    base_facts: null }),
  provider: { acquire: { error: { kind: "not_verified", detail: "x" } } } });

process.stdout.write(cases.map((x) => JSON.stringify(x)).join("\n") + "\n");
