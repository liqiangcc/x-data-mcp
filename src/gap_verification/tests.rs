use super::*;
use crate::market_storage::{
    CONFIRMED_MISSING_RANGE, GAP_DECISION_EVIDENCE_KIND, MarketRepository, derive_gap_id,
};
fn setup() -> (tempfile::TempDir, SqliteMarket) {
    let (_dir, db, plan) = crate::sqlite_market::tests::setup();
    db.commit(&plan).unwrap();
    (_dir, db)
}
fn target() -> GapTarget {
    GapTarget {
        symbol: "sh.600006".into(),
        start_date: "2025-01-01".parse().unwrap(),
        end_date: "2025-01-05".parse().unwrap(),
    }
}
fn counts(db: &SqliteMarket) -> (i64, i64) {
    let c = Connection::open(db.tasks.path()).unwrap();
    (
        c.query_row("SELECT count(*) FROM gap_verification_decisions", [], |r| {
            r.get(0)
        })
        .unwrap(),
        c.query_row("SELECT count(*) FROM proven_gaps", [], |r| r.get(0))
            .unwrap(),
    )
}
/// Test-only verifier minting a well-formed basis for the requested
/// target+subject; knobs emulate dishonest or divergent verifiers.
struct FixtureVerifier {
    id: &'static str,
    conclusion: &'static str,
    claim_id: Option<&'static str>,
    bad_digest: bool,
    wrong_subject: bool,
    wrong_target: bool,
}
impl SourceGapVerifier for FixtureVerifier {
    fn verifier_id(&self) -> &str {
        self.id
    }
    fn verify(&self, target: &GapTarget, subject: &SubjectBinding) -> Result<GapBasis, Error> {
        let mut basis = GapBasis {
            source_identity: "fixture-source".into(),
            source_contract: "fixture-source-v1".into(),
            symbol: if self.wrong_target {
                "sz.000001".into()
            } else {
                target.symbol.clone()
            },
            start_date: target.start_date,
            end_date: if self.wrong_target {
                target.end_date + chrono::Days::new(1)
            } else {
                target.end_date
            },
            subject_version_id: if self.wrong_subject {
                "other-version".into()
            } else {
                subject.version_id.clone()
            },
            subject_version_digest: subject.integrity_sha256.clone(),
            artifact_identity: "fixture:source-facts".into(),
            artifact_sha256: "a".repeat(64),
            conclusion: self.conclusion.into(),
            observed_at: "2026-09-15T00:00:00Z".parse().unwrap(),
            verifier_id: self.claim_id.unwrap_or(self.id).into(),
            basis_sha256: String::new(),
        };
        basis.basis_sha256 = basis.content_digest();
        if self.bad_digest {
            basis.basis_sha256 = "0".repeat(64);
        }
        Ok(basis)
    }
}
fn fixture() -> FixtureVerifier {
    FixtureVerifier {
        id: "fixture-verifier-1",
        conclusion: CONFIRMED_MISSING_RANGE,
        claim_id: None,
        bad_digest: false,
        wrong_subject: false,
        wrong_target: false,
    }
}
#[test]
fn official_stub_fails_closed_with_basis_not_verified() {
    let (_dir, db) = setup();
    let subject = db.freeze_gap_subject(&target()).unwrap();
    assert!(matches!(
        decide_gap(
            &db,
            &UnverifiedGapVerifier,
            &target(),
            &subject,
            "d1",
            Utc::now()
        ),
        Err(Error::BasisNotVerified(_))
    ));
    assert_eq!(counts(&db), (0, 0));
}
#[test]
fn fixture_verifier_commits_decision_and_gap_atomically() {
    let (_dir, db) = setup();
    let t = target();
    let subject = db.freeze_gap_subject(&t).unwrap();
    assert_eq!(subject.version_id, "legacy-daily");
    assert!(crate::hash::is_sha256(&subject.integrity_sha256));
    let now: DateTime<Utc> = "2026-09-15T01:00:00Z".parse().unwrap();
    assert!(decide_gap(&db, &fixture(), &t, &subject, "d1", now).unwrap());
    let d = db.gap_verification_decision("d1").unwrap().unwrap();
    assert_eq!(d.symbol, "sh.600006");
    assert_eq!(d.period, Period::Day);
    assert_eq!(d.subject_version_id, "legacy-daily");
    assert_eq!(d.subject_version_digest, subject.integrity_sha256);
    assert_eq!(d.conclusion, CONFIRMED_MISSING_RANGE);
    assert_eq!(d.basis_kind, GAP_BASIS_KIND);
    assert_eq!(d.verifier_id, "fixture-verifier-1");
    assert_eq!(d.decision_sha256, d.content_digest());
    let open = db.open_proven_gaps("sh.600006", Period::Day).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].gap_id, derive_gap_id(&d));
    assert_eq!(open[0].evidence_kind, GAP_DECISION_EVIDENCE_KIND);
    assert_eq!(open[0].evidence_id, "d1");
    assert_eq!(open[0].evidence_digest, d.decision_sha256);
    assert_eq!(open[0].start_date, t.start_date);
    assert_eq!(open[0].end_date, t.end_date);
    // Identical replay is a no-op.
    assert!(!decide_gap(&db, &fixture(), &t, &subject, "d1", now).unwrap());
    assert_eq!(counts(&db), (1, 1));
}
#[test]
fn missing_or_damaged_subject_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = crate::sqlite_tasks::SqliteTasks::new(dir.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let db = SqliteMarket::new(tasks);
    // No published head at all.
    assert!(matches!(
        db.freeze_gap_subject(&target()),
        Err(Error::NotFound)
    ));
    // Staged but not yet published: head row absent.
    let (_dir2, db, _plan) = crate::sqlite_market::tests::setup();
    assert!(matches!(
        db.freeze_gap_subject(&target()),
        Err(Error::NotFound)
    ));
    // Damage the stored record after publish: full recheck must fail.
    let (_dir3, db, plan) = crate::sqlite_market::tests::setup();
    db.commit(&plan).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    c.execute(
        "UPDATE daily_bars SET close='9.99' WHERE version_id='legacy-daily'",
        [],
    )
    .unwrap();
    drop(c);
    assert!(db.freeze_gap_subject(&target()).is_err());
}
#[test]
fn forged_subject_binding_fails_closed() {
    let (_dir, db) = setup();
    let t = target();
    // A binding naming a real version but a fabricated digest must never
    // reach the verifier or the fact layer.
    let forged = SubjectBinding {
        version_id: "legacy-daily".into(),
        integrity_sha256: "f".repeat(64),
    };
    assert!(matches!(
        decide_gap(&db, &fixture(), &t, &forged, "d1", Utc::now()),
        Err(Error::Integrity(_))
    ));
    // A binding naming a version that does not exist fails equally.
    let absent = SubjectBinding {
        version_id: "ghost-version".into(),
        integrity_sha256: "f".repeat(64),
    };
    assert!(decide_gap(&db, &fixture(), &t, &absent, "d1", Utc::now()).is_err());
    assert_eq!(counts(&db), (0, 0));
}
#[test]
fn moved_subject_head_aborts_commit_without_rows() {
    let (_dir, db) = setup();
    let t = target();
    let subject = db.freeze_gap_subject(&t).unwrap();
    // Head moved after acceptance froze the subject (test-injected drift;
    // the real move would be a later publish promoting another version).
    let c = Connection::open(db.tasks.path()).unwrap();
    c.execute_batch(
        "PRAGMA foreign_keys=OFF; UPDATE effective_heads SET version_id='moved-head' WHERE symbol='sh.600006' AND period='day';",
    )
    .unwrap();
    drop(c);
    assert!(matches!(
        decide_gap(&db, &fixture(), &t, &subject, "d1", Utc::now()),
        Err(Error::Integrity(_))
    ));
    assert_eq!(counts(&db), (0, 0));
    assert!(
        db.open_proven_gaps("sh.600006", Period::Day)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn non_gap_conclusion_stores_decision_without_gap() {
    let (_dir, db) = setup();
    let t = target();
    let subject = db.freeze_gap_subject(&t).unwrap();
    let v = FixtureVerifier {
        conclusion: "source_limited",
        ..fixture()
    };
    let now: DateTime<Utc> = "2026-09-15T01:00:00Z".parse().unwrap();
    assert!(decide_gap(&db, &v, &t, &subject, "d1", now).unwrap());
    assert_eq!(counts(&db), (1, 0));
    assert!(
        db.open_proven_gaps("sh.600006", Period::Day)
            .unwrap()
            .is_empty()
    );
    // Non-gap decision replay (same frozen decided_at) is also a no-op.
    assert!(!decide_gap(&db, &v, &t, &subject, "d1", now).unwrap());
}
#[test]
fn basis_digest_or_binding_mismatch_is_rejected() {
    let (_dir, db) = setup();
    let t = target();
    let subject = db.freeze_gap_subject(&t).unwrap();
    for v in [
        FixtureVerifier {
            bad_digest: true,
            ..fixture()
        },
        FixtureVerifier {
            wrong_subject: true,
            ..fixture()
        },
        FixtureVerifier {
            wrong_target: true,
            ..fixture()
        },
    ] {
        assert!(matches!(
            decide_gap(&db, &v, &t, &subject, "d1", Utc::now()),
            Err(Error::Integrity(_))
        ));
    }
    assert_eq!(counts(&db), (0, 0));
}
#[test]
fn basis_verifier_identity_mismatch_is_rejected() {
    let (_dir, db) = setup();
    let t = target();
    let subject = db.freeze_gap_subject(&t).unwrap();
    // A basis minted under another verifier identity cannot be committed
    // through the installed verifier.
    let v = FixtureVerifier {
        claim_id: Some("fixture-verifier-2"),
        ..fixture()
    };
    assert!(matches!(
        decide_gap(&db, &v, &t, &subject, "d1", Utc::now()),
        Err(Error::Integrity(_))
    ));
    assert_eq!(counts(&db), (0, 0));
}
