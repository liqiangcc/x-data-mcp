//! `#607` PR-B: the read-only loader and its `(entity, evidence_digest)` cache.

use std::os::unix::fs::PermissionsExt;

use chrono::NaiveDate;
use rusqlite::Connection;
use rust_decimal::Decimal;
use serde_json::json;

use super::*;
use crate::compact_market::{Digest, EvidenceKind, SourceBinding};
use crate::market_data::{AdjustmentEvent, AdjustmentEventSet, FactorBasisStatus, FactorPrecision};
use crate::source_store::etf_factor_checks::{CheckBasis, CheckStatus, EtfFactorCheck};
use crate::source_store::etf_factor_gate::{BlockReason, FactorFact};
use crate::sqlite_market::{derived, evidence_store};
use crate::sqlite_tasks::SqliteTasks;

const DOC_SHA: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn check(entity: &str, ordinal: i64, digest: &str, status: CheckStatus) -> EtfFactorCheck {
    EtfFactorCheck {
        record_version: crate::source_store::etf_factor_checks::CHECKS_VERSION.into(),
        entity: entity.into(),
        ordinal,
        effective_day: "2024-12-02".into(),
        category: 11,
        check_status: status,
        basis: CheckBasis::OfficialQss,
        basis_ref: "szse_1815_stock:2024-12-02".into(),
        evidence_url: "https://example.invalid/qss".into(),
        evidence_sha256: DOC_SHA.into(),
        announced_value: "2.5".into(),
        gbbq_value: "2.5".into(),
        price_delta: "0.0004".into(),
        within_one_tick: Some(true),
        checked_at: "2026-11-09".into(),
        evidence_digest: digest.into(),
    }
}

fn facts(entity: &str, digests: &[&str]) -> EntityFacts {
    EntityFacts {
        entity: entity.into(),
        first_bar_day: Some(day(2019, 1, 4)),
        facts: digests
            .iter()
            .enumerate()
            .map(|(index, digest)| FactorFact {
                ordinal: index as i64,
                effective_day: day(2024, 12, 2),
                category: 11,
                factors: [
                    Decimal::ZERO,
                    Decimal::ZERO,
                    "2.5".parse().unwrap(),
                    Decimal::ZERO,
                ],
                evidence_digest: (*digest).to_string(),
            })
            .collect(),
    }
}

fn loader(records: Vec<EtfFactorCheck>) -> Loader {
    let shas = records
        .iter()
        .map(|record| record.digest().unwrap())
        .collect();
    Loader::from_set(CheckSet { records, shas })
}

#[test]
fn a_changed_digest_is_a_cache_miss_and_a_new_verdict() {
    let digest_a = "a".repeat(64);
    let digest_b = "b".repeat(64);
    let loader = loader(vec![check(
        "sz.159922",
        0,
        &digest_a,
        CheckStatus::VerifiedOfficial,
    )]);
    let verified = facts("sz.159922", &[&digest_a]);
    assert!(loader.verdict(&verified).is_admitted());
    assert_eq!(loader.cache_stats(), (0, 1));
    // Same facts → cache hit, same verdict.
    assert!(loader.verdict(&verified).is_admitted());
    assert_eq!(loader.cache_stats(), (1, 1));

    // A re-derived event (new digest) must not hit the old entry.
    let changed = facts("sz.159922", &[&digest_b]);
    let verdict = loader.verdict(&changed);
    assert_eq!(verdict.reason(), Some(BlockReason::FactorUnverified));
    assert_eq!(
        loader.cache_stats(),
        (1, 2),
        "the cache key carries the digest"
    );
    // And the old key still answers from the cache.
    assert!(loader.verdict(&verified).is_admitted());
    assert_eq!(loader.cache_stats(), (2, 2));
}

#[test]
fn the_loader_reports_the_checks_digest_of_the_set_it_judges_with() {
    let record = check(
        "sz.159922",
        0,
        &"a".repeat(64),
        CheckStatus::VerifiedOfficial,
    );
    let set = CheckSet {
        shas: vec![record.digest().unwrap()],
        records: vec![record.clone()],
    };
    let expected = set.digest();
    let loader = Loader::from_set(set);
    assert_eq!(loader.checks_digest(), expected);
    assert_eq!(loader.record_count(), 1);
    // The digest is order-free over the record ids.
    let other = CheckSet {
        shas: vec![record.digest().unwrap()],
        records: vec![record],
    };
    assert_eq!(other.digest(), expected);
}

