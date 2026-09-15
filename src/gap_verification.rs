//! Trusted source-gap verifier port and atomic decision producer (#155,
//! Final Gate ruling 5675608420, docs/tasks/issue-155-gap-producer.md §3).
//!
//! The verifier is a sealed implementation port — `Serialize`-only output,
//! never `Deserialize`: a JSON claim is not verifier output
//! (`StopInspector` precedent). A caller may name only the target
//! (symbol + day range); subject version, conclusion, digests and verifier
//! identity are derived server-side and are never caller input.
//!
//! Producer flow (no wire/task acceptance here; that is P3):
//! 1. `freeze_subject_on` freezes the current effective published QFQ
//!    daily head inside one read snapshot, fully re-verified by
//!    `read_record` (identity, period, adjustment, digest, typed records);
//! 2. `decide_gap` asks the installed verifier for a durable basis, then
//!    re-checks every binding fail-closed (target/subject echo, verifier
//!    identity, canonical digests);
//! 3. the decision commits through `record_gap_decision_guarded`, whose
//!    write transaction re-asserts the effective head still equals the
//!    frozen subject (stale guard) before inserting decision + proven gap
//!    atomically.
use crate::{
    market_data::Period,
    market_storage::{Error, GapVerificationDecision, Scope},
    sqlite_market::{SqliteMarket, head, read_record},
};
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
/// `basis_kind` recorded on decisions produced through this contract.
pub const GAP_BASIS_KIND: &str = "source_gap_verifier";
/// The only caller input a gap verification ever names. Day-only in the
/// first phase; yearly stays derived from fixed local QFQ daily data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapTarget {
    pub symbol: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
