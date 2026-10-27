use chrono::NaiveDate;

use super::*;
use crate::security_master::{ArrangementFact, DelistingReason, DelistingSource, ListingSource};

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// A TDX-only row exactly as PR-2 emits it: subject-level `SSDATE`, no issuer
/// key, no B-share date.
fn tdx(
    exchange: Exchange,
    code: &str,
    from: NaiveDate,
    to: Option<NaiveDate>,
) -> SecurityLifecycleFact {
    SecurityLifecycleFact {
        exchange,
        code: code.into(),
        effective_from: from,
        effective_to: to,
        instrument_class: 0,
        issuer_key: None,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_date: Some(from),
        listing_date_tdx: Some(from),
        listing_source: ListingSource::TdxSsdate,
        b_listing_date: None,
        delisting_date: to,
        delisting_date_tdx: to,
        delisting_source: if to.is_some() {
            DelistingSource::TdxArrangement
        } else {
            DelistingSource::Unknown
        },
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: to,
        listing_conflict: false,
        delisting_conflict: false,
        known_at: None,
    }
}

fn listing(
    exchange: Exchange,
    code: &str,
    date: Option<NaiveDate>,
    b_date: Option<NaiveDate>,
    issuer_key: Option<&str>,
) -> ExchangeListingFact {
    ExchangeListingFact {
        exchange,
        code: code.into(),
        listing_date: date,
        b_listing_date: b_date,
        issuer_key: issuer_key.map(Into::into),
        instrument_class: 0,
    }
}

fn snapshot(
    listing: Vec<ExchangeListingFact>,
    delisting: Vec<ExchangeDelistingFact>,
    migrations: Vec<ExchangeCodeMigration>,
) -> ExchangeSource {
    ExchangeSource::Admitted(ExchangeSnapshot {
        as_of: day(2026, 10, 26),
        listing,
        delisting,
        migrations,
    })
}

#[test]
fn a_missing_exchange_snapshot_is_pending_not_an_empty_list() {
    let facts = vec![tdx(Exchange::Sz, "000001", day(1991, 4, 3), None)];
    let source = exchange_source(None);
    assert!(matches!(source, ExchangeSource::Pending(_)));
    let assembly = assemble_security_master(&facts, &[], source, &[]).unwrap();
    assert_eq!(assembly.rows, facts);
    assert_eq!(assembly.pending_sources.len(), 1);
    assert_eq!(assembly.pending_sources[0].dataset, DATASET_KIND);
    assert_eq!(assembly.pending_sources[0].source, "exchange_list");
    assert!(assembly.conflicts.is_empty());
    assert!(assembly.unresolved.is_empty());
    assert_eq!(assembly.exchange_as_of, None);
}

#[test]
fn an_admitted_snapshot_is_not_pending() {
    let source = exchange_source(Some(ExchangeSnapshot {
        as_of: day(2026, 10, 26),
        listing: vec![],
        delisting: vec![],
        migrations: vec![],
    }));
    assert!(matches!(source, ExchangeSource::Admitted(_)));
}

#[test]
fn an_agreeing_exchange_listing_merges_into_the_tdx_row() {
    let facts = vec![tdx(Exchange::Sz, "000001", day(1991, 4, 3), None)];
    let assembly = assemble_security_master(
        &facts,
        &[],
        snapshot(
            vec![listing(
                Exchange::Sz,
                "000001",
                Some(day(1991, 4, 3)),
                None,
                Some("平安银行"),
            )],
            vec![],
            vec![],
        ),
        &[],
    )
    .unwrap();
    assert_eq!(assembly.rows.len(), 1);
    let row = &assembly.rows[0];
    assert_eq!(row.effective_from, day(1991, 4, 3));
    assert_eq!(row.listing_date, Some(day(1991, 4, 3)));
    assert_eq!(row.listing_date_tdx, Some(day(1991, 4, 3)));
    assert_eq!(row.listing_source, ListingSource::ExchangeList);
    assert_eq!(row.issuer_key.as_deref(), Some("平安银行"));
    assert_eq!(row.issuer_resolution, IssuerResolution::IssuerKey);
    assert!(!row.listing_conflict);
    assert!(assembly.conflicts.is_empty());
}

