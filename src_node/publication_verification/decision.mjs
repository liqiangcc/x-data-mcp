// NM-098 — src/publication_verification/decision.rs port.
// Publication conclusions do not decide whether original writers have
// stopped. The pure prepare/validate_audit engine landed in NM-056 as
// storage/publication_checks.mjs::prepareCheck/validateCheckAudit —
// this module re-exposes it with the domain layer's types and adds the
// wire shapes + AuditStore seam.
import { prepareCheck, validateCheckAudit }
  from "../storage/publication_checks.mjs";

// Conclusion — serde snake_case enum: "all_recorded" |
// "insufficient_evidence" | "not_applicable".
export const CONCLUSION = Object.freeze({
  ALL_RECORDED: "all_recorded",
  INSUFFICIENT: "insufficient_evidence",
  NOT_APPLICABLE: "not_applicable",
});

// Receipt wire shape — serde field order:
// verification_task_id, target_task_id, verification_revision,
// target_revision, conclusion, reason, missing_receipt_item_ids.
// Audit — actor, checked_at, facts, verification_before,
// verification_after, receipt.

/** decision::prepare — evaluates a committed check snapshot plus the
 *  evidence facts into a sealed Plan {audit}. check/facts use the
 *  canonical serde snapshot shape (snapshotCanon output).
 *  Throws task-layer errors (MarketError kind "task"). */
export const prepare = prepareCheck;

export const receipt = (plan) => plan.audit.receipt;
export const finishedCheck = (plan) => plan.audit.verification_after;

/** decision::validate_audit — replays prepare over the embedded
 *  snapshots; mismatch -> NeedsVerification verbatim. */
export const validateAudit = validateCheckAudit;

// AuditStore seam (implemented by store.mjs):
//   commitCheck(plan) -> Receipt   (market_storage::Error)
//   checkReceipt(id)  -> Receipt?  (market_storage::Error)
