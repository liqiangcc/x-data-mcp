//! Cross-file compatibility is a separate gate from per-file byte integrity.
//!
//! This module deliberately accepts compatibility only from an explicit,
//! immutable proof that binds all required file roles to their exact hashes,
//! source identity and revision window. Matching URLs, dates, ETags, or
//! hashes alone are retained as evidence but never inferred as compatibility.

use crate::snapshot::FileRole;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CONTRACT_VERSION: &str = "official-file-consistency-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteIntegrity {
    Verified,
    Unverified,
}

/// Facts produced by the per-file input verifier. This type contains no
/// semantic assertion that the three files can be used together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileFact {
    pub role: FileRole,
    pub bytes: u64,
    pub sha256: String,
    pub source_url: String,
    pub acquisition_started_at: DateTime<Utc>,
    pub acquisition_completed_at: DateTime<Utc>,
    pub last_modified: Option<String>,
    pub etag: Option<String>,
    pub byte_integrity: ByteIntegrity,
    /// Optional file-level annotations. They are checked against the proof
    /// when present, never used to infer a proof when absent.
    pub source_revision: Option<String>,
    pub source_identity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityWindow {
    pub valid_from: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
}

/// An independently retained statement that the exact file set is
/// composable. A URL, download date, ETag, or common hash does not constitute
/// this proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityEvidence {
    pub evidence_ref: String,
    pub rule_version: String,
    pub source_revision: String,
    pub source_identity: String,
    pub file_hashes: BTreeMap<FileRole, String>,
    pub valid_window: CompatibilityWindow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsistencyInput {
    pub files: Vec<FileFact>,
    pub compatibility: Option<CompatibilityEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConsistencyStatus {
    VerifiedCompatible,
    NotVerified,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "code")]
