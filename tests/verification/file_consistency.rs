use x_data_mcp::file_consistency::{
    ByteIntegrity, ConsistencyInput, ConsistencyReason, ConsistencyStatus, FileFact, validate,
};
use x_data_mcp::snapshot::SnapshotManifest;

#[test]
fn retained_issue_74_snapshot_has_byte_evidence_but_no_cross_file_approval() {
    let manifest: SnapshotManifest = serde_json::from_str(include_str!(
        "../../evidence/issue-74-snapshot-input/retained-manifest.json"
    ))
    .unwrap();
    let files = manifest
        .files
        .into_iter()
        .map(|file| FileFact {
            role: file.role,
            bytes: file.bytes,
            sha256: file.sha256,
            source_url: file.source_url,
            acquisition_started_at: file.acquisition_started_at,
            acquisition_completed_at: file.acquisition_completed_at,
            last_modified: file.last_modified,
            etag: file.etag,
            byte_integrity: ByteIntegrity::Verified,
            source_revision: None,
            source_identity: None,
        })
        .collect();
    let decision = validate(ConsistencyInput {
        files,
        compatibility: None,
    });
    assert_eq!(decision.status, ConsistencyStatus::NotVerified);
    assert_eq!(decision.evidence_refs, Vec::<String>::new());
    assert!(
        decision
            .reasons
            .contains(&ConsistencyReason::MissingCompatibilityEvidence)
    );
}
