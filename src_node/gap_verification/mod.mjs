// NM-076 — src/gap_verification.rs port (S10a app layer).
// Trusted source-gap verifier port and atomic decision producer.
// Producer flow (verbatim order):
//   1. freezeSubjectOn freezes the current effective published QFQ
//      daily head inside one read snapshot, fully re-verified by
//      readRecord (identity, period, adjustment, digest, typed records);
//   2. decideGap asks the installed verifier for a durable basis, then
//      re-checks every binding fail-closed (target/subject echo,
//      verifier identity, canonical digests);
//   3. the decision commits through recordGapDecisionGuarded (NM-056),
//      whose write transaction re-asserts the effective head still
//      equals the frozen subject (stale guard) before inserting
//      decision + proven gap atomically.
import { createHash } from "node:crypto";
import { MarketError } from "../market/record_codec.mjs";
import * as MR from "../market/market_read.mjs";
import * as GAPS from "../market/gaps.mjs";

const sha256 = (s) => createHash("sha256").update(s).digest("hex");
const isSha256 = (s) =>
  typeof s === "string" && s.length === 64 && /^[0-9a-f]+$/.test(s);

// market_storage::Error Display texts — verbatim. `detail` carries the
// raw variant payload (un-prefixed) for repository_error mapping.
const mk = (kind, text, raw) =>
  Object.assign(new MarketError(kind, text), { detail: raw });
const invalid = (m) => mk("invalid", `INVALID_MARKET_COMMAND: ${m}`, m);
const integrity = (m) => mk("integrity", `MARKET_INTEGRITY: ${m}`, m);
const notFound = () => mk("not_found", "MARKET_NOT_FOUND", "");
export const outcomeUnknown = (m) =>
  mk("outcome_unknown", `PUBLISH_OUTCOME_UNKNOWN: ${m}; inspect the same publication_id`, m);
export const basisNotVerified = (m) =>
  mk("basis_not_verified", `GAP_BASIS_NOT_VERIFIED: ${m}`, m);

export { invalid as gapInvalid, integrity as gapIntegrity, notFound as gapNotFound };

/// `basis_kind` recorded on decisions produced through this contract.
export const GAP_BASIS_KIND = "source_gap_verifier";
/// Public command identity (`verify_data_gap` /
/// `market.gap_verification` / `gap-verification-task-v1`); day-only.
export const OPERATION = "verify_data_gap";
export const DATASET = "market.gap_verification";
export const CONTRACT_VERSION = "gap-verification-task-v1";
export const commandKey = () => ({
  operation: OPERATION,
  dataset: DATASET,
  contract_version: CONTRACT_VERSION,
});

// The only caller input a gap verification ever names. Day-only in the
// first phase; yearly stays derived from fixed local QFQ daily data.
// GapTarget = { symbol, start_date, end_date } — canonical "YYYY-MM-DD".
export const validateTarget = (t) => {
  if (typeof t.symbol !== "string" || t.symbol.trim() === "" ||
      t.start_date > t.end_date) {
    throw invalid("invalid gap verification target");
  }
};

// The published QFQ daily version a decision is relative to, frozen in
// one acceptance read snapshot and re-checked at decision commit.
// SubjectBinding = { version_id, integrity_sha256 }.

// ------------------------------------------------------------------
// GapBasis — durable verifier output. Deliberately produce-only: a JSON
// claim is not verifier output (StopInspector precedent). Binds source
// identity + contract, the requested target, the frozen subject, the
// checked source facts/artifact identity + digest, a conclusion,
// observation time, verifier identity, and a canonical digest over all
// of them.
//
// basis = { source_identity, source_contract, symbol, start_date,
//           end_date, subject_version_id, subject_version_digest,
//           artifact_identity, artifact_sha256, conclusion,
//           observed_at, verifier_id, basis_sha256 }
// start/end/observed_at are canonical RFC3339/NaiveDate strings.

/// Canonical digest over every bound field except `basis_sha256`
/// (`gap-basis-v1` domain-separated; serde_json::json! array to_string).
export const basisContentDigest = (b) =>
  sha256(JSON.stringify([
    "gap-basis-v1",
    b.source_identity,
    b.source_contract,
    b.symbol,
    "day",
    b.start_date,
    b.end_date,
    b.subject_version_id,
    b.subject_version_digest,
    b.artifact_identity,
    b.artifact_sha256,
    b.conclusion,
    b.observed_at,
    b.verifier_id,
  ]));

const blank = (s) => typeof s !== "string" || s.trim() === "";

/// Mechanical invariants only; semantic truth belongs to the verifier
/// and is never decided here.
export const validateBasis = (b) => {
  if (blank(b.source_identity) || blank(b.source_contract) ||
      blank(b.symbol) || blank(b.subject_version_id) ||
      !isSha256(b.subject_version_digest) ||
      blank(b.artifact_identity) || !isSha256(b.artifact_sha256) ||
      blank(b.conclusion) || blank(b.verifier_id) ||
      !isSha256(b.basis_sha256) || b.start_date > b.end_date) {
    throw invalid("invalid gap basis");
  }
};

