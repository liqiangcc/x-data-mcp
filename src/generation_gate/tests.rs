//! Retirement-gate rules: evidence before facts, exemptions last.

use super::*;
use crate::source_store::{Registration, StoreKind};

const GENERATION: &str = "6f9f0a4f0d3e4c8b1a2c3d4e5f60718293a4b5c6d7e8f90123456789abcdef01";

fn digest(seed: u8) -> String {
    format!("{seed:02x}").repeat(32)
}

fn store_with_evidence(fixture: &tempfile::TempDir) -> SourceStore {
    let store = SourceStore::new(fixture.path().join("source-store"));
    for (source_id, bytes, parser) in [
        (
            TDXHY_SOURCE_ID,
            b"1|600006|T090302|||X6102\r\n".as_slice(),
            TDXHY_PARSER_VERSION,
        ),
        (
            INCON_SOURCE_ID,
            b"T090302|bank\r\n".as_slice(),
            INCON_PARSER_VERSION,
        ),
    ] {
        let mut registration = Registration::new(
            source_id,
            StoreKind::GenerationMember,
            format!("generation:{GENERATION}"),
            "2026-10-29T00:00:00Z",
        );
        registration.parser_version = Some(parser.to_string());
        store.register(bytes, &registration).unwrap();
    }
    store
}

fn all_facts(seed: u8) -> Vec<GenerationFact> {
    let mut facts = Vec::new();
    for (index, (role, kind)) in REQUIRED_FACTS.into_iter().enumerate() {
        facts.push(GenerationFact {
            generation_manifest_sha256: GENERATION.into(),
            role: role.into(),
            fact_kind: kind.into(),
            fact_digest: digest(seed + index as u8),
            evidence_sha256: None,
            parser_version: None,
            registered_at: "2026-10-29T00:00:00Z".into(),
        });
    }
    facts
}

fn exemption(key: &str) -> RetireExemption {
    RetireExemption {
        generation_manifest_sha256: GENERATION.into(),
        missing_kind: key.into(),
        reason: "owner approved: bytes re-imported from the private archive".into(),
        recorded_by: "owner".into(),
        recorded_at: "2026-10-29T00:00:00Z".into(),
    }
}

#[test]
fn complete_evidence_and_facts_pass() {
    let fixture = tempfile::tempdir().unwrap();
    let store = store_with_evidence(&fixture);
    let check = evaluate(&store, GENERATION, &all_facts(0), &[]).unwrap();
    assert!(check.passes(), "{check:?}");
    assert_eq!(check.reason_code(), None);
    assert!(check.exemptions.is_empty());
}

#[test]
fn a_missing_fact_kind_refuses_and_lists_the_role_and_kind() {
    let fixture = tempfile::tempdir().unwrap();
    let store = store_with_evidence(&fixture);
    let facts: Vec<_> = all_facts(0)
        .into_iter()
        .filter(|fact| fact.fact_kind != KIND_CAPITAL_EVENTS)
        .collect();
    let check = evaluate(&store, GENERATION, &facts, &[]).unwrap();
    assert!(!check.passes());
    assert_eq!(check.reason_code(), Some("derived_facts_missing"));
    assert_eq!(
        check.missing_facts,
        vec![FactGap {
            role: ROLE_GBBQ.into(),
            fact_kind: KIND_CAPITAL_EVENTS.into(),
        }]
    );
    let detail = check.detail();
    assert!(detail.contains("gbbq.zip|capital_events"), "{detail}");
    assert!(
        detail.contains("replay --scope generation_facts"),
        "{detail}"
    );
}

#[test]
fn evidence_registered_in_the_store_but_absent_from_the_index_refuses_with_a_replay_hint() {
    let fixture = tempfile::tempdir().unwrap();
    let store = store_with_evidence(&fixture);
    let check = evaluate(&store, GENERATION, &[], &[]).unwrap();
    assert!(!check.passes());
    // Evidence passes (the blobs read back), so the index is what refuses.
    assert!(check.missing_evidence.is_empty());
    assert_eq!(check.reason_code(), Some("derived_facts_missing"));
    assert_eq!(check.missing_facts.len(), REQUIRED_FACTS.len());
}

#[test]
fn missing_source_store_evidence_refuses_with_role_source_and_sha() {
    let fixture = tempfile::tempdir().unwrap();
    // Only the base member is registered; the dictionary is missing.
    let store = SourceStore::new(fixture.path().join("source-store"));
    let mut registration = Registration::new(
        TDXHY_SOURCE_ID,
        StoreKind::GenerationMember,
        format!("generation:{GENERATION}"),
        "2026-10-29T00:00:00Z",
    );
    registration.parser_version = Some(TDXHY_PARSER_VERSION.into());
    store
        .register(b"1|600006|T090302|||X6102\r\n", &registration)
        .unwrap();
    let check = evaluate(&store, GENERATION, &all_facts(0), &[]).unwrap();
    assert_eq!(check.reason_code(), Some("source_store_evidence_missing"));
    assert_eq!(check.missing_evidence.len(), 1);
    let gap = &check.missing_evidence[0];
    assert_eq!(gap.role, ROLE_GBBQ);
    assert_eq!(gap.source_id, INCON_SOURCE_ID);
    assert_eq!(gap.sha256, None);
    let detail = check.detail();
    assert!(
        detail.contains(&format!("gbbq.zip|{INCON_SOURCE_ID}|-")),
        "{detail}"
    );
}

