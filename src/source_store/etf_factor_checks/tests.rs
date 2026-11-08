//! `#607` PR-B: `etf-factor-checks-v1` record shape and the
//! `source_store import --format etf-factor-checks` path.

use std::path::Path;

use super::*;
use crate::source_store::import::{ImportFormat, ImportOptions};
use crate::source_store::{SourceStore, replay};

const EVIDENCE_DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const DOC_SHA: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn record(entity: &str, ordinal: i64, status: CheckStatus, digest: &str) -> EtfFactorCheck {
    EtfFactorCheck {
        record_version: CHECKS_VERSION.into(),
        entity: entity.into(),
        ordinal,
        effective_day: "2024-12-02".into(),
        category: 11,
        check_status: status,
        basis: CheckBasis::Announcement,
        basis_ref: "announcement:2024-11-29".into(),
        evidence_url: "https://example.invalid/a.pdf".into(),
        evidence_sha256: DOC_SHA.into(),
        announced_value: "2.5".into(),
        gbbq_value: "2.5".into(),
        price_delta: "0.0004".into(),
        within_one_tick: Some(true),
        checked_at: "2026-11-09".into(),
        evidence_digest: digest.into(),
    }
}

const HEADER: &str = "entity,ordinal,effective_day,category,check_status,basis,basis_ref,\
evidence_url,evidence_sha256,announced_value,gbbq_value,price_delta,within_one_tick,\
checked_at,evidence_digest";

fn csv_line(entity: &str, ordinal: i64, status: &str, digest: &str) -> String {
    format!(
        "{entity},{ordinal},2024-12-02,11,{status},announcement,announcement:2024-11-29,\
         https://example.invalid/a.pdf,{DOC_SHA},2.5,2.5,0.0004,true,2026-11-09,{digest}"
    )
}

fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn options(format: ImportFormat, dry_run: bool) -> ImportOptions {
    ImportOptions {
        dry_run,
        format,
        ..ImportOptions::default()
    }
}

#[test]
fn a_record_round_trips_and_its_sha_is_its_canonical_content() {
    let record = record(
        "sz.159922",
        1,
        CheckStatus::VerifiedAnnouncement,
        EVIDENCE_DIGEST,
    );
    let canonical = record.canonical_json().unwrap();
    let digest = record.digest().unwrap();
    assert_eq!(digest, crate::hash::sha256(canonical.as_bytes()));
    let parsed = EtfFactorCheck::parse_line(&canonical).unwrap();
    assert_eq!(parsed, record);
    assert_eq!(parsed.digest().unwrap(), digest);
    // Every field the design asks for is in the record.
    for key in [
        "check_status",
        "basis",
        "evidence_url",
        "evidence_sha256",
        "announced_value",
        "gbbq_value",
        "price_delta",
        "within_one_tick",
        "checked_at",
        "evidence_digest",
    ] {
        assert!(canonical.contains(key), "{key} missing from {canonical}");
    }
}

#[test]
fn validation_rejects_a_status_basis_mismatch_and_a_bad_sha() {
    let mut official = record(
        "sz.159922",
        1,
        CheckStatus::VerifiedOfficial,
        EVIDENCE_DIGEST,
    );
    // `verified_official` may only cite the official qss.
    official.basis = CheckBasis::Announcement;
    assert!(official.validate().is_err());
    official.basis = CheckBasis::OfficialQss;
    assert!(official.validate().is_ok());

    let mut bad_sha = record(
        "sz.159922",
        1,
        CheckStatus::VerifiedAnnouncement,
        EVIDENCE_DIGEST,
    );
    bad_sha.evidence_sha256 = "not-a-sha".into();
    assert!(bad_sha.validate().is_err());

    let mut bad_digest = record("sz.159922", 1, CheckStatus::VerifiedAnnouncement, "abc");
    bad_digest.evidence_digest = "abc".into();
    assert!(bad_digest.validate().is_err());

    let mut verified_without_basis = record(
        "sz.159922",
        1,
        CheckStatus::VerifiedAnnouncement,
        EVIDENCE_DIGEST,
    );
    verified_without_basis.basis = CheckBasis::None;
    verified_without_basis.evidence_sha256.clear();
    assert!(verified_without_basis.validate().is_err());

    let mut odd_category = record(
        "sz.159922",
        1,
        CheckStatus::VerifiedAnnouncement,
        EVIDENCE_DIGEST,
    );
    odd_category.category = 2;
    assert!(odd_category.validate().is_err());
}

