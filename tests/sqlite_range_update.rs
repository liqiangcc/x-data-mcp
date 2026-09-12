use rusqlite::Connection;
use serde_json::{Value, json};
use x_data_mcp::{
    market_data::{MarketDataBatch, Period},
    market_storage::{
        Error, MarketRepository, Publication, PublishIntent, Publisher, Range, Scope, compat,
    },
    market_update::{
        self, DateRange, Evidence, Reason,
        publication::{CombinedEvidence, Preparation, RangePlan, RangeRepository},
    },
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
};
struct Fixture {
    _dir: tempfile::TempDir,
    db: SqliteMarket,
    command: Publication,
    batch: MarketDataBatch,
    range: DateRange,
    evidence: Evidence,
    whole: Option<CombinedEvidence>,
}
fn command(db: &SqliteMarket, request: &str, version: &str, expected: Option<&str>) -> Publication {
    // Legacy fixture contract supplies the existing item identity and real occupancy.
    // Public update/backfill normalization is a separate pending integration.
    let ctl = TaskControl::new(
        db.tasks.clone(),
        "range-run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let request = ctl
        .accept(
            request,
            LegacyCollection.key(),
            json!({"symbols":["sh.600006"],"periods":["day"]}),
        )
        .unwrap();
    let RequestOutcome::Task(task) = request.record.outcome else {
        panic!()
    };
    let task = db
        .tasks
        .transition(&task.task.task_id, 0, "range-run", Change::Claim)
        .unwrap();
    let task = db
        .tasks
        .transition(
            &task.task.task_id,
            task.task.state_revision,
            "range-run",
            Change::StartItem(task.items[0].spec.item_id.clone()),
        )
        .unwrap();
    Publication {
        publication_id: format!("result-{}", task.task.task_id),
        version_id: version.into(),
        task_id: task.task.task_id,
        item_id: task.items[0].spec.item_id.clone(),
        run_id: "range-run".into(),
        expected_revision: task.task.state_revision,
        intent: PublishIntent::Daily {
            expected_head: expected.map(str::to_owned),
        },
    }
}
fn fixture(changed: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteMarket::new(SqliteTasks::new(dir.path().join("range.sqlite")));
    db.tasks.initialize().unwrap();
    let samples: Value =
        serde_json::from_str(include_str!("fixtures/issue79/legacy-records.json")).unwrap();
    let base = samples[0]["record"].clone();
    db.stage(&base).unwrap();
    let publisher = Publisher {
        repository: db.clone(),
    };
    let first = publisher
        .prepare(command(&db, "first", "legacy-daily", None))
        .unwrap();
    db.commit(&first).unwrap();
    let mut batch: MarketDataBatch = serde_json::from_value(base["batch"].clone()).unwrap();
    batch.bars = vec![batch.bars[1].clone()];
    if changed {
        batch.bars[0].close = "10.06".parse().unwrap();
    }
    let range = DateRange {
        start: batch.bars[0].date,
        end: batch.bars[0].date,
    };
    let evidence = Evidence {
        base_version_id: "legacy-daily".into(),
        base_integrity_sha256: base["integrity_sha256"].as_str().unwrap().into(),
        candidate_sha256: market_update::candidate_digest(&batch).unwrap(),
        range: range.clone(),
        same_qfq_basis_ref: "fixture:basis".into(),
        revision_ref: Some("fixture:revision".into()),
    };
    let whole = if changed {
        let mut raw = base.clone();
        raw["version_id"] = json!("updated-daily");
        raw["records"][1]["close"] = json!("10.06");
        raw["batch"]["bars"][1]["close"] = json!("10.06");
        raw["batch"]["coverage"]["reason"] =
            json!("fixture: independently checked combined coverage");
        raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
        db.stage(&raw).unwrap();
        Some(CombinedEvidence {
            base_integrity_sha256: evidence.base_integrity_sha256.clone(),
            segment_sha256: evidence.candidate_sha256.clone(),
            combined_integrity_sha256: raw["integrity_sha256"].as_str().unwrap().into(),
            complete_coverage_ref: "fixture:whole-range".into(),
        })
    } else {
        None
    };
    let command = command(
        &db,
        "range",
        if changed {
            "updated-daily"
        } else {
            "legacy-daily"
        },
        Some("legacy-daily"),
    );
    Fixture {
        _dir: dir,
        db,
        command,
        batch,
        range,
        evidence,
        whole,
    }
}
impl Fixture {
    fn prepare(&self) -> Result<Preparation, Error> {
        Publisher {
            repository: self.db.clone(),
        }
        .prepare_range(
            self.command.clone(),
            &self.batch,
            &self.range,
            &self.evidence,
            self.whole.as_ref(),
        )
    }
    fn plan(&self) -> Box<RangePlan> {
        let Preparation::Ready(plan) = self.prepare().unwrap() else {
            panic!("expected approval")
        };
        plan
    }
    fn sql(&self, sql: &str) {
        Connection::open(self.db.tasks.path())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
    fn count(&self, table: &str) -> i64 {
        Connection::open(self.db.tasks.path())
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn head(&self) -> String {
        self.db
            .range(
                &Scope {
                    symbol: "sh.600006".into(),
                    period: Period::Day,
                },
                None,
                &Range {
                    start: "2024-01-01".parse().unwrap(),
                    end: "2026-12-31".parse().unwrap(),
                    after: None,
                    limit: 10,
                },
            )
            .unwrap()
            .version
            .version_id
    }
}
#[test]
fn range_changed_and_no_change_commit_real_sqlite_and_replay() {
    for changed in [false, true] {
        let f = fixture(changed);
        let before = f.db.fixed("legacy-daily").unwrap();
        let plan = f.plan();
        assert_eq!(plan.is_no_change(), !changed);
        let receipt = f.db.commit_range(&plan).unwrap();
        assert_eq!(
            f.db.range_receipt(&f.command.publication_id).unwrap(),
            Some(receipt.clone())
        );
        assert_eq!(f.db.commit_range(&plan).unwrap(), receipt);
        assert_eq!(f.count("data_versions"), if changed { 2 } else { 1 });
        assert_eq!(f.count("publication_batches"), if changed { 2 } else { 1 });
        assert_eq!(f.count("task_publications"), if changed { 2 } else { 1 });
        assert_eq!(f.count("market_update_results"), 1);
        assert_eq!(f.head(), f.command.version_id);
        assert!(f.db.task(&f.command.task_id).unwrap().claims.is_empty());
        assert_eq!(f.db.fixed("legacy-daily").unwrap().raw(), before.raw());
        let after = f.db.fixed(&f.command.version_id).unwrap();
        assert_eq!(after.version().records[0], before.version().records[0]);
        assert_eq!(after.version().records[2], before.version().records[2]);
        assert_eq!(
            after.version().records[1].close.to_string(),
            if changed { "10.06" } else { "10.05" }
        );
    }
}
#[test]
fn range_result_write_failure_rolls_back_publication_and_task() {
    for changed in [false, true] {
        let f = fixture(changed);
        let plan = f.plan();
        let before = f.db.task(&f.command.task_id).unwrap();
        f.sql("CREATE TRIGGER reject_result BEFORE INSERT ON market_update_results BEGIN SELECT RAISE(ABORT,'injected'); END;");
        assert!(f.db.commit_range(&plan).is_err());
        assert_eq!(f.head(), "legacy-daily");
        assert_eq!(f.count("publication_batches"), 1);
        assert_eq!(f.count("task_publications"), 1);
        assert_eq!(f.count("market_update_results"), 0);
        assert_eq!(f.db.task(&f.command.task_id).unwrap(), before);
        if changed {
            assert!(f.db.fixed("updated-daily").is_err());
        }
        f.sql("DROP TRIGGER reject_result");
        f.db.commit_range(&plan).unwrap();
    }
}
#[test]
fn range_head_and_task_revision_changes_never_rebase() {
    for changed in [false, true] {
        for revision in [false, true] {
            let f = fixture(changed);
            let plan = f.plan();
            if revision {
                f.db.tasks
                    .transition(
                        &f.command.task_id,
                        f.command.expected_revision,
                        "range-run",
                        Change::RequestCancel,
                    )
                    .unwrap();
            } else {
                f.sql("DELETE FROM effective_heads");
            }
            let before = f.db.task(&f.command.task_id).unwrap();
            assert!(matches!(f.db.commit_range(&plan), Err(Error::Conflict(_))));
            assert_eq!(f.count("market_update_results"), 0);
            assert_eq!(f.count("publication_batches"), 1);
            assert_eq!(f.db.task(&f.command.task_id).unwrap(), before);
        }
    }
}
#[test]
fn range_base_and_candidate_tampering_reject_before_publish() {
    for sql in [
        "UPDATE daily_bars SET close='10.07' WHERE version_id='legacy-daily' AND trade_date='2025-01-02'",
        "UPDATE daily_bars SET close='10.07' WHERE version_id='updated-daily' AND trade_date='2025-01-02'",
    ] {
        let f = fixture(true);
        let plan = f.plan();
        f.sql(sql);
        assert!(matches!(f.db.commit_range(&plan), Err(Error::Integrity(_))));
        assert_eq!(f.count("market_update_results"), 0);
        assert_eq!(f.count("publication_batches"), 1);
    }
}
#[test]
fn range_full_coverage_evidence_cannot_be_inferred_from_segment() {
    let mut f = fixture(true);
    f.whole = None;
    assert!(matches!(
        f.prepare().unwrap(),
        Preparation::NeedsVerification(Reason::CoverageUnknown)
    ));
    let mut f = fixture(true);
    f.whole.as_mut().unwrap().combined_integrity_sha256 = "other".into();
    assert!(matches!(
        f.prepare().unwrap(),
        Preparation::NeedsVerification(Reason::CoverageUnknown)
    ));
    assert_eq!(f.count("market_update_results"), 0);
}
#[test]
fn range_candidate_must_preserve_outside_rows_even_with_bound_whole_evidence() {
    let mut f = fixture(true);
    // A valid, digest-consistent whole record still cannot change the outside range.
    let mut raw = f.db.candidate("updated-daily").unwrap().raw().clone();
    raw["version_id"] = json!("wrong-merge");
    raw["records"][0]["close"] = json!("10.07");
    raw["batch"]["bars"][0]["close"] = json!("10.07");
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    f.db.stage(&raw).unwrap();
    f.command.version_id = "wrong-merge".into();
    f.whole.as_mut().unwrap().combined_integrity_sha256 =
        raw["integrity_sha256"].as_str().unwrap().into();
    assert!(matches!(f.prepare(), Err(Error::Invalid(_))));
}
#[test]
fn range_receipt_refuses_corrupt_result_or_fixed_version() {
    for sql in [
        "UPDATE market_update_results SET receipt_json=json_set(receipt_json,'$.task_revision',999)",
        "UPDATE market_update_results SET audit_json=json_set(audit_json,'$.no_change',0)",
        "UPDATE market_update_results SET no_change=0",
        "UPDATE daily_bars SET close='10.07' WHERE trade_date='2025-01-02'",
    ] {
        let f = fixture(false);
        f.db.commit_range(&f.plan()).unwrap();
        f.sql(sql);
        assert!(matches!(
            f.db.range_receipt(&f.command.publication_id),
            Err(Error::Integrity(_))
        ));
    }
}
#[test]
fn range_needs_verification_and_no_change_do_not_fabricate_new_versions() {
    let mut f = fixture(false);
    f.evidence.same_qfq_basis_ref.clear();
    assert!(matches!(
        f.prepare().unwrap(),
        Preparation::NeedsVerification(Reason::BasisUnknown)
    ));
    assert_eq!(f.count("data_versions"), 1);
    let mut f = fixture(false);
    f.command.version_id = "new-but-unchanged".into();
    assert!(matches!(f.prepare(), Err(Error::Invalid(_))));
    assert_eq!(f.count("data_versions"), 1);
}
#[test]
fn range_result_id_replay_compares_all_evidence_inputs() {
    let mut f = fixture(false);
    let plan = f.plan();
    f.evidence.same_qfq_basis_ref = "fixture:different-evidence".into();
    let different = f.plan();
    f.db.commit_range(&plan).unwrap();
    assert!(matches!(
        f.db.commit_range(&different),
        Err(Error::Conflict(_))
    ));
}
#[test]
fn range_candidates_compete_on_the_same_real_task_and_head() {
    let f = fixture(true);
    let first = f.plan();
    let mut command = f.command.clone();
    command.publication_id = "competing-result".into();
    let Preparation::Ready(second) = (Publisher {
        repository: f.db.clone(),
    })
    .prepare_range(command, &f.batch, &f.range, &f.evidence, f.whole.as_ref())
    .unwrap() else {
        panic!()
    };
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = [first, second]
        .into_iter()
        .map(|plan| {
            let db = f.db.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                db.commit_range(&plan)
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| matches!(e, Error::Conflict(_)))
    );
    assert_eq!(f.count("market_update_results"), 1);
    assert_eq!(f.count("publication_batches"), 2);
}