fn validate_target(t: &GapTarget) -> Result<(), Error> {
    if t.symbol.trim().is_empty() || t.start_date > t.end_date {
        return Err(Error::Invalid("invalid gap verification target".into()));
    }
    Ok(())
}
/// The published QFQ daily version a decision is relative to, frozen in
/// one acceptance read snapshot and re-checked at decision commit
/// (stale-head guard).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectBinding {
    pub version_id: String,
    pub integrity_sha256: String,
}
/// Durable verifier output. Deliberately `Serialize`-only: serializing a
/// claim does not make one. The basis must bind source identity +
/// contract, the requested target, the frozen subject, the checked source
/// facts/artifact identity + digest, a conclusion, observation time, the
/// verifier's self-declared identity, and a canonical digest over all of
/// them.
#[derive(Debug, Clone, Serialize)]
pub struct GapBasis {
    pub source_identity: String,
    pub source_contract: String,
    /// Target echo; must equal the requested target exactly.
    pub symbol: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    /// Subject echo; must equal the frozen subject binding.
    pub subject_version_id: String,
    pub subject_version_digest: String,
    /// Identity + digest of the checked source facts/artifact the
    /// conclusion replays from.
    pub artifact_identity: String,
    pub artifact_sha256: String,
    /// `confirmed_missing_range` = provable local coverage gap relative to
    /// the subject version (produces a proven gap). Any other conclusion —
    /// e.g. a source limitation — stores a decision without a gap.
    pub conclusion: String,
    pub observed_at: DateTime<Utc>,
    /// Self-declared verifier identity; must equal the installed port's
    /// `verifier_id()`, so a basis minted by one verifier cannot be
    /// replayed through another.
    pub verifier_id: String,
    /// Canonical digest over every field above (`gap-basis-v1`).
    pub basis_sha256: String,
}
impl GapBasis {
    /// Canonical digest over every bound field except `basis_sha256`
    /// (`gap-basis-v1` domain-separated).
    pub fn content_digest(&self) -> String {
        crate::hash::sha256(
            json!([
                "gap-basis-v1",
                self.source_identity,
                self.source_contract,
                self.symbol,
                "day",
                self.start_date.to_string(),
                self.end_date.to_string(),
                self.subject_version_id,
                self.subject_version_digest,
                self.artifact_identity,
                self.artifact_sha256,
                self.conclusion,
                self.observed_at.to_rfc3339(),
                self.verifier_id,
            ])
            .to_string()
            .as_bytes(),
        )
    }
    /// Mechanical invariants only; semantic truth belongs to the verifier
    /// and is never decided here.
    pub fn validate(&self) -> Result<(), Error> {
        if self.source_identity.trim().is_empty()
            || self.source_contract.trim().is_empty()
            || self.symbol.trim().is_empty()
            || self.subject_version_id.trim().is_empty()
            || !crate::hash::is_sha256(&self.subject_version_digest)
            || self.artifact_identity.trim().is_empty()
            || !crate::hash::is_sha256(&self.artifact_sha256)
            || self.conclusion.trim().is_empty()
            || self.verifier_id.trim().is_empty()
            || !crate::hash::is_sha256(&self.basis_sha256)
            || self.start_date > self.end_date
        {
            return Err(Error::Invalid("invalid gap basis".into()));
        }
        Ok(())
    }
}
/// Installed trusted source-gap verifier. `verifier_id` is the
/// server-derived identity (implementation/version + source
/// contract/config identity); callers can never supply it. An absent or
/// unauthorized verifier fails closed.
pub trait SourceGapVerifier {
    fn verifier_id(&self) -> &str;
    fn verify(&self, target: &GapTarget, subject: &SubjectBinding) -> Result<GapBasis, Error>;
}
/// Production port while #54/#46/#71 stay unresolved: no real source-gap
/// evidence protocol exists, so every verification fails closed.
pub struct UnverifiedGapVerifier;
impl SourceGapVerifier for UnverifiedGapVerifier {
    fn verifier_id(&self) -> &str {
        "unverified-source-gap-verifier"
    }
    fn verify(&self, _: &GapTarget, _: &SubjectBinding) -> Result<GapBasis, Error> {
        Err(Error::BasisNotVerified(
            "no installed trusted source-gap verifier (#54/#46/#71)".into(),
        ))
    }
}
/// Repository port: the subject freeze inside one read snapshot, the
/// frozen-subject re-verification at decision time, and the guarded
/// atomic decision+gap commit. Implemented by `SqliteMarket`.
pub trait GapDecisionStore {
    fn freeze_gap_subject(&self, target: &GapTarget) -> Result<SubjectBinding, Error>;
    fn check_gap_subject(&self, subject: &SubjectBinding) -> Result<(), Error>;
    fn commit_gap_decision(&self, decision: &GapVerificationDecision) -> Result<bool, Error>;
}
impl GapDecisionStore for SqliteMarket {
    fn freeze_gap_subject(&self, target: &GapTarget) -> Result<SubjectBinding, Error> {
        self.read(|c| freeze_subject_on(c, target))
    }
    fn check_gap_subject(&self, subject: &SubjectBinding) -> Result<(), Error> {
        self.read(|c| check_subject_on(c, subject))
    }
    fn commit_gap_decision(&self, decision: &GapVerificationDecision) -> Result<bool, Error> {
        self.record_gap_decision_guarded(decision)
    }
}
/// Freeze the subject inside one caller-provided read snapshot: the
/// current effective qfq daily head for `target.symbol`, fully re-verified
/// by `read_record`. A missing, unpublished, unreadable or mismatched
/// subject fails closed — a decision is never relative to an unverified
/// version.
pub fn freeze_subject_on(c: &Connection, target: &GapTarget) -> Result<SubjectBinding, Error> {
    validate_target(target)?;
    let scope = Scope {
        symbol: target.symbol.clone(),
        period: Period::Day,
    };
    let version_id = head(c, &scope)?.ok_or(Error::NotFound)?;
    let record = read_record(c, &version_id, true, false)?;
    let v = record.version();
    if v.symbol != target.symbol || v.period != Period::Day || v.version_id != version_id {
        return Err(Error::Integrity(
            "gap subject head identity mismatch".into(),
        ));
    }
    let integrity_sha256 = record.raw()["integrity_sha256"]
        .as_str()
        .filter(|d| crate::hash::is_sha256(d))
        .ok_or_else(|| Error::Integrity("gap subject digest unreadable".into()))?
        .to_owned();
    Ok(SubjectBinding {
        version_id,
        integrity_sha256,
    })
}
/// Re-verify a frozen subject binding against stored facts: the version
/// must still be a fully readable published QFQ daily record whose
/// integrity digest equals the frozen one. A fabricated or drifted
/// binding fails closed before the verifier is ever asked.
pub fn check_subject_on(c: &Connection, subject: &SubjectBinding) -> Result<(), Error> {
    if subject.version_id.trim().is_empty() || !crate::hash::is_sha256(&subject.integrity_sha256) {
        return Err(Error::Invalid("invalid gap subject binding".into()));
    }
    let record = read_record(c, &subject.version_id, true, false)?;
    if record.version().period != Period::Day
        || record.raw()["integrity_sha256"].as_str() != Some(subject.integrity_sha256.as_str())
    {
        return Err(Error::Integrity("gap subject binding mismatch".into()));
    }
    Ok(())
}
/// Produce and commit a gap-verification decision for `target` against a
/// previously frozen `subject`. Fail-closed end to end: verifier error or
/// absence, basis target/subject echo mismatch, verifier identity
/// mismatch, canonical digest mismatch, and a moved subject head
/// (in-transaction stale guard) all abort without writing any row.
/// Returns whether the decision was newly written; an identical replay is
/// a no-op.
pub fn decide_gap(
    store: &impl GapDecisionStore,
    verifier: &dyn SourceGapVerifier,
    target: &GapTarget,
    subject: &SubjectBinding,
    decision_id: &str,
    decided_at: DateTime<Utc>,
) -> Result<bool, Error> {
    validate_target(target)?;
    store.check_gap_subject(subject)?;
    let basis = verifier.verify(target, subject)?;
    basis.validate()?;
    if basis.symbol != target.symbol
        || basis.start_date != target.start_date
        || basis.end_date != target.end_date
    {
        return Err(Error::Integrity(
            "gap basis does not bind requested target".into(),
        ));
    }
    if basis.subject_version_id != subject.version_id
        || basis.subject_version_digest != subject.integrity_sha256
    {
        return Err(Error::Integrity(
            "gap basis does not bind frozen subject".into(),
        ));
    }
    if basis.verifier_id != verifier.verifier_id() {
        return Err(Error::Integrity(
            "gap basis verifier identity mismatch".into(),
        ));
    }
    if basis.basis_sha256 != basis.content_digest() {
        return Err(Error::Integrity("gap basis digest mismatch".into()));
    }
    let mut decision = GapVerificationDecision {
        decision_id: decision_id.into(),
        symbol: target.symbol.clone(),
        period: Period::Day,
        start_date: target.start_date,
        end_date: target.end_date,
        subject_version_id: subject.version_id.clone(),
        subject_version_digest: subject.integrity_sha256.clone(),
        conclusion: basis.conclusion.clone(),
        basis_kind: GAP_BASIS_KIND.into(),
        basis_ref: basis.artifact_identity.clone(),
        basis_sha256: basis.basis_sha256.clone(),
        verifier_id: verifier.verifier_id().into(),
        decided_at,
        decision_sha256: String::new(),
    };
    decision.decision_sha256 = decision.content_digest();
    store.commit_gap_decision(&decision)
}
#[cfg(test)]
mod tests;
