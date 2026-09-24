// NM-082 — src/market_update/tasks.rs port: contract versions, Operation,
// EntityRange/ExplicitRanges strict decode, RangeContract
// key/normalize/resolve, frozen() shape, DefaultFrozen strict decode +
// required_ranges/union, parse_default_frozen, repository_error mapping.
import {
  tDate, tString, tStruct, tOpt, tVec, tValue, serValue, serF64,
  dateCmp, dateCanon, Decode,
} from "./serde.mjs";
import {
  MarketError, integrity, tcError, tcInvalid, tcNeedsVerification,
  tcStorageUnavailable, tcOutcomeUnknown, innerMsg,
} from "./mod.mjs";

export const CONTRACT_VERSION = "explicit-range-task-v1";
/// Snapshot-resolved default daily contract — distinct identity from
/// explicit ranges; never shares command identity with CONTRACT_VERSION.
export const DEFAULT_CONTRACT_VERSION = "default-range-task-v1";
/// Explicit republish contract (#247): same payload shape, distinct
/// command identity.
export const REPUBLISH_CONTRACT_VERSION = "explicit-republish-task-v1";

// Operation::key() -> CommandKey
export const operationKey = (operation) => ({
  operation: { update: "update_data", backfill: "backfill_data", republish: "republish_data" }[operation],
  dataset: "market.daily",
  contract_version: operation === "republish" ? REPUBLISH_CONTRACT_VERSION : CONTRACT_VERSION,
});

const invalidCmd = (m) => tcInvalid(m);

// EntityRange {symbol, base_version_id, start_date, end_date} —
// deny_unknown_fields; Ord derives in field order.
export const tEntityRange = tStruct("EntityRange", [
  { name: "symbol", dec: tString },
  { name: "base_version_id", dec: tString },
  { name: "start_date", dec: tDate },
  { name: "end_date", dec: tDate },
], true);
export const tExplicitRanges = tStruct("ExplicitRanges", [
  { name: "items", dec: tVec(tEntityRange) },
], true);
const entityRangeCmp = (a, b) =>
  (a.symbol < b.symbol ? -1 : a.symbol > b.symbol ? 1 : 0) ||
  (a.base_version_id < b.base_version_id ? -1 : a.base_version_id > b.base_version_id ? 1 : 0) ||
  dateCmp(a.start_date, b.start_date) || dateCmp(a.end_date, b.end_date);
const entityRangeEq = (a, b) => entityRangeCmp(a, b) === 0;
const entityRangeValue = (i) => ({
  symbol: i.symbol, base_version_id: i.base_version_id,
  start_date: i.start_date.s, end_date: i.end_date.s,
});

// task_control::legacy::scope_id(symbol, period) — serialized
// ["market", symbol, period] JSON array.
export const scopeId = (symbol, period) => serValue(["market", symbol, period]);
export const scopeOccupancyKey = scopeId;

// frozen(id, digest, range) — json! object -> sorted Value.
export const frozen = (id, digest, range) => ({
  base_integrity_sha256: digest,
  base_version_id: id,
  range: { end: range.end.s ?? range.end, start: range.start.s ?? range.start },
});

// RangeContract — {operation, repository, today} where today() returns a
// parsed {y,mo,d,s} date and repository is the MarketRepository seam.
export const rangeContract = ({ operation, repository, today }) => ({
  key: () => operationKey(operation),
  // Contract::normalize — strict decode -> per-item validation ->
  // sort/dedup -> budget -> same-symbol conflict -> to_value.
  normalize(raw) {
    let input;
    try { input = tExplicitRanges(raw); } catch (e) { throw invalidCmd(e.message); }
    for (const item of input.items) {
      const dot = item.symbol.indexOf(".");
      const validSymbol = dot >= 0 &&
        (item.symbol.slice(0, dot) === "sh" || item.symbol.slice(0, dot) === "sz") &&
        /^[0-9]{6}$/.test(item.symbol.slice(dot + 1));
      if (!validSymbol || item.base_version_id.trim() === "" ||
          dateCmp(item.start_date, item.end_date) > 0 ||
          item.start_date.y < 1 || item.end_date.y > 9999) {
        throw invalidCmd("invalid daily entity/base/date range");
      }
    }
    input.items.sort(entityRangeCmp);
    input.items = input.items.filter((x, i) => i === 0 || !entityRangeEq(x, input.items[i - 1]));
    if (input.items.length === 0 || input.items.length > 100) {
      throw invalidCmd("explicit range budget 1..100");
    }
    for (let i = 1; i < input.items.length; i++) {
      if (input.items[i - 1].symbol === input.items[i].symbol) {
        throw invalidCmd("conflicting ranges or base versions for the same entity");
      }
    }
    return { items: input.items.map(entityRangeValue) };
  },
  // Contract::resolve — strict decode -> future gate -> per-item
  // fixed_facts + identity -> FrozenWork.
  resolve(raw) {
    let input;
    try { input = tExplicitRanges(raw); } catch (e) { throw invalidCmd(e.message); }
    const todayD = today();
    const future = input.items
      .filter((i) => dateCmp(i.end_date, todayD) > 0)
      .map((i) => ({ symbol: i.symbol, start_date: i.start_date.s, end_date: i.end_date.s }));
    if (future.length > 0) {
      throw invalidCmd(`future daily write rejected on ${todayD.s}: ${serValue(future)}`);
    }
    const items = [];
    const scopes = new Set();
    for (const item of input.items) {
      let base;
      try { base = repository.fixed_facts(item.base_version_id); }
      catch (e) { throw repositoryError(e); }
      if (base.symbol !== item.symbol || base.period !== "Day" ||
          base.version_id !== item.base_version_id) {
        throw invalidCmd("fixed daily base identity mismatch");
      }
      const scope = scopeId(item.symbol, "day");
      scopes.add(scope);
      items.push({
        item_id: scope,
        scope_keys: [scope],
        target: { period: "day", symbol: item.symbol },
        progress_unit: "security_period",
        frozen_inputs: frozen(item.base_version_id, base.integrity_sha256,
          { start: item.start_date, end: item.end_date }),
      });
    }
    // FrozenWork{items, scopes(BTreeSet), shared_evidence:None} — serde
    // omits shared_evidence entirely when None (skip_serializing_if).
    return { items, scopes: [...scopes].sort() };
  },
});

