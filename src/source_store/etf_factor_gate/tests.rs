//! `#607` PR-B: the pure blocking decision (design §3, D-92 Q1 (a), Q10).
//!
//! The fixtures use the two ETF anchors the design names: `sz.159922`
//! (2024-12-02, cat11 `c3=2.5` + cat1 `c1=1.292` on one effective day, official
//! `qss` 2.334 → 6.159 / 2.5 − 0.1292 = 2.3344, D-91 §1) and `sh.510180`
//! (`c3=10` share conversion recorded as `verified_announcement`).

use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::*;
use crate::source_store::etf_factor_checks::{CheckBasis, CheckStatus, EtfFactorCheck};

const DIGEST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const DOC_SHA: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn fact(
    ordinal: i64,
    effective: NaiveDate,
    category: u8,
    c1: &str,
    c3: &str,
    digest: &str,
) -> FactorFact {
    FactorFact {
        ordinal,
        effective_day: effective,
        category,
        factors: [dec(c1), dec("0"), dec(c3), dec("0")],
        evidence_digest: digest.into(),
    }
}

fn check(
    entity: &str,
    ordinal: i64,
    category: u8,
    status: CheckStatus,
    digest: &str,
) -> EtfFactorCheck {
    let (basis, basis_ref, url) = match status {
        CheckStatus::VerifiedOfficial => (
            CheckBasis::OfficialQss,
            "szse_1815_stock:2024-12-02".to_string(),
            "https://example.invalid/qss".to_string(),
        ),
        CheckStatus::VerifiedAnnouncement => (
            CheckBasis::Announcement,
            "sse_ann:2013-04-17".to_string(),
            "https://example.invalid/a.pdf".to_string(),
        ),
        CheckStatus::Unverified | CheckStatus::NoEffect => {
            (CheckBasis::None, String::new(), String::new())
        }
    };
    EtfFactorCheck {
        record_version: crate::source_store::etf_factor_checks::CHECKS_VERSION.into(),
        entity: entity.into(),
        ordinal,
        effective_day: "2024-12-02".into(),
        category,
        check_status: status,
        basis,
        basis_ref,
        evidence_url: url,
        evidence_sha256: if basis == CheckBasis::None {
            String::new()
        } else {
            DOC_SHA.into()
        },
        announced_value: "2.5".into(),
        gbbq_value: "2.5".into(),
        price_delta: "0.0004".into(),
        within_one_tick: Some(true),
        checked_at: "2026-11-09".into(),
        evidence_digest: digest.into(),
    }
}

/// `sz.159922`: cat11 (`c3=2.5`) and cat1 (`c1=1.292`) on one effective day.
fn composite() -> EntityFacts {
    EntityFacts {
        entity: "sz.159922".into(),
        first_bar_day: Some(day(2019, 1, 4)),
        facts: vec![
            fact(1, day(2024, 12, 2), 11, "0", "2.5", DIGEST_A),
            fact(2, day(2024, 12, 2), 1, "1.292", "0", DIGEST_B),
        ],
    }
}

/// `sh.510180`: one `c3=10` share conversion.
fn conversion() -> EntityFacts {
    EntityFacts {
        entity: "sh.510180".into(),
        first_bar_day: Some(day(2006, 5, 18)),
        facts: vec![fact(0, day(2013, 4, 17), 11, "0", "10", DIGEST_A)],
    }
}

#[test]
fn all_verified_admits_the_entity() {
    let facts = composite();
    let checks = vec![
        check("sz.159922", 1, 11, CheckStatus::VerifiedOfficial, DIGEST_A),
        check("sz.159922", 2, 1, CheckStatus::VerifiedOfficial, DIGEST_B),
    ];
    let verdict = decide(&facts, &checks);
    assert!(verdict.is_admitted(), "{verdict:?}");
    assert_eq!(
        verdict,
        Verdict::Admitted {
            judged: 2,
            excluded: 0
        }
    );
}

