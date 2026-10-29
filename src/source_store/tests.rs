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

#[test]
fn concurrent_registers_do_not_interleave_lines_or_duplicate_keys() {
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let threads = 8;
    let barrier = Arc::new(Barrier::new(threads));
    let mut handles = Vec::new();
    for _ in 0..threads {
        let root = root.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let store = SourceStore::new(root);
            barrier.wait();
            store.register(BODY, &facts("import:same")).unwrap();
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let store = SourceStore::new(&root);
    let records = store.records().unwrap();
    assert_eq!(records.len(), 1, "one key must register exactly one line");
    assert_eq!(records[0].sha256, sha());

    // Distinct content from parallel writers: every line is whole and unique.
    let mut handles = Vec::new();
    for index in 0..threads {
        let root = root.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let store = SourceStore::new(root);
            let body = format!("0|{index:06}|T1001|||X500102\r\n");
            let registration = facts(&format!("import:distinct:{index}"));
            barrier.wait();
            store.register(body.as_bytes(), &registration).unwrap();
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let records = store.records().unwrap();
    assert_eq!(records.len(), threads + 1);
    let shas: std::collections::BTreeSet<&str> = records
        .iter()
        .map(|record| record.sha256.as_str())
        .collect();
    assert_eq!(shas.len(), records.len(), "no duplicate content keys");
    for record in &records {
        // Digest round-trip plus the recorded size agree for every line.
        let bytes = store.read_verified(&record.sha256).unwrap();
        assert_eq!(bytes.len() as u64, record.size_bytes);
    }
}

#[test]
fn torn_tail_is_named_and_never_auto_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    store.register(BODY, &facts("import:one")).unwrap();
    let path = store.manifest_path();
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("{\"sha256\":\"dead");
    fs::write(&path, &text).unwrap();

    let error = store.records().unwrap_err();
    assert_eq!(error.code(), "manifest_torn_tail");
    assert!(error.to_string().contains("line 2"), "{error}");

    // The write path refuses on the same error and leaves the bytes alone.
    let before = fs::read(&path).unwrap();
    let error = store
        .register(b"0|999999|T1001|||X500102\r\n", &facts("import:two"))
        .unwrap_err();
    assert_eq!(error.code(), "manifest_torn_tail");
    assert_eq!(fs::read(&path).unwrap(), before, "no truncation, no repair");
}

#[test]
fn a_final_line_without_newline_is_torn_even_when_it_parses() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    fs::create_dir_all(store.root()).unwrap();
    let record = ManifestRecord {
        sha256: sha(),
        source_id: TDXHY_SOURCE_ID.to_string(),
        store_kind: StoreKind::ArchiveImport,
        origin: "import:manual".to_string(),
        acquired_at: "2026-10-28T00:00:00Z".to_string(),
        publisher_last_modified: None,
        evidence_grade: Some("A".to_string()),
        basis_status: None,
        parser_version: Some(TDXHY_PARSER_VERSION.to_string()),
        size_bytes: BODY.len() as u64,
        container_sha256: None,
        import_batch: None,
    };
    let line = serde_json::to_string(&record).unwrap();
    fs::write(store.manifest_path(), line).unwrap();
    let error = store.records().unwrap_err();
    assert_eq!(error.code(), "manifest_torn_tail");
    assert!(error.to_string().contains("line 1"), "{error}");
}
