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
    use x_data_mcp::market_update::tasks::{Operation, RangeContract};
    let ctl = TaskControl::new(
        db.tasks.clone(),
        "range-run".into(),
        vec![
            Box::new(LegacyCollection),
            Box::new(RangeContract {
                operation: Operation::Backfill,
                repository: db.clone(),
                today: || "2026-09-12".parse().unwrap(),
            }),
        ],
    )
    .unwrap();
    let (key, payload) = match expected {
        None => (
            LegacyCollection.key(),
            json!({"symbols":["sh.600006"],"periods":["day"]}),
        ),
        Some(base) => (Operation::Backfill.key(), range_payload(base)),
    };
    let request = ctl.accept(request, key, payload).unwrap();
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

#[test]
fn range_schema_v4_requires_explicit_upgrade_and_preserves_published_records() {
    let f = fixture(false);
    let original = f.db.fixed("legacy-daily").unwrap().raw().clone();
    f.sql("DROP TABLE publication_checks; DROP TABLE stopped_write_checks; DROP TABLE yearly_execution_results; DROP TABLE market_update_results; DELETE FROM schema_migrations WHERE version>=5; PRAGMA user_version=4;");
    assert!(f.db.fixed("legacy-daily").is_err());
    assert!(f.db.range_receipt("absent").is_err());
    let version: u32 = Connection::open(f.db.tasks.path())
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 4);
    f.db.tasks.upgrade().unwrap();
    assert_eq!(f.db.fixed("legacy-daily").unwrap().raw(), &original);
    assert_eq!(f.count("market_update_results"), 0);
    f.db.commit_range(&f.plan()).unwrap();
}

#[test]
fn range_coverage_only_revision_publishes_and_acquisition_only_reuses() {
    let mut f = fixture(false);
    f.batch.provenance.acquired_at += chrono::Duration::days(1);
    f.evidence.candidate_sha256 = market_update::candidate_digest(&f.batch).unwrap();
    let plan = f.plan();
    f.db.commit_range(&plan).unwrap();
    assert_eq!(f.count("data_versions"), 1);
    let stored: String = Connection::open(f.db.tasks.path())
        .unwrap()
        .query_row("SELECT audit_json FROM market_update_results", [], |r| {
            r.get(0)
        })
        .unwrap();
    let audit: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(
        audit["segment"]["provenance"]["acquired_at"],
        serde_json::to_value(f.batch.provenance.acquired_at).unwrap()
    );

    let mut f = fixture(false);
    f.batch.coverage.reason = "fixture: newly verified coverage".into();
    f.evidence.candidate_sha256 = market_update::candidate_digest(&f.batch).unwrap();
    let mut raw = f.db.fixed("legacy-daily").unwrap().raw().clone();
    raw["version_id"] = json!("coverage-only");
    raw["batch"]["coverage"] = serde_json::to_value(&f.batch.coverage).unwrap();
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    f.db.stage(&raw).unwrap();
    f.command.version_id = "coverage-only".into();
    f.whole = Some(CombinedEvidence {
        base_integrity_sha256: f.evidence.base_integrity_sha256.clone(),
        segment_sha256: f.evidence.candidate_sha256.clone(),
        combined_integrity_sha256: raw["integrity_sha256"].as_str().unwrap().into(),
        complete_coverage_ref: "fixture:independent whole coverage".into(),
    });
    let plan = f.plan();
    assert!(!plan.is_no_change());
    f.db.commit_range(&plan).unwrap();
    assert_eq!(f.count("data_versions"), 2);
    assert_eq!(f.head(), "coverage-only");
    assert_eq!(
        f.db.fixed("coverage-only").unwrap().version().records,
        f.db.fixed("legacy-daily").unwrap().version().records
    );
}

fn range_payload(base: &str) -> Value {
    json!({"items":[{"symbol":"sh.600006","base_version_id":base,"start_date":"2025-01-02","end_date":"2025-01-02"}]})
}
fn range_control(f: &Fixture) -> TaskControl<SqliteTasks> {
    use x_data_mcp::market_update::tasks::{Operation, RangeContract};
    TaskControl::new(
        f.db.tasks.clone(),
        "range-run".into(),
        vec![
            Box::new(RangeContract {
                operation: Operation::Backfill,
                repository: f.db.clone(),
                today: || "2026-09-12".parse().unwrap(),
            }),
            Box::new(RangeContract {
                operation: Operation::Update,
                repository: f.db.clone(),
                today: || "2026-09-12".parse().unwrap(),
            }),
        ],
    )
    .unwrap()
}
#[test]
fn explicit_range_admission_normalizes_sets_and_freezes_base_without_requery_on_replay() {
    use x_data_mcp::market_update::tasks::Operation;
    let f = fixture(false);
    let ctl = range_control(&f);
    let mut payload = range_payload("legacy-daily");
    let item = payload["items"][0].clone();
    payload["items"].as_array_mut().unwrap().push(item);
    let accepted = ctl
        .accept("range", Operation::Backfill.key(), payload.clone())
        .unwrap();
    assert!(accepted.reused);
    let RequestOutcome::Task(task) = accepted.record.outcome else {
        panic!()
    };
    assert_eq!(
        task.items[0].spec.frozen_inputs["base_integrity_sha256"],
        f.evidence.base_integrity_sha256
    );
    f.sql("UPDATE daily_bars SET close='10.07' WHERE trade_date='2025-01-02'");
    assert!(
        ctl.accept("range", Operation::Backfill.key(), payload)
            .unwrap()
            .reused
    );
    assert!(
        ctl.accept(
            "new",
            Operation::Backfill.key(),
            range_payload("legacy-daily")
        )
        .is_err()
    );
    assert!(f.db.tasks.read_request("new").unwrap().is_none());
}
#[test]
fn explicit_range_operations_and_different_dates_do_not_evade_request_or_scope_conflicts() {
    use x_data_mcp::{market_update::tasks::Operation, task_control::Error as TaskError};
    let f = fixture(false);
    let ctl = range_control(&f);
    assert!(matches!(
        ctl.accept(
            "range",
            Operation::Update.key(),
            range_payload("legacy-daily")
        ),
        Err(TaskError::RequestConflict)
    ));
    let mut other = range_payload("legacy-daily");
    other["items"][0]["start_date"] = json!("2025-12-31");
    other["items"][0]["end_date"] = json!("2025-12-31");
    assert!(matches!(
        ctl.accept("other", Operation::Backfill.key(), other),
        Err(TaskError::RangeConflict(_))
    ));
    assert!(f.db.tasks.read_request("other").unwrap().is_none());
}
#[test]
fn explicit_range_invalid_inputs_leave_no_request_binding() {
    use x_data_mcp::market_update::tasks::Operation;
    let f = fixture(false);
    let ctl = range_control(&f);
    for kind in 0..5 {
        let mut input = range_payload("legacy-daily");
        match kind {
            0 => input["items"] = json!([]),
            1 => input["items"][0]["extra"] = json!(true),
            2 => input["items"][0]["end_date"] = json!("2024-01-01"),
            3 => input["items"][0]["symbol"] = json!("bj.430047"),
            _ => {
                let mut other = input["items"][0].clone();
                other["base_version_id"] = json!("other");
                input["items"].as_array_mut().unwrap().push(other);
            }
        }
        assert!(
            ctl.accept("invalid", Operation::Backfill.key(), input)
                .is_err()
        );
        assert!(f.db.tasks.read_request("invalid").unwrap().is_none());
    }
}
#[test]
fn range_publish_cannot_widen_the_accepted_frozen_dates() {
    let mut f = fixture(false);
    f.range.start = "2025-01-01".parse().unwrap();
    f.evidence.range = f.range.clone();
    assert!(matches!(f.prepare(), Err(Error::Conflict(_))));
    assert_eq!(f.count("market_update_results"), 0);
}