// ------------------------------------------------------ DefaultFrozen
// Strict decode (deny_unknown_fields everywhere); malformed input is a
// storage integrity failure, never a silent skip.
const tFrozenDayRange = tStruct("FrozenDayRange", [
  { name: "start_date", dec: tDate },
  { name: "end_date", dec: tDate },
], true);
const tFrozenGapRange = tStruct("FrozenGapRange", [
  { name: "gap_id", dec: tString },
  { name: "range", dec: tFrozenDayRange },
  { name: "evidence_digest", dec: tString },
], true);
const tDefaultFrozenEvidence = tStruct("DefaultFrozenEvidence", [
  { name: "evidence_digest", dec: tString },
], true);
const tDefaultFrozen = tStruct("DefaultFrozen", [
  { name: "selection_evidence", dec: tDefaultFrozenEvidence },
  { name: "base_version_id", dec: tString },
  { name: "base_integrity_sha256", dec: tString },
  { name: "refresh_range", dec: tOpt(tFrozenDayRange), default: () => null },
  { name: "proven_gap_ranges", dec: tVec(tFrozenGapRange) },
  { name: "source_generation_id", dec: tOpt(tValue), default: () => null },
], true);

// FrozenDayRange::range() — inverted bounds are an integrity failure.
export const frozenDayRange = (r) => {
  if (dateCmp(r.start_date, r.end_date) > 0) {
    throw integrity("frozen range inverted");
  }
  return { start: r.start_date, end: r.end_date };
};

// DefaultFrozen::required_ranges — refresh (update) plus every gap.
export const requiredRanges = (frozen) => {
  const ranges = [];
  if (frozen.refresh_range != null) ranges.push(frozenDayRange(frozen.refresh_range));
  for (const gap of frozen.proven_gap_ranges) ranges.push(frozenDayRange(gap.range));
  return ranges;
};
// DefaultFrozen::union — single fetch range satisfying every required
// range; empty required list is an integrity failure.
export const union = (frozen) => {
  const ranges = requiredRanges(frozen);
  if (ranges.length === 0) throw integrity("default item has no required range");
  return ranges.reduce((a, b) => ({
    start: dateCmp(a.start, b.start) <= 0 ? a.start : b.start,
    end: dateCmp(a.end, b.end) >= 0 ? a.end : b.end,
  }));
};

export const parseDefaultFrozen = (frozenV) => {
  try { return tDefaultFrozen(frozenV); }
  catch (e) {
    if (e instanceof Decode) throw integrity(`default frozen inputs: ${e.message}`);
    throw e;
  }
};

// repository_error — market_storage::Error -> task_control::Error.
export const repositoryError = (e) => {
  if (e != null && e.taskControl) return e;              // Error::Task passthrough
  if (!(e instanceof MarketError)) return tcStorageUnavailable(e.message);
  switch (e.kind) {
    case "storage": return tcStorageUnavailable(innerMsg(e));
    case "integrity":
    case "base":
    case "conflict": return tcNeedsVerification(innerMsg(e));
    case "outcome_unknown": return tcOutcomeUnknown(innerMsg(e));
    case "basis_not_verified":
      return tcNeedsVerification(`GAP_BASIS_NOT_VERIFIED: ${innerMsg(e)}`);
    case "invalid": return tcInvalid(innerMsg(e));
    case "not_found": return tcInvalid("explicit published daily base unavailable");
    default: return tcInvalid(e.message);
  }
};