#[test]
fn importing_the_same_file_twice_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let path = write(
        dir.path(),
        "checks.csv",
        &format!(
            "{HEADER}\n{}\n{}\n",
            csv_line("sz.159922", 1, "verified_announcement", EVIDENCE_DIGEST),
            csv_line("sh.510180", 3, "verified_announcement", EVIDENCE_DIGEST),
        ),
    );
    let first = import(
        &store,
        &path,
        &options(ImportFormat::EtfFactorChecksCsv, false),
    )
    .unwrap();
    assert_eq!(first.status, "ok");
    assert_eq!(first.imported.len(), 2);
    assert_eq!(first.manifest_lines_added, 2);
    assert!(first.skipped_unverified.is_empty());
    let lines_after_first = store.records().unwrap().len();

    let second = import(
        &store,
        &path,
        &options(ImportFormat::EtfFactorChecksCsv, false),
    )
    .unwrap();
    assert_eq!(second.imported.len(), 0, "nothing may be written twice");
    assert_eq!(second.already_present.len(), 2);
    assert_eq!(second.manifest_lines_added, 0);
    assert_eq!(store.records().unwrap().len(), lines_after_first);

    let set = read(&store).unwrap();
    assert_eq!(set.records.len(), 2);
    assert_eq!(
        set.records
            .iter()
            .map(|record| record.entity.as_str())
            .collect::<Vec<_>>(),
        vec!["sh.510180", "sz.159922"],
        "records come back in a deterministic order"
    );
}

#[test]
fn dry_run_writes_nothing_and_the_manifest_sha_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let path = write(
        dir.path(),
        "checks.csv",
        &format!(
            "{HEADER}\n{}\n",
            csv_line("sz.159922", 1, "verified_announcement", EVIDENCE_DIGEST)
        ),
    );
    // A store with one prior unrelated record so "unchanged" is meaningful.
    let mut facts = crate::source_store::Registration::new(
        crate::source_store::TDXHY_SOURCE_ID,
        crate::source_store::StoreKind::ArchiveImport,
        "import:test",
        "2026-11-09T00:00:00Z",
    );
    facts.parser_version = Some(crate::source_store::TDXHY_PARSER_VERSION.into());
    store
        .register(b"0|000001|T1001|||X500102\r\n", &facts)
        .unwrap();
    let before = crate::hash::sha256(&std::fs::read(store.manifest_path()).unwrap());
    let blobs_before = std::fs::read_dir(store.blobs_dir()).unwrap().count();

    let report = import(
        &store,
        &path,
        &options(ImportFormat::EtfFactorChecksCsv, true),
    )
    .unwrap();
    assert!(report.dry_run);
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.manifest_lines_added, 1);
    let after = crate::hash::sha256(&std::fs::read(store.manifest_path()).unwrap());
    assert_eq!(before, after, "a dry run must not touch the manifest");
    assert_eq!(
        std::fs::read_dir(store.blobs_dir()).unwrap().count(),
        blobs_before,
        "a dry run must not write a blob"
    );
    assert_eq!(store.records().unwrap().len(), 1);
}

#[test]
fn bad_rows_are_rejected_and_counted_without_stopping_the_import() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let path = write(
        dir.path(),
        "checks.csv",
        &format!(
            "{HEADER}\n{}\n{}\n{}\n{}\n",
            csv_line("sz.159922", 1, "verified_announcement", EVIDENCE_DIGEST),
            // Unknown status.
            csv_line("sz.159922", 2, "probably_fine", EVIDENCE_DIGEST),
            // sha256 that is not a sha256.
            "sh.510180,1,2013-04-17,11,verified_announcement,announcement,a,bad-sha,\
             0.01,0.01,,true,2026-11-09,1111111111111111111111111111111111111111111111111111\
             111111111111",
            // A verified record with no basis is not evidence.
            "sh.510180,2,2013-04-17,11,verified_announcement,none,,,,,,,,2026-11-09,\
             1111111111111111111111111111111111111111111111111111111111111111",
        ),
    );
    let report = import(
        &store,
        &path,
        &options(ImportFormat::EtfFactorChecksCsv, false),
    )
    .unwrap();
    assert_eq!(report.imported.len(), 1, "the good row still lands");
    assert_eq!(report.skipped_unverified.len(), 3);
    assert_eq!(report.index_lines, 4);
    let reasons: Vec<&str> = report
        .skipped_unverified
        .iter()
        .map(|row| row.reason)
        .collect();
    assert!(reasons.iter().all(|reason| *reason == "invalid_record"));
    assert_eq!(store.records().unwrap().len(), 1);
}

#[test]
fn jsonl_import_matches_the_csv_import() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let record = record(
        "sz.159922",
        1,
        CheckStatus::VerifiedAnnouncement,
        EVIDENCE_DIGEST,
    );
    let path = write(
        dir.path(),
        "checks.jsonl",
        &format!("{}\n", record.canonical_json().unwrap()),
    );
    let report = import(
        &store,
        &path,
        &options(ImportFormat::EtfFactorChecksJsonl, false),
    )
    .unwrap();
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.imported[0].sha256, record.digest().unwrap());
    let set = read(&store).unwrap();
    assert_eq!(set.records, vec![record]);
}

#[test]
fn an_unmodified_record_registers_once_per_record_not_per_batch() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let record = record(
        "sz.159922",
        1,
        CheckStatus::VerifiedAnnouncement,
        EVIDENCE_DIGEST,
    );
    let body = format!("{}\n", record.canonical_json().unwrap());
    let a = write(dir.path(), "a.jsonl", &body);
    let b = write(dir.path(), "b.jsonl", &body);
    let first = import(
        &store,
        &a,
        &options(ImportFormat::EtfFactorChecksJsonl, false),
    )
    .unwrap();
    assert_eq!(first.imported.len(), 1);
    // Same content from a different file is still the same registration: the
    // origin is the record's own sha, not the batch name.
    let second = import(
        &store,
        &b,
        &options(ImportFormat::EtfFactorChecksJsonl, false),
    )
    .unwrap();
    assert_eq!(second.already_present.len(), 1);
    assert_eq!(store.records().unwrap().len(), 1);
}

