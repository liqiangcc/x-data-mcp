//! Source-store behaviour on synthetic archives: verified-only import,
//! idempotency, deterministic replay, corrupt-read fail-closed and the empty
//! store. Everything is built in a tempdir; no network and no host path.

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use serde_json::json;
use x_data_mcp::hash::sha256;
use x_data_mcp::source_store::import as import_mod;
use x_data_mcp::source_store::replay as replay_mod;
use x_data_mcp::source_store::{INCON_SOURCE_ID, SourceStore, TDXHY_SOURCE_ID};

fn zip_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in members {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

const TDXHY: &[u8] = b"0|000001|T1001|||X500102\r\n1|600000|T090302|||X6102\r\n";
const INCON: &[u8] = b"#TDXRSHY\r\nT1001|Bank\r\n######\r\n";

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    index: PathBuf,
    blobs: PathBuf,
    tdxhy_sha: String,
    incon_sha: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let blobs = dir.path().join("archive-blobs");
    fs::create_dir_all(&blobs).unwrap();
    let base = zip_bytes(&[("tdxhy.cfg", TDXHY)]);
    fs::write(blobs.join("base-A.zip"), &base).unwrap();
    let zhb = zip_bytes(&[("incon.dat", INCON)]);
    let gbbq = zip_bytes(&[("zhb.zip", zhb.as_slice())]);
    fs::write(blobs.join("gbbq-A.zip"), &gbbq).unwrap();
    // A second container whose bytes are valid but whose index line claims a
    // manifest mismatch; it must never be imported.
    fs::write(blobs.join("base-B.zip"), &base).unwrap();

    let manifest_sha = "a".repeat(64);
    let good = json!({
        "batch": "fixture-batch",
        "kind": "industry",
        "tier": "A",
        "source_path": "base-A.zip",
        "sha256": sha256(&base),
        "tdxhy_sha256": sha256(TDXHY),
        "manifest_sha256": manifest_sha,
        "manifest_match": true,
        "last_modified": "2026-10-24T01:00:00Z",
        "etag": "\"etag-a\"",
        "acquisition_completed_at": "2026-10-24T02:00:00Z",
        "gbbq_source_path": "gbbq-A.zip",
        "gbbq_sha256": sha256(&gbbq),
        "gbbq_last_modified": "2026-10-24T01:00:00Z",
        "incon_sha256": sha256(INCON)
    });
    let mismatched = json!({
        "batch": "fixture-batch",
        "kind": "industry",
        "tier": "A",
        "source_path": "base-B.zip",
        "sha256": sha256(&base),
        "tdxhy_sha256": sha256(TDXHY),
        "manifest_sha256": manifest_sha,
        "manifest_match": false,
        "last_modified": "2026-10-25T01:00:00Z",
        "etag": "\"etag-b\"",
        "acquisition_completed_at": "2026-10-25T02:00:00Z"
    });
    let index = dir.path().join("archive-index.jsonl");
    let mut text = String::new();
    text.push_str(&serde_json::to_string(&good).unwrap());
    text.push('\n');
    text.push_str("# comment lines are ignored\n");
    text.push_str(&serde_json::to_string(&mismatched).unwrap());
    text.push('\n');
    fs::write(&index, text).unwrap();
    let root = dir.path().to_path_buf();
    Fixture {
        _dir: dir,
        root,
        index,
        blobs,
        tdxhy_sha: sha256(TDXHY),
        incon_sha: sha256(INCON),
    }
}

impl Fixture {
    fn store(&self) -> SourceStore {
        SourceStore::new(self.root.join("store"))
    }

    fn import(&self, options: &import_mod::ImportOptions) -> import_mod::ImportReport {
        import_mod::import(&self.store(), &self.index, &self.blobs, options).unwrap()
    }
}