#[test]
fn code_reuse_splits_600018_at_the_tdx_subject_date() {
    // #396: the exchange `LIST_DATE=2000-07-19` is the code's first listing
    // (the old subject); the TDX `SSDATE=2006-10-26` is the current subject's
    // listing day. They are two intervals, not one conflict to average away.
    let facts = vec![tdx(Exchange::Sh, "600018", day(2006, 10, 26), None)];
    let assembly = assemble_security_master(
        &facts,
        &[],
        snapshot(
            vec![listing(
                Exchange::Sh,
                "600018",
                Some(day(2000, 7, 19)),
                None,
                Some("上港集团"),
            )],
            vec![],
            vec![],
        ),
        &[],
    )
    .unwrap();
    assert_eq!(assembly.rows.len(), 2);
    assert_eq!(assembly.exchange_as_of, Some(day(2026, 10, 26)));
    let earlier = &assembly.rows[0];
    assert_eq!(earlier.effective_from, day(2000, 7, 19));
    assert_eq!(earlier.effective_to, Some(day(2006, 10, 26)));
    assert_eq!(earlier.listing_date, Some(day(2000, 7, 19)));
    assert_eq!(earlier.listing_source, ListingSource::ExchangeList);
    // The exchange issuer key belongs to the current subject, not the old one.
    assert_eq!(earlier.issuer_key, None);
    assert_eq!(earlier.issuer_resolution, IssuerResolution::CodeDate);
    assert!(earlier.listing_conflict);

    let later = &assembly.rows[1];
    assert_eq!(later.effective_from, day(2006, 10, 26));
    assert_eq!(later.effective_to, None);
    assert_eq!(later.listing_date, Some(day(2006, 10, 26)));
    assert_eq!(later.listing_date_tdx, Some(day(2006, 10, 26)));
    assert_eq!(later.listing_source, ListingSource::TdxSsdate);
    assert_eq!(later.issuer_key.as_deref(), Some("上港集团"));
    assert_eq!(later.issuer_resolution, IssuerResolution::Ambiguous);
    assert!(later.listing_conflict);

    assert_eq!(
        assembly.conflicts,
        vec![Conflict::ListingDate {
            exchange: Exchange::Sh,
            code: "600018".into(),
            exchange_date: day(2000, 7, 19),
            tdx_date: day(2006, 10, 26),
        }]
    );
    // One row per day: the old subject before 2006-10-26, the current one after.
    assert_eq!(
        lookup(&assembly.rows, Exchange::Sh, "600018", day(2003, 1, 2))
            .unwrap()
            .unwrap()
            .effective_from,
        day(2000, 7, 19)
    );
    assert_eq!(
        lookup(&assembly.rows, Exchange::Sh, "600018", day(2010, 1, 4))
            .unwrap()
            .unwrap()
            .effective_from,
        day(2006, 10, 26)
    );
    assert_eq!(
        lookup(&assembly.rows, Exchange::Sh, "600018", day(1999, 1, 4)).unwrap(),
        None
    );
}

#[test]
fn overlapping_intervals_are_not_merged_and_lookup_fails_closed() {
    // The normal paths split or merge; this guard covers a future source or a
    // hand-built fixture that hands over two open intervals for one code.
    let facts = vec![
        tdx(Exchange::Sh, "600018", day(2000, 7, 19), None),
        tdx(Exchange::Sh, "600018", day(2006, 10, 26), None),
    ];
    let assembly = assemble_security_master(&facts, &[], exchange_source(None), &[]).unwrap();
    assert_eq!(assembly.rows.len(), 2);
    assert!(
        assembly
            .rows
            .iter()
            .all(|row| row.issuer_resolution == IssuerResolution::Ambiguous)
    );
    assert_eq!(assembly.ambiguous_code_reuse_count(), 1);
    assert!(matches!(
        assembly.conflicts.as_slice(),
        [Conflict::AmbiguousCodeReuse { from, to: None, .. }] if *from == day(2006, 10, 26)
    ));
    let error = lookup(&assembly.rows, Exchange::Sh, "600018", day(2010, 1, 4)).unwrap_err();
    assert!(matches!(
        error,
        LookupError::AmbiguousCodeReuse { rows: 2, .. }
    ));
}