fn fail_range_without_publication(f: &Fixture) {
    use x_data_mcp::task_control::ItemOutcome;
    let task =
        f.db.tasks
            .transition(
                &f.command.task_id,
                f.command.expected_revision,
                "range-run",
                Change::FinishItem(
                    f.command.item_id.clone(),
                    ItemOutcome {
                        success: false,
                        error: Some("fixture: no publication attempted".into()),
                    },
                ),
            )
            .unwrap();
    f.db.tasks
        .transition(
            &task.task.task_id,
            task.task.state_revision,
            "range-run",
            Change::FinishExecution,
        )
        .unwrap();
}
#[test]
fn explicit_range_same_request_competes_atomically_and_retry_keeps_frozen_inputs() {
    use x_data_mcp::market_update::tasks::{Operation, RangeContract};
    let f = fixture(false);
    let original = f.db.task(&f.command.task_id).unwrap();
    fail_range_without_publication(&f);
    let ctl = range_control(&f);
    let retry = ctl
        .retry(
            &f.command.task_id,
            "retry-range",
            std::collections::BTreeSet::from([f.command.item_id.clone()]),
        )
        .unwrap();
    let RequestOutcome::Task(retry) = retry.record.outcome else {
        panic!()
    };
    assert_eq!(retry.task.key, original.task.key);
    assert_eq!(retry.items[0].spec, original.items[0].spec);
    let cancelled = ctl
        .cancel(&retry.task.task_id, retry.task.state_revision)
        .unwrap();
    f.db.tasks
        .transition(
            &cancelled.task.task_id,
            cancelled.task.state_revision,
            "range-run",
            Change::FinishExecution,
        )
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = (0..2)
        .map(|i| {
            let db = f.db.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let ctl = TaskControl::new(
                    db.tasks.clone(),
                    "range-run".into(),
                    vec![Box::new(RangeContract {
                        operation: Operation::Update,
                        repository: db,
                        today: || "2026-09-12".parse().unwrap(),
                    })],
                )
                .unwrap();
                let mut payload = range_payload("legacy-daily");
                if i == 1 {
                    let duplicate = payload["items"][0].clone();
                    payload["items"].as_array_mut().unwrap().push(duplicate);
                }
                barrier.wait();
                ctl.accept("concurrent", Operation::Update.key(), payload)
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.reused).count(), 1);
    assert_eq!(results[0].record, results[1].record);
    assert_eq!(f.count("scope_occupancy"), 1);
    assert_eq!(f.count("market_update_results"), 0);
}

#[test]
fn range_progress_exposes_frozen_inputs_and_verified_reuse_or_publication() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    for changed in [false, true] {
        let f = fixture(changed);
        let api = DataApi::new(f.db.tasks.clone(), "range-run".into(), Access::local()).unwrap();
        let input = json!({"task_id":f.command.task_id});
        let pending = api.call("get_task_progress", input.clone()).unwrap();
        assert_eq!(
            pending["frozen_daily_ranges"][0]["base_version_id"],
            "legacy-daily"
        );
        assert_eq!(
            pending["frozen_daily_ranges"][0]["base_integrity_sha256"],
            f.evidence.base_integrity_sha256
        );
        assert_eq!(
            pending["frozen_daily_ranges"][0]["start_date"],
            "2025-01-02"
        );
        assert_eq!(pending["range_results"], json!([]));
        f.db.commit_range(&f.plan()).unwrap();
        let completed = api.call("get_task_progress", input.clone()).unwrap();
        assert_eq!(completed["range_results"][0]["no_change"], !changed);
        assert_eq!(
            completed["range_results"][0]["result_version"]["version_id"],
            f.command.version_id
        );
        assert_eq!(
            completed["range_results"][0]["base_version"]["version_id"],
            "legacy-daily"
        );
        assert_eq!(
            completed["range_results"][0]["acquired_at"],
            serde_json::to_value(f.batch.provenance.acquired_at).unwrap()
        );
        assert_eq!(
            completed["publication_versions"].as_array().unwrap().len(),
            usize::from(changed)
        );
        assert_eq!(
            completed["frozen_daily_ranges"],
            pending["frozen_daily_ranges"]
        );
        assert_eq!(completed["unfinished"], json!([]));
        f.sql("DELETE FROM market_update_results");
        assert_eq!(
            api.call("get_task_progress", input).unwrap_err().code,
            "integrity_error"
        );
    }
}
#[test]
fn range_progress_refuses_damaged_audit_and_immutable_base_without_fallback() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    for sql in [
        "UPDATE market_update_results SET receipt_json='{}'",
        "UPDATE daily_bars SET close='10.09' WHERE version_id='legacy-daily' AND trade_date='2025-01-02'",
    ] {
        let f = fixture(false);
        f.db.commit_range(&f.plan()).unwrap();
        let api = DataApi::new(f.db.tasks.clone(), "range-run".into(), Access::local()).unwrap();
        f.sql(sql);
        assert_eq!(
            api.call("get_task_progress", json!({"task_id":f.command.task_id}))
                .unwrap_err()
                .code,
            "integrity_error"
        );
    }
}