#[test]
fn one_unverified_event_blocks_the_whole_entity() {
    let facts = composite();
    let checks = vec![
        check("sz.159922", 1, 11, CheckStatus::VerifiedOfficial, DIGEST_A),
        check("sz.159922", 2, 1, CheckStatus::Unverified, DIGEST_B),
    ];
    let verdict = decide(&facts, &checks);
    assert_eq!(verdict.reason(), Some(BlockReason::FactorUnverified));
    assert_eq!(verdict.blocking_events(), 1);
    assert!(matches!(
        verdict,
        Verdict::Blocked {
            categories,
            ..
        } if categories == vec![1]
    ));
}

#[test]
fn a_missing_record_blocks_instead_of_admitting_by_default() {
    let facts = composite();
    // Only the cat11 event is checked.
    let checks = vec![check(
        "sz.159922",
        1,
        11,
        CheckStatus::VerifiedOfficial,
        DIGEST_A,
    )];
    let verdict = decide(&facts, &checks);
    assert_eq!(verdict.reason(), Some(BlockReason::FactorUnverified));
    assert_eq!(verdict.blocking_events(), 1);

    // No record at all is the same verdict, never a pass.
    assert_eq!(
        decide(&facts, &[]).reason(),
        Some(BlockReason::FactorUnverified)
    );
}

#[test]
fn a_changed_evidence_digest_stops_matching_and_blocks() {
    let facts = composite();
    let checks = vec![
        check("sz.159922", 1, 11, CheckStatus::VerifiedOfficial, DIGEST_A),
        check("sz.159922", 2, 1, CheckStatus::VerifiedOfficial, DIGEST_B),
    ];
    assert!(decide(&facts, &checks).is_admitted());
    // The daily run re-derived the cat1 event: same ordinal, new evidence.
    let mut changed = composite();
    changed.facts[1].evidence_digest = "d".repeat(64);
    let verdict = decide(&changed, &checks);
    assert_eq!(
        verdict.reason(),
        Some(BlockReason::FactorUnverified),
        "a new event digest must not reuse the old verdict (Q10)"
    );
}

#[test]
fn events_before_the_first_bar_and_no_effect_events_do_not_take_part() {
    let mut facts = composite();
    facts.facts.push(fact(
        3,
        day(2018, 6, 1),
        1,
        "0.5",
        "0",
        "e".repeat(64).as_str(),
    ));
    facts.facts.push(fact(
        4,
        day(2023, 6, 1),
        1,
        "0.3",
        "0",
        "f".repeat(64).as_str(),
    ));
    let checks = vec![
        check("sz.159922", 1, 11, CheckStatus::VerifiedOfficial, DIGEST_A),
        check("sz.159922", 2, 1, CheckStatus::VerifiedOfficial, DIGEST_B),
        // The event before the first bar has no record at all: it must not block.
        check(
            "sz.159922",
            4,
            1,
            CheckStatus::NoEffect,
            "f".repeat(64).as_str(),
        ),
    ];
    let verdict = decide(&facts, &checks);
    assert_eq!(
        verdict,
        Verdict::Admitted {
            judged: 2,
            excluded: 2
        }
    );

    // A `no_effect` record on an in-range event excludes it just the same.
    let checks = vec![
        check("sz.159922", 1, 11, CheckStatus::VerifiedOfficial, DIGEST_A),
        check("sz.159922", 2, 1, CheckStatus::NoEffect, DIGEST_B),
        check(
            "sz.159922",
            4,
            1,
            CheckStatus::NoEffect,
            "f".repeat(64).as_str(),
        ),
    ];
    assert!(decide(&facts, &checks).is_admitted());
}

#[test]
fn a_composite_day_needs_both_records_verified() {
    let facts = composite();
    // Both verified → admitted (D-91 §1 anchor: 6.159 / 2.5 − 0.1292 = 2.3344).
    let both = vec![
        check("sz.159922", 1, 11, CheckStatus::VerifiedOfficial, DIGEST_A),
        check("sz.159922", 2, 1, CheckStatus::VerifiedOfficial, DIGEST_B),
    ];
    assert!(decide(&facts, &both).is_admitted());
    for drop in [0usize, 1] {
        let partial: Vec<_> = both
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != drop)
            .map(|(_, record)| record.clone())
            .collect();
        let verdict = decide(&facts, &partial);
        assert_eq!(
            verdict.reason(),
            Some(BlockReason::FactorUnverified),
            "one record of the same X is not enough (dropped {drop})"
        );
    }
    // Both records exist but the composition was compared and did not anchor.
    let mut unanchored = both.clone();
    unanchored[0].within_one_tick = Some(false);
    assert_eq!(
        decide(&facts, &unanchored).reason(),
        Some(BlockReason::EventOrderUnverified)
    );
}