pub enum ConsistencyReason {
    MissingRole { role: FileRole },
    ConflictingDuplicate { role: FileRole },
    MissingCompatibilityEvidence,
    FileIntegrityNotVerified { role: FileRole },
    InvalidFileHash { role: FileRole },
    InvalidFileInterval { role: FileRole },
    IncompleteCompatibilityEvidence,
    InvalidCompatibilityWindow,
    HashMismatch { role: FileRole },
    SourceRevisionMismatch { role: FileRole },
    SourceIdentityMismatch { role: FileRole },
    AcquisitionOutsideCompatibilityWindow { role: FileRole },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConsistencyDecision {
    pub contract_version: &'static str,
    pub status: ConsistencyStatus,
    pub reasons: Vec<ConsistencyReason>,
    pub evidence_refs: Vec<String>,
    /// Sorted, de-duplicated facts are returned so callers retain the exact
    /// role/hash/source evidence used by this decision.
    pub files: Vec<FileFact>,
}

const REQUIRED_ROLES: [FileRole; 3] = [FileRole::BaseIndustry, FileRole::Day, FileRole::Gbbq];

pub fn validate(input: ConsistencyInput) -> ConsistencyDecision {
    let mut files = input.files;
    files.sort();
    files.dedup();

    let mut reasons = Vec::new();
    let mut by_role: BTreeMap<FileRole, Vec<&FileFact>> = BTreeMap::new();
    for file in &files {
        by_role.entry(file.role).or_default().push(file);
    }

    for role in REQUIRED_ROLES {
        match by_role.get(&role) {
            None => reasons.push(ConsistencyReason::MissingRole { role }),
            Some(facts) if facts.len() > 1 => {
                reasons.push(ConsistencyReason::ConflictingDuplicate { role })
            }
            Some(_) => {}
        }
    }

    for file in &files {
        if file.byte_integrity != ByteIntegrity::Verified {
            reasons.push(ConsistencyReason::FileIntegrityNotVerified { role: file.role });
        }
        if file.acquisition_started_at > file.acquisition_completed_at {
            reasons.push(ConsistencyReason::InvalidFileInterval { role: file.role });
        }
        if !canonical_sha256(&file.sha256) {
            reasons.push(ConsistencyReason::InvalidFileHash { role: file.role });
        }
    }

    let Some(evidence) = input.compatibility else {
        reasons.push(ConsistencyReason::MissingCompatibilityEvidence);
        return decision(ConsistencyStatus::NotVerified, reasons, files, Vec::new());
    };

    let evidence_ref = evidence.evidence_ref.clone();
    let mut evidence_refs = vec![evidence_ref];
    if evidence.evidence_ref.trim().is_empty()
        || evidence.rule_version != CONTRACT_VERSION
        || evidence.source_revision.trim().is_empty()
        || evidence.source_identity.trim().is_empty()
        || evidence.file_hashes.len() != REQUIRED_ROLES.len()
        || REQUIRED_ROLES
            .iter()
            .any(|role| !evidence.file_hashes.contains_key(role))
        || evidence
            .file_hashes
            .values()
            .any(|hash| !canonical_sha256(hash))
    {
        reasons.push(ConsistencyReason::IncompleteCompatibilityEvidence);
        return decision(
            ConsistencyStatus::NotVerified,
            reasons,
            files,
            std::mem::take(&mut evidence_refs),
        );
    }
    if evidence.valid_window.valid_from > evidence.valid_window.valid_until {
        reasons.push(ConsistencyReason::InvalidCompatibilityWindow);
        return decision(
            ConsistencyStatus::Conflict,
            reasons,
            files,
            std::mem::take(&mut evidence_refs),
        );
    }

    for role in REQUIRED_ROLES {
        let Some(facts) = by_role.get(&role) else {
            continue;
        };
        if facts.len() != 1 {
            continue;
        }
        let file = facts[0];
        if evidence.file_hashes.get(&role) != Some(&file.sha256) {
            reasons.push(ConsistencyReason::HashMismatch { role });
        }
        if let Some(revision) = &file.source_revision
            && revision != &evidence.source_revision
        {
            reasons.push(ConsistencyReason::SourceRevisionMismatch { role });
        }
        if let Some(identity) = &file.source_identity
            && identity != &evidence.source_identity
        {
            reasons.push(ConsistencyReason::SourceIdentityMismatch { role });
        }
        if file.acquisition_started_at < evidence.valid_window.valid_from
            || file.acquisition_completed_at > evidence.valid_window.valid_until
        {
            reasons.push(ConsistencyReason::AcquisitionOutsideCompatibilityWindow { role });
        }
    }

    let status = if reasons.iter().any(ConsistencyReason::is_conflict) {
        ConsistencyStatus::Conflict
    } else if reasons.is_empty() {
        ConsistencyStatus::VerifiedCompatible
    } else {
        ConsistencyStatus::NotVerified
    };
    evidence_refs.sort();
    decision(status, reasons, files, evidence_refs)
}

fn decision(
    status: ConsistencyStatus,
    mut reasons: Vec<ConsistencyReason>,
    files: Vec<FileFact>,
    mut evidence_refs: Vec<String>,
) -> ConsistencyDecision {
    reasons.sort_by_key(|reason| serde_json::to_string(reason).unwrap_or_default());
    evidence_refs.sort();
    ConsistencyDecision {
        contract_version: CONTRACT_VERSION,
        status,
        reasons,
        evidence_refs,
        files,
    }
}

fn canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl ConsistencyReason {
    fn is_conflict(&self) -> bool {
        matches!(
            self,
            Self::ConflictingDuplicate { .. }
                | Self::InvalidFileInterval { .. }
                | Self::InvalidCompatibilityWindow
                | Self::HashMismatch { .. }
                | Self::SourceRevisionMismatch { .. }
                | Self::SourceIdentityMismatch { .. }
                | Self::AcquisitionOutsideCompatibilityWindow { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn instant(hour: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-09-10T{hour:02}:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    fn hash(byte: u8) -> String {
        std::iter::repeat_n(byte as char, 64).collect()
    }

    fn file(role: FileRole, byte: u8) -> FileFact {
        FileFact {
            role,
            bytes: 10,
            sha256: hash(byte),
            source_url: "https://tdx.example.invalid/file".into(),
            acquisition_started_at: instant(1),
            acquisition_completed_at: instant(2),
            last_modified: Some("revision-1".into()),
            etag: Some("etag-1".into()),
            byte_integrity: ByteIntegrity::Verified,
            source_revision: Some("r1".into()),
            source_identity: Some("tdx-public-set".into()),
        }
    }

    fn evidence(files: &[FileFact]) -> CompatibilityEvidence {
        CompatibilityEvidence {
            evidence_ref: "fixture:compatible-set".into(),
            rule_version: CONTRACT_VERSION.into(),
            source_revision: "r1".into(),
            source_identity: "tdx-public-set".into(),
            file_hashes: files
                .iter()
                .map(|file| (file.role, file.sha256.clone()))
                .collect(),
            valid_window: CompatibilityWindow {
                valid_from: instant(0),
                valid_until: instant(3),
            },
        }
    }

    #[test]
    fn explicit_hash_revision_identity_and_window_are_required_for_compatibility() {
        let files = vec![
            file(FileRole::Day, b'd'),
            file(FileRole::BaseIndustry, b'b'),
            file(FileRole::Gbbq, b'g'),
        ];
        let decision = validate(ConsistencyInput {
            compatibility: Some(evidence(&files)),
            files,
        });
        assert_eq!(decision.status, ConsistencyStatus::VerifiedCompatible);
        assert!(decision.reasons.is_empty());
        assert_eq!(decision.evidence_refs, ["fixture:compatible-set"]);
    }

    #[test]
    fn matching_metadata_without_explicit_proof_remains_not_verified() {
        let files = vec![
            file(FileRole::BaseIndustry, b'b'),
            file(FileRole::Day, b'd'),
            file(FileRole::Gbbq, b'g'),
        ];
        let decision = validate(ConsistencyInput {
            files,
            compatibility: None,
        });
        assert_eq!(decision.status, ConsistencyStatus::NotVerified);
        assert!(
            decision
                .reasons
                .contains(&ConsistencyReason::MissingCompatibilityEvidence)
        );
    }

    #[test]
    fn missing_role_is_not_verified_and_does_not_infer_latest() {
        let files = vec![
            file(FileRole::BaseIndustry, b'b'),
            file(FileRole::Day, b'd'),
        ];
        let decision = validate(ConsistencyInput {
            files,
            compatibility: None,
        });
        assert_eq!(decision.status, ConsistencyStatus::NotVerified);
        assert!(decision.reasons.contains(&ConsistencyReason::MissingRole {
            role: FileRole::Gbbq
        }));
    }

    #[test]
    fn proof_hash_revision_identity_or_window_conflict_is_rejected() {
        let files = vec![
            file(FileRole::BaseIndustry, b'b'),
            file(FileRole::Day, b'd'),
            file(FileRole::Gbbq, b'g'),
        ];
        let mut proof = evidence(&files);
        proof.file_hashes.insert(FileRole::Day, hash(b'x'));
        assert_eq!(
            validate(ConsistencyInput {
                files: files.clone(),
                compatibility: Some(proof),
            })
            .status,
            ConsistencyStatus::Conflict
        );

        let mut proof = evidence(&files);
        proof.source_revision = "r2".into();
        assert_eq!(
            validate(ConsistencyInput {
                files: files.clone(),
                compatibility: Some(proof),
            })
            .status,
            ConsistencyStatus::Conflict
        );

        let mut proof = evidence(&files);
        proof.valid_window.valid_from = instant(2);
        assert_eq!(
            validate(ConsistencyInput {
                files,
                compatibility: Some(proof),
            })
            .status,
            ConsistencyStatus::Conflict
        );
    }

    #[test]
    fn order_and_exact_duplicates_do_not_change_decision_or_evidence() {
        let files = vec![
            file(FileRole::BaseIndustry, b'b'),
            file(FileRole::Day, b'd'),
            file(FileRole::Gbbq, b'g'),
        ];
        let proof = evidence(&files);
        let mut reordered = files.clone();
        reordered.reverse();
        reordered.push(files[1].clone());
        let expected = validate(ConsistencyInput {
            files,
            compatibility: Some(proof.clone()),
        });
        let actual = validate(ConsistencyInput {
            files: reordered,
            compatibility: Some(proof),
        });
        assert_eq!(actual, expected);
    }

    #[test]
    fn divergent_duplicate_role_is_conflict() {
        let mut day = file(FileRole::Day, b'd');
        day.sha256 = hash(b'x');
        let files = vec![
            file(FileRole::BaseIndustry, b'b'),
            file(FileRole::Day, b'd'),
            day,
            file(FileRole::Gbbq, b'g'),
        ];
        let proof = evidence(&files[..3]);
        let decision = validate(ConsistencyInput {
            files,
            compatibility: Some(proof),
        });
        assert_eq!(decision.status, ConsistencyStatus::Conflict);
        assert!(
            decision
                .reasons
                .contains(&ConsistencyReason::ConflictingDuplicate {
                    role: FileRole::Day
                })
        );
    }

    #[test]
    fn all_required_roles_are_explicit() {
        let roles: BTreeSet<_> = REQUIRED_ROLES.into_iter().collect();
        assert_eq!(roles.len(), 3);
    }
}