#[path = "support/market.rs"]
mod observation_fixture;
#[test]
fn range_progress_cursor_rechecks_evidence_after_partial_task_snapshot() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        market_update::tasks::Operation,
        mcp::ToolHandler,
    };
    let mut f = fixture(false);
    fail_range_without_publication(&f);
    let mut payload = range_payload("legacy-daily");
    for (symbol, version) in [("sh.600007", "other-a"), ("sh.600008", "other-b")] {
        let mut raw = f.db.fixed("legacy-daily").unwrap().raw().clone();
        raw["version_id"] = json!(version);
        raw["symbol"] = json!(symbol);
        raw["batch"]["symbol"] = json!(symbol);
        raw["batch"]["provenance"]["symbol"] = json!(symbol);
        raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
        observation_fixture::published_fixture(&f.db, &raw);
        payload["items"].as_array_mut().unwrap().push(json!({"symbol":symbol,"base_version_id":version,"start_date":"2025-01-02","end_date":"2025-01-02"}));
    }
    let accepted = range_control(&f)
        .accept("paged-range", Operation::Backfill.key(), payload)
        .unwrap();
    let RequestOutcome::Task(task) = accepted.record.outcome else {
        panic!()
    };
    let task =
        f.db.tasks
            .transition(&task.task.task_id, 0, "range-run", Change::Claim)
            .unwrap();
    let item = task
        .items
        .iter()
        .find(|i| i.spec.target["symbol"] == "sh.600006")
        .unwrap()
        .spec
        .item_id
        .clone();
    let task =
        f.db.tasks
            .transition(
                &task.task.task_id,
                task.task.state_revision,
                "range-run",
                Change::StartItem(item.clone()),
            )
            .unwrap();
    f.command.task_id = task.task.task_id;
    f.command.item_id = item;
    f.command.expected_revision = task.task.state_revision;
    f.command.publication_id = "partial-result".into();
    f.db.commit_range(&f.plan()).unwrap();
    let api = DataApi::new(f.db.tasks.clone(), "range-run".into(), Access::local()).unwrap();
    let first = api
        .call(
            "get_task_progress",
            json!({"task_id":f.command.task_id,"page_size":1}),
        )
        .unwrap();
    assert_eq!(first["frozen_daily_ranges"].as_array().unwrap().len(), 3);
    assert_eq!(first["range_results"].as_array().unwrap().len(), 1);
    assert!(first["next_cursor"].is_string());
    assert_eq!(first["succeeded"], 1);
    f.sql("UPDATE market_update_results SET receipt_json='{}'");
    let error = api
        .call(
            "get_task_progress",
            json!({"task_id":f.command.task_id,"page_size":1,"cursor":first["next_cursor"]}),
        )
        .unwrap_err();
    assert_eq!(error.code, "integrity_error");
}

