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
        runner::CombinedCandidate,
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
    whole: Option<CombinedCandidate>,
}
fn command(db: &SqliteMarket, request: &str, version: &str, expected: Option<&str>) -> Publication {
    command_for_range(db, request, version, expected, ("2025-01-02", "2025-01-02"))
}
fn command_for_range(
    db: &SqliteMarket,
    request: &str,
    version: &str,
    expected: Option<&str>,
    range: (&str, &str),
) -> Publication {
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
        Some(base) => (
            Operation::Backfill.key(),
            json!({"items":[{
                "symbol":"sh.600006",
                "base_version_id":base,
                "start_date":range.0,
                "end_date":range.1
            }]}),
        ),
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
        serde_json::from_str(include_str!("../fixtures/issue79/legacy-records.json")).unwrap();
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
        continuity: None,
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
        Some(CombinedCandidate {
            record: raw.clone(),
            evidence: CombinedEvidence {
                base_integrity_sha256: evidence.base_integrity_sha256.clone(),
                segment_sha256: evidence.candidate_sha256.clone(),
                combined_integrity_sha256: raw["integrity_sha256"].as_str().unwrap().into(),
                complete_coverage_ref: "fixture:whole-range".into(),
            },
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
        "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','10.07') WHERE trade_date='2025-01-02' AND segment_id IN (SELECT segment_id FROM manifest_nodes WHERE segment_id IS NOT NULL AND node_id IN (WITH RECURSIVE t(i) AS (SELECT manifest_root FROM data_versions WHERE version_id='legacy-daily' UNION ALL SELECT n.left_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.left_id IS NOT NULL UNION ALL SELECT n.right_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.right_id IS NOT NULL) SELECT i FROM t))",
        "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','10.07') WHERE trade_date='2025-01-02' AND segment_id IN (SELECT segment_id FROM manifest_nodes WHERE segment_id IS NOT NULL AND node_id IN (WITH RECURSIVE t(i) AS (SELECT manifest_root FROM data_versions WHERE version_id='updated-daily' UNION ALL SELECT n.left_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.left_id IS NOT NULL UNION ALL SELECT n.right_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.right_id IS NOT NULL) SELECT i FROM t))",
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
    f.whole.as_mut().unwrap().evidence.combined_integrity_sha256 = "other".into();
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
    f.whole.as_mut().unwrap().record = raw.clone();
    f.whole.as_mut().unwrap().evidence.combined_integrity_sha256 =
        raw["integrity_sha256"].as_str().unwrap().into();
    assert!(matches!(f.prepare(), Err(Error::Invalid(_))));
}
#[test]
fn range_receipt_refuses_corrupt_result_or_fixed_version() {
    for sql in [
        "UPDATE market_update_results SET receipt_json=json_set(receipt_json,'$.task_revision',999)",
        "UPDATE market_update_results SET audit_json=json_set(audit_json,'$.no_change',0)",
        "UPDATE market_update_results SET no_change=0",
        "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','10.07') WHERE trade_date='2025-01-02'",
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
            // The loser's Immediate transaction must wait out the winner's
            // commit to reach the app-level conflict check; the 250ms
            // default can expire under CI load (observed flake, #193).
            let db = SqliteMarket::new(
                SqliteTasks::new(f.db.tasks.path())
                    .with_busy_timeout(std::time::Duration::from_secs(5)),
            );
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
    let compat = observation_fixture::stored_compat(&f.db);
    f.sql("CREATE TABLE daily_bars AS WITH RECURSIVE t(version_id,i) AS (SELECT version_id,manifest_root FROM data_versions WHERE manifest_root IS NOT NULL UNION ALL SELECT t.version_id,n.left_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.left_id IS NOT NULL UNION ALL SELECT t.version_id,n.right_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.right_id IS NOT NULL) SELECT t.version_id,s.trade_date,json_extract(s.bar_json,'$.open') AS open,json_extract(s.bar_json,'$.high') AS high,json_extract(s.bar_json,'$.low') AS low,json_extract(s.bar_json,'$.close') AS close,json_extract(s.bar_json,'$.volume') AS volume,json_extract(s.bar_json,'$.amount') AS amount FROM t JOIN manifest_nodes m ON m.node_id=t.i AND m.segment_id IS NOT NULL JOIN segment_bars s ON s.segment_id=m.segment_id AND s.trade_date>=m.clip_start AND s.trade_date<=m.clip_end; DROP TABLE verification_inspections; DROP TABLE verification_runtime_guards; DROP TABLE publication_checks; DROP TABLE stopped_write_checks; DROP TABLE yearly_execution_results; DROP TABLE market_update_results; DROP TABLE task_item_execution_starts; DROP TABLE gap_coverings; DROP TABLE proven_gaps; DROP TABLE gap_verification_decisions; DROP TABLE IF EXISTS version_segments; DROP TABLE IF EXISTS manifest_nodes; DROP TABLE segment_bars; DROP TABLE bar_segments; DROP TABLE payloads; ALTER TABLE data_versions RENAME COLUMN envelope_json TO compat_json; ALTER TABLE data_versions DROP COLUMN manifest_root; ALTER TABLE data_versions DROP COLUMN evidence_digest; ALTER TABLE tasks DROP COLUMN item_count; ALTER TABLE tasks DROP COLUMN items_pending; ALTER TABLE tasks DROP COLUMN items_running; ALTER TABLE tasks DROP COLUMN items_finished; ALTER TABLE tasks DROP COLUMN items_succeeded; ALTER TABLE tasks DROP COLUMN evidence_digest; DELETE FROM schema_migrations WHERE version>=5; PRAGMA user_version=4;");
    observation_fixture::restore_compat(&f.db, &compat);
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
    f.whole = Some(CombinedCandidate {
        record: raw.clone(),
        evidence: CombinedEvidence {
            base_integrity_sha256: f.evidence.base_integrity_sha256.clone(),
            segment_sha256: f.evidence.candidate_sha256.clone(),
            combined_integrity_sha256: raw["integrity_sha256"].as_str().unwrap().into(),
            complete_coverage_ref: "fixture:independent whole coverage".into(),
        },
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
    f.sql("UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','10.07') WHERE trade_date='2025-01-02'");
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
                        evidence: None,
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
        "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','10.09') WHERE trade_date='2025-01-02' AND segment_id IN (SELECT segment_id FROM manifest_nodes WHERE segment_id IS NOT NULL AND node_id IN (WITH RECURSIVE t(i) AS (SELECT manifest_root FROM data_versions WHERE version_id='legacy-daily' UNION ALL SELECT n.left_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.left_id IS NOT NULL UNION ALL SELECT n.right_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.right_id IS NOT NULL) SELECT i FROM t))",
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

#[path = "../support/market.rs"]
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
    seen_ranges: std::cell::RefCell<Vec<DateRange>>,
    fail_symbol: Option<String>,
}
impl SyntheticRangeProvider {
    fn new(changed: bool) -> Self {
        Self {
            changed,
            calls: std::cell::Cell::new(0),
            during_acquisition: None,
            seen_ranges: std::cell::RefCell::new(vec![]),
            fail_symbol: None,
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
        self.seen_ranges.borrow_mut().push(range.clone());
        if self.fail_symbol.as_deref() == Some(base.version().symbol.as_str()) {
            return Err(x_data_mcp::market_data::SourceError::NotVerified(
                "fixture: acquisition failed for this symbol".into(),
            ));
        }
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
            continuity: None,
        };
        let combined = if self.changed {
            let mut raw = base.raw().clone();
            raw["version_id"] = json!(candidate_id);
            // Mutate the same bar in the full record as in the segment.
            let mutated_date = batch.bars[0].date.to_string();
            let index = raw["batch"]["bars"]
                .as_array()
                .unwrap()
                .iter()
                .position(|b| b["date"] == mutated_date)
                .unwrap();
            raw["records"][index]["close"] = json!("10.06");
            raw["batch"]["bars"][index]["close"] = json!("10.06");
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

#[path = "../support/range_response_fault.rs"]
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
        assert_eq!(accepted["runtime_readiness"], "blocked");
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
        "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','999')",
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
    // Remove the migrations added after the deployed schema7 from this isolated
    // fixture. The retained v1-v7 DDL and checksums are exactly those used by
    // the deployed schema7.
    let compat = observation_fixture::stored_compat(&f.db);
    f.sql("CREATE TABLE daily_bars AS WITH RECURSIVE t(version_id,i) AS (SELECT version_id,manifest_root FROM data_versions WHERE manifest_root IS NOT NULL UNION ALL SELECT t.version_id,n.left_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.left_id IS NOT NULL UNION ALL SELECT t.version_id,n.right_id FROM manifest_nodes n JOIN t ON n.node_id=t.i WHERE n.right_id IS NOT NULL) SELECT t.version_id,s.trade_date,json_extract(s.bar_json,'$.open') AS open,json_extract(s.bar_json,'$.high') AS high,json_extract(s.bar_json,'$.low') AS low,json_extract(s.bar_json,'$.close') AS close,json_extract(s.bar_json,'$.volume') AS volume,json_extract(s.bar_json,'$.amount') AS amount FROM t JOIN manifest_nodes m ON m.node_id=t.i AND m.segment_id IS NOT NULL JOIN segment_bars s ON s.segment_id=m.segment_id AND s.trade_date>=m.clip_start AND s.trade_date<=m.clip_end; CREATE TABLE payloads_bak AS SELECT * FROM payloads; DROP TABLE verification_inspections; DROP TABLE verification_runtime_guards; DROP TABLE publication_checks; DROP TABLE task_item_execution_starts; DROP TABLE gap_coverings; DROP TABLE proven_gaps; DROP TABLE gap_verification_decisions; DROP TABLE IF EXISTS version_segments; DROP TABLE IF EXISTS manifest_nodes; DROP TABLE segment_bars; DROP TABLE bar_segments; DROP TABLE payloads; ALTER TABLE data_versions RENAME COLUMN envelope_json TO compat_json; ALTER TABLE data_versions DROP COLUMN manifest_root; ALTER TABLE data_versions DROP COLUMN evidence_digest; ALTER TABLE tasks DROP COLUMN item_count; ALTER TABLE tasks DROP COLUMN items_pending; ALTER TABLE tasks DROP COLUMN items_running; ALTER TABLE tasks DROP COLUMN items_finished; ALTER TABLE tasks DROP COLUMN items_succeeded; ALTER TABLE tasks DROP COLUMN evidence_digest; DELETE FROM schema_migrations WHERE version>=8; PRAGMA user_version=7;");
    observation_fixture::restore_compat(&f.db, &compat);
    assert!(f.db.publication_facts(&f.command.task_id).is_err());
    let c = Connection::open(f.db.tasks.path()).unwrap();
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        7
    );
    f.db.tasks.upgrade().unwrap();
    f.sql("INSERT OR IGNORE INTO payloads SELECT * FROM payloads_bak; DROP TABLE payloads_bak;");
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        x_data_mcp::sqlite_tasks::SCHEMA_VERSION
    );
    assert_eq!(f.db.publication_facts(&f.command.task_id).unwrap(), before);
    assert_eq!(f.count("publication_checks"), 0);
}

#[test]
fn range_audit_segment_payload_missing_fails_closed() {
    use x_data_mcp::publication_verification::EvidenceRepository;
    let f = fixture(true);
    f.db.commit_range(&f.plan()).unwrap();
    f.db.publication_facts(&f.command.task_id).unwrap();
    let c = Connection::open(f.db.tasks.path()).unwrap();
    let payloads: i64 = c
        .query_row("SELECT count(*) FROM payloads", [], |r| r.get(0))
        .unwrap();
    assert!(payloads >= 1);
    f.sql("DELETE FROM payloads");
    assert!(f.db.publication_facts(&f.command.task_id).is_err());
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

#[path = "../support/publication_check_fault.rs"]
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

#[test]
fn publication_runner_read_integrity_failure_marks_check_unresolved_without_false_receipt() {
    use x_data_mcp::publication_verification::{decision::AuditStore, runner::Runner};
    let f = fixture(true);
    let check = pending_publication_check(&f, "bad-evidence-check");
    f.db.commit_range(&f.plan()).unwrap();
    let original = f.db.task(&f.command.task_id).unwrap();
    Connection::open(f.db.tasks.path())
        .unwrap()
        .execute(
            "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','999')",
            [],
        )
        .unwrap();
    let runner = Runner {
        tasks: f.db.tasks.clone(),
        evidence: f.db.clone(),
        run_id: "inspection-run".into(),
        actor: "test-owner".into(),
    };
    assert!(matches!(
        runner.run(&check.task.task_id),
        Err(Error::Integrity(_))
    ));
    let unresolved = f.db.tasks.read_task(&check.task.task_id).unwrap().unwrap();
    assert_eq!(
        unresolved.task.status,
        x_data_mcp::task_control::Status::NeedsVerification
    );
    assert!(unresolved.execution_uncertainty().unwrap().is_some());
    assert!(!unresolved.claims.is_empty());
    assert!(f.db.check_receipt(&check.task.task_id).unwrap().is_none());
    assert_eq!(f.db.task(&f.command.task_id).unwrap(), original);
    assert!(matches!(
        runner.run(&check.task.task_id),
        Err(Error::OutcomeUnknown(_))
    ));
}

#[test]
fn known_publication_request_replays_without_new_task_and_rejects_damaged_evidence() {
    use x_data_mcp::publication_verification::{
        admission::PublicationVerification,
        existing,
        runner::{Outcome, Runner},
    };
    let f = fixture(true);
    let check = pending_publication_check(&f, "known-check");
    f.db.commit_range(&f.plan()).unwrap();
    let Outcome::Completed(receipt) = (Runner {
        tasks: f.db.tasks.clone(),
        evidence: f.db.clone(),
        run_id: "inspection-run".into(),
        actor: "test-owner".into(),
    })
    .run(&check.task.task_id)
    .unwrap() else {
        panic!()
    };
    let control = TaskControl::new(
        f.db.tasks.clone(),
        "inspection-run".into(),
        vec![Box::new(
            PublicationVerification::new(f.db.tasks.clone(), "inspection-run".into()).unwrap(),
        )],
    )
    .unwrap();
    let original = f.db.task(&f.command.task_id).unwrap();
    let counts = || {
        let c = Connection::open(f.db.tasks.path()).unwrap();
        [
            "tasks",
            "task_items",
            "scope_occupancy",
            "publication_checks",
        ]
        .map(|table| {
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        })
    };
    let before = counts();
    let accepted = existing::accept(&control, "known-request", &f.command.task_id).unwrap();
    let RequestOutcome::Terminal(value) = &accepted.record.outcome else {
        panic!()
    };
    let result: existing::TerminalResult = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(result.receipt(), &receipt);
    let replay = existing::accept(&control, "known-request", &f.command.task_id).unwrap();
    assert_eq!(replay.record, accepted.record);
    assert!(matches!(
        existing::accept(&control, "known-request", "another-task"),
        Err(x_data_mcp::task_control::Error::RequestConflict)
    ));
    assert_eq!(counts(), before);
    assert_eq!(f.db.task(&f.command.task_id).unwrap(), original);
    Connection::open(f.db.tasks.path())
        .unwrap()
        .execute(
            "UPDATE publication_checks SET audit_sha256='damaged' WHERE verification_task_id=?1",
            [&check.task.task_id],
        )
        .unwrap();
    assert!(f.db.tasks.read_request("known-request").is_err());
    assert!(existing::accept(&control, "new-known-request", &f.command.task_id).is_err());
    assert!(
        f.db.tasks
            .read_request("new-known-request")
            .unwrap()
            .is_none()
    );
    assert_eq!(counts(), before);
}

#[test]
fn known_publication_two_connections_share_one_binding_and_failed_insert_leaves_none() {
    use x_data_mcp::publication_verification::{
        admission::PublicationVerification, existing, runner::Runner,
    };
    let f = fixture(true);
    let check = pending_publication_check(&f, "known-race-check");
    f.db.commit_range(&f.plan()).unwrap();
    Runner {
        tasks: f.db.tasks.clone(),
        evidence: f.db.clone(),
        run_id: "inspection-run".into(),
        actor: "test-owner".into(),
    }
    .run(&check.task.task_id)
    .unwrap();
    let original = f.db.task(&f.command.task_id).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let db =
                f.db.tasks
                    .clone()
                    .with_busy_timeout(std::time::Duration::from_secs(5));
            let barrier = barrier.clone();
            let target = f.command.task_id.clone();
            std::thread::spawn(move || {
                let control = TaskControl::new(
                    db.clone(),
                    "inspection-run".into(),
                    vec![Box::new(
                        PublicationVerification::new(db, "inspection-run".into()).unwrap(),
                    )],
                )
                .unwrap();
                barrier.wait();
                existing::accept(&control, "known-race", &target).unwrap()
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results[0].record, results[1].record);
    assert_eq!(results.iter().filter(|r| !r.reused).count(), 1);
    let c = Connection::open(f.db.tasks.path()).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM requests WHERE request_id='known-race'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    c.execute_batch("CREATE TRIGGER reject_known_request BEFORE INSERT ON requests WHEN NEW.request_id='known-failure' BEGIN SELECT RAISE(ABORT,'fixture reject binding'); END;").unwrap();
    let control = TaskControl::new(
        f.db.tasks.clone(),
        "inspection-run".into(),
        vec![Box::new(
            PublicationVerification::new(f.db.tasks.clone(), "inspection-run".into()).unwrap(),
        )],
    )
    .unwrap();
    assert!(existing::accept(&control, "known-failure", &f.command.task_id).is_err());
    assert!(f.db.tasks.read_request("known-failure").unwrap().is_none());
    c.execute_batch("DROP TRIGGER reject_known_request")
        .unwrap();
    assert!(matches!(
        existing::accept(&control, "known-failure", &f.command.task_id)
            .unwrap()
            .record
            .outcome,
        RequestOutcome::Terminal(_)
    ));
    assert_eq!(f.db.task(&f.command.task_id).unwrap(), original);
}

// ---- #147 Slice 4: default-range task execution + gap covering ----
mod default_runner {
    use super::*;
    use x_data_mcp::{
        catalog::{
            CatalogSnapshot, CompletenessEvidence, Exchange, IndustryInfo, SecurityRecord,
            SecurityType,
        },
        data_supply::default_range::DefaultRangeContract,
        market_storage::ProvenGap,
        market_update::{
            runner::{ItemExecution, RangeRunner},
            tasks::Operation,
        },
        sqlite_catalog::SqliteCatalog,
    };
    const DAY: &str = "2026-01-15";
    fn today() -> chrono::NaiveDate {
        DAY.parse().unwrap()
    }
    fn publish_catalog(db: &SqliteMarket, codes: &[&str]) {
        let records = codes
            .iter()
            .map(|code| SecurityRecord {
                symbol: (*code).into(),
                name: (*code).into(),
                exchange: Exchange::Shanghai,
                security_type: SecurityType::AShare,
                source_security_id: (*code).into(),
                industry: IndustryInfo::Missing {
                    reason: "fixture".into(),
                },
                source: "fixture:catalog".into(),
                acquired_at: chrono::Utc::now(),
            })
            .collect::<Vec<_>>();
        SqliteCatalog::new(db.tasks.clone())
            .publish_fixture(
                "catalog-1",
                CatalogSnapshot {
                    source: "fixture:catalog".into(),
                    acquired_at: chrono::Utc::now(),
                    completeness: CompletenessEvidence::Complete {
                        expected_count: records.len(),
                    },
                    records,
                },
                None,
            )
            .unwrap();
    }
    /// Real legacy collection + publish: writes the execution-start marker
    /// and promotes `daily-{code}` to the effective qfq daily head.
    fn publish_daily(db: &SqliteMarket, ctl: &TaskControl<SqliteTasks>, code: &str, seq: usize) {
        let symbol = format!("sh.{code}");
        let version_id = format!("daily-{code}");
        let samples: Value =
            serde_json::from_str(include_str!("../fixtures/issue79/legacy-records.json")).unwrap();
        let mut raw = samples[0]["record"].clone();
        raw["version_id"] = json!(version_id);
        raw["symbol"] = json!(symbol);
        raw["batch"]["symbol"] = json!(symbol);
        raw["batch"]["provenance"]["symbol"] = json!(symbol);
        raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
        db.stage(&raw).unwrap();
        let accepted = ctl
            .accept(
                &format!("req-{version_id}"),
                LegacyCollection.key(),
                json!({"symbols":[symbol],"periods":["day"]}),
            )
            .unwrap();
        let RequestOutcome::Task(s) = accepted.record.outcome else {
            panic!()
        };
        let s = db
            .tasks
            .transition(&s.task.task_id, 0, "range-run", Change::Claim)
            .unwrap();
        let s = db
            .tasks
            .transition(
                &s.task.task_id,
                s.task.state_revision,
                "range-run",
                Change::StartItem(s.items[0].spec.item_id.clone()),
            )
            .unwrap();
        let plan = Publisher {
            repository: db.clone(),
        }
        .prepare(Publication {
            publication_id: format!("pub-{version_id}-{seq}"),
            version_id,
            task_id: s.task.task_id,
            item_id: s.items[0].spec.item_id.clone(),
            run_id: "range-run".into(),
            expected_revision: s.task.state_revision,
            intent: PublishIntent::Daily {
                expected_head: None,
            },
        })
        .unwrap();
        db.commit(&plan).unwrap();
    }
    fn gap(id: &str, symbol: &str, start: &str, end: &str) -> ProvenGap {
        ProvenGap {
            gap_id: id.into(),
            symbol: symbol.into(),
            period: Period::Day,
            start_date: start.parse().unwrap(),
            end_date: end.parse().unwrap(),
            evidence_kind: "gap_verification_decision".into(),
            evidence_id: format!("evidence-{id}"),
            evidence_digest: "e".repeat(64),
            observed_at: "2026-09-15T00:00:00Z".parse().unwrap(),
        }
    }
    struct DefaultFixture {
        _dir: tempfile::TempDir,
        db: SqliteMarket,
        ctl: TaskControl<SqliteTasks>,
        task_id: String,
        items: Vec<String>,
    }
    /// Catalog + published daily for `codes`, recorded `gaps`, then a real
    /// `selection {"kind":"default"}` acceptance for `op`.
    fn default_fixture(
        op: Operation,
        codes: &[&str],
        gaps: &[(&str, &str, &str, &str)],
    ) -> DefaultFixture {
        let dir = tempfile::tempdir().unwrap();
        let tasks = SqliteTasks::new(dir.path().join("range.sqlite"));
        tasks.initialize().unwrap();
        let db = SqliteMarket::new(tasks.clone());
        publish_catalog(&db, codes);
        let ctl = TaskControl::new(
            tasks,
            "range-run".into(),
            vec![
                Box::new(LegacyCollection),
                Box::new(DefaultRangeContract {
                    operation: Operation::Update,
                    resolver: db.clone(),
                    today,
                }),
                Box::new(DefaultRangeContract {
                    operation: Operation::Backfill,
                    resolver: db.clone(),
                    today,
                }),
            ],
        )
        .unwrap();
        for (i, code) in codes.iter().enumerate() {
            publish_daily(&db, &ctl, code, i);
        }
        for (id, symbol, start, end) in gaps {
            db.record_proven_gap(&gap(id, symbol, start, end)).unwrap();
        }
        let key = DefaultRangeContract {
            operation: op,
            resolver: db.clone(),
            today,
        }
        .key();
        let accepted = ctl
            .accept("req-default", key, json!({"selection":{"kind":"default"}}))
            .unwrap();
        let RequestOutcome::Task(snap) = accepted.record.outcome else {
            panic!()
        };
        DefaultFixture {
            _dir: dir,
            db,
            ctl,
            task_id: snap.task.task_id,
            items: snap.items.iter().map(|i| i.spec.item_id.clone()).collect(),
        }
    }
    fn runner(db: &SqliteMarket) -> RangeRunner<SqliteMarket> {
        RangeRunner {
            repository: db.clone(),
            run_id: "range-run".into(),
        }
    }
    #[test]
    fn default_update_executes_union_covers_frozen_gaps_and_replays() {
        use x_data_mcp::data_supply::{Access, mcp::DataApi};
        use x_data_mcp::mcp::ToolHandler;
        for changed in [true, false] {
            let f = default_fixture(
                Operation::Update,
                &["600006"],
                &[
                    ("g1", "sh.600006", "2025-02-01", "2025-02-28"),
                    ("g2", "sh.600006", "2024-01-01", "2024-06-01"),
                ],
            );
            let provider = SyntheticRangeProvider::new(changed);
            let result = runner(&f.db)
                .run_item(&f.task_id, &f.items[0], &provider)
                .unwrap();
            let ItemExecution::Published(receipt) = result else {
                let ItemExecution::NotPublished(t) = result else {
                    unreachable!()
                };
                panic!("item outcome: {:?}", t.items[0].outcome)
            };
            // One union acquisition/commit for refresh + both gap ranges.
            assert_eq!(provider.calls.get(), 1);
            let seen = provider.seen_ranges.borrow();
            assert_eq!(
                *seen,
                vec![DateRange {
                    start: "2024-01-01".parse().unwrap(),
                    end: DAY.parse().unwrap()
                }]
            );
            drop(seen);
            assert_eq!(
                receipt.version_id != "daily-600006",
                changed,
                "changed run must publish a new version"
            );
            // Both frozen gaps are covered in full by the committed union.
            for gap_id in ["g1", "g2"] {
                let bindings = f.db.gap_coverings(gap_id).unwrap();
                assert_eq!(bindings.len(), 1);
                assert_eq!(bindings[0].version_id, receipt.version_id);
            }
            assert!(
                f.db.open_proven_gaps("sh.600006", Period::Day)
                    .unwrap()
                    .is_empty()
            );
            // Availability no longer reports either gap.
            let api =
                DataApi::new(f.db.tasks.clone(), "range-run".into(), Access::local()).unwrap();
            let availability = api
                .call(
                    "get_data_availability",
                    json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},"filter":{"kind":"daily","start_date":"2024-01-01","end_date":"2026-01-15"},"version":{"kind":"latest_per_entity"},"page_size":1}),
                )
                .unwrap();
            assert!(
                availability["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|i| i["confirmed_gaps"].as_array().unwrap().is_empty())
            );
            // Progress projection still exposes frozen ranges + one result.
            let progress = api
                .call(
                    "get_task_progress",
                    json!({"task_id":f.task_id,"page_size":10}),
                )
                .unwrap();
            assert_eq!(progress["frozen_daily_ranges"].as_array().unwrap().len(), 3);
            assert_eq!(progress["range_results"].as_array().unwrap().len(), 1);
            assert_eq!(progress["succeeded"], 1);
            // Replay: same receipt, no re-acquisition, no duplicate bindings.
            let ItemExecution::Published(replayed) = runner(&f.db)
                .run_item(&f.task_id, &f.items[0], &provider)
                .unwrap()
            else {
                panic!()
            };
            assert_eq!(receipt, replayed);
            assert_eq!(provider.calls.get(), 1);
            for gap_id in ["g1", "g2"] {
                assert_eq!(f.db.gap_coverings(gap_id).unwrap().len(), 1);
            }
        }
    }
    #[test]
    fn default_backfill_executes_gap_ranges_only() {
        let f = default_fixture(
            Operation::Backfill,
            &["600006"],
            &[
                ("g1", "sh.600006", "2025-01-02", "2025-02-28"),
                ("g2", "sh.600006", "2024-01-01", "2024-12-31"),
            ],
        );
        let provider = SyntheticRangeProvider::new(true);
        let ItemExecution::Published(receipt) = runner(&f.db)
            .run_item(&f.task_id, &f.items[0], &provider)
            .unwrap()
        else {
            panic!()
        };
        // Union of the two gap ranges only; no refresh range is fetched.
        let seen = provider.seen_ranges.borrow();
        assert_eq!(
            *seen,
            vec![DateRange {
                start: "2024-01-01".parse().unwrap(),
                end: "2025-02-28".parse().unwrap()
            }]
        );
        assert_ne!(receipt.version_id, "daily-600006");
        for gap_id in ["g1", "g2"] {
            assert_eq!(
                f.db.gap_coverings(gap_id).unwrap()[0].version_id,
                receipt.version_id
            );
        }
        assert!(
            f.db.open_proven_gaps("sh.600006", Period::Day)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn gap_not_in_frozen_required_ranges_is_never_covered() {
        let f = default_fixture(
            Operation::Update,
            &["600006"],
            &[("g1", "sh.600006", "2025-02-01", "2025-02-28")],
        );
        // g3 is recorded after acceptance: inside the committed union but
        // not part of this item's frozen required ranges.
        f.db.record_proven_gap(&gap("g3", "sh.600006", "2025-03-01", "2025-03-10"))
            .unwrap();
        let provider = SyntheticRangeProvider::new(true);
        let ItemExecution::Published(_) = runner(&f.db)
            .run_item(&f.task_id, &f.items[0], &provider)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(f.db.gap_coverings("g1").unwrap().len(), 1);
        assert!(f.db.gap_coverings("g3").unwrap().is_empty());
        let open = f.db.open_proven_gaps("sh.600006", Period::Day).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].gap_id, "g3");
    }
    #[test]
    fn failed_required_range_leaves_item_incomplete_and_other_items_continue() {
        let f = default_fixture(
            Operation::Backfill,
            &["600006", "600007"],
            &[
                ("g1", "sh.600006", "2025-01-02", "2025-02-28"),
                ("g2", "sh.600007", "2025-03-01", "2025-03-10"),
            ],
        );
        assert_eq!(f.items.len(), 2);
        let mut provider = SyntheticRangeProvider::new(true);
        provider.fail_symbol = Some("sh.600007".into());
        // First item publishes and covers its frozen gap.
        let ItemExecution::Published(receipt) = runner(&f.db)
            .run_item(&f.task_id, &f.items[0], &provider)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            f.db.gap_coverings("g1").unwrap()[0].version_id,
            receipt.version_id
        );
        // Second item's acquisition fails: item finishes failed, gap open.
        let ItemExecution::NotPublished(task) = runner(&f.db)
            .run_item(&f.task_id, &f.items[1], &provider)
            .unwrap()
        else {
            panic!()
        };
        let failed = task
            .items
            .iter()
            .find(|i| i.spec.item_id == f.items[1])
            .unwrap();
        assert!(!failed.outcome.as_ref().unwrap().success);
        assert!(
            failed
                .outcome
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("SOURCE_NOT_VERIFIED")
        );
        assert!(f.db.gap_coverings("g2").unwrap().is_empty());
        assert_eq!(
            f.db.open_proven_gaps("sh.600007", Period::Day).unwrap()[0].gap_id,
            "g2"
        );
        assert!(
            f.db.item_receipt(&f.task_id, &f.items[1])
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn malformed_default_frozen_shape_fails_closed_without_commit() {
        let f = default_fixture(
            Operation::Backfill,
            &["600006"],
            &[("g1", "sh.600006", "2025-02-01", "2025-02-28")],
        );
        // Corrupt the frozen shape: the stored item spec diverges from the
        // task's frozen work, so the snapshot consistency gate fails closed
        // before any acquisition.
        let c = Connection::open(f.db.tasks.path()).unwrap();
        c.execute(
            "UPDATE task_items SET frozen_inputs_json=json_set(frozen_inputs_json,'$.bogus',1) WHERE item_id=?1",
            [&f.items[0]],
        )
        .unwrap();
        drop(c);
        let provider = SyntheticRangeProvider::new(true);
        let err = runner(&f.db)
            .run_item(&f.task_id, &f.items[0], &provider)
            .unwrap_err();
        assert!(err.to_string().contains("inconsistent"), "{err}");
        assert_eq!(provider.calls.get(), 0);
        assert!(f.db.gap_coverings("g1").unwrap().is_empty());
    }
    #[test]
    fn frozen_inputs_projection_and_shape_stay_stable() {
        let f = default_fixture(
            Operation::Update,
            &["600006"],
            &[("g1", "sh.600006", "2025-02-01", "2025-02-28")],
        );
        let snap = f.ctl.repository.read_task(&f.task_id).unwrap().unwrap();
        let frozen = &snap.items[0].spec.frozen_inputs;
        assert_eq!(frozen["base_version_id"], "daily-600006");
        assert!(frozen["selection_evidence"]["evidence_digest"].is_string());
        assert_eq!(frozen["refresh_range"]["end_date"], serde_json::json!(DAY));
        assert_eq!(frozen["proven_gap_ranges"][0]["gap_id"], "g1");
        assert!(frozen["proven_gap_ranges"][0]["evidence_digest"].is_string());
    }
    /// #262 regression: the resolved default selection (securities plus
    /// per-symbol start evidence) must be stored once per task, not copied
    /// into every item's frozen inputs — and again inside the task row's
    /// frozen work. At the 100-item budget that duplication is ~100x the
    /// evidence per task; at market scale (~5,221 item-updates/day) it is
    /// ~150MB/day against the ≤16MiB hard cap.
    #[test]
    fn default_selection_evidence_is_stored_once_and_item_markers_bind() {
        let owned: Vec<String> = (0..100).map(|i| format!("60{i:04}")).collect();
        let codes: Vec<&str> = owned.iter().map(String::as_str).collect();
        let f = default_fixture(Operation::Update, &codes, &[]);
        let snap = f.ctl.repository.read_task(&f.task_id).unwrap().unwrap();
        let shared = snap.task.work.shared_evidence.as_ref().unwrap();
        let shared_len = serde_json::to_string(shared).unwrap().len();
        assert!(shared_len > 10_000, "fixture selection must be non-trivial");
        for item in &snap.items {
            let frozen_len = serde_json::to_string(&item.spec.frozen_inputs)
                .unwrap()
                .len();
            assert!(
                frozen_len < 2_000,
                "item frozen inputs must carry only the digest marker, got {frozen_len}B"
            );
            let marker = item.spec.frozen_inputs["selection_evidence"]["evidence_digest"]
                .as_str()
                .unwrap();
            assert_eq!(marker, shared["evidence_digest"].as_str().unwrap());
        }
        // The task row embeds one shared copy, not the evidence per item.
        let work_len = serde_json::to_string(&snap.task.work).unwrap().len();
        assert!(
            work_len < shared_len + snap.items.len() * 2_000 + 8_192,
            "frozen work re-embeds evidence per item: {work_len}B"
        );
        // The task head carries the digest so the O(1) binding path can
        // check item markers without decoding the shared blob.
        let head = f.db.task_head(&f.task_id).unwrap();
        assert_eq!(
            head.evidence_digest.as_deref(),
            shared["evidence_digest"].as_str()
        );
        assert!(
            f.db.task_item_binding(&f.task_id, &f.items[0])
                .unwrap()
                .is_some()
        );
    }
    /// A tampered or orphaned `selection_evidence` marker — mismatched
    /// digest, or one on a task carrying no shared evidence — fails closed
    /// on both the full-snapshot and the O(1) binding read paths.
    #[test]
    fn tampered_item_evidence_marker_fails_closed() {
        let f = default_fixture(Operation::Update, &["600006"], &[]);
        let c = Connection::open(f.db.tasks.path()).unwrap();
        c.execute(
            "UPDATE task_items SET frozen_inputs_json=json_replace(frozen_inputs_json,'$.selection_evidence.evidence_digest','tampered') WHERE task_id=?1",
            [&f.task_id],
        )
        .unwrap();
        drop(c);
        assert!(f.db.task_item_binding(&f.task_id, &f.items[0]).is_err());
        assert!(f.ctl.repository.read_task(&f.task_id).is_err());
    }
}

/// #259 micro-soak evidence: 250 consecutive fixture append updates keep
/// per-day net database growth bounded — the final window mean must stay
/// within 120% of the first window mean. This proves the manifest growth
/// shape only; it is not the 5,221-symbol, production-provider, WAL, timing,
/// or absolute 8/16MiB acceptance test.
#[test]
fn two_fifty_day_append_growth_stays_bounded() {
    use x_data_mcp::market_update::tasks::{Operation, RangeContract};
    // Defaults keep normal test runs bounded. Full-scale acceptance must set
    // its own production-shaped inputs and report DB/WAL/time separately.
    let symbols: usize = std::env::var("SOAK_SYMBOLS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let days: usize = std::env::var("SOAK_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(250);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("soak.sqlite");
    let db = SqliteMarket::new(SqliteTasks::new(path.clone()));
    db.tasks.initialize().unwrap();
    let samples: Value =
        serde_json::from_str(include_str!("../fixtures/issue79/legacy-records.json")).unwrap();
    let base = samples[0]["record"].clone();
    // One published base version per symbol through the real collection path.
    let mut heads: Vec<(String, Value)> = Vec::new();
    for i in 0..symbols {
        let symbol = format!("sh.6000{i:02}");
        let mut raw = base.clone();
        raw["version_id"] = json!(format!("soak-base-{symbol}"));
        raw["symbol"] = json!(symbol);
        raw["batch"]["symbol"] = json!(symbol);
        raw["batch"]["provenance"]["symbol"] = json!(symbol);
        raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
        db.stage(&raw).unwrap();
        let ctl = TaskControl::new(
            db.tasks.clone(),
            "range-run".into(),
            vec![Box::new(LegacyCollection)],
        )
        .unwrap();
        let request = ctl
            .accept(
                &format!("soak-base-{symbol}"),
                LegacyCollection.key(),
                json!({"symbols":[symbol],"periods":["day"]}),
            )
            .unwrap();
        let RequestOutcome::Task(task) = request.record.outcome else {
            panic!()
        };
        let t = db
            .tasks
            .transition(&task.task.task_id, 0, "range-run", Change::Claim)
            .unwrap();
        let t = db
            .tasks
            .transition(
                &t.task.task_id,
                t.task.state_revision,
                "range-run",
                Change::StartItem(t.items[0].spec.item_id.clone()),
            )
            .unwrap();
        let plan = Publisher {
            repository: db.clone(),
        }
        .prepare(Publication {
            publication_id: format!("pub-{}", t.task.task_id),
            version_id: raw["version_id"].as_str().unwrap().into(),
            task_id: t.task.task_id,
            item_id: t.items[0].spec.item_id.clone(),
            run_id: "range-run".into(),
            expected_revision: t.task.state_revision,
            intent: PublishIntent::Daily {
                expected_head: None,
            },
        })
        .unwrap();
        db.commit(&plan).unwrap();
        heads.push((symbol, raw));
    }
    let file_size = |path: &std::path::Path| -> u64 {
        Connection::open(path)
            .unwrap()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        std::fs::metadata(path).unwrap().len()
    };
    let mut sizes = vec![file_size(&path)];
    for day in 1..=days {
        let date = "2026-01-05".parse::<chrono::NaiveDate>().unwrap()
            + chrono::Duration::days(day as i64 - 1);
        // One update task per day carrying every symbol, as production does.
        let ctl = TaskControl::new(
            db.tasks.clone(),
            "range-run".into(),
            vec![Box::new(RangeContract {
                operation: Operation::Update,
                repository: db.clone(),
                today: || "2026-09-12".parse().unwrap(),
            })],
        )
        .unwrap();
        let items: Vec<Value> = heads
            .iter()
            .map(|(symbol, raw)| {
                json!({
                    "symbol": symbol,
                    "base_version_id": raw["version_id"],
                    "start_date": date,
                    "end_date": date,
                })
            })
            .collect();
        let request = ctl
            .accept(
                &format!("soak-day-{day}"),
                Operation::Update.key(),
                json!({"items": items}),
            )
            .unwrap();
        let RequestOutcome::Task(task) = request.record.outcome else {
            panic!()
        };
        let mut t = db
            .tasks
            .transition(&task.task.task_id, 0, "range-run", Change::Claim)
            .unwrap();
        for item in &t.items.clone() {
            let item_id = item.spec.item_id.clone();
            t = db
                .tasks
                .transition(
                    &t.task.task_id,
                    t.task.state_revision,
                    "range-run",
                    Change::StartItem(item_id),
                )
                .unwrap();
            let symbol = item.spec.target["symbol"].as_str().unwrap().to_owned();
            let head = heads.iter().position(|(s, _)| *s == symbol).unwrap();
            let head_raw = heads[head].1.clone();
            let bar = json!({
                "date": date, "open": "10.0", "high": "11.0",
                "low": "9.5", "close": "10.5", "volume": null, "amount": null,
            });
            let mut raw = head_raw.clone();
            raw["version_id"] = json!(format!("soak-{symbol}-d{day}"));
            raw["records"].as_array_mut().unwrap().push(bar.clone());
            raw["batch"]["bars"]
                .as_array_mut()
                .unwrap()
                .push(bar.clone());
            let count = raw["records"].as_array().unwrap().len();
            raw["validation"] =
                json!({"record_count": count, "unique_record_count": count, "duplicate_count": 0});
            raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
            db.stage(&raw).unwrap();
            let mut batch: MarketDataBatch =
                serde_json::from_value(head_raw["batch"].clone()).unwrap();
            batch.bars = vec![serde_json::from_value(bar).unwrap()];
            let range = DateRange {
                start: date,
                end: date,
            };
            let evidence = Evidence {
                base_version_id: head_raw["version_id"].as_str().unwrap().into(),
                base_integrity_sha256: head_raw["integrity_sha256"].as_str().unwrap().into(),
                candidate_sha256: market_update::candidate_digest(&batch).unwrap(),
                range: range.clone(),
                same_qfq_basis_ref: "soak".into(),
                revision_ref: None,
                continuity: None,
            };
            let whole = CombinedCandidate {
                record: raw.clone(),
                evidence: CombinedEvidence {
                    base_integrity_sha256: evidence.base_integrity_sha256.clone(),
                    segment_sha256: evidence.candidate_sha256.clone(),
                    combined_integrity_sha256: raw["integrity_sha256"].as_str().unwrap().into(),
                    complete_coverage_ref: "soak".into(),
                },
            };
            let prepared = Publisher {
                repository: db.clone(),
            }
            .prepare_range(
                Publication {
                    publication_id: format!("pub-{symbol}-d{day}"),
                    version_id: raw["version_id"].as_str().unwrap().into(),
                    task_id: t.task.task_id.clone(),
                    item_id: item.spec.item_id.clone(),
                    run_id: "range-run".into(),
                    expected_revision: t.task.state_revision,
                    intent: PublishIntent::Daily {
                        expected_head: Some(head_raw["version_id"].as_str().unwrap().into()),
                    },
                },
                &batch,
                &range,
                &evidence,
                Some(&whole),
            )
            .unwrap();
            let Preparation::Ready(plan) = prepared else {
                panic!("day {day} {symbol}: update not approved")
            };
            db.commit_range(&plan).unwrap();
            heads[head].1 = raw;
            // commit_range decided item i and bumped the task revision;
            // refresh before starting the next item.
            t = db.tasks.read_task(&t.task.task_id).unwrap().unwrap();
        }
        sizes.push(file_size(&path));
    }
    if std::env::var("SOAK_DUMP").is_ok() {
        let c = Connection::open(&path).unwrap();
        let mut q = c
            .prepare(
                "SELECT name, sum(pgsize) FROM dbstat WHERE name NOT LIKE 'sqlite_%' GROUP BY name ORDER BY 2 DESC",
            )
            .unwrap();
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for (name, bytes) in rows {
            println!("  {name}: {bytes}B");
        }
        for (label, sql) in [
            (
                "audit_json",
                "SELECT avg(length(audit_json)), max(length(audit_json)) FROM market_update_results",
            ),
            (
                "receipt_json",
                "SELECT avg(length(receipt_json)), max(length(receipt_json)) FROM market_update_results",
            ),
            (
                "envelope_json",
                "SELECT avg(length(envelope_json)), max(length(envelope_json)) FROM data_versions",
            ),
        ] {
            let (avg, max): (f64, i64) = c
                .query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
            println!("  avg/max {label}: {avg:.0}B/{max}B");
        }
        let sample: String = c
            .query_row(
                "SELECT audit_json FROM market_update_results ORDER BY rowid DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        println!("  sample audit_json: {}", &sample[..sample.len().min(3000)]);
    }
    let growth: Vec<u64> = sizes.windows(2).map(|w| w[1] - w[0]).collect();
    let mean = |s: &[u64]| s.iter().sum::<u64>() as f64 / s.len() as f64;
    let window = (days / 10).clamp(1, 20);
    let early = mean(&growth[..window]);
    let late = mean(&growth[growth.len() - window..]);
    println!(
        "{days}-day soak: base={}B final={}B early_mean={early:.0}B/day late_mean={late:.0}B/day last_day={}B",
        sizes[0],
        sizes[days],
        growth[days - 1]
    );
    assert!(
        late <= early * 1.2,
        "final-{window}-day mean {late:.0}B/day exceeds 120% of first-{window}-day mean {early:.0}B/day"
    );
    // Single-day slack for 4KiB page quantization; a quadratic leak would be
    // ~250x the early mean, not ~3x.
    assert!(
        growth[days - 1] as f64 <= early * 3.0 + 4096.0,
        "day-250 growth {}B is not commensurate with early mean {early:.0}B/day",
        growth[days - 1]
    );
}

/// Shared batch evidence (unit/currency, coverage, adjustment) is
/// generation-common: it must be stored once in `payloads` and referenced
/// by `$ref` from both the version envelope and the update audit —
/// two stored references, one payload row.
#[test]
fn shared_batch_evidence_is_content_addressed_in_envelope_and_audit() {
    let f = fixture(false);
    f.db.commit_range(&f.plan()).unwrap();
    let c = Connection::open(f.db.tasks.path()).unwrap();
    // The version envelope references, not embeds, the shared blocks.
    let env: String = c
        .query_row(
            "SELECT envelope_json FROM data_versions WHERE version_id='legacy-daily'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    for key in ["unit_currency", "coverage", "adjustment_evidence"] {
        assert!(
            env.contains(&format!(r#""{key}":{{"$ref":"payload:"#)),
            "envelope keeps {key} inline"
        );
    }
    // Same for the stored audit segment.
    let audit: String = c
        .query_row(
            "SELECT audit_json FROM market_update_results LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(audit.contains(r#""unit_currency":{"$ref":"payload:"#));
    // Both references resolve to a single stored payload row.
    let shared: i64 = c
        .query_row(
            "SELECT count(*) FROM payloads WHERE payload_json LIKE '%volume_unit%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(shared, 1, "shared evidence must be stored once");
    // Round-trip: the record still expands to the identical stored bytes.
    f.db.fixed("legacy-daily").unwrap();
}

/// #259 P0-B acceptance test for the production-shaped no-change path.
///
/// Unlike the append micro-soak, the provider can hand `prepare_range` the
/// complete requested history. A no-change result must bind the existing
/// version/manifest digests; it must not copy that history into `payloads`.
///
/// The no-change segment carries a `base` reference rather than a bars payload.
#[test]
fn production_shaped_no_change_audit_does_not_persist_history_payload() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteMarket::new(SqliteTasks::new(dir.path().join("range.sqlite")));
    db.tasks.initialize().unwrap();
    let samples: Value =
        serde_json::from_str(include_str!("../fixtures/issue79/legacy-records.json")).unwrap();
    let base = samples[0]["record"].clone();
    db.stage(&base).unwrap();
    let publisher = Publisher {
        repository: db.clone(),
    };
    let first = publisher
        .prepare(command(&db, "full-first", "legacy-daily", None))
        .unwrap();
    db.commit(&first).unwrap();

    let batch: MarketDataBatch = serde_json::from_value(base["batch"].clone()).unwrap();
    let range = DateRange {
        start: batch.bars.first().unwrap().date,
        end: batch.bars.last().unwrap().date,
    };
    let evidence = Evidence {
        base_version_id: "legacy-daily".into(),
        base_integrity_sha256: base["integrity_sha256"].as_str().unwrap().into(),
        candidate_sha256: market_update::candidate_digest(&batch).unwrap(),
        range: range.clone(),
        same_qfq_basis_ref: "fixture:basis".into(),
        revision_ref: None,
        continuity: None,
    };
    let command = command_for_range(
        &db,
        "full-no-change",
        "legacy-daily",
        Some("legacy-daily"),
        ("2024-12-31", "2025-12-31"),
    );
    let Preparation::Ready(plan) = publisher
        .prepare_range(command, &batch, &range, &evidence, None)
        .unwrap()
    else {
        panic!("full-history no-change candidate was not ready")
    };
    assert!(plan.is_no_change());

    let c = Connection::open(db.tasks.path()).unwrap();
    let payload_bytes = |c: &Connection| {
        c.query_row(
            "SELECT coalesce(sum(length(payload_json)),0) FROM payloads",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    };
    let before = payload_bytes(&c);
    drop(c);
    db.commit_range(&plan).unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    assert_eq!(
        payload_bytes(&c),
        before,
        "no-change audit copied candidate history into payloads"
    );
}