#[test]
fn a_confirmed_migration_dedups_300114_and_302132() {
    // A-2 (#398): 300114 -> 302132 effective 2025-02-17; the new code's TDX
    // history is a copy of the old code's, so the interval must not overlap.
    let facts = vec![
        tdx(Exchange::Sz, "300114", day(2010, 8, 27), None),
        tdx(Exchange::Sz, "302132", day(2010, 8, 27), None),
    ];
    let assembly = assemble_security_master(
        &facts,
        &[],
        snapshot(
            vec![listing(
                Exchange::Sz,
                "302132",
                Some(day(2010, 8, 27)),
                None,
                Some("中航成飞"),
            )],
            vec![],
            vec![ExchangeCodeMigration {
                from_exchange: Exchange::Sz,
                from_code: "300114".into(),
                to_exchange: Exchange::Sz,
                to_code: "302132".into(),
                effective_from: day(2025, 2, 17),
                source_record_id: Some("szse-code-change-20250215".into()),
                known_at: Some(day(2025, 2, 15)),
            }],
        ),
        &[],
    )
    .unwrap();
    assert_eq!(assembly.rows.len(), 2);
    assert_eq!(assembly.migrations.len(), 1);
    assert_eq!(
        assembly.migrations[0].certainty,
        MigrationCertainty::Confirmed
    );
    assert_eq!(assembly.migrations[0].source_kind, "exchange_announcement");
    assert!(assembly.conflicts.is_empty());

    let old = lookup(&assembly.rows, Exchange::Sz, "300114", day(2024, 6, 3))
        .unwrap()
        .unwrap();
    assert_eq!(old.effective_from, day(2010, 8, 27));
    assert_eq!(old.effective_to, Some(day(2025, 2, 17)));
    assert_eq!(old.delisting_date, None);

    let new = lookup(&assembly.rows, Exchange::Sz, "302132", day(2025, 3, 3))
        .unwrap()
        .unwrap();
    assert_eq!(new.effective_from, day(2025, 2, 17));
    assert_eq!(new.effective_to, None);
    assert_eq!(new.listing_date, Some(day(2010, 8, 27)));
    assert_eq!(new.listing_source, ListingSource::ExchangeList);
    // The old code is gone on the migration day; the new code owns the history.
    assert_eq!(
        lookup(&assembly.rows, Exchange::Sz, "302132", day(2025, 2, 14)).unwrap(),
        None
    );
    assert_eq!(
        lookup(&assembly.rows, Exchange::Sz, "300114", day(2025, 2, 17)).unwrap(),
        None
    );
}

#[test]
fn a_bar_evidence_candidate_is_recorded_but_does_not_change_intervals() {
    let spans = vec![
        BarSpan {
            exchange: Exchange::Sz,
            code: "300114".into(),
            first_day: day(2010, 8, 27),
            last_day: day(2025, 2, 14),
        },
        BarSpan {
            exchange: Exchange::Sz,
            code: "302132".into(),
            first_day: day(2010, 8, 27),
            last_day: day(2026, 10, 23),
        },
    ];
    let candidates = detect_candidate_migrations(&spans);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].from_code, "300114");
    assert_eq!(candidates[0].to_code, "302132");
    // The day after the old code's last bar: the source has no calendar, and a
    // non-trading day is a valid exclusive boundary.
    assert_eq!(candidates[0].effective_from, day(2025, 2, 15));
    assert_eq!(candidates[0].certainty, MigrationCertainty::Candidate);
    assert_eq!(candidates[0].source_kind, "bar_evidence");

    let facts = vec![
        tdx(Exchange::Sz, "300114", day(2010, 8, 27), None),
        tdx(Exchange::Sz, "302132", day(2010, 8, 27), None),
    ];
    let assembly =
        assemble_security_master(&facts, &[], exchange_source(None), &candidates).unwrap();
    assert_eq!(assembly.rows.len(), 2);
    assert!(assembly.rows.iter().all(|row| row.effective_to.is_none()));
    assert_eq!(assembly.migrations.len(), 1);
    assert_eq!(
        assembly.migrations[0].certainty,
        MigrationCertainty::Candidate
    );

    // A confirmed migration for the same pair suppresses the candidate.
    let confirmed = assemble_security_master(
        &facts,
        &[],
        snapshot(
            vec![listing(
                Exchange::Sz,
                "302132",
                Some(day(2010, 8, 27)),
                None,
                None,
            )],
            vec![],
            vec![ExchangeCodeMigration {
                from_exchange: Exchange::Sz,
                from_code: "300114".into(),
                to_exchange: Exchange::Sz,
                to_code: "302132".into(),
                effective_from: day(2025, 2, 17),
                source_record_id: None,
                known_at: None,
            }],
        ),
        &candidates,
    )
    .unwrap();
    assert_eq!(confirmed.migrations.len(), 1);
    assert_eq!(
        confirmed.migrations[0].certainty,
        MigrationCertainty::Confirmed
    );
}