fn runner_fixture() -> Fixture {
    use x_data_mcp::market_update::tasks::Operation;
    let mut f = fixture(false);
    fail_range_without_publication(&f);
    let accepted = range_control(&f)
        .accept(
            "runner",
            Operation::Backfill.key(),
            range_payload("legacy-daily"),
        )
        .unwrap();
    let RequestOutcome::Task(task) = accepted.record.outcome else {
        panic!()
    };
    f.command.task_id = task.task.task_id;
    f.command.item_id = task.items[0].spec.item_id.clone();
    f.command.expected_revision = 0;
    f
}
struct SyntheticRangeProvider {
    changed: bool,
    calls: std::cell::Cell<usize>,
    during_acquisition: Option<Box<dyn Fn()>>,
}
impl SyntheticRangeProvider {
    fn new(changed: bool) -> Self {
        Self {
            changed,
            calls: std::cell::Cell::new(0),
            during_acquisition: None,
        }
    }
}
impl x_data_mcp::market_update::runner::RangeProvider for SyntheticRangeProvider {
    fn acquire(
        &self,
        base: &compat::CheckedRecord,
        range: &DateRange,
        candidate_id: &str,
    ) -> Result<x_data_mcp::market_update::runner::Acquisition, x_data_mcp::market_data::SourceError>
    {
        use x_data_mcp::market_update::runner::{Acquisition, CombinedCandidate};
        self.calls.set(self.calls.get() + 1);
        if let Some(hook) = &self.during_acquisition {
            hook();
        }
        let mut batch: MarketDataBatch =
            serde_json::from_value(base.raw()["batch"].clone()).unwrap();
        batch
            .bars
            .retain(|bar| bar.date >= range.start && bar.date <= range.end);
        if self.changed {
            batch.bars[0].close = "10.06".parse().unwrap();
        }
        let evidence = Evidence {
            base_version_id: base.version().version_id.clone(),
            base_integrity_sha256: base.raw()["integrity_sha256"].as_str().unwrap().into(),
            candidate_sha256: market_update::candidate_digest(&batch).unwrap(),
            range: range.clone(),
            same_qfq_basis_ref: "fixture:runner-basis".into(),
            revision_ref: Some("fixture:runner-revision".into()),
        };
        let combined = if self.changed {
            let mut raw = base.raw().clone();
            raw["version_id"] = json!(candidate_id);
            raw["records"][1]["close"] = json!("10.06");
            raw["batch"]["bars"][1]["close"] = json!("10.06");
            raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
            let proof = CombinedEvidence {
                base_integrity_sha256: evidence.base_integrity_sha256.clone(),
                segment_sha256: evidence.candidate_sha256.clone(),
                combined_integrity_sha256: raw["integrity_sha256"].as_str().unwrap().into(),
                complete_coverage_ref: "fixture:runner-full".into(),
            };
            Some(CombinedCandidate {
                record: raw,
                evidence: proof,
            })
        } else {
            None
        };
        Ok(Acquisition {
            batch,
            evidence,
            combined,
        })
    }
}
#[test]
fn range_runner_executes_and_replays_real_publication_without_reacquiring() {
    use x_data_mcp::market_update::runner::{ItemExecution, RangeRunner};
    for changed in [false, true] {
        let f = runner_fixture();
        let provider = SyntheticRangeProvider::new(changed);
        let runner = RangeRunner {
            repository: f.db.clone(),
            run_id: "range-run".into(),
        };
        let ItemExecution::Published(receipt) = runner
            .run_item(&f.command.task_id, &f.command.item_id, &provider)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(provider.calls.get(), 1);
        assert_eq!(f.count("data_versions"), if changed { 2 } else { 1 });
        let ItemExecution::Published(replayed) = runner
            .run_item(&f.command.task_id, &f.command.item_id, &provider)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(receipt, replayed);
        assert_eq!(provider.calls.get(), 1);
        assert!(f.db.task(&f.command.task_id).unwrap().claims.is_empty());
        assert_eq!(f.count("market_update_results"), 1);
    }
}
#[test]
fn range_runner_does_not_reexecute_running_or_old_run_items() {
    use x_data_mcp::market_update::runner::RangeRunner;
    let f = runner_fixture();
    let provider = SyntheticRangeProvider::new(true);
    let old = RangeRunner {
        repository: f.db.clone(),
        run_id: "new-process".into(),
    };
    assert!(
        old.run_item(&f.command.task_id, &f.command.item_id, &provider)
            .is_err()
    );
    assert_eq!(
        f.db.task(&f.command.task_id).unwrap().task.state_revision,
        0
    );
    let task =
        f.db.tasks
            .transition(&f.command.task_id, 0, "range-run", Change::Claim)
            .unwrap();
    f.db.tasks
        .transition(
            &f.command.task_id,
            task.task.state_revision,
            "range-run",
            Change::StartItem(f.command.item_id.clone()),
        )
        .unwrap();
    let same = RangeRunner {
        repository: f.db.clone(),
        run_id: "range-run".into(),
    };
    assert!(matches!(
        same.run_item(&f.command.task_id, &f.command.item_id, &provider),
        Err(Error::OutcomeUnknown(_))
    ));
    assert_eq!(provider.calls.get(), 0);
    assert_eq!(f.count("scope_occupancy"), 1);
    assert_eq!(f.count("market_update_results"), 0);
}
#[test]
fn range_runner_cancellation_and_official_gate_never_publish() {
    use x_data_mcp::market_update::runner::{ItemExecution, OfficialRangeProvider, RangeRunner};
    let f = runner_fixture();
    let runner = RangeRunner {
        repository: f.db.clone(),
        run_id: "range-run".into(),
    };
    let ItemExecution::NotPublished(task) = runner
        .run_item(
            &f.command.task_id,
            &f.command.item_id,
            &OfficialRangeProvider,
        )
        .unwrap()
    else {
        panic!()
    };
    assert!(
        task.items[0]
            .outcome
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("SOURCE_NOT_VERIFIED")
    );
    assert_eq!(f.count("market_update_results"), 0);
    assert_eq!(f.count("data_versions"), 1);
    for started in [false, true] {
        let f = runner_fixture();
        let runner = RangeRunner {
            repository: f.db.clone(),
            run_id: "range-run".into(),
        };
        let mut provider = SyntheticRangeProvider::new(true);
        if started {
            let db = f.db.clone();
            let task = f.command.task_id.clone();
            provider.during_acquisition = Some(Box::new(move || {
                let snapshot = db.task(&task).unwrap();
                db.tasks
                    .transition(
                        &task,
                        snapshot.task.state_revision,
                        "range-run",
                        Change::RequestCancel,
                    )
                    .unwrap();
            }));
        } else {
            range_control(&f).cancel(&f.command.task_id, 0).unwrap();
        }
        let ItemExecution::NotPublished(task) = runner
            .run_item(&f.command.task_id, &f.command.item_id, &provider)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            task.task.status,
            x_data_mcp::task_control::Status::Cancelled
        );
        assert!(task.claims.is_empty());
        if started {
            assert_eq!(
                task.task.result.as_ref().unwrap()["cancellation_acknowledged"],
                true
            );
            let connection = Connection::open(f.db.tasks.path()).unwrap();
            connection.execute("UPDATE tasks SET result_json=json_remove(result_json,'$.cancellation_acknowledged') WHERE task_id=?1",[&f.command.task_id]).unwrap();
            assert!(f.db.task(&f.command.task_id).is_err());
        }
        assert_eq!(provider.calls.get(), usize::from(started));
        assert_eq!(f.count("market_update_results"), 0);
        assert_eq!(f.count("data_versions"), 1);
    }
}

