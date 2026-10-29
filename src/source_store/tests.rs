//! Write-order fault-injection tests for §9.4.1.
//!
//! These run inside the crate because the fault points are private: production
//! callers cannot inject them. The property under test is the forced order's
//! only acceptable intermediate states: either nothing is registered, an
//! orphan blob exists without a manifest line, or the line exists together
//! with a blob that reads back to its digest. A manifest line must never point
//! at a missing or mismatching blob.

use std::fs;

use super::*;

const BODY: &[u8] = b"0|000001|T1001|||X500102\r\n";

fn facts(origin: &str) -> Registration {
    let mut facts = Registration::new(
        TDXHY_SOURCE_ID,
        StoreKind::ArchiveImport,
        origin,
        "2026-10-28T00:00:00Z",
    );
    facts.evidence_grade = Some("A".into());
    facts.parser_version = Some(TDXHY_PARSER_VERSION.into());
    facts
}

fn sha() -> String {
    crate::hash::sha256(BODY)
}

#[test]
fn fsync_before_failures_leave_no_manifest_line() {
    for point in [
        FaultPoint::FileFsync,
        FaultPoint::Publish,
        FaultPoint::DirFsync,
        FaultPoint::ManifestAppend,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = SourceStore::new(dir.path().join("store"));
        let error = store
            .register_inner(BODY, &facts("import:test"), Some(point))
            .expect_err("injected fault must fail the registration");
        assert_eq!(error.code(), "source_store_io", "{point:?}");
        assert!(
            store.records().unwrap().is_empty(),
            "{point:?} must not leave a manifest line"
        );
        // An orphan blob is an allowed intermediate state before the manifest
        // append; after it, a line requires a readable blob.
        if store.blob_exists(&sha()) {
            assert_eq!(store.read_verified(&sha()).unwrap(), BODY);
        }
    }
}

#[test]
fn manifest_fsync_failure_keeps_line_and_verified_blob_only() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    store
        .register_inner(BODY, &facts("import:test"), Some(FaultPoint::ManifestFsync))
        .expect_err("injected fault must fail the registration");
    let records = store.records().unwrap();
    assert_eq!(
        records.len(),
        1,
        "the line was appended before the failed fsync"
    );
    assert_eq!(records[0].sha256, sha());
    assert_eq!(store.read_verified(&sha()).unwrap(), BODY);
}

#[test]
fn registration_is_idempotent_per_origin_and_adds_origin_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let first = store
        .register(BODY, &facts("import:batch-a:base.zip"))
        .unwrap();
    assert!(first.written_blob && first.appended_line);
    let again = store
        .register(BODY, &facts("import:batch-a:base.zip"))
        .unwrap();
    assert!(!again.written_blob && !again.appended_line);
    assert_eq!(store.records().unwrap().len(), 1);

    // Same bytes, same logical source, different origin: one blob, one extra
    // evidence line (§9.4.4).
    let other = store
        .register(BODY, &facts("generation:manifest-sha"))
        .unwrap();
    assert!(!other.written_blob && other.appended_line);
    let records = store.records().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].sha256, records[1].sha256);
    assert_eq!(records[0].size_bytes, BODY.len() as u64);
    assert_eq!(records[1].origin, "generation:manifest-sha");
}

#[test]
fn read_back_mismatch_is_source_store_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    store.register(BODY, &facts("import:test")).unwrap();
    // Corrupt the blob behind the store's back.
    fs::write(store.blob_path(&sha()), b"tampered").unwrap();
    let error = store.read_verified(&sha()).unwrap_err();
    assert_eq!(error.code(), "source_store_corrupt");
    match error {
        StoreError::Corrupt {
            sha256, expected, ..
        } => {
            assert_eq!(sha256, sha());
            assert_eq!(expected, sha());
        }
        other => panic!("expected corrupt, got {other:?}"),
    }
}

#[test]
fn missing_blob_and_bad_names_are_distinct_codes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let missing = store.read_verified(&sha()).unwrap_err();
    assert_eq!(missing.code(), "source_store_missing_blob");
    let not_hex = "g".repeat(64);
    for bad in ["../blobs/x", "ABC", not_hex.as_str()] {
        let error = store.read_verified(bad).unwrap_err();
        assert_eq!(error.code(), "source_store_invalid_sha256");
    }
}

#[test]
fn manifest_parse_error_names_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    fs::create_dir_all(store.root()).unwrap();
    fs::write(store.manifest_path(), b"{\"sha256\":\"nope\"}\n").unwrap();
    let error = store.records().unwrap_err();
    assert_eq!(error.code(), "source_store_manifest_invalid");
    assert!(error.to_string().contains("manifest.jsonl:1"), "{error}");
}
