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
        "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','9.99')",
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
// ---- P3: verify_data_gap public wire + task acceptance/execution ----
use crate::sqlite_tasks::SqliteTasks;
use crate::task_control::{
    Change, RequestOutcome, Stage, Status, TaskRepository, service::TaskControl,
};
fn task_setup() -> (tempfile::TempDir, SqliteMarket, TaskControl<SqliteTasks>) {
    let (_dir, db, plan) = crate::sqlite_market::tests::setup();
    db.commit(&plan).unwrap();
    let control = TaskControl::new(
        db.tasks.clone(),
        "run".into(),
        vec![Box::new(contract::GapVerificationContract {
            store: db.clone(),
        })],
    )
    .unwrap();
    (_dir, db, control)
}
fn gap_payload() -> serde_json::Value {
    json!({"target":{"symbol":"sh.600006","period":"day","start_date":"2025-01-01","end_date":"2025-01-05"}})
}
fn accepted_id(control: &TaskControl<SqliteTasks>, request: &str) -> (String, String) {
    let accepted = control
        .accept(request, command_key(), gap_payload())
        .unwrap();
    let RequestOutcome::Task(task) = accepted.record.outcome else {
        panic!()
    };
    (
        task.task.task_id.clone(),
        task.items[0].spec.item_id.clone(),
    )
}
#[test]
fn acceptance_rejects_every_caller_pass_through_field() {
    let (_dir, db, control) = task_setup();
    for (i, field) in [
        "conclusion",
        "basis_kind",
        "basis_ref",
        "basis_sha256",
        "decision_sha256",
        "decided_by",
        "decided_at",
        "verifier_id",
        "subject_version_id",
        "subject_version_digest",
        "source_identity",
        "approved",
    ]
    .iter()
    .enumerate()
    {
        // Top-level pass-through rejected.
        let mut payload = gap_payload();
        payload[field] = json!("x");
        assert!(matches!(
            control.accept(&format!("top-{i}"), command_key(), payload),
            Err(crate::task_control::Error::InvalidCommand(_))
        ));
        // Nested inside target rejected as well.
        let mut payload = gap_payload();
        payload["target"][field] = json!("x");
        assert!(matches!(
            control.accept(&format!("nested-{i}"), command_key(), payload),
            Err(crate::task_control::Error::InvalidCommand(_))
        ));
    }
    // Nothing persisted for any rejected request.
    for i in 0..12 {
        assert!(
            db.tasks
                .read_request(&format!("top-{i}"))
                .unwrap()
                .is_none()
        );
        assert!(
            db.tasks
                .read_request(&format!("nested-{i}"))
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(counts(&db), (0, 0));
}
#[test]
fn acceptance_rejects_non_day_period_inverted_range_and_bad_symbols() {
    let (_dir, db, control) = task_setup();
    let mut cases: Vec<serde_json::Value> = vec![];
    let mut year = gap_payload();
    year["target"]["period"] = json!("year");
    cases.push(year);
    let mut inverted = gap_payload();
    inverted["target"]["start_date"] = json!("2025-01-05");
    inverted["target"]["end_date"] = json!("2025-01-01");
    cases.push(inverted);
    for symbol in [
        "sh.60006",
        "SH.600006",
        "sh.6000067",
        "bj.600006",
        "600006",
        "sh.60000a",
    ] {
        let mut bad = gap_payload();
        bad["target"]["symbol"] = json!(symbol);
        cases.push(bad);
    }
    for (i, payload) in cases.into_iter().enumerate() {
        assert!(matches!(
            control.accept(&format!("bad-{i}"), command_key(), payload),
            Err(crate::task_control::Error::InvalidCommand(_))
        ));
        assert!(
            db.tasks
                .read_request(&format!("bad-{i}"))
                .unwrap()
                .is_none()
        );
    }
}
#[test]
fn first_acceptance_freezes_subject_in_item_frozen_inputs() {
    let (_dir, db, control) = task_setup();
    let subject = db.freeze_gap_subject(&target()).unwrap();
    let accepted = control
        .accept("freeze-1", command_key(), gap_payload())
        .unwrap();
    assert!(!accepted.reused);
    let RequestOutcome::Task(task) = accepted.record.outcome else {
        panic!()
    };
    assert_eq!(task.task.key, command_key());
    assert_eq!(task.items.len(), 1);
    let frozen = &task.items[0].spec.frozen_inputs;
    assert_eq!(
        frozen["target"],
        json!({"symbol":"sh.600006","period":"day","start_date":"2025-01-01","end_date":"2025-01-05"})
    );
    assert_eq!(frozen["subject_version_id"], json!("legacy-daily"));
    assert_eq!(
        frozen["subject_version_digest"],
        json!(subject.integrity_sha256)
    );
    // The item occupies the same symbol x day scope as daily writes.
    assert!(
        task.items[0]
            .spec
            .scope_keys
            .contains(&crate::task_control::legacy::scope_id("sh.600006", "daily"))
    );
}
#[test]
fn official_stub_execution_fails_closed_without_rows() {
    let (_dir, db, control) = task_setup();
    let (task_id, _item) = accepted_id(&control, "official-1");
    let runner = runner::GapVerifyRunner {
        repository: db.clone(),
        run_id: "run".into(),
    };
    let outcome = runner
        .run(&task_id, &UnverifiedGapVerifier, Utc::now())
        .unwrap();
    let runner::GapRunOutcome::NotDecided(task) = outcome else {
        panic!("official stub must not decide")
    };
    assert_eq!(task.items[0].stage, Stage::Finished);
    let item = &task.items[0].outcome.as_ref().unwrap();
    assert!(!item.success);
    assert!(
        item.error
            .as_ref()
            .unwrap()
            .starts_with("GAP_BASIS_NOT_VERIFIED")
    );
    assert_eq!(counts(&db), (0, 0));
}
#[test]
fn fixture_verifier_end_to_end_persists_decision_and_gap() {
    let (_dir, db, control) = task_setup();
    let (task_id, item_id) = accepted_id(&control, "fixture-1");
    let runner = runner::GapVerifyRunner {
        repository: db.clone(),
        run_id: "run".into(),
    };
    let outcome = runner.run(&task_id, &fixture(), Utc::now()).unwrap();
    let runner::GapRunOutcome::Decided(task) = outcome else {
        panic!("fixture verifier should decide")
    };
    assert_eq!(task.task.status, Status::Succeeded);
    assert!(task.items[0].outcome.as_ref().unwrap().success);
    let decision_id = runner::gap_decision_id(&task_id, &item_id);
    let d = db.gap_verification_decision(&decision_id).unwrap().unwrap();
    assert_eq!(d.conclusion, CONFIRMED_MISSING_RANGE);
    assert_eq!(d.subject_version_id, "legacy-daily");
    assert_eq!(counts(&db), (1, 1));
    // Availability surfaces the open proven gap for the target range.
    let open = db.open_proven_gaps("sh.600006", Period::Day).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].start_date, "2025-01-01".parse().unwrap());
    assert_eq!(open[0].evidence_id, decision_id);
    // Runner replay neither re-verifies nor duplicates rows.
    let outcome = runner.run(&task_id, &fixture(), Utc::now()).unwrap();
    assert!(matches!(outcome, runner::GapRunOutcome::Decided(_)));
    assert_eq!(counts(&db), (1, 1));
}
#[test]
fn head_drift_after_acceptance_fails_closed_without_rows() {
    let (_dir, db, control) = task_setup();
    let (task_id, _item) = accepted_id(&control, "drift-1");
    let c = Connection::open(db.tasks.path()).unwrap();
    c.execute_batch(
        "PRAGMA foreign_keys=OFF; UPDATE effective_heads SET version_id='moved-head' WHERE symbol='sh.600006' AND period='day';",
    )
    .unwrap();
    drop(c);
    let runner = runner::GapVerifyRunner {
        repository: db.clone(),
        run_id: "run".into(),
    };
    let outcome = runner.run(&task_id, &fixture(), Utc::now()).unwrap();
    let runner::GapRunOutcome::NotDecided(task) = outcome else {
        panic!("stale subject must not decide")
    };
    assert!(!task.items[0].outcome.as_ref().unwrap().success);
    assert_eq!(counts(&db), (0, 0));
    assert!(
        db.open_proven_gaps("sh.600006", Period::Day)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn same_request_id_replays_acceptance_without_recomputing_subject() {
    let (_dir, db, control) = task_setup();
    let first = control
        .accept("replay-1", command_key(), gap_payload())
        .unwrap();
    // Move the head: a fresh freeze would now fail or freeze differently.
    let c = Connection::open(db.tasks.path()).unwrap();
    c.execute_batch(
        "PRAGMA foreign_keys=OFF; UPDATE effective_heads SET version_id='moved-head' WHERE symbol='sh.600006' AND period='day';",
    )
    .unwrap();
    drop(c);
    let replay = control
        .accept("replay-1", command_key(), gap_payload())
        .unwrap();
    assert!(replay.reused);
    let RequestOutcome::Task(original) = first.record.outcome else {
        panic!()
    };
    let RequestOutcome::Task(replayed) = replay.record.outcome else {
        panic!()
    };
    assert_eq!(replayed.task.task_id, original.task.task_id);
    assert_eq!(
        replayed.items[0].spec.frozen_inputs,
        original.items[0].spec.frozen_inputs
    );
    // Same request_id with a changed target conflicts.
    let mut changed = gap_payload();
    changed["target"]["end_date"] = json!("2025-01-06");
    assert!(matches!(
        control.accept("replay-1", command_key(), changed),
        Err(crate::task_control::Error::RequestConflict)
    ));
}
#[test]
fn cancelled_task_closes_without_decision() {
    let (_dir, db, control) = task_setup();
    let (task_id, _item) = accepted_id(&control, "cancel-1");
    let snap = db
        .tasks
        .transition(&task_id, 0, "run", Change::RequestCancel)
        .unwrap();
    assert_eq!(snap.task.status, Status::CancelRequested);
    let runner = runner::GapVerifyRunner {
        repository: db.clone(),
        run_id: "run".into(),
    };
    let outcome = runner.run(&task_id, &fixture(), Utc::now()).unwrap();
    let runner::GapRunOutcome::NotDecided(task) = outcome else {
        panic!("cancelled task must not decide")
    };
    assert_eq!(task.task.status, Status::Cancelled);
    assert_eq!(counts(&db), (0, 0));
}