#[path = "support/range_response_fault.rs"]
mod response_fault;
#[test]
fn range_runner_reconciles_lost_commit_response_and_preserves_unconfirmed_execution() {
    use x_data_mcp::market_update::runner::{ItemExecution, RangeRunner};
    for commit_first in [false, true] {
        let f = runner_fixture();
        let provider = SyntheticRangeProvider::new(true);
        let runner = RangeRunner {
            repository: response_fault::ResponseFault {
                db: f.db.clone(),
                commit_first,
                fired: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            run_id: "range-run".into(),
        };
        let result = runner.run_item(&f.command.task_id, &f.command.item_id, &provider);
        assert_eq!(provider.calls.get(), 1);
        if commit_first {
            let ItemExecution::Published(receipt) = result.unwrap() else {
                panic!()
            };
            assert_eq!(
                f.db.item_receipt(&f.command.task_id, &f.command.item_id)
                    .unwrap(),
                Some(receipt)
            );
            assert!(f.db.task(&f.command.task_id).unwrap().claims.is_empty());
        } else {
            assert!(matches!(result, Err(Error::OutcomeUnknown(_))));
            assert_eq!(
                f.db.task(&f.command.task_id).unwrap().task.status,
                x_data_mcp::task_control::Status::NeedsVerification
            );
            assert_eq!(f.count("scope_occupancy"), 1);
            assert_eq!(f.count("market_update_results"), 0);
        }
        let _ = runner.run_item(&f.command.task_id, &f.command.item_id, &provider);
        assert_eq!(provider.calls.get(), 1);
    }
}

#[test]
fn public_range_request_to_fixture_execution_and_verified_query_is_connected() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        market_update::runner::{ItemExecution, RangeRunner},
        mcp::ToolHandler,
    };
    for changed in [false, true] {
        let f = fixture(false);
        fail_range_without_publication(&f);
        let api = DataApi::new(f.db.tasks.clone(), "range-run".into(), Access::local()).unwrap();
        let accepted=api.call("backfill_data",json!({"request_id":"fixture-e2e","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"legacy-daily","start_date":"2025-01-02","end_date":"2025-01-02"}]})).unwrap();
        assert_eq!(accepted["readiness"], "blocked");
        // Explicit test composition only: no JSON/MCP flag can select this provider.
        let provider = SyntheticRangeProvider::new(changed);
        let runner = RangeRunner {
            repository: f.db.clone(),
            run_id: "range-run".into(),
        };
        let ItemExecution::Published(receipt) = runner
            .run_item(
                accepted["task_id"].as_str().unwrap(),
                accepted["item_ids"][0].as_str().unwrap(),
                &provider,
            )
            .unwrap()
        else {
            panic!()
        };
        let progress = api
            .call("get_task_progress", json!({"task_id":accepted["task_id"]}))
            .unwrap();
        assert_eq!(progress["succeeded"], 1);
        assert_eq!(
            progress["range_results"][0]["result_version"]["version_id"],
            receipt.version_id
        );
        let page=api.call("query_data",json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},"filter":{"kind":"daily","start_date":"2025-01-02","end_date":"2025-01-02"},"version":{"kind":"latest_per_entity"}})).unwrap();
        assert_eq!(
            page["records"][0]["close"],
            if changed { "10.06" } else { "10.05" }
        );
        assert_eq!(
            page["version_manifest"][0]["version_ref"]["version_id"],
            receipt.version_id
        );
        assert_eq!(provider.calls.get(), 1);
    }
}

#[test]
fn range_cancel_ack_requires_stopped_items_and_does_not_rewrite_terminals() {
    let f = runner_fixture();
    let t =
        f.db.tasks
            .transition(&f.command.task_id, 0, "range-run", Change::Claim)
            .unwrap();
    let t =
        f.db.tasks
            .transition(
                &t.task.task_id,
                t.task.state_revision,
                "range-run",
                Change::StartItem(f.command.item_id.clone()),
            )
            .unwrap();
    let t =
        f.db.tasks
            .transition(
                &t.task.task_id,
                t.task.state_revision,
                "range-run",
                Change::RequestCancel,
            )
            .unwrap();
    assert!(
        f.db.tasks
            .transition(
                &t.task.task_id,
                t.task.state_revision,
                "range-run",
                Change::FinishCancellation
            )
            .is_err()
    );
    assert_eq!(f.count("scope_occupancy"), 1);
    let completed = fixture(false);
    completed.db.commit_range(&completed.plan()).unwrap();
    let terminal = completed.db.task(&completed.command.task_id).unwrap();
    assert!(
        completed
            .db
            .tasks
            .transition(
                &terminal.task.task_id,
                terminal.task.state_revision,
                "range-run",
                Change::FinishCancellation
            )
            .is_err()
    );
    assert_eq!(completed.db.task(&terminal.task.task_id).unwrap(), terminal);
}

#[test]
fn publication_facts_verify_new_and_reused_versions_without_mutating_tasks() {
    use x_data_mcp::publication_verification::{Evidence, EvidenceRepository};
    for changed in [false, true] {
        let f = fixture(changed);
        let before = f.db.task(&f.command.task_id).unwrap();
        let missing = f.db.publication_facts(&f.command.task_id).unwrap();
        assert_eq!(missing.task, before);
        assert!(matches!(missing.items[0].evidence, Evidence::Missing));
        let receipt = f.db.commit_range(&f.plan()).unwrap();
        let after = f.db.task(&f.command.task_id).unwrap();
        let facts = f.db.publication_facts(&f.command.task_id).unwrap();
        assert_eq!(facts.task, after);
        let Evidence::Committed {
            receipt: actual,
            reused_version,
            ..
        } = &facts.items[0].evidence
        else {
            panic!("committed evidence required")
        };
        assert_eq!(actual, &receipt);
        assert_eq!(*reused_version, !changed);
        assert_eq!(f.db.task(&f.command.task_id).unwrap(), after);
        assert_eq!(f.head(), f.command.version_id);
    }
}

#[test]
fn publication_facts_reject_missing_success_receipt_and_damaged_fixed_data() {
    use x_data_mcp::publication_verification::EvidenceRepository;
    for damage in [
        "DELETE FROM market_update_results",
        "UPDATE market_update_results SET result_sha256='broken'",
        "UPDATE daily_bars SET close='999'",
    ] {
        let f = fixture(false);
        f.db.commit_range(&f.plan()).unwrap();
        Connection::open(f.db.tasks.path())
            .unwrap()
            .execute(damage, [])
            .unwrap();
        assert!(
            f.db.publication_facts(&f.command.task_id).is_err(),
            "{damage}"
        );
    }
}

