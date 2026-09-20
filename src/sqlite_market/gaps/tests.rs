use super::*;
use crate::sqlite_tasks::MIGRATIONS;
fn setup() -> (tempfile::TempDir, SqliteMarket) {
    let dir = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(dir.path().join("market.sqlite"));
    tasks.initialize().unwrap();
    (dir, SqliteMarket::new(tasks))
}
fn gap(id: &str, symbol: &str) -> ProvenGap {
    ProvenGap {
        gap_id: id.into(),
        symbol: symbol.into(),
        period: Period::Day,
        start_date: "2025-02-01".parse().unwrap(),
        end_date: "2025-02-10".parse().unwrap(),
        evidence_kind: "fixture_evidence".into(),
        evidence_id: "fixture-object".into(),
        evidence_digest: "f".repeat(64),
        observed_at: "2026-09-14T00:00:00Z".parse().unwrap(),
    }
}
fn counts(db: &SqliteMarket) -> (i64, i64) {
    let c = Connection::open(db.tasks.path()).unwrap();
    (
        c.query_row("SELECT count(*) FROM proven_gaps", [], |r| r.get(0))
            .unwrap(),
        c.query_row("SELECT count(*) FROM gap_coverings", [], |r| r.get(0))
            .unwrap(),
    )
}
#[test]
fn record_and_read_open_gaps() {
    let (_dir, db) = setup();
    let g = gap("g1", "sh.600006");
    assert!(db.record_proven_gap(&g).unwrap());
    assert_eq!(db.proven_gap("g1").unwrap(), Some(g.clone()));
    assert_eq!(
        db.open_proven_gaps("sh.600006", Period::Day).unwrap(),
        vec![g]
    );
    assert!(
        db.open_proven_gaps("sh.600006", Period::Year)
            .unwrap()
            .is_empty()
    );
    assert!(
        db.open_proven_gaps("sz.000001", Period::Day)
            .unwrap()
            .is_empty()
    );
    assert!(db.proven_gap("absent").unwrap().is_none());
}
#[test]
fn covering_supersedes_open_gap_without_erasing_history() {
    let (_dir, db, plan) = super::super::tests::setup();
    db.commit(&plan).unwrap();
    let g = gap("g1", "sh.600006");
    db.record_proven_gap(&g).unwrap();
    let covering = GapCovering {
        gap_id: "g1".into(),
        version_id: "legacy-daily".into(),
        covered_at: "2026-09-14T01:00:00Z".parse().unwrap(),
    };
    assert!(db.cover_gap(&covering).unwrap());
    assert!(
        db.open_proven_gaps("sh.600006", Period::Day)
            .unwrap()
            .is_empty()
    );
    assert_eq!(db.proven_gap("g1").unwrap(), Some(g));
    assert_eq!(db.gap_coverings("g1").unwrap(), vec![covering]);
    assert_eq!(counts(&db), (1, 1));
}
#[test]
fn identical_rerecord_and_recovering_are_idempotent() {
    let (_dir, db, plan) = super::super::tests::setup();
    db.commit(&plan).unwrap();
    let g = gap("g1", "sh.600006");
    assert!(db.record_proven_gap(&g).unwrap());
    assert!(!db.record_proven_gap(&g).unwrap());
    let covering = GapCovering {
        gap_id: "g1".into(),
        version_id: "legacy-daily".into(),
        covered_at: "2026-09-14T01:00:00Z".parse().unwrap(),
    };
    assert!(db.cover_gap(&covering).unwrap());
    assert!(!db.cover_gap(&covering).unwrap());
    assert_eq!(counts(&db), (1, 1));
    // same identity, different content: fail closed
    let mut changed = g.clone();
    changed.end_date = "2025-02-11".parse().unwrap();
    assert!(matches!(
        db.record_proven_gap(&changed),
        Err(Error::Integrity(_))
    ));
    let mut changed = covering.clone();
    changed.covered_at = "2026-09-15T01:00:00Z".parse().unwrap();
    assert!(matches!(db.cover_gap(&changed), Err(Error::Integrity(_))));
    assert_eq!(counts(&db), (1, 1));
}
#[test]
fn gap_and_covering_rows_are_immutable() {
    let (_dir, db, plan) = super::super::tests::setup();
    db.commit(&plan).unwrap();
    db.record_proven_gap(&gap("g1", "sh.600006")).unwrap();
    db.cover_gap(&GapCovering {
        gap_id: "g1".into(),
        version_id: "legacy-daily".into(),
        covered_at: "2026-09-14T01:00:00Z".parse().unwrap(),
    })
    .unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    for sql in [
        "UPDATE proven_gaps SET end_date='2030-01-01'",
        "DELETE FROM proven_gaps",
        "UPDATE gap_coverings SET version_id='x'",
        "DELETE FROM gap_coverings",
    ] {
        assert!(c.execute_batch(sql).is_err(), "{sql}");
    }
    drop(c);
    assert_eq!(counts(&db), (1, 1));
    assert_eq!(db.gap_coverings("g1").unwrap().len(), 1);
}
#[test]
fn invalid_facts_are_rejected_without_half_writes() {
    let (_dir, db, plan) = super::super::tests::setup();
    db.commit(&plan).unwrap();
    // inverted range and empty identities fail before any row exists
    let mut bad = gap("bad-range", "sh.600006");
    bad.end_date = "2025-01-01".parse().unwrap();
    assert!(matches!(db.record_proven_gap(&bad), Err(Error::Invalid(_))));
    let mut bad = gap("bad-evidence", "sh.600006");
    bad.evidence_digest = "  ".into();
    assert!(matches!(db.record_proven_gap(&bad), Err(Error::Invalid(_))));
    // a schema-level CHECK also rejects junk written outside the adapter
    let c = Connection::open(db.tasks.path()).unwrap();
    assert!(
        c.execute(
            "INSERT INTO proven_gaps VALUES('x','s','month','2025-01-01','2025-01-02','k','e','d','t')",
            []
        )
        .is_err()
    );
    assert!(
        c.execute(
            "INSERT INTO proven_gaps VALUES('y','s','day','2025-02-01','2025-01-01','k','e','d','t')",
            []
        )
        .is_err()
    );
    assert!(
        c.execute(
            "INSERT INTO proven_gaps VALUES('z','s','day','not-a-date','2025-01-01','k','e','d','t')",
            []
        )
        .is_err()
    );
    drop(c);
    // covering must bind a real gap and a same-scope published version
    assert!(matches!(
        db.cover_gap(&GapCovering {
            gap_id: "absent".into(),
            version_id: "legacy-daily".into(),
            covered_at: "2026-09-14T01:00:00Z".parse().unwrap(),
        }),
        Err(Error::Invalid(_))
    ));
    db.record_proven_gap(&gap("g1", "sh.600006")).unwrap();
    assert!(matches!(
        db.cover_gap(&GapCovering {
            gap_id: "g1".into(),
            version_id: "no-such-version".into(),
            covered_at: "2026-09-14T01:00:00Z".parse().unwrap(),
        }),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        db.cover_gap(&GapCovering {
            gap_id: "g1".into(),
            version_id: "".into(),
            covered_at: "2026-09-14T01:00:00Z".parse().unwrap(),
        }),
        Err(Error::Invalid(_))
    ));
    assert_eq!(counts(&db), (1, 0));
    assert_eq!(
        db.open_proven_gaps("sh.600006", Period::Day).unwrap().len(),
        1
    );
}
#[test]
fn only_published_versions_cover_a_gap() {
    let (_dir, db, plan) = super::super::tests::setup();
    // 'legacy-daily' is 'validated' after prepare: not a covering
    db.record_proven_gap(&gap("g1", "sh.600006")).unwrap();
    let covering = GapCovering {
        gap_id: "g1".into(),
        version_id: "legacy-daily".into(),
        covered_at: "2026-09-14T01:00:00Z".parse().unwrap(),
    };
    assert!(matches!(db.cover_gap(&covering), Err(Error::Invalid(_))));
    // publish the daily version, then stage a yearly candidate on it
    db.commit(&plan).unwrap();
    let samples: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/issue79/legacy-records.json"
    ))
    .unwrap();
    db.stage(&samples[2]["record"]).unwrap(); // legacy-yearly stays 'staging'
    let mut yearly = gap("gy", "sh.600006");
    yearly.period = Period::Year;
    db.record_proven_gap(&yearly).unwrap();
    let yearly_covering = |v: &str| GapCovering {
        gap_id: "gy".into(),
        version_id: v.into(),
        covered_at: "2026-09-14T01:00:00Z".parse().unwrap(),
    };
    // staging version cannot cover; a published version from the wrong
    // period scope cannot either
    assert!(matches!(
        db.cover_gap(&yearly_covering("legacy-yearly")),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        db.cover_gap(&yearly_covering("legacy-daily")),
        Err(Error::Invalid(_))
    ));
    // validated-but-unpublished still cannot cover
    db.mark_validated(&db.candidate("legacy-yearly").unwrap())
        .unwrap();
    assert!(matches!(
        db.cover_gap(&yearly_covering("legacy-yearly")),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        db.open_proven_gaps("sh.600006", Period::Year)
            .unwrap()
            .len(),
        1
    );
    // published daily covers the daily gap
    assert!(db.cover_gap(&covering).unwrap());
    assert!(
        db.open_proven_gaps("sh.600006", Period::Day)
            .unwrap()
            .is_empty()
    );
    assert_eq!(counts(&db), (2, 1));
}
#[test]
fn malformed_and_corrupt_digests_fail_closed() {
    let (_dir, db) = setup();
    // adapter rejects a digest that is not in sha256 format
    let mut bad = gap("bad-digest", "sh.600006");
    bad.evidence_digest = "x".into();
    assert!(matches!(db.record_proven_gap(&bad), Err(Error::Invalid(_))));
    let mut bad = gap("bad-digest2", "sh.600006");
    bad.evidence_digest = "F".repeat(64); // uppercase is not this repo's format
    assert!(matches!(db.record_proven_gap(&bad), Err(Error::Invalid(_))));
    assert_eq!(counts(&db), (0, 0));
    // a stored row whose digest was later damaged fails closed on every read
    db.record_proven_gap(&gap("g1", "sh.600006")).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    c.execute("DROP TRIGGER proven_gaps_no_update", []).unwrap();
    c.execute(
        "UPDATE proven_gaps SET evidence_digest='x' WHERE gap_id='g1'",
        [],
    )
    .unwrap();
    drop(c);
    assert!(matches!(db.proven_gap("g1"), Err(Error::Integrity(_))));
    assert!(matches!(
        db.open_proven_gaps("sh.600006", Period::Day),
        Err(Error::Integrity(_))
    ));
}
#[test]
fn covering_scope_mismatch_is_rejected() {
    let (_dir, db, plan) = super::super::tests::setup();
    db.commit(&plan).unwrap();
    // gap on a different symbol than the published version's scope
    db.record_proven_gap(&gap("g1", "sz.000001")).unwrap();
    assert!(matches!(
        db.cover_gap(&GapCovering {
            gap_id: "g1".into(),
            version_id: "legacy-daily".into(),
            covered_at: "2026-09-14T01:00:00Z".parse().unwrap(),
        }),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        db.open_proven_gaps("sz.000001", Period::Day).unwrap().len(),
        1
    );
    assert_eq!(counts(&db), (1, 0));
}
#[test]
fn concurrent_duplicate_recording_commits_exactly_one_fact() {
    for _ in 0..8 {
        let (_dir, db) = setup();
        let a = db.clone();
        let t = std::thread::spawn(move || a.record_proven_gap(&gap("g1", "sh.600006")));
        let b = SqliteMarket::new(SqliteTasks::new(db.tasks.path()));
        let r2 = b.record_proven_gap(&gap("g1", "sh.600006"));
        let r1 = t.join().unwrap();
        // winner commits exactly one fact; the loser may only be an
        // idempotent no-op or a transient whose message explicitly says
        // SQLite lock contention — any other variant or message fails
        fn busy(e: &Error) -> bool {
            let msg = match e {
                Error::Storage(m) | Error::OutcomeUnknown(m) => m,
                Error::Task(crate::task_control::Error::StorageUnavailable(m)) => m,
                _ => return false,
            };
            let m = msg.to_ascii_lowercase();
            m.contains("busy") || m.contains("locked")
        }
        match (&r1, &r2) {
            (Ok(a), Ok(b)) => assert_eq!(*a as u8 + *b as u8, 1, "{r1:?} {r2:?}"),
            (Ok(true), Err(e)) | (Err(e), Ok(true)) => {
                assert!(busy(e), "non-transient error {e:?}")
            }
            _ => panic!("unexpected concurrent outcome {r1:?} {r2:?}"),
        }
        assert_eq!(counts(&db), (1, 0));
    }
}
#[test]
fn upgraded_v10_database_has_no_fabricated_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");
    {
        let c = Connection::open(&path).unwrap();
        c.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        for (i, sql) in MIGRATIONS[..10].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.execute(
                "INSERT INTO schema_migrations VALUES(?1,?2,?3)",
                params![
                    i as u32 + 1,
                    crate::hash::sha256(sql.as_bytes()),
                    "2026-09-13T00:00:00Z"
                ],
            )
            .unwrap();
            c.pragma_update(None, "user_version", i as u32 + 1).unwrap();
        }
    }
    let db = SqliteMarket::new(SqliteTasks::new(&path));
    db.tasks.upgrade().unwrap();
    let v: i64 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 14);
    assert_eq!(counts(&db), (0, 0));
    assert_eq!(decision_count(&db), 0);
    assert!(
        db.open_proven_gaps("sh.600006", Period::Day)
            .unwrap()
            .is_empty()
    );
    // the fact layer works after upgrade, without backfilling history
    assert!(db.record_proven_gap(&gap("g1", "sh.600006")).unwrap());
    assert_eq!(counts(&db), (1, 0));
    assert!(db.record_gap_decision(&decision("d1")).unwrap());
    assert_eq!(decision_count(&db), 1);
}
#[test]
fn read_open_gaps_inside_caller_snapshot_matches_public_read() {
    let (_dir, db) = setup();
    let mut earlier = gap("g2", "sh.600006");
    earlier.start_date = "2025-01-01".parse().unwrap();
    earlier.end_date = "2025-01-02".parse().unwrap();
    db.record_proven_gap(&gap("g1", "sh.600006")).unwrap();
    db.record_proven_gap(&earlier).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    let shared = read_open_gaps(&c, "sh.600006", Period::Day).unwrap();
    assert_eq!(
        shared,
        db.open_proven_gaps("sh.600006", Period::Day).unwrap()
    );
    assert_eq!(shared[0].gap_id, "g2");
}
#[test]
fn corrupt_stored_rows_fail_closed() {
    let (_dir, db) = setup();
    db.record_proven_gap(&gap("g1", "sh.600006")).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    c.execute("DROP TRIGGER proven_gaps_no_update", []).unwrap();
    c.execute(
        "UPDATE proven_gaps SET observed_at='unreadable' WHERE gap_id='g1'",
        [],
    )
    .unwrap();
    drop(c);
    assert!(matches!(db.proven_gap("g1"), Err(Error::Integrity(_))));
    assert!(matches!(
        db.open_proven_gaps("sh.600006", Period::Day),
        Err(Error::Integrity(_))
    ));
}
fn decision(id: &str) -> GapVerificationDecision {
    GapVerificationDecision {
        decision_id: id.into(),
        symbol: "sh.600006".into(),
        period: Period::Day,
        start_date: "2025-03-01".parse().unwrap(),
        end_date: "2025-03-10".parse().unwrap(),
        subject_version_id: "daily-600006".into(),
        subject_version_digest: "b".repeat(64),
        conclusion: "confirmed_missing_range".into(),
        basis_kind: "fixture_basis".into(),
        basis_ref: "fixture:basis-object".into(),
        basis_sha256: "c".repeat(64),
        verifier_id: "fixture:verifier".into(),
        decided_at: "2026-09-15T00:00:00Z".parse().unwrap(),
        decision_sha256: "d".repeat(64),
    }
}
fn decision_count(db: &SqliteMarket) -> i64 {
    Connection::open(db.tasks.path())
        .unwrap()
        .query_row("SELECT count(*) FROM gap_verification_decisions", [], |r| {
            r.get(0)
        })
        .unwrap()
}
#[test]
fn decision_commits_gap_atomically_and_reads_back() {
    let (_dir, db) = setup();
    let d = decision("d1");
    assert!(db.record_gap_decision(&d).unwrap());
    assert_eq!(db.gap_verification_decision("d1").unwrap(), Some(d.clone()));
    assert_eq!(decision_count(&db), 1);
    assert_eq!(counts(&db), (1, 0));
    let open = db.open_proven_gaps("sh.600006", Period::Day).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].gap_id, derive_gap_id(&d));
    assert_eq!(open[0].evidence_kind, GAP_DECISION_EVIDENCE_KIND);
    assert_eq!(open[0].evidence_id, "d1");
    assert_eq!(open[0].evidence_digest, d.decision_sha256);
    assert_eq!(open[0].observed_at, d.decided_at);
    assert_eq!(open[0].start_date, d.start_date);
    assert_eq!(open[0].end_date, d.end_date);
}
#[test]
fn decision_replay_is_idempotent_and_conflicting_content_fails() {
    let (_dir, db) = setup();
    let d = decision("d1");
    assert!(db.record_gap_decision(&d).unwrap());
    // Identical replay: no new rows.
    assert!(!db.record_gap_decision(&d).unwrap());
    assert_eq!(decision_count(&db), 1);
    assert_eq!(counts(&db), (1, 0));
    // Same decision_id, different content: integrity conflict.
    let mut other = decision("d1");
    other.basis_ref = "fixture:other-basis".into();
    assert!(matches!(
        db.record_gap_decision(&other),
        Err(Error::Integrity(_))
    ));
    // A different decision_id about the same subject+range derives the
    // same gap_id but a different evidence binding: identity conflict, and
    // the same-transaction rollback leaves no orphan decision row either.
    let mut second = decision("d2");
    second.decision_sha256 = "e".repeat(64);
    assert!(matches!(
        db.record_gap_decision(&second),
        Err(Error::Integrity(_))
    ));
    assert_eq!(decision_count(&db), 1);
    assert_eq!(counts(&db), (1, 0));
}
#[test]
fn non_gap_conclusion_stores_decision_without_gap() {
    let (_dir, db) = setup();
    let mut d = decision("d1");
    d.conclusion = "source_not_available".into();
    assert!(db.record_gap_decision(&d).unwrap());
    // Replaying a non-gap decision inserts nothing: not newly written.
    assert!(!db.record_gap_decision(&d).unwrap());
    assert_eq!(db.gap_verification_decision("d1").unwrap(), Some(d));
    assert_eq!(decision_count(&db), 1);
    assert_eq!(counts(&db), (0, 0));
    assert!(
        db.open_proven_gaps("sh.600006", Period::Day)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn decision_insert_with_preexisting_identical_gap_reports_written() {
    let (_dir, db) = setup();
    let d = decision("d1");
    // The derived gap row already exists verbatim (e.g. a pre-decision fact
    // recorded through the same binding): the decision insert still counts.
    let preexisting = ProvenGap {
        gap_id: derive_gap_id(&d),
        symbol: d.symbol.clone(),
        period: d.period,
        start_date: d.start_date,
        end_date: d.end_date,
        evidence_kind: GAP_DECISION_EVIDENCE_KIND.into(),
        evidence_id: d.decision_id.clone(),
        evidence_digest: d.decision_sha256.clone(),
        observed_at: d.decided_at,
    };
    assert!(db.record_proven_gap(&preexisting).unwrap());
    assert!(db.record_gap_decision(&d).unwrap());
    assert_eq!(decision_count(&db), 1);
    assert_eq!(counts(&db), (1, 0));
    // Full replay afterwards is a no-op again.
    assert!(!db.record_gap_decision(&d).unwrap());
}
#[test]
fn invalid_decision_or_gap_writes_neither_row() {
    let (_dir, db) = setup();
    // Mechanically invalid decision (non-sha256 digest): no decision, no gap.
    let mut bad = decision("d1");
    bad.decision_sha256 = "not-a-digest".into();
    assert!(db.record_gap_decision(&bad).is_err());
    assert_eq!(decision_count(&db), 0);
    assert_eq!(counts(&db), (0, 0));
    // Inverted range: validated before any insert.
    let mut bad = decision("d2");
    bad.start_date = "2025-04-01".parse().unwrap();
    assert!(db.record_gap_decision(&bad).is_err());
    assert_eq!(decision_count(&db), 0);
    assert_eq!(counts(&db), (0, 0));
    // Yearly decisions are out of first-phase scope entirely.
    let mut bad = decision("d3");
    bad.period = Period::Year;
    assert!(db.record_gap_decision(&bad).is_err());
    assert_eq!(decision_count(&db), 0);
}
#[test]
fn decision_rows_reject_update_and_delete() {
    let (_dir, db) = setup();
    db.record_gap_decision(&decision("d1")).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    assert!(
        c.execute(
            "UPDATE gap_verification_decisions SET conclusion='x' WHERE decision_id='d1'",
            []
        )
        .is_err()
    );
    assert!(
        c.execute(
            "DELETE FROM gap_verification_decisions WHERE decision_id='d1'",
            []
        )
        .is_err()
    );
    drop(c);
    assert_eq!(
        db.gap_verification_decision("d1").unwrap(),
        Some(decision("d1"))
    );
}
#[test]
fn derive_gap_id_is_deterministic_and_content_bound() {
    let d = decision("d1");
    assert_eq!(derive_gap_id(&d), derive_gap_id(&d));
    assert!(crate::hash::is_sha256(&derive_gap_id(&d)));
    // decision_id/decided_at are not part of gap identity.
    let mut same = d.clone();
    same.decision_id = "d9".into();
    same.decided_at = "2026-09-16T00:00:00Z".parse().unwrap();
    assert_eq!(derive_gap_id(&d), derive_gap_id(&same));
    // Different range or subject binds a different gap.
    let mut other = d.clone();
    other.end_date = "2025-03-11".parse().unwrap();
    assert_ne!(derive_gap_id(&d), derive_gap_id(&other));
    let mut other = d.clone();
    other.subject_version_id = "daily-other".into();
    assert_ne!(derive_gap_id(&d), derive_gap_id(&other));
}
#[test]
fn decision_validate_is_mechanical_only() {
    let mut d = decision("d1");
    assert!(d.validate().is_ok());
    d.symbol = " ".into();
    assert!(d.validate().is_err());
    let mut d = decision("d1");
    d.subject_version_digest = "xyz".into();
    assert!(d.validate().is_err());
    let mut d = decision("d1");
    d.verifier_id = String::new();
    assert!(d.validate().is_err());
    let mut d = decision("d1");
    d.period = Period::Year;
    assert!(d.validate().is_err());
}
#[test]
fn corrupt_stored_decision_fails_closed() {
    let (_dir, db) = setup();
    db.record_gap_decision(&decision("d1")).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    c.execute("DROP TRIGGER gap_verification_decisions_no_update", [])
        .unwrap();
    c.execute(
        "UPDATE gap_verification_decisions SET decided_at='unreadable' WHERE decision_id='d1'",
        [],
    )
    .unwrap();
    drop(c);
    assert!(matches!(
        db.gap_verification_decision("d1"),
        Err(Error::Integrity(_))
    ));
}