#[test]
fn a_later_verdict_is_a_new_record_and_the_old_one_stays() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let mut first = record("sz.159922", 1, CheckStatus::Unverified, EVIDENCE_DIGEST);
    first.checked_at = "2026-11-09".into();
    let mut second = record(
        "sz.159922",
        1,
        CheckStatus::VerifiedOfficial,
        EVIDENCE_DIGEST,
    );
    second.basis = CheckBasis::OfficialQss;
    second.checked_at = "2026-11-10".into();
    let path = write(
        dir.path(),
        "checks.jsonl",
        &format!(
            "{}\n{}\n",
            first.canonical_json().unwrap(),
            second.canonical_json().unwrap()
        ),
    );
    import(
        &store,
        &path,
        &options(ImportFormat::EtfFactorChecksJsonl, false),
    )
    .unwrap();
    let set = read(&store).unwrap();
    assert_eq!(set.records.len(), 2, "append-only: the old record is kept");
    let latest = set.latest_by_key();
    let chosen = latest
        .get(&("sz.159922".into(), 1, EVIDENCE_DIGEST.into()))
        .unwrap();
    assert_eq!(chosen.check_status, CheckStatus::VerifiedOfficial);
}

#[test]
fn the_scope_selects_only_check_records_and_every_other_scope_ignores_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let record = record(
        "sz.159922",
        1,
        CheckStatus::VerifiedAnnouncement,
        EVIDENCE_DIGEST,
    );
    let path = write(
        dir.path(),
        "checks.jsonl",
        &format!("{}\n", record.canonical_json().unwrap()),
    );
    import(
        &store,
        &path,
        &options(ImportFormat::EtfFactorChecksJsonl, false),
    )
    .unwrap();
    let records = store.records().unwrap();
    let selected: Vec<&str> = records
        .iter()
        .filter(|row| replay::Scope::EtfFactorChecks.selects(row))
        .map(|row| row.source_id.as_str())
        .collect();
    assert_eq!(selected, vec![CHECKS_SOURCE_ID]);
    for scope in replay::Scope::ALL {
        assert!(
            records.iter().all(|row| !scope.selects(row)),
            "{scope:?} must not claim a check record"
        );
    }
    assert!(
        !replay::Scope::ALL.contains(&replay::Scope::EtfFactorChecks),
        "the check scope is read-only evidence and stays out of `all`"
    );
}

#[test]
fn replay_validates_every_record_and_reports_the_checks_digest() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let a = record(
        "sz.159922",
        1,
        CheckStatus::VerifiedAnnouncement,
        EVIDENCE_DIGEST,
    );
    let b = record(
        "sh.510180",
        3,
        CheckStatus::VerifiedAnnouncement,
        EVIDENCE_DIGEST,
    );
    let path = write(
        dir.path(),
        "checks.jsonl",
        &format!(
            "{}\n{}\n",
            a.canonical_json().unwrap(),
            b.canonical_json().unwrap()
        ),
    );
    import(
        &store,
        &path,
        &options(ImportFormat::EtfFactorChecksJsonl, false),
    )
    .unwrap();
    let request = replay::ReplayRequest::new(Some(replay::Scope::EtfFactorChecks), true, None);
    let report = replay::replay(&store, &request).unwrap();
    assert_eq!(report.status, "ok", "{report:?}");
    let scope = report
        .scopes
        .iter()
        .find(|row| row.scope == "etf_factor_checks")
        .unwrap();
    assert_eq!(scope.status, "ok");
    assert_eq!(scope.entries, 2);
    assert_eq!(scope.snapshots, Some(2));
    assert_eq!(
        scope.etf_checks_digest.as_deref(),
        Some(read(&store).unwrap().digest().as_str())
    );
    assert_eq!(scope.database, Some("not_compared"));
    assert!(scope.summary.is_some());
}

#[test]
fn replay_fails_closed_on_a_record_whose_bytes_are_not_its_content() {
    let dir = tempfile::tempdir().unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    // A blob registered under the checks source id but with the wrong shape.
    let mut facts = crate::source_store::Registration::new(
        CHECKS_SOURCE_ID,
        crate::source_store::StoreKind::ArchiveImport,
        "etf-factor-checks:synthetic",
        "2026-11-09T00:00:00Z",
    );
    facts.parser_version = Some(CHECKS_VERSION.into());
    store
        .register(b"{\"entity\":\"sz.159922\"}\n", &facts)
        .unwrap();
    let request = replay::ReplayRequest::new(Some(replay::Scope::EtfFactorChecks), true, None);
    let report = replay::replay(&store, &request).unwrap();
    assert_eq!(report.status, "record_invalid");
    assert_eq!(report.scopes[0].status, "record_invalid");
}