#[test]
fn registered_bytes_that_do_not_read_back_refuse() {
    let fixture = tempfile::tempdir().unwrap();
    let store = store_with_evidence(&fixture);
    let record = store
        .records()
        .unwrap()
        .into_iter()
        .find(|record| record.source_id == TDXHY_SOURCE_ID)
        .unwrap();
    // Tamper with the blob after registration: the manifest still names it,
    // but the read-back digest no longer matches.
    std::fs::write(store.blob_path(&record.sha256), b"tampered").unwrap();
    let check = evaluate(&store, GENERATION, &all_facts(0), &[]).unwrap();
    assert_eq!(check.reason_code(), Some("source_store_evidence_missing"));
    assert_eq!(check.missing_evidence.len(), 1);
    let gap = &check.missing_evidence[0];
    assert_eq!(gap.role, ROLE_BASE);
    assert_eq!(gap.sha256.as_deref(), Some(record.sha256.as_str()));
    assert!(
        gap.problem.as_deref().unwrap_or("").contains("corrupt"),
        "{gap:?}"
    );
}

#[test]
fn an_exemption_lets_the_retire_through_and_is_recorded() {
    let fixture = tempfile::tempdir().unwrap();
    let store = store_with_evidence(&fixture);
    let facts: Vec<_> = all_facts(0)
        .into_iter()
        .filter(|fact| fact.fact_kind != KIND_DAILY_SERIES)
        .collect();
    let key = FactGap {
        role: ROLE_DAY.into(),
        fact_kind: KIND_DAILY_SERIES.into(),
    }
    .key();
    let check = evaluate(&store, GENERATION, &facts, &[exemption(&key)]).unwrap();
    assert!(check.passes(), "{check:?}");
    assert_eq!(check.exemptions, vec![key.clone()]);
    assert!(check.detail().contains(&key), "{}", check.detail());
}

#[test]
fn an_exemption_for_a_different_key_does_not_hide_a_missing_fact() {
    let fixture = tempfile::tempdir().unwrap();
    let store = store_with_evidence(&fixture);
    let facts: Vec<_> = all_facts(0)
        .into_iter()
        .filter(|fact| fact.fact_kind != KIND_DAILY_SERIES)
        .collect();
    let wrong = exemption(&format!(
        "{FACT_KEY_PREFIX}:{ROLE_GBBQ}:{KIND_CAPITAL_EVENTS}"
    ));
    let check = evaluate(&store, GENERATION, &facts, &[wrong]).unwrap();
    assert!(!check.passes());
    assert_eq!(check.reason_code(), Some("derived_facts_missing"));
}

#[test]
fn evidence_gaps_need_their_own_exemption_key() {
    let fixture = tempfile::tempdir().unwrap();
    // The dictionary member is registered and reads back; only the base
    // member is missing, and only its key is exempted.
    let store = SourceStore::new(fixture.path().join("source-store"));
    let mut registration = Registration::new(
        INCON_SOURCE_ID,
        StoreKind::GenerationMember,
        format!("generation:{GENERATION}"),
        "2026-10-29T00:00:00Z",
    );
    registration.parser_version = Some(INCON_PARSER_VERSION.into());
    store.register(b"T090302|bank\r\n", &registration).unwrap();
    let gap = EvidenceGap {
        role: ROLE_BASE.into(),
        source_id: TDXHY_SOURCE_ID.into(),
        sha256: None,
        problem: None,
    };
    let check = evaluate(&store, GENERATION, &all_facts(0), &[exemption(&gap.key())]).unwrap();
    assert!(check.passes(), "{check:?}");
    assert_eq!(check.exemptions, vec![gap.key()]);
}

#[test]
fn industry_fact_rows_verify_the_evidence_blobs_before_returning_rows() {
    let fixture = tempfile::tempdir().unwrap();
    let store = store_with_evidence(&fixture);
    let records = store.records().unwrap();
    let tdxhy = records
        .iter()
        .find(|record| record.source_id == TDXHY_SOURCE_ID)
        .unwrap();
    let incon = records
        .iter()
        .find(|record| record.source_id == INCON_SOURCE_ID)
        .unwrap();
    let rows = industry_fact_rows(&store, GENERATION, &tdxhy.sha256, &incon.sha256).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].role, ROLE_BASE);
    assert_eq!(rows[0].fact_kind, KIND_INDUSTRY_SNAPSHOT);
    assert_eq!(
        rows[0].evidence_sha256.as_deref(),
        Some(tdxhy.sha256.as_str())
    );
    assert_eq!(rows[2].role, ROLE_GBBQ);
    assert_eq!(rows[2].fact_kind, KIND_INDUSTRY_DICTIONARY);
    // A digest the store cannot read back is refused at registration time.
    let error = industry_fact_rows(&store, GENERATION, &digest(9), &incon.sha256).unwrap_err();
    assert!(matches!(error, GateError::Store(_)));
}

#[test]
fn store_rows_rebuild_the_same_industry_facts_as_the_writer() {
    let fixture = tempfile::tempdir().unwrap();
    let store = store_with_evidence(&fixture);
    let records = store.records().unwrap();
    let tdxhy = records
        .iter()
        .find(|record| record.source_id == TDXHY_SOURCE_ID)
        .unwrap();
    let incon = records
        .iter()
        .find(|record| record.source_id == INCON_SOURCE_ID)
        .unwrap();
    let written = industry_fact_rows(&store, GENERATION, &tdxhy.sha256, &incon.sha256).unwrap();
    let replayed = industry_fact_rows_from_store(&store).unwrap();
    assert_eq!(facts_summary(&written), facts_summary(&replayed));
    assert!(drift_keys(&written, &replayed, 20).is_empty());
}

#[test]
fn registered_at_is_not_part_of_the_summary() {
    let mut facts = all_facts(0);
    let summary = facts_summary(&facts);
    facts[0].registered_at = "2030-01-01T00:00:00Z".into();
    assert_eq!(summary, facts_summary(&facts));
}