#[test]
fn the_two_design_anchors_decide_as_the_design_says() {
    // sz.159922 with both records verified admits.
    let facts = composite();
    let checks = vec![
        check(
            "sz.159922",
            1,
            11,
            CheckStatus::VerifiedAnnouncement,
            DIGEST_A,
        ),
        check("sz.159922", 2, 1, CheckStatus::VerifiedOfficial, DIGEST_B),
    ];
    assert!(decide(&facts, &checks).is_admitted());

    // sh.510180 with its `c3=10` conversion verified from the announcement.
    let facts = conversion();
    let checks = vec![check(
        "sh.510180",
        0,
        11,
        CheckStatus::VerifiedAnnouncement,
        DIGEST_A,
    )];
    assert!(decide(&facts, &checks).is_admitted());
    // Without the record it blocks: the Shanghai path has no official qss.
    assert_eq!(
        decide(&facts, &[]).reason(),
        Some(BlockReason::FactorUnverified)
    );
}

#[test]
fn an_unexpected_category_blocks() {
    let mut facts = conversion();
    facts
        .facts
        .push(fact(1, day(2015, 1, 5), 2, "0", "0", DIGEST_B));
    let checks = vec![
        check(
            "sh.510180",
            0,
            11,
            CheckStatus::VerifiedAnnouncement,
            DIGEST_A,
        ),
        check(
            "sh.510180",
            1,
            2,
            CheckStatus::VerifiedAnnouncement,
            DIGEST_B,
        ),
    ];
    assert_eq!(
        decide(&facts, &checks).reason(),
        Some(BlockReason::EventCategoryUnexpected)
    );
}

#[test]
fn an_unparsable_factor_blocks() {
    let mut facts = conversion();
    facts.facts[0].factors[2] = Decimal::ZERO;
    let checks = vec![check(
        "sh.510180",
        0,
        11,
        CheckStatus::VerifiedAnnouncement,
        DIGEST_A,
    )];
    assert_eq!(
        decide(&facts, &checks).reason(),
        Some(BlockReason::FactorUnparsable)
    );
}

#[test]
fn a_newer_record_replaces_an_older_verdict() {
    let facts = conversion();
    let mut old = check("sh.510180", 0, 11, CheckStatus::Unverified, DIGEST_A);
    old.checked_at = "2026-11-09".into();
    let mut new = check("sh.510180", 0, 11, CheckStatus::VerifiedOfficial, DIGEST_A);
    new.checked_at = "2026-11-12".into();
    assert!(decide(&facts, &[old.clone(), new.clone()]).is_admitted());
    assert_eq!(
        decide(&facts, &[new, old]).reason(),
        None,
        "record order in the set must not change the verdict"
    );
}

#[test]
fn one_entity_never_borrows_another_entity_s_records() {
    let facts = conversion();
    let checks = vec![check(
        "sh.510300",
        0,
        11,
        CheckStatus::VerifiedAnnouncement,
        DIGEST_A,
    )];
    assert_eq!(
        decide(&facts, &checks).reason(),
        Some(BlockReason::FactorUnverified)
    );
}

#[test]
fn every_reason_code_is_a_stable_wire_string() {
    for (reason, text) in [
        (BlockReason::FactorUnverified, "etf_factor_unverified"),
        (
            BlockReason::EventOrderUnverified,
            "etf_event_order_unverified",
        ),
        (
            BlockReason::EventCategoryUnexpected,
            "etf_event_category_unexpected",
        ),
        (BlockReason::FactorUnparsable, "etf_factor_unparsable"),
        (BlockReason::QfqNotBuilt, "etf_qfq_not_built"),
    ] {
        assert_eq!(reason.as_str(), text);
        assert_eq!(BlockReason::parse(text), Some(reason));
    }
    assert_eq!(not_built().reason(), Some(BlockReason::QfqNotBuilt));
}