/// Installed trusted source-gap verifier. verifierId() is the
/// server-derived identity (implementation/version + source
/// contract/config identity); callers can never supply it. An absent
/// or unauthorized verifier fails closed.
//   interface: { verifierId(): string, verify(target, subject): basis }

/// Production port while #54/#46/#71 stay unresolved: no real
/// source-gap evidence protocol exists, so every verification fails
/// closed.
export class UnverifiedGapVerifier {
  verifierId() { return "unverified-source-gap-verifier"; }
  verify(_target, _subject) {
    throw basisNotVerified(
      "no installed trusted source-gap verifier (#54/#46/#71)");
  }
}

// ------------------------------------------------------------------
// GapDecisionStore port (SqliteMarket impl in Rust). The Node surface
// takes the opened db handle — the store adapter in store.mjs wires
// these onto one readwrite connection.
//
// freeze_gap_subject(target)  -> freezeSubjectOn(db, target)
// check_gap_subject(subject)  -> checkSubjectOn(db, subject)
// commit_gap_decision(d)      -> GAPS.recordGapDecisionGuarded(db, d)
// gap_decision(id)            -> GAPS.readDecision(db, id)

/// Freeze the subject inside one caller-provided read snapshot: the
/// current effective qfq daily head for `target.symbol`, fully
/// re-verified by `readRecord`. A missing, unpublished, unreadable or
/// mismatched subject fails closed — a decision is never relative to
/// an unverified version.
export const freezeSubjectOn = (db, target) => {
  validateTarget(target);
  const scope = { symbol: target.symbol, period: "Day" };
  const versionId = MR.head(db, scope);
  if (versionId === null || versionId === undefined) throw notFound();
  const record = MR.readRecord(db, versionId, true, false);
  const v = record.version;
  if (v.symbol !== target.symbol || v.period !== "Day" ||
      v.version_id !== versionId) {
    throw integrity("gap subject head identity mismatch");
  }
  const digest = record.raw["integrity_sha256"];
  if (typeof digest !== "string" || !isSha256(digest)) {
    throw integrity("gap subject digest unreadable");
  }
  return { version_id: versionId, integrity_sha256: digest };
};

/// Re-verify a frozen subject binding against stored facts: the version
/// must still be a fully readable published QFQ daily record whose
/// integrity digest equals the frozen one. A fabricated or drifted
/// binding fails closed before the verifier is ever asked.
export const checkSubjectOn = (db, subject) => {
  if (blank(subject.version_id) || !isSha256(subject.integrity_sha256)) {
    throw invalid("invalid gap subject binding");
  }
  const record = MR.readRecord(db, subject.version_id, true, false);
  if (record.version.period !== "Day" ||
      record.raw["integrity_sha256"] !== subject.integrity_sha256) {
    throw integrity("gap subject binding mismatch");
  }
};

/// Produce and commit a gap-verification decision for `target` against
/// a previously frozen `subject`. Fail-closed end to end: verifier
/// error or absence, basis target/subject echo mismatch, verifier
/// identity mismatch, canonical digest mismatch, and a moved subject
/// head (in-transaction stale guard) all abort without writing any
/// row. Returns whether the decision was newly written; an identical
/// replay is a no-op.
export const decideGap = (store, verifier, target, subject, decisionId, decidedAt) => {
  validateTarget(target);
  store.checkGapSubject(subject);
  const basis = verifier.verify(target, subject);
  validateBasis(basis);
  if (basis.symbol !== target.symbol ||
      basis.start_date !== target.start_date ||
      basis.end_date !== target.end_date) {
    throw integrity("gap basis does not bind requested target");
  }
  if (basis.subject_version_id !== subject.version_id ||
      basis.subject_version_digest !== subject.integrity_sha256) {
    throw integrity("gap basis does not bind frozen subject");
  }
  if (basis.verifier_id !== verifier.verifierId()) {
    throw integrity("gap basis verifier identity mismatch");
  }
  if (basis.basis_sha256 !== basisContentDigest(basis)) {
    throw integrity("gap basis digest mismatch");
  }
  const decision = {
    decision_id: decisionId,
    symbol: target.symbol,
    period: "Day",
    start_date: target.start_date,
    end_date: target.end_date,
    subject_version_id: subject.version_id,
    subject_version_digest: subject.integrity_sha256,
    conclusion: basis.conclusion,
    basis_kind: GAP_BASIS_KIND,
    basis_ref: basis.artifact_identity,
    basis_sha256: basis.basis_sha256,
    verifier_id: verifier.verifierId(),
    decided_at: decidedAt,
    decision_sha256: "",
  };
  decision.decision_sha256 = GAPS.decisionContentDigest(decision);
  return store.commitGapDecision(decision);
};