#[test]
fn publication_commit_and_uncertainty_marker_compete_on_the_same_revision() {
    use std::sync::{Arc, Barrier};
    use x_data_mcp::task_control::Status;
    for changed in [false, true] {
        let f = fixture(changed);
        let plan = f.plan();
        let task = f.db.task(&f.command.task_id).unwrap();
        let other = SqliteTasks::new(f.db.tasks.path())
            .with_busy_timeout(std::time::Duration::from_secs(5));
        let gate = Arc::new(Barrier::new(2));
        let go = gate.clone();
        let id = task.task.task_id.clone();
        let item = f.command.item_id.clone();
        let marker = std::thread::spawn(move || {
            go.wait();
            other.transition(
                &id,
                task.task.state_revision,
                "range-run",
                Change::RequireVerification {
                    item_id: item,
                    reason: "commit outcome uncertain".into(),
                },
            )
        });
        gate.wait();
        let published = f.db.commit_range(&plan);
        let marked = marker.join().unwrap();
        match (published, marked) {
            (Ok(receipt), Err(x_data_mcp::task_control::Error::RevisionConflict)) => {
                assert_eq!(
                    f.db.range_receipt(&f.command.publication_id).unwrap(),
                    Some(receipt)
                );
                assert!(f.db.task(&f.command.task_id).unwrap().claims.is_empty());
            }
            (Err(Error::Conflict(_)), Ok(task)) => {
                assert_eq!(task.task.status, Status::NeedsVerification);
                assert!(!task.claims.is_empty());
                assert!(task.execution_uncertainty().unwrap().is_some());
                assert!(
                    f.db.range_receipt(&f.command.publication_id)
                        .unwrap()
                        .is_none()
                );
                assert_eq!(f.head(), "legacy-daily");
            }
            other => panic!("exactly one revision-conditional outcome must win: {other:?}"),
        }
    }
}

