//! `#607` PR-D: the read gate — store resolution, the change probe, and the
//! fail-closed answers that wrap PR-B's decision.

use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::*;
use crate::source_store::etf_factor_checks::{self, CheckBasis, CheckStatus, EtfFactorCheck};
use crate::source_store::import::{ImportFormat, ImportOptions};
use crate::sqlite_market::series::EtfForwardFact;

const DOC_SHA: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn record(entity: &str, digest: &str) -> EtfFactorCheck {
    EtfFactorCheck {
        record_version: etf_factor_checks::CHECKS_VERSION.into(),
        entity: entity.into(),
        ordinal: 0,
        effective_day: "2024-12-02".into(),
        category: 11,
        check_status: CheckStatus::VerifiedOfficial,
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

fn facts(entity: &str, digests: &[&str], forward_built: bool) -> EtfForwardFacts {
    EtfForwardFacts {
        entity: entity.into(),
        first_bar_day: Some(day(2019, 1, 4)),
        forward_built,
        facts: digests
            .iter()
            .enumerate()
            .map(|(index, digest)| EtfForwardFact {
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

fn store_with(dir: &tempfile::TempDir, records: &[EtfFactorCheck]) -> SourceStore {
    let store = SourceStore::new(dir.path().join("store"));
    import(&store, records);
    store
}

/// Import through the real importer: the probe must see what a deployment
/// would see (a manifest append), not a hand-made cache.
fn import(store: &SourceStore, records: &[EtfFactorCheck]) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checks.jsonl");
    let body: String = records
        .iter()
        .map(|record| record.canonical_json().unwrap() + "\n")
        .collect();
    std::fs::write(&path, body).unwrap();
    etf_factor_checks::import(
        store,
        &path,
        &ImportOptions {
            format: ImportFormat::EtfFactorChecksJsonl,
            ..ImportOptions::default()
        },
    )
    .unwrap();
}

fn admitted(gate: &ForwardGate, facts: &EtfForwardFacts) -> String {
    match gate.admit(facts) {
        EtfForwardAdmission::Admitted { checks_digest } => checks_digest,
        other => panic!("expected admission, got {other:?}"),
    }
}

fn blocked(
    gate: &ForwardGate,
    facts: &EtfForwardFacts,
) -> (String, usize, Vec<u8>, Option<String>) {
    match gate.admit(facts) {
        EtfForwardAdmission::Blocked {
            reason,
            blocking_events,
            categories,
            detail,
        } => (reason.into(), blocking_events, categories, detail),
        other => panic!("expected a block, got {other:?}"),
    }
}

#[test]
fn an_entity_whose_events_are_verified_is_admitted_with_the_checks_digest() {
    let dir = tempfile::tempdir().unwrap();
    let digest = "a".repeat(64);
    let store = store_with(&dir, &[record("sz.159922", &digest)]);
    let gate = ForwardGate::store(store.clone());
    let checks_digest = admitted(&gate, &facts("sz.159922", &[&digest], true));
    assert_eq!(
        checks_digest,
        etf_factor_checks::read(&store).unwrap().digest(),
        "the response names the exact check set the verdict used"
    );
    // The gate exposes the same digest for diagnostics.
    assert_eq!(
        gate.checks_digest().as_deref(),
        Some(checks_digest.as_str())
    );
}

#[test]
fn an_unverified_event_blocks_that_entity_only() {
    let dir = tempfile::tempdir().unwrap();
    let verified = "a".repeat(64);
    let unchecked = "b".repeat(64);
    let store = store_with(&dir, &[record("sz.159922", &verified)]);
    let gate = ForwardGate::store(store);
    assert!(!admitted(&gate, &facts("sz.159922", &[&verified], true)).is_empty());
    let (reason, blocking_events, categories, detail) =
        blocked(&gate, &facts("sz.159922", &[&verified, &unchecked], true));
    assert_eq!(reason, BlockReason::FactorUnverified.as_str());
    assert_eq!(blocking_events, 1);
    assert_eq!(categories, vec![11]);
    assert_eq!(detail, None, "the stored facts are the reason");
}

#[test]
fn an_entity_without_a_stored_forward_series_is_not_built() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(&dir, &[record("sz.159922", &"a".repeat(64))]);
    let gate = ForwardGate::store(store);
    // Even a fully verified entity is `not_built` while nothing is stored; the
    // reason code is distinct so a client can tell "wait for the build" from
    // "this entity is blocked".
    let (reason, blocking_events, categories, detail) =
        blocked(&gate, &facts("sz.159922", &[&"a".repeat(64)], false));
    assert_eq!(reason, BlockReason::QfqNotBuilt.as_str());
    assert_eq!(blocking_events, 0);
    assert!(categories.is_empty());
    assert_eq!(detail, None);
}

#[test]
fn a_missing_store_fails_closed_with_the_cause() {
    let gate = ForwardGate::unavailable("source store unreadable: permission denied");
    let (reason, _, categories, detail) =
        blocked(&gate, &facts("sz.159922", &[&"a".repeat(64)], true));
    assert_eq!(reason, BlockReason::FactorUnverified.as_str());
    assert!(categories.is_empty());
    assert!(
        detail.as_deref().unwrap().contains("permission denied"),
        "{detail:?}"
    );
}

#[test]
fn no_store_configured_fails_closed_rather_than_admitting() {
    // The env-resolved gate over an unset variable: the answer is a block with
    // the variable named, never an admission.
    let dir = tempfile::tempdir().unwrap();
    let saved = std::env::var_os(CONFIG_ENV);
    // SAFETY: this test is the only reader of the variable in this module and
    // sets it back immediately (Rust 2024 marks env mutation unsafe).
    unsafe { std::env::remove_var(CONFIG_ENV) };
    let result = {
        let gate = ForwardGate::from_env();
        blocked(&gate, &facts("sz.159922", &[&"a".repeat(64)], true))
    };
    if let Some(value) = saved {
        unsafe { std::env::set_var(CONFIG_ENV, value) };
    }
    let _ = dir;
    let (reason, _, _, detail) = result;
    assert_eq!(reason, BlockReason::FactorUnverified.as_str());
    assert!(
        detail.as_deref().unwrap().contains(CONFIG_ENV),
        "{detail:?}"
    );
}

#[test]
fn the_env_store_is_resolved_per_request_so_a_mount_needs_no_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let digest = "a".repeat(64);
    import(&SourceStore::new(&root), &[record("sz.159922", &digest)]);
    let saved = std::env::var_os(CONFIG_ENV);
    unsafe { std::env::set_var(CONFIG_ENV, &root) };
    let gate = ForwardGate::from_env();
    let result = admitted(&gate, &facts("sz.159922", &[&digest], true));
    match saved {
        Some(value) => unsafe { std::env::set_var(CONFIG_ENV, value) },
        None => unsafe { std::env::remove_var(CONFIG_ENV) },
    }
    assert!(!result.is_empty());
}

#[test]
fn an_imported_check_record_admits_the_next_read_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let digest = "a".repeat(64);
    let store = store_with(&dir, &[]);
    let gate = ForwardGate::store(store.clone());
    let fact_set = facts("sz.159922", &[&digest], true);
    // Unverified: no record exists yet.
    assert_eq!(
        blocked(&gate, &fact_set).0,
        BlockReason::FactorUnverified.as_str()
    );
    // The owner imports the record into the running store.
    import(&store, &[record("sz.159922", &digest)]);
    // Nothing restarted, no cache cleared: the next read sees the new set.
    let checks_digest = admitted(&gate, &fact_set);
    assert_eq!(
        checks_digest,
        etf_factor_checks::read(&store).unwrap().digest()
    );
}

#[test]
fn a_new_unchecked_event_blocks_the_next_read_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let verified = "a".repeat(64);
    let store = store_with(&dir, &[record("sz.159922", &verified)]);
    let gate = ForwardGate::store(store);
    admitted(&gate, &facts("sz.159922", &[&verified], true));
    // The daily run stores one more event; its evidence digest matches no
    // record. The verdict cache is keyed by digests, so this is a miss.
    let (reason, blocking_events, _, _) = blocked(
        &gate,
        &facts("sz.159922", &[&verified, &"c".repeat(64)], true),
    );
    assert_eq!(reason, BlockReason::FactorUnverified.as_str());
    assert_eq!(blocking_events, 1);
}

#[test]
fn an_unreadable_store_fails_closed_with_the_cause() {
    let dir = tempfile::tempdir().unwrap();
    // A store root that is a regular file cannot be read as a store. (A
    // permission-only denial is useless here: the test suite may run as root.)
    let root = dir.path().join("not-a-store");
    std::fs::write(&root, b"").unwrap();
    let gate = ForwardGate::store(SourceStore::new(root));
    let (reason, blocking_events, categories, detail) =
        blocked(&gate, &facts("sz.159922", &[&"a".repeat(64)], true));
    assert_eq!(reason, BlockReason::FactorUnverified.as_str());
    assert_eq!(blocking_events, 0);
    assert!(categories.is_empty());
    assert!(detail.is_some(), "the cause is reported: {detail:?}");
}

#[test]
fn a_store_that_disappears_after_an_admission_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let digest = "a".repeat(64);
    let store = store_with(&dir, &[record("sz.159922", &digest)]);
    let gate = ForwardGate::store(store.clone());
    let fact_set = facts("sz.159922", &[&digest], true);
    admitted(&gate, &fact_set);
    let admitted_with = admitted(&gate, &fact_set);
    // The mount goes away under the running process. The next read must not
    // keep answering from memory: the set it holds is no longer what the store
    // says, so the entity is unverified again.
    std::fs::remove_dir_all(store.root()).unwrap();
    let (reason, blocking_events, _, _) = blocked(&gate, &fact_set);
    assert_eq!(reason, BlockReason::FactorUnverified.as_str());
    assert_eq!(blocking_events, 1);
    assert_ne!(
        gate.checks_digest().as_deref(),
        Some(admitted_with.as_str()),
        "the loader was refreshed against the empty store, not reused"
    );
}

