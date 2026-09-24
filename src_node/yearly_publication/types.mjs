// Shared types/constants for yearly_publication — port of the serde
// surface in src/yearly_publication.rs + tasks.rs + task_control
// values used by the pure layer.
//
// Error parity:
//   market_storage::Error  -> MarketError(kind, "PREFIX: msg")
//   task_control::Error    -> StorageError(code, "PREFIX: msg")
//   SourceError::NotVerified (verifier seam) -> YearlySeamError? no —
//   verifier failures carry the Rust SourceError::NotVerified text on
//   Error::Base via the runner; the typed seam is only for unported
//   storage-commit arms.
import { MarketError } from "../market/record_codec.mjs";
import { StorageError } from "../storage/sqlite_adapter.mjs";

export const CONTRACT_VERSION = "explicit-yearly-task-v1";
export const DATASET = "market.yearly";
export const YEARLY_ADAPTER = "local-yearly-publication-v1";

// ---- market_storage::Error constructors (verbatim Display) ----
export const mInvalid = (m) => new MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);
export const mBase = (m) => new MarketError("base", `BASE_UNAVAILABLE: ${m}`);
export const mConflict = (m) => new MarketError("conflict", `PUBLISH_PRECONDITION: ${m}`);
export const mIntegrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
export const mNotFound = () => new MarketError("not_found", "MARKET_NOT_FOUND");
export const mOutcomeUnknown = (m) =>
  new MarketError("outcome_unknown",
    `PUBLISH_OUTCOME_UNKNOWN: ${m}; inspect the same publication_id`);
export const mStorage = (m) => new MarketError("storage", `MARKET_STORAGE: ${m}`);
export const mTask = (e) => e; // task_control errors pass through as-is