#[test]
fn import_registers_verified_members_and_skips_manifest_mismatch() {
    let fixture = fixture();
    let report = fixture.import(&import_mod::ImportOptions::default());
    assert_eq!(report.status, "ok");
    assert_eq!(report.index_lines, 2);
    assert_eq!(report.imported.len(), 2);
    assert_eq!(report.blobs_added, 2);
    assert_eq!(report.manifest_lines_added, 2);
    assert_eq!(report.already_present.len(), 0);
    assert_eq!(report.skipped_unverified.len(), 1);
    assert_eq!(report.skipped_unverified[0].reason, "manifest_mismatch");
    assert_eq!(report.skipped_unverified[0].index_line, 3);
    assert_eq!(report.batches, vec!["fixture-batch".to_string()]);

    let records = fixture.store().records().unwrap();
    assert_eq!(records.len(), 2);
    let tdxhy = records
        .iter()
        .find(|record| record.source_id == TDXHY_SOURCE_ID)
        .expect("tdxhy.cfg extraction registered");
    assert_eq!(tdxhy.sha256, fixture.tdxhy_sha);
    assert_eq!(tdxhy.evidence_grade.as_deref(), Some("A"));
    assert_eq!(tdxhy.parser_version.as_deref(), Some("tdxhy-v1"));
    assert_eq!(
        tdxhy.publisher_last_modified.as_deref(),
        Some("2026-10-24T01:00:00Z")
    );
    assert_eq!(tdxhy.import_batch.as_deref(), Some("fixture-batch"));
    assert_eq!(tdxhy.acquired_at, "2026-10-24T02:00:00Z");
    assert!(tdxhy.container_sha256.is_some());
    assert_eq!(tdxhy.size_bytes, TDXHY.len() as u64);
    let incon = records
        .iter()
        .find(|record| record.source_id == INCON_SOURCE_ID)
        .expect("incon.dat extraction registered");
    assert_eq!(incon.sha256, fixture.incon_sha);
    assert_eq!(incon.parser_version.as_deref(), Some("incon-v1"));

    // Read back by digest, not by path.
    assert_eq!(
        fixture
            .store()
            .read_verified(&fixture.tdxhy_sha)
            .unwrap()
            .as_slice(),
        TDXHY
    );
    assert_eq!(
        fixture
            .store()
            .read_verified(&fixture.incon_sha)
            .unwrap()
            .as_slice(),
        INCON
    );
}

#[test]
fn repeated_import_is_idempotent() {
    let fixture = fixture();
    fixture.import(&import_mod::ImportOptions::default());
    let store = fixture.store();
    let first = store.records().unwrap().len();
    let second = fixture.import(&import_mod::ImportOptions::default());
    assert_eq!(second.imported.len(), 0);
    assert_eq!(second.already_present.len(), 2);
    assert_eq!(second.blobs_added, 0);
    assert_eq!(second.manifest_lines_added, 0);
    assert_eq!(store.records().unwrap().len(), first);
    let blobs = fs::read_dir(store.blobs_dir())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(blobs.len(), 2, "one blob per distinct content");
}

#[test]
fn dry_run_writes_nothing() {
    let fixture = fixture();
    let options = import_mod::ImportOptions {
        dry_run: true,
        ..import_mod::ImportOptions::default()
    };
    let report = fixture.import(&options);
    assert!(report.dry_run);
    assert_eq!(report.imported.len(), 2);
    assert_eq!(report.blobs_added, 2);
    let store = fixture.store();
    assert!(!store.root().exists(), "dry run must not create the store");
    assert!(store.records().unwrap().is_empty());
}

#[test]
fn replay_twice_has_the_same_digest_and_scope_selection() {
    let fixture = fixture();
    fixture.import(&import_mod::ImportOptions::default());
    let store = fixture.store();
    let industry = replay_mod::ReplayRequest::new(Some(replay_mod::Scope::Industry), false, None);
    let first = replay_mod::replay(&store, &industry).unwrap();
    let second = replay_mod::replay(&store, &industry).unwrap();
    assert_eq!(first.status, "ok");
    assert_eq!(first.digest, second.digest);
    assert_eq!(first.entries, 2);
    assert_eq!(first.scopes.len(), 1);
    assert_eq!(first.scopes[0].scope, "industry");
    // M3 PR-6: the industry body is implemented, so the scope now computes the
    // snapshot set and reports its deterministic summary (no `--db` here).
    assert_eq!(first.scopes[0].rebuilt, replay_mod::COMPUTED_INDUSTRY_NO_DB);
    assert!(first.scopes[0].summary.is_some());
    assert_eq!(first.scopes[0].summary, second.scopes[0].summary);
    assert!(first.problems.is_empty());
    // A scope with no matching records is empty, not an error.
    let exchange = replay_mod::ReplayRequest::new(Some(replay_mod::Scope::Exchange), false, None);
    let exchange = replay_mod::replay(&store, &exchange).unwrap();
    assert_eq!(exchange.status, "ok");
    assert_eq!(exchange.scopes[0].status, "store_empty");
}