/// The probe is a `stat` of the manifest; a request that changes nothing must
/// not pay for a verified re-read. This pins the mechanism: a second decision
/// over the same store is a cache hit and keeps the same digest.
#[test]
fn the_change_probe_keeps_the_same_set_across_requests() {
    let dir = tempfile::tempdir().unwrap();
    let digest = "a".repeat(64);
    let store = store_with(&dir, &[record("sz.159922", &digest)]);
    let gate = ForwardGate::store(store.clone());
    let fact_set = facts("sz.159922", &[&digest], true);
    let first = admitted(&gate, &fact_set);
    let probe = Probe::of(&store).unwrap();
    let second = admitted(&gate, &fact_set);
    assert_eq!(first, second);
    assert_eq!(
        Probe::of(&store).unwrap(),
        probe,
        "an unchanged store keeps its probe value, so no re-read happens"
    );
}

/// The manifest-only digest is the probe's second step: it must equal the
/// loader's `checks_digest` whenever the store verifies.
#[test]
fn the_manifest_digest_equals_the_full_set_digest() {
    let dir = tempfile::tempdir().unwrap();
    let first = record("sz.159922", &"a".repeat(64));
    let mut second = record("sz.159158", &"b".repeat(64));
    second.ordinal = 3;
    let store = store_with(&dir, &[first, second]);
    let full = etf_factor_checks::read(&store).unwrap();
    assert_eq!(
        etf_factor_checks::manifest_digest(&store).unwrap(),
        full.digest()
    );
    // Deduplicated and order-free: the same audit file imported twice keeps
    // the digest, so a re-import does not force a needless re-read.
    let before = etf_factor_checks::read(&store).unwrap();
    import(&store, &[record("sz.159922", &"a".repeat(64))]);
    let after = etf_factor_checks::read(&store).unwrap();
    assert_eq!(before.digest(), after.digest());
    assert_eq!(
        etf_factor_checks::manifest_digest(&store).unwrap(),
        after.digest()
    );
}
