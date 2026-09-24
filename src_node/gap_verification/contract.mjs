// NM-076 — src/gap_verification/contract.rs port.
// Acceptance contract for `verify_data_gap`: first acceptance freezes
// the published QFQ daily subject inside one read snapshot; the task
// payload stays the deterministic minimal {"target":{...}}.
import {
  tStruct, tString, tEnumUnit, tDate, decode, Decode,
} from "../mcp/serde_value.mjs";
import { MarketError } from "../market/record_codec.mjs";
import { commandKey } from "./mod.mjs";

// task_control::Error verbatim display texts (kind "task" marks the
// tc layer — market_storage::Error::Task(e) keeps e's own Display).
export const taskError = (m) => new MarketError("task", m);
const invalidCommand = (m) => taskError(`INVALID_COMMAND: ${m}`);

// market_update::tasks::repository_error — market_storage::Error ->
// task_control::Error. `detail` carries the raw (un-prefixed) variant
// payload; w1 MarketErrors carry only the display text, so strip the
// kind prefix when detail is absent.
const RAW_PREFIX = {
  integrity: "MARKET_INTEGRITY: ",
  base: "BASE_UNAVAILABLE: ",
  conflict: "PUBLISH_PRECONDITION: ",
  invalid: "INVALID_MARKET_COMMAND: ",
  storage: "MARKET_STORAGE: ",
  outcome_unknown: null, // payload is un-prefixed inside the variant
  basis_not_verified: "GAP_BASIS_NOT_VERIFIED: ",
};
export const repositoryError = (e) => {
  if (e instanceof MarketError && e.kind === "task") return e;
  const p = RAW_PREFIX[e.kind];
  const raw = e.detail !== undefined ? e.detail
    : p ? e.message.replace(new RegExp("^" + p.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")), "")
    : e.message;
  switch (e.kind) {
    case "storage":
      return taskError(`STORAGE_UNAVAILABLE: ${raw}`);
    case "integrity": case "base": case "conflict":
      return taskError(`ACCEPTANCE_NEEDS_VERIFICATION: ${raw}`);
    case "outcome_unknown":
      return taskError(
        `OUTCOME_UNKNOWN: ${raw}; query the same request_id before retrying`);
    case "basis_not_verified":
      return taskError(
        `ACCEPTANCE_NEEDS_VERIFICATION: GAP_BASIS_NOT_VERIFIED: ${raw}`);
    case "invalid":
      return taskError(`INVALID_COMMAND: ${raw}`);
    case "not_found":
      return taskError("INVALID_COMMAND: explicit published daily base unavailable");
    default:
      return e;
  }
};

// Normalized command intent: target only. deny_unknown_fields makes
// any caller pass-through field (conclusion, digests,
// subject_version_*, verifier identity, ...) a decode failure, not a
// stored field.
export const IntentPeriod = tEnumUnit("IntentPeriod", ["day"]);
export const IntentTarget = tStruct("IntentTarget", [
  { name: "symbol", dec: tString },
  { name: "period", dec: IntentPeriod },
  { name: "start_date", dec: tDate },
  { name: "end_date", dec: tDate },
]);
export const GapIntent = tStruct("GapIntent", [
  { name: "target", dec: IntentTarget },
]);

const validSymbol = (s) => {
  const i = s.indexOf(".");
  if (i < 0) return false;
  const [exchange, code] = [s.slice(0, i), s.slice(i + 1)];
  return (exchange === "sh" || exchange === "sz") &&
    code.length === 6 && /^[0-9]{6}$/.test(code);
};

/// Rebuild the verified target from intent fields; malformed or
/// non-day evidence is an integrity failure, never a skip.
export const intentTargetToGap = (t) => {
  if (t.period !== "day" || !validSymbol(t.symbol) ||
      t.start_date > t.end_date) {
    throw invalidCommand("invalid gap verification target");
  }
  return {
    symbol: t.symbol,
    start_date: t.start_date,
    end_date: t.end_date,
  };
};

// Serde-stable frozen item evidence: the caller-named target and the
// subject binding frozen from the acceptance snapshot.
export const GapFrozen = tStruct("GapFrozen", [
  { name: "target", dec: IntentTarget },
  { name: "subject_version_id", dec: tString },
  { name: "subject_version_digest", dec: tString },
]);

// market_storage::Error::Integrity — verbatim MARKET_INTEGRITY text.
const integrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);

/// Rebuild the verified target/subject pair from frozen item
/// evidence. Malformed or non-day evidence is an integrity failure,
/// never a skip.
export const gapFrozenParts = (frozen) => {
  let target;
  try {
    target = intentTargetToGap(frozen.target);
  } catch {
    throw integrity("gap frozen target invalid");
  }
  return [target, {
    version_id: frozen.subject_version_id,
    integrity_sha256: frozen.subject_version_digest,
  }];
};

/// Strictly decode the frozen item shape; malformed input is a storage
/// integrity failure, never a silent skip.
export const parseGapFrozen = (frozen) => {
  try {
    return decode(GapFrozen, frozen);
  } catch (e) {
    if (e instanceof Decode) {
      throw integrity(`gap frozen inputs: ${e.message}`);
    }
    throw e;
  }
};

// task_control::legacy::scope_id — `["market",symbol,period]` JSON.
const scopeId = (symbol, period) => JSON.stringify(["market", symbol, period]);

/// Application contract for `verify_data_gap`/`market.gap_verification`/
/// `gap-verification-task-v1`: one item per task, occupying the same
/// symbol x day resource as daily writes.
// store: { freezeGapSubject(target) -> SubjectBinding }
export class GapVerificationContract {
  constructor(store) {
    this.store = store;
  }
  key() { return commandKey(); }
  normalize(payload) {
    let intent;
    try {
      intent = decode(GapIntent, payload);
    } catch (e) {
      if (e instanceof Decode) throw invalidCommand(e.message);
      throw e;
    }
    intentTargetToGap(intent.target);
    return intent;
  }
  resolve(intent) {
    let parsed;
    try {
      parsed = decode(GapIntent, intent);
    } catch (e) {
      if (e instanceof Decode) throw invalidCommand(e.message);
      throw e;
    }
    const target = intentTargetToGap(parsed.target);
    // repository_error: market_storage::Error -> task_control::Error.
    let subject;
    try {
      subject = this.store.freezeGapSubject(target);
    } catch (e) {
      throw repositoryError(e);
    }
    const scope = scopeId(target.symbol, "daily");
    const item = {
      item_id: target.symbol,
      scope_keys: new Set([scope]),
      target: { symbol: target.symbol, period: "day" },
      progress_unit: "evidence_check",
      frozen_inputs: {
        target: parsed.target,
        subject_version_id: subject.version_id,
        subject_version_digest: subject.integrity_sha256,
      },
    };
    return {
      scopes: new Set([scope]),
      items: [item],
      shared_evidence: null,
    };
  }
}