// ---- task_control::Error constructors (verbatim Display) ----
export const tcInvalid = (m) => new StorageError("invalid_input", `INVALID_COMMAND: ${m}`);
export const tcNeedsVerification = (m) =>
  new StorageError("needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: ${m}`);
export const tcStorageUnavailable = (m) =>
  new StorageError("unavailable", `STORAGE_UNAVAILABLE${m ? `: ${m}` : ""}`);
export const tcOutcomeUnknown = (m) =>
  new StorageError("outcome_unknown", `OUTCOME_UNKNOWN: ${m}`);
export const tcTaskNotFound = () =>
  new StorageError("task_not_found", "TASK_NOT_FOUND");

/** tasks.rs::repository_error — MarketError -> task_control::Error */
export const repositoryError = (e) => {
  if (e instanceof StorageError) return e; // MarketError::Task passthrough
  if (!(e instanceof MarketError)) return e;
  switch (e.kind) {
    case "storage": return tcStorageUnavailable(e.message.slice("MARKET_STORAGE".length + 2));
    case "outcome_unknown": return tcOutcomeUnknown(e.message);
    case "invalid": return tcInvalid(e.message.slice("INVALID_MARKET_COMMAND".length + 2));
    default:
      return tcNeedsVerification(e.message);
  }
};

// ---- task_control::Status / Stage as wire strings ----
export const Status = {
  Pending: "pending", Running: "running", CancelRequested: "cancel_requested",
  NeedsVerification: "needs_verification", Succeeded: "succeeded",
  PartiallyFailed: "partially_failed", Failed: "failed",
  Cancelled: "cancelled", Interrupted: "interrupted",
};
const TERMINAL = new Set(["succeeded", "partially_failed", "failed",
  "cancelled", "interrupted"]);
export const statusTerminal = (s) => TERMINAL.has(s);
// task_control::visible_status
export const visibleStatus = (status, accepted, owner, run) =>
  !TERMINAL.has(status) && (accepted !== run || (owner !== null && owner !== run))
    ? "needs_verification" : status;
export const Stage = { Pending: "pending", Running: "running", Finished: "finished" };

/** task_control::legacy::scope_id — compact JSON triple. */
export const scopeId = (symbol, period) =>
  JSON.stringify(["market", symbol, period]);

// ---- PublishIntent serde (externally tagged enum) ----
// "HistoricalFixedBase" | {"Daily":{expected_head:str|null}} |
// {"CurrentHead":{expected_daily_head:str, expected_yearly_head:str|null}}
const SERDE_FIELDS = {
  Daily: ["expected_head"],
  CurrentHead: ["expected_daily_head", "expected_yearly_head"],
};
export const parseIntent = (v, missing = "invalid") => {
  const bad = (m) => { throw tcInvalid(m); };
  if (v === "HistoricalFixedBase") return "HistoricalFixedBase";
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    return bad(`invalid type: ${v === null ? "null" : typeof v}, expected enum PublishIntent`);
  }
  const keys = Object.keys(v);
  if (keys.length !== 1 || !(keys[0] in SERDE_FIELDS)) {
    return bad(`unknown variant \`${keys[0]}\`, expected \`Daily\` or \`HistoricalFixedBase\` or \`CurrentHead\``);
  }
  const variant = keys[0];
  const body = v[variant];
  if (body === null || typeof body !== "object" || Array.isArray(body)) {
    return bad(`invalid type: expected struct variant ${variant}`);
  }
  for (const k of Object.keys(body)) {
    if (!SERDE_FIELDS[variant].includes(k)) {
      return bad(`unknown field \`${k}\`, expected ${SERDE_FIELDS[variant].map((f) => `\`${f}\``).join(" or ")}`);
    }
  }
  const out = {};
  if (variant === "Daily") {
    const h = body.expected_head;
    if (h !== undefined && h !== null && typeof h !== "string") {
      return bad("invalid type: expected option string");
    }
    out.expected_head = h ?? null;
  } else {
    if (typeof body.expected_daily_head !== "string") {
      return bad("missing field `expected_daily_head`");
    }
    const yh = body.expected_yearly_head;
    if (yh !== undefined && yh !== null && typeof yh !== "string") {
      return bad("invalid type: expected option string");
    }
    out.expected_daily_head = body.expected_daily_head;
    out.expected_yearly_head = yh ?? null;
  }
  return { [variant]: out };
};
export const serIntent = (i) => {
  if (i === "HistoricalFixedBase") return "HistoricalFixedBase";
  const v = Object.keys(i)[0];
  return { [v]: i[v] };
};
// Normalized intent accessors — Rust pattern-matches the variants.
export const intentVariant = (i) =>
  typeof i === "string" ? i : Object.keys(i)[0];
export const intentBody = (i) => (typeof i === "string" ? {} : i[intentVariant(i)]);

/** Publication serde shape (7 fields; intent per parseIntent). */
export const parsePublication = (c, err = mIntegrity) => {
  if (c === null || typeof c !== "object" || Array.isArray(c)) {
    throw err("missing field `publication_id`");
  }
  for (const k of ["publication_id", "version_id", "task_id", "item_id", "run_id"]) {
    if (typeof c[k] !== "string") throw err(`missing field \`${k}\``);
  }
  if (typeof c.expected_revision !== "number" || !Number.isInteger(c.expected_revision)) {
    throw err("missing field `expected_revision`");
  }
  if (c.intent === undefined) throw err("missing field `intent`");
  return c;
};

// ---- value equality mirroring serde PartialEq on Values ----
export const valEq = (a, b) => {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") {
    return false;
  }
  const aArr = Array.isArray(a), bArr = Array.isArray(b);
  if (aArr !== bArr) return false;
  if (aArr) {
    return a.length === b.length && a.every((x, i) => valEq(x, b[i]));
  }
  const ka = Object.keys(a), kb = Object.keys(b);
  return ka.length === kb.length &&
    ka.every((k) => k in b && valEq(a[k], b[k]));
};

/** Typed seam for storage-commit arms not yet ported on Node —
 *  fail-closed by construction: the arm refuses rather than guessing. */
export class YearlySeamError extends Error {
  constructor(arm, detail = "") {
    super(`seam: storage commit arm not ported on Node: ${arm}${detail ? ` — ${detail}` : ""}`);
    this.name = "YearlySeamError";
    this.kind = "seam";
    this.arm = arm;
  }
}

// ---- BTreeSet<i32> emulation: sorted unique int array ----
export const asBTreeSet = (arr, err) => {
  if (!Array.isArray(arr)) throw err("invalid type: expected a sequence");
  const s = new Set();
  for (const y of arr) {
    if (!Number.isInteger(y) || y < -2147483648 || y > 2147483647) {
      throw err("invalid type: expected i32");
    }
    s.add(y);
  }
  return [...s].sort((a, b) => a - b);
};
export const setSubset = (a, b) => a.every((x) => b.includes(x));