#[test]
fn publication_decision_records_insufficiency_or_confirmed_commit_without_changing_original() {
    use x_data_mcp::publication_verification::{
        EvidenceRepository,
        admission::{PublicationVerification, command_key},
        decision::{Conclusion, prepare},
    };
    let f = fixture(true);
    let control = TaskControl::new(
        f.db.tasks.clone(),
        "inspection-run".into(),
        vec![Box::new(
            PublicationVerification::new(f.db.tasks.clone(), "inspection-run".into()).unwrap(),
        )],
    )
    .unwrap();
    let accepted = control
        .accept(
            "inspect",
            command_key(),
            json!({"target_task_id":f.command.task_id}),
        )
        .unwrap();
    let RequestOutcome::Task(check) = accepted.record.outcome else {
        panic!()
    };
    let check =
        f.db.tasks
            .transition(&check.task.task_id, 0, "inspection-run", Change::Claim)
            .unwrap();
    let check =
        f.db.tasks
            .transition(
                &check.task.task_id,
                check.task.state_revision,
                "inspection-run",
                Change::StartItem("publication-results".into()),
            )
            .unwrap();
    let original = f.db.task(&f.command.task_id).unwrap();
    let insufficient = prepare(
        check.clone(),
        f.db.publication_facts(&f.command.task_id).unwrap(),
        "inspection-run",
        "test-owner",
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(
        insufficient.receipt().conclusion,
        Conclusion::InsufficientEvidence
    );
    assert!(insufficient.finished_check().claims.is_empty());
    assert_eq!(f.db.task(&f.command.task_id).unwrap(), original);
    assert!(!original.claims.is_empty());
    f.db.commit_range(&f.plan()).unwrap();
    let published = f.db.task(&f.command.task_id).unwrap();
    let facts = f.db.publication_facts(&f.command.task_id).unwrap();
    let confirmed = prepare(
        check.clone(),
        facts.clone(),
        "inspection-run",
        "test-owner",
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(confirmed.receipt().conclusion, Conclusion::AllRecorded);
    assert_eq!(f.db.task(&f.command.task_id).unwrap(), published);
    let mut incomplete = facts;
    incomplete.items.clear();
    assert!(
        prepare(
            check,
            incomplete,
            "inspection-run",
            "test-owner",
            chrono::Utc::now()
        )
        .is_err()
    );
}

fn running_publication_check(f: &Fixture, request: &str) -> x_data_mcp::task_control::TaskSnapshot {
    use x_data_mcp::publication_verification::admission::{PublicationVerification, command_key};
    let ctl = TaskControl::new(
        f.db.tasks.clone(),
        "inspection-run".into(),
        vec![Box::new(
            PublicationVerification::new(f.db.tasks.clone(), "inspection-run".into()).unwrap(),
        )],
    )
    .unwrap();
    let RequestOutcome::Task(check) = ctl
        .accept(
            request,
            command_key(),
            json!({"target_task_id":f.command.task_id}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let check =
        f.db.tasks
            .transition(&check.task.task_id, 0, "inspection-run", Change::Claim)
            .unwrap();
    f.db.tasks
        .transition(
            &check.task.task_id,
            check.task.state_revision,
            "inspection-run",
            Change::StartItem("publication-results".into()),
        )
        .unwrap()
}
#[test]
fn publication_check_audit_is_atomic_replayable_and_never_mutates_original() {
    use x_data_mcp::publication_verification::{
        EvidenceRepository,
        decision::{AuditStore, Conclusion, prepare},
    };
    for published in [false, true] {
        let f = fixture(true);
        let check = running_publication_check(&f, "audit-check");
        if published {
            f.db.commit_range(&f.plan()).unwrap();
        }
        let original = f.db.task(&f.command.task_id).unwrap();
        let plan = prepare(
            check.clone(),
            f.db.publication_facts(&f.command.task_id).unwrap(),
            "inspection-run",
            "test-owner",
            chrono::Utc::now(),
        )
        .unwrap();
        let c = Connection::open(f.db.tasks.path()).unwrap();
        c.execute_batch("CREATE TRIGGER refuse_check BEFORE INSERT ON publication_checks BEGIN SELECT RAISE(ABORT,'injected audit failure'); END").unwrap();
        assert!(f.db.commit_check(&plan).is_err());
        assert_eq!(
            f.db.tasks.read_task(&check.task.task_id).unwrap().unwrap(),
            check
        );
        assert_eq!(f.db.task(&f.command.task_id).unwrap(), original);
        assert!(f.db.check_receipt(&check.task.task_id).unwrap().is_none());
        c.execute_batch("DROP TRIGGER refuse_check").unwrap();
        let receipt = f.db.commit_check(&plan).unwrap();
        assert_eq!(
            receipt.conclusion,
            if published {
                Conclusion::AllRecorded
            } else {
                Conclusion::InsufficientEvidence
            }
        );
        assert_eq!(f.db.commit_check(&plan).unwrap(), receipt);
        assert_eq!(
            f.db.check_receipt(&check.task.task_id).unwrap(),
            Some(receipt)
        );
        assert_eq!(f.db.task(&f.command.task_id).unwrap(), original);
        assert!(
            f.db.tasks
                .read_task(&check.task.task_id)
                .unwrap()
                .unwrap()
                .claims
                .is_empty()
        );
        c.execute("UPDATE publication_checks SET audit_sha256='damaged'", [])
            .unwrap();
        assert!(f.db.check_receipt(&check.task.task_id).is_err());
        assert!(f.db.tasks.read_task(&check.task.task_id).is_err());
    }
}
#[test]
fn publication_check_rejects_changed_target_or_cancelled_check_before_recording() {
    use x_data_mcp::publication_verification::{
        EvidenceRepository,
        decision::{AuditStore, prepare},
    };
    for cancel in [false, true] {
        let f = fixture(true);
        let check = running_publication_check(&f, "stale-check");
        let plan = prepare(
            check.clone(),
            f.db.publication_facts(&f.command.task_id).unwrap(),
            "inspection-run",
            "test-owner",
            chrono::Utc::now(),
        )
        .unwrap();
        if cancel {
            f.db.tasks
                .transition(
                    &check.task.task_id,
                    check.task.state_revision,
                    "inspection-run",
                    Change::RequestCancel,
                )
                .unwrap();
        } else {
            f.db.commit_range(&f.plan()).unwrap();
        }
        let original = f.db.task(&f.command.task_id).unwrap();
        assert!(matches!(f.db.commit_check(&plan), Err(Error::Conflict(_))));
        assert_eq!(f.count("publication_checks"), 0);
        assert_eq!(f.db.task(&f.command.task_id).unwrap(), original);
    }
}

#[test]
fn publication_check_rejects_changed_evidence_even_when_receipt_and_task_are_unchanged() {
    use x_data_mcp::publication_verification::{
        EvidenceRepository,
        decision::{AuditStore, prepare},
    };
    let f = fixture(false);
    let check = running_publication_check(&f, "fingerprint-check");
    f.db.commit_range(&f.plan()).unwrap();
    let original = f.db.task(&f.command.task_id).unwrap();
    let facts = f.db.publication_facts(&f.command.task_id).unwrap();
    let plan = prepare(
        check.clone(),
        facts,
        "inspection-run",
        "test-owner",
        chrono::Utc::now(),
    )
    .unwrap();
    let c = Connection::open(f.db.tasks.path()).unwrap();
    let (raw, receipt): (String, String) = c
        .query_row(
            "SELECT audit_json,receipt_json FROM market_update_results",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let mut audit: Value = serde_json::from_str(&raw).unwrap();
    audit["inspection_annotation"] = json!("updated after prepare");
    let raw = audit.to_string();
    let digest = x_data_mcp::hash::sha256(json!([raw, receipt]).to_string().as_bytes());
    c.execute(
        "UPDATE market_update_results SET audit_json=?1,result_sha256=?2",
        rusqlite::params![raw, digest],
    )
    .unwrap();
    assert!(f.db.publication_facts(&f.command.task_id).is_ok());
    assert_eq!(f.db.task(&f.command.task_id).unwrap(), original);
    assert!(matches!(f.db.commit_check(&plan), Err(Error::Conflict(_))));
    assert_eq!(f.count("publication_checks"), 0);
    assert_eq!(
        f.db.tasks.read_task(&check.task.task_id).unwrap().unwrap(),
        check
    );
}

#[test]
fn publication_check_schema_upgrade_is_explicit_and_preserves_published_facts() {
    use x_data_mcp::publication_verification::EvidenceRepository;
    let f = fixture(true);
    f.db.commit_range(&f.plan()).unwrap();
    let before = f.db.publication_facts(&f.command.task_id).unwrap();
    // Remove only the new v8 migration from this isolated fixture. The retained
    // v1-v7 DDL and checksums are exactly those used by the deployed schema7.
    f.sql("DROP TABLE publication_checks; DELETE FROM schema_migrations WHERE version=8; PRAGMA user_version=7;");
    assert!(f.db.publication_facts(&f.command.task_id).is_err());
    let c = Connection::open(f.db.tasks.path()).unwrap();
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        7
    );
    f.db.tasks.upgrade().unwrap();
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        x_data_mcp::sqlite_tasks::SCHEMA_VERSION
    );
    assert_eq!(f.db.publication_facts(&f.command.task_id).unwrap(), before);
    assert_eq!(f.count("publication_checks"), 0);
}

fn pending_publication_check(f: &Fixture, id: &str) -> x_data_mcp::task_control::TaskSnapshot {
    use x_data_mcp::publication_verification::admission::{PublicationVerification, command_key};
    let control = TaskControl::new(
        f.db.tasks.clone(),
        "inspection-run".into(),
        vec![Box::new(
            PublicationVerification::new(f.db.tasks.clone(), "inspection-run".into()).unwrap(),
        )],
    )
    .unwrap();
    let RequestOutcome::Task(check) = control
        .accept(
            id,
            command_key(),
            json!({"target_task_id":f.command.task_id}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    *check
}
#[test]
fn publication_runner_records_actual_conclusion_and_replays_it_without_target_mutation() {
    use x_data_mcp::publication_verification::{
        decision::Conclusion,
        runner::{Outcome, Runner},
    };
    for published in [false, true] {
        let f = fixture(true);
        let check = pending_publication_check(&f, "runner-check");
        if published {
            f.db.commit_range(&f.plan()).unwrap();
        }
        let original = f.db.task(&f.command.task_id).unwrap();
        let runner = Runner {
            tasks: f.db.tasks.clone(),
            evidence: f.db.clone(),
            run_id: "inspection-run".into(),
            actor: "test-owner".into(),
        };
        let Outcome::Completed(first) = runner.run(&check.task.task_id).unwrap() else {
            panic!()
        };
        assert_eq!(
            first.conclusion,
            if published {
                Conclusion::AllRecorded
            } else {
                Conclusion::InsufficientEvidence
            }
        );
        let Outcome::Completed(repeated) = runner.run(&check.task.task_id).unwrap() else {
            panic!()
        };
        assert_eq!(first, repeated);
        assert_eq!(f.db.task(&f.command.task_id).unwrap(), original);
        if !published {
            f.db.commit_range(&f.plan()).unwrap();
            let Outcome::Completed(history) = runner.run(&check.task.task_id).unwrap() else {
                panic!()
            };
            assert_eq!(history, first);
        }
    }
}
#[test]
fn publication_runner_cancels_only_pending_check_and_does_not_reexecute_old_or_running_checks() {
    use x_data_mcp::publication_verification::runner::{Outcome, Runner};
    for mode in ["cancel", "old", "running"] {
        let f = fixture(true);
        let mut check = pending_publication_check(&f, "runner-guard");
        let original = f.db.task(&f.command.task_id).unwrap();
        if mode == "cancel" {
            check =
                f.db.tasks
                    .transition(
                        &check.task.task_id,
                        0,
                        "inspection-run",
                        Change::RequestCancel,
                    )
                    .unwrap();
        }
        if mode == "running" {
            check =
                f.db.tasks
                    .transition(&check.task.task_id, 0, "inspection-run", Change::Claim)
                    .unwrap();
            check =
                f.db.tasks
                    .transition(
                        &check.task.task_id,
                        check.task.state_revision,
                        "inspection-run",
                        Change::StartItem("publication-results".into()),
                    )
                    .unwrap();
        }
        let runner = Runner {
            tasks: f.db.tasks.clone(),
            evidence: f.db.clone(),
            run_id: if mode == "old" {
                "another-run".into()
            } else {
                "inspection-run".into()
            },
            actor: "test-owner".into(),
        };
        let outcome = runner.run(&check.task.task_id);
        if mode == "cancel" {
            assert!(matches!(outcome, Ok(Outcome::Cancelled(_))));
        } else {
            assert!(matches!(outcome, Err(Error::OutcomeUnknown(_))));
            assert_eq!(
                f.db.tasks.read_task(&check.task.task_id).unwrap().unwrap(),
                check
            );
        }
        assert_eq!(f.db.task(&f.command.task_id).unwrap(), original);
        assert_eq!(f.count("publication_checks"), 0);
    }
}

#[path = "support/publication_check_fault.rs"]
mod publication_check_fault;
#[test]
fn publication_runner_reconciles_response_loss_cancellation_and_bounded_conflicts() {
    use publication_check_fault::{Fault, Mode};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use x_data_mcp::publication_verification::runner::{Outcome, Runner};
    for mode in [
        Mode::CancelRead,
        Mode::CancelCommit,
        Mode::LostBefore,
        Mode::LostAfter,
        Mode::ChangeTarget,
        Mode::ConflictTwice,
    ] {
        let f = fixture(true);
        let check = pending_publication_check(&f, "fault-check");
        let before = f.db.task(&f.command.task_id).unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let commits = Arc::new(AtomicUsize::new(0));
        let runner = Runner {
            tasks: f.db.tasks.clone(),
            evidence: Fault {
                db: f.db.clone(),
                check_id: check.task.task_id.clone(),
                mode,
                reads: reads.clone(),
                commits: commits.clone(),
            },
            run_id: "inspection-run".into(),
            actor: "test-owner".into(),
        };
        let result = runner.run(&check.task.task_id);
        match mode {
            Mode::CancelRead | Mode::CancelCommit => {
                assert!(matches!(result, Ok(Outcome::Cancelled(_))))
            }
            Mode::LostAfter | Mode::ChangeTarget => {
                assert!(matches!(result, Ok(Outcome::Completed(_))))
            }
            Mode::LostBefore => assert!(matches!(result, Err(Error::OutcomeUnknown(_)))),
            Mode::ConflictTwice => assert!(matches!(result, Err(Error::Conflict(_)))),
        }
        let task = f.db.tasks.read_task(&check.task.task_id).unwrap().unwrap();
        assert_eq!(
            task.claims.is_empty(),
            !matches!(mode, Mode::LostBefore | Mode::ConflictTwice)
        );
        if !matches!(mode, Mode::ChangeTarget | Mode::ConflictTwice) {
            assert_eq!(f.db.task(&f.command.task_id).unwrap(), before);
        } else {
            assert_eq!(reads.load(Ordering::SeqCst), 2);
            assert_eq!(commits.load(Ordering::SeqCst), 2);
        }
        let count = reads.load(Ordering::SeqCst);
        let _ = runner.run(&check.task.task_id);
        assert_eq!(reads.load(Ordering::SeqCst), count);
        assert_eq!(f.head(), "legacy-daily");
    }
}
