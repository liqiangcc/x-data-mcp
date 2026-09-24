// NM-098 — src/publication_verification.rs (mod) port.
// Evidence facts, not permission to republish or release a task's
// claims. Facts/ItemFact/Evidence canonical wire shapes, contract
// re-export, and the verbatim task_control::Error / market_storage::
// Error display helpers shared by admission/decision/existing/runner.
import { MarketError } from "../market/record_codec.mjs";
import { StorageError } from "../storage/sqlite_adapter.mjs";
import { CHECK_KEY } from "../storage/publication_checks.mjs";
import { contractKind } from "../market/verification_facts.mjs";

export { CHECK_KEY, contractKind };

// ---- canonical wire shapes -----------------------------------------
// serde externally-tagged Evidence enum (no rename): "Missing" or
// {"Committed":{receipt,reused_version,evidence_sha256}}.
export const evMissing = () => "Missing";
export const evCommitted = (receipt, reusedVersion, evidenceSha256) => ({
  Committed: {
    receipt, reused_version: reusedVersion, evidence_sha256: evidenceSha256,
  },
});
export const isMissing = (e) => e === "Missing";

// ---- error helpers --------------------------------------------------
// task_control::Error — kind "task" carries the verbatim Display text.
export const tcError = (m) => new MarketError("task", m);
export const tcInvalidCommand = (m) => tcError(`INVALID_COMMAND: ${m}`);
export const tcTaskNotFound = () => tcError("TASK_NOT_FOUND");
export const tcUnsupportedContract = () => tcError("UNSUPPORTED_CONTRACT");
export const tcRevisionConflict = () => tcError("STATE_REVISION_CONFLICT");
export const tcRequestConflict = () => tcError("REQUEST_CONFLICT");
export const tcNeedsVerification = (m) =>
  tcError(`ACCEPTANCE_NEEDS_VERIFICATION: ${m}`);
export const tcOutcomeUnknown = (m) =>
  tcError(`OUTCOME_UNKNOWN: ${m}; query the same request_id before retrying`);
export const tcStorageUnavailable = (m) => tcError(`STORAGE_UNAVAILABLE: ${m}`);

// market_storage::Error — kind mirrors the variant name; message is the
// verbatim Display text.
export const msError = (kind, m) => new MarketError(kind, m);
export const msTask = (tcErr) => tcErr instanceof MarketError &&
  tcErr.kind === "task" ? tcErr : msError("task", String(tcErr));
export const msStorage = (m) => msError("storage", `MARKET_STORAGE: ${m}`);
export const msIntegrity = (m) => msError("integrity", `MARKET_INTEGRITY: ${m}`);
export const msConflict = (m) => msError("conflict", `PUBLISH_PRECONDITION: ${m}`);
export const msOutcomeUnknown = (m) =>
  msError("outcome_unknown", `PUBLISH_OUTCOME_UNKNOWN: ${m}; inspect the same publication_id`);
export const msInvalid = (m) => msError("invalid", `INVALID_MARKET_COMMAND: ${m}`);

// market_storage::Error -> task_control::Error (sqlite_market's
// task_error mapper): Task passthrough; Storage -> StorageUnavailable;
// OutcomeUnknown passthrough; anything else -> NeedsVerification(Display).
export const toTaskError = (e) => {
  if (e instanceof MarketError) {
    if (e.kind === "task") return e;
    const raw = e.detail ?? e.message;
    if (e.kind === "storage") {
      return tcStorageUnavailable(raw.replace(/^MARKET_STORAGE: /, ""));
    }
    if (e.kind === "outcome_unknown") return tcOutcomeUnknown(String(raw));
    return tcNeedsVerification(e.message);
  }
  if (e instanceof StorageError) {
    if (e.code === "needs_verification") return tcError(e.message);
    if (e.code === "revision_conflict") return tcRevisionConflict();
    if (e.code === "task_not_found") return tcTaskNotFound();
    if (e.code === "conflict") return tcError(e.message);
    if (e.message?.startsWith("STORAGE_UNAVAILABLE:")) return tcError(e.message);
    return tcStorageUnavailable(e.message);
  }
  return tcNeedsVerification(String(e?.message ?? e));
};