#[test]
fn an_exchange_only_code_gets_the_tdx_arrangement_bounds() {
    // The 62 codes whose `SSDATE='0'` have no TDX lifecycle row; the exchange
    // list supplies the listing and delisting, and the TDX arrangement still
    // supplies the 退市整理期 interval (#401 A-6).
    let arrangements = vec![ArrangementFact {
        exchange: Exchange::Sz,
        code: "002257".into(),
        start: Some(day(2020, 1, 1)),
        end: Some(day(2020, 2, 1)),
    }];
    let assembly = assemble_security_master(
        &[],
        &arrangements,
        snapshot(
            vec![listing(
                Exchange::Sz,
                "002257",
                Some(day(1995, 1, 1)),
                None,
                None,
            )],
            vec![ExchangeDelistingFact {
                exchange: Exchange::Sz,
                code: "002257".into(),
                delisting_date: day(2020, 2, 1),
                reason_class: DelistingReason::Arrangement,
            }],
            vec![],
        ),
        &[],
    )
    .unwrap();
    assert_eq!(assembly.rows.len(), 1);
    let row = &assembly.rows[0];
    assert_eq!(row.effective_from, day(1995, 1, 1));
    assert_eq!(row.effective_to, Some(day(2020, 2, 1)));
    assert_eq!(row.listing_source, ListingSource::ExchangeList);
    assert_eq!(row.delisting_date, Some(day(2020, 2, 1)));
    assert_eq!(row.delisting_source, DelistingSource::ExchangeList);
    assert_eq!(row.delisting_date_tdx, Some(day(2020, 2, 1)));
    assert_eq!(row.delisting_reason_class, DelistingReason::Arrangement);
    assert_eq!(row.arrangement_start, Some(day(2020, 1, 1)));
    assert_eq!(row.arrangement_end, Some(day(2020, 2, 1)));
    assert!(!row.delisting_conflict);
}

#[test]
fn a_delisting_disagreement_keeps_both_values_and_sets_the_flag() {
    let facts = vec![tdx(
        Exchange::Sz,
        "000004",
        day(1991, 1, 1),
        Some(day(2026, 7, 14)),
    )];
    let assembly = assemble_security_master(
        &facts,
        &[],
        snapshot(
            vec![],
            vec![ExchangeDelistingFact {
                exchange: Exchange::Sz,
                code: "000004".into(),
                delisting_date: day(2026, 7, 15),
                reason_class: DelistingReason::Other,
            }],
            vec![],
        ),
        &[],
    )
    .unwrap();
    assert_eq!(assembly.rows.len(), 1);
    let row = &assembly.rows[0];
    assert_eq!(row.delisting_date, Some(day(2026, 7, 15)));
    assert_eq!(row.delisting_date_tdx, Some(day(2026, 7, 14)));
    assert_eq!(row.delisting_source, DelistingSource::ExchangeList);
    assert_eq!(row.effective_to, Some(day(2026, 7, 15)));
    assert!(row.delisting_conflict);
    assert_eq!(
        assembly.conflicts,
        vec![Conflict::DelistingDate {
            exchange: Exchange::Sz,
            code: "000004".into(),
            exchange_date: day(2026, 7, 15),
            tdx_date: day(2026, 7, 14),
        }]
    );
}

#[test]
fn a_listing_without_a_date_is_unresolved_not_a_guessed_interval() {
    let assembly = assemble_security_master(
        &[],
        &[],
        snapshot(
            vec![listing(Exchange::Sh, "600849", None, None, None)],
            vec![],
            vec![],
        ),
        &[],
    )
    .unwrap();
    assert!(assembly.rows.is_empty());
    assert_eq!(
        assembly.unresolved,
        vec![UnresolvedFact::ListingWithoutDate {
            exchange: Exchange::Sh,
            code: "600849".into(),
        }]
    );
}

#[test]
fn the_digest_is_stable_and_covers_migrations_flags_and_known_at() {
    let facts = vec![tdx(Exchange::Sz, "000001", day(1991, 4, 3), None)];
    let digest = assembly_digest(&facts, &[]);
    assert_eq!(digest, assembly_digest(&facts, &[]));

    let migration = CodeMigrationFact {
        from_exchange: Exchange::Sz,
        from_code: "300114".into(),
        to_exchange: Exchange::Sz,
        to_code: "302132".into(),
        effective_from: day(2025, 2, 17),
        certainty: MigrationCertainty::Confirmed,
        source_kind: "exchange_announcement".into(),
        source_record_id: None,
        known_at: None,
    };
    assert_ne!(digest, assembly_digest(&facts, &[migration]));

    let mut changed = facts.clone();
    changed[0].listing_conflict = true;
    assert_ne!(digest, assembly_digest(&changed, &[]));

    // `known_at` is a stored column, so two otherwise identical datasets whose
    // 知晓时间 differs must not share an identity.
    let mut dated = facts.clone();
    dated[0].known_at = Some(day(2025, 2, 17));
    assert_ne!(digest, assembly_digest(&dated, &[]));
}