#[test]
fn the_loader_reads_the_store_without_writing_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::source_store::SourceStore::new(dir.path().join("store"));
    let record = check(
        "sz.159922",
        0,
        &"a".repeat(64),
        CheckStatus::VerifiedOfficial,
    );
    let path = dir.path().join("checks.jsonl");
    std::fs::write(&path, format!("{}\n", record.canonical_json().unwrap())).unwrap();
    let options = crate::source_store::import::ImportOptions {
        format: crate::source_store::import::ImportFormat::EtfFactorChecksJsonl,
        ..crate::source_store::import::ImportOptions::default()
    };
    crate::source_store::etf_factor_checks::import(&store, &path, &options).unwrap();
    let manifest_before = std::fs::read(store.manifest_path()).unwrap();
    let loader = Loader::open(&store).unwrap();
    assert_eq!(loader.record_count(), 1);
    assert_eq!(
        loader.checks_digest(),
        crate::source_store::etf_factor_checks::read(&store)
            .unwrap()
            .digest()
    );
    assert_eq!(
        std::fs::read(store.manifest_path()).unwrap(),
        manifest_before,
        "opening a loader never writes the store"
    );
}

#[test]
fn refresh_picks_up_a_new_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::source_store::SourceStore::new(dir.path().join("store"));
    let mut loader = Loader::open(&store).unwrap();
    assert_eq!(loader.record_count(), 0);

    let record = check(
        "sz.159922",
        0,
        &"a".repeat(64),
        CheckStatus::VerifiedOfficial,
    );
    let path = dir.path().join("checks.jsonl");
    std::fs::write(&path, format!("{}\n", record.canonical_json().unwrap())).unwrap();
    let options = crate::source_store::import::ImportOptions {
        format: crate::source_store::import::ImportFormat::EtfFactorChecksJsonl,
        ..crate::source_store::import::ImportOptions::default()
    };
    crate::source_store::etf_factor_checks::import(&store, &path, &options).unwrap();
    let before = loader.checks_digest();
    assert_eq!(
        loader.record_count(),
        0,
        "the loader holds its own snapshot"
    );
    loader.refresh(&store).unwrap();
    assert_eq!(loader.record_count(), 1);
    assert_ne!(loader.checks_digest(), before);
}

/// The read path against a real compact database: the stored
/// `adjustment_events.evidence_digest` is what the decision keys on.
#[test]
fn facts_read_from_the_database_carry_the_stored_evidence_digest() {
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let db = dir.path().join("market.db");
    SqliteTasks::new(&db).initialize_compact().unwrap();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    let c = Connection::open(&db).unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    let generation = evidence_store::put_evidence(
        &c,
        EvidenceKind::Generation,
        &json!({"generation_id": "gen-loader", "manifest_sha256": "00"}),
    )
    .unwrap();
    let binding = evidence_store::put_source_binding(
        &c,
        &SourceBinding {
            generation_id: "gen-loader".into(),
            generation_manifest_sha256: [0x0d; 32],
            day_member_sha256: [0x0e; 32],
            security_event_sha256: [0x0f; 32],
            evidence_digest: generation,
        },
    )
    .unwrap();
    let set = AdjustmentEventSet {
        entity: "sz.159922".into(),
        events: vec![AdjustmentEvent {
            ordinal: 0,
            effective_day: day(2024, 12, 2),
            category: 11,
            c_bits: [
                0.0f32.to_bits(),
                0.0f32.to_bits(),
                2.5f32.to_bits(),
                0.0f32.to_bits(),
            ],
            factors: [
                Decimal::ZERO,
                Decimal::ZERO,
                "2.5".parse().unwrap(),
                Decimal::ZERO,
            ],
            known_at: None,
            basis_status: FactorBasisStatus::Pending,
        }],
        excluded: 0,
        precision: FactorPrecision::EtfF32Shortest,
    };
    let stored = derived::replace_adjustment_events(&c, &set, binding, "task", "item")
        .unwrap()
        .unwrap();
    let expected = stored
        .evidence_digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let tasks = SqliteTasks::new(&db);
    let backend = MarketBackend::open(&tasks).unwrap();
    let entity = facts_from_database(&backend, "sz.159922", Some(day(2019, 1, 4))).unwrap();
    assert_eq!(entity.facts.len(), 1);
    assert_eq!(entity.facts[0].evidence_digest, expected);
    assert_eq!(entity.facts[0].category, 11);
    assert_eq!(entity.facts[0].ordinal, 0);
    assert_eq!(entity.first_bar_day, Some(day(2019, 1, 4)));

    // The decision therefore uses exactly the digest the record names.
    let loader = loader(vec![check(
        "sz.159922",
        0,
        &expected,
        CheckStatus::VerifiedOfficial,
    )]);
    assert!(loader.verdict(&entity).is_admitted());
    // An entity with no stored fact has nothing to block on.
    let empty = facts_from_database(&backend, "sz.159830", None).unwrap();
    assert!(empty.facts.is_empty());
    assert!(loader.verdict(&empty).is_admitted());

    // A digest that is not stored blocks.
    let _ = Digest::default();
    let stale = loader.verdict(&facts("sz.159922", &[&"a".repeat(64)]));
    assert_eq!(stale.reason(), Some(BlockReason::FactorUnverified));
}