#[test]
fn all_scope_replay_covers_every_scope_and_agrees_with_verify() {
    let fixture = fixture();
    fixture.import(&import_mod::ImportOptions::default());
    let store = fixture.store();
    let replay =
        replay_mod::replay(&store, &replay_mod::ReplayRequest::new(None, false, None)).unwrap();
    assert_eq!(replay.scope, "all");
    assert_eq!(replay.scopes.len(), 5);
    assert_eq!(replay.entries, 2);
    let verify =
        replay_mod::verify(&store, &replay_mod::ReplayRequest::new(None, false, None)).unwrap();
    assert_eq!(verify.mode, "verify");
    assert!(verify.verify_only);
    assert_eq!(verify.status, replay.status);
    assert_eq!(verify.digest, replay.digest);
}

#[test]
fn corrupt_blob_fails_replay_and_verify_with_source_store_corrupt() {
    let fixture = fixture();
    fixture.import(&import_mod::ImportOptions::default());
    let store = fixture.store();
    fs::write(store.blob_path(&fixture.tdxhy_sha), b"tampered").unwrap();
    let replay =
        replay_mod::replay(&store, &replay_mod::ReplayRequest::new(None, false, None)).unwrap();
    assert_eq!(replay.status, "source_store_corrupt");
    assert_eq!(replay.problems[0].reason, "source_store_corrupt");
    assert_eq!(replay.problems[0].sha256, fixture.tdxhy_sha);
    assert_ne!(replay.problems[0].expected, replay.problems[0].actual);
    let verify =
        replay_mod::verify(&store, &replay_mod::ReplayRequest::new(None, true, None)).unwrap();
    assert_eq!(verify.status, "source_store_corrupt");
}

#[test]
fn empty_store_is_store_empty_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let replay =
        replay_mod::replay(&store, &replay_mod::ReplayRequest::new(None, false, None)).unwrap();
    assert_eq!(replay.status, "store_empty");
    assert_eq!(replay.entries, 0);
    assert!(
        replay
            .scopes
            .iter()
            .all(|scope| scope.status == "store_empty")
    );
    let verify =
        replay_mod::verify(&store, &replay_mod::ReplayRequest::new(None, false, None)).unwrap();
    assert_eq!(verify.status, "store_empty");
}

#[test]
fn unknown_parser_version_is_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let mut facts = registration(TDXHY_SOURCE_ID, "origin:a");
    facts.parser_version = Some("tdxhy-v99".to_string());
    store.register(TDXHY, &facts).unwrap();
    let request = replay_mod::ReplayRequest::new(Some(replay_mod::Scope::Industry), false, None);
    let report = replay_mod::replay(&store, &request).unwrap();
    assert_eq!(report.status, "unknown_parser");
    assert_eq!(report.problems[0].reason, "unknown_parser");
}

fn registration(source_id: &str, origin: &str) -> x_data_mcp::source_store::Registration {
    let mut facts = x_data_mcp::source_store::Registration::new(
        source_id,
        x_data_mcp::source_store::StoreKind::ArchiveImport,
        origin,
        "2026-10-24T02:00:00Z",
    );
    facts.evidence_grade = Some("A".to_string());
    facts.parser_version = Some("tdxhy-v1".to_string());
    facts
}

#[test]
fn invalid_index_line_is_a_hard_error() {
    let fixture = fixture();
    let index: &Path = &fixture.index;
    let mut text = fs::read_to_string(index).unwrap();
    text.push_str("{not json}\n");
    fs::write(index, text).unwrap();
    let error = import_mod::import(
        &fixture.store(),
        index,
        &fixture.blobs,
        &import_mod::ImportOptions::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "source_store_manifest_invalid");
    assert!(error.to_string().contains(":4:"), "{error}");
}
