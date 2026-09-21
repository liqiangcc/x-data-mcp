//! #99: real SQLite admission and independent verification occupancy.
use serde_json::{Value, json};
use std::sync::{Arc, Barrier};
use x_data_mcp::{
    sqlite_tasks::SqliteTasks,
    task_control::{legacy::LegacyCollection, service::TaskControl, *},
    task_verification::{StopVerification, verification_scope},
};
fn task(a: Acceptance) -> TaskSnapshot {
    let RequestOutcome::Task(s) = a.record.outcome else {
        panic!("task required")
    };
    *s
}
fn db() -> (tempfile::TempDir, SqliteTasks) {
    let d = tempfile::tempdir().unwrap();
    let r = SqliteTasks::new(d.path().join("tasks.sqlite"));
    r.initialize().unwrap();
    (d, r)
}
fn original_task(r: &SqliteTasks, id: &str) -> TaskSnapshot {
    let control = TaskControl::new(
        r.clone(),
        "old-run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    task(
        control
            .accept(
                id,
                LegacyCollection.key(),
                json!({"symbols":[id],"periods":["day","year"]}),
            )
            .unwrap(),
    )
}
fn control(r: &SqliteTasks, run: &str) -> TaskControl<SqliteTasks> {
    TaskControl::new(
        r.clone(),
        run.into(),
        vec![Box::new(
            StopVerification::new(r.clone(), run.into()).unwrap(),
        )],
    )
    .unwrap()
}
fn key(r: &SqliteTasks) -> CommandKey {
    StopVerification::new(r.clone(), "new-run".into())
        .unwrap()
        .key()
}
#[test]
fn verification_admission_never_takes_original_claims_or_reinterprets_replay() {
    let (_d, r) = db();
    let original = original_task(&r, "first");
    let ctl = control(&r, "new-run");
    let input = json!({"target_task_id":original.task.task_id});
    let accepted = task(ctl.accept("inspection", key(&r), input.clone()).unwrap());
    assert_eq!(
        r.read_task(&original.task.task_id).unwrap().unwrap(),
        original
    );
    assert_eq!(accepted.items[0].spec.progress_unit, "evidence_check");
    assert_eq!(accepted.claims.len(), 1);
    assert_eq!(
        accepted.claims[0].scope_key,
        verification_scope(&original.task.task_id)
    );
    assert!(
        accepted
            .claims
            .iter()
            .all(|v| original.claims.iter().all(|o| o.scope_key != v.scope_key))
    );
    // Replay is a durable accepted fact, even if the caller's current run differs.
    let replay = control(&r, "old-run")
        .accept("inspection", key(&r), input)
        .unwrap();
    assert!(replay.reused);
    assert_eq!(task(replay).task.task_id, accepted.task.task_id);
    assert!(matches!(
        ctl.accept(
            "first",
            key(&r),
            json!({"target_task_id":original.task.task_id})
        ),
        Err(Error::RequestConflict)
    ));
    let other = original_task(&r, "other");
    assert!(matches!(
        ctl.accept(
            "inspection",
            key(&r),
            json!({"target_task_id":other.task.task_id})
        ),
        Err(Error::RequestConflict)
    ));
    ctl.accept(
        "other-inspection",
        key(&r),
        json!({"target_task_id":other.task.task_id}),
    )
    .unwrap();
}
#[test]
fn verification_missing_normal_recursive_and_untrusted_flags_never_bind() {
    let (_d, r) = db();
    let target = original_task(&r, "first");
    assert!(matches!(
        control(&r, "old-run").accept(
            "normal",
            key(&r),
            json!({"target_task_id":target.task.task_id})
        ),
        Err(Error::InvalidCommand(_))
    ));
    let ctl = control(&r, "new-run");
    assert!(matches!(
        ctl.accept("missing", key(&r), json!({"target_task_id":"missing"})),
        Err(Error::TaskNotFound)
    ));
    for field in ["stopped", "force", "owner_dead", "evidence_approved"] {
        let mut p = json!({"target_task_id":target.task.task_id});
        p[field] = json!(true);
        assert!(matches!(
            ctl.accept(field, key(&r), p),
            Err(Error::InvalidCommand(_))
        ));
        assert!(r.read_request(field).unwrap().is_none());
    }
    let inspection = task(
        ctl.accept(
            "valid",
            key(&r),
            json!({"target_task_id":target.task.task_id}),
        )
        .unwrap(),
    );
    assert!(matches!(
        ctl.accept(
            "recursive",
            key(&r),
            json!({"target_task_id":inspection.task.task_id})
        ),
        Err(Error::InvalidCommand(_))
    ));
    for id in ["normal", "missing", "recursive"] {
        assert!(r.read_request(id).unwrap().is_none());
    }
    assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
}
#[test]
fn verification_two_connections_replay_same_request_and_reject_competing_ids() {
    for same_id in [true, false] {
        let (_d, r) = db();
        let target = original_task(&r, "first");
        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|i| {
                let r = r
                    .clone()
                    .with_busy_timeout(std::time::Duration::from_secs(5));
                let barrier = barrier.clone();
                let target = target.task.task_id.clone();
                std::thread::spawn(move || {
                    let ctl = control(&r, "new-run");
                    barrier.wait();
                    ctl.accept(
                        if same_id || i == 0 { "a" } else { "b" },
                        key(&r),
                        json!({"target_task_id":target}),
                    )
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            if same_id { 2 } else { 1 }
        );
        if same_id {
            assert_eq!(
                results
                    .iter()
                    .filter(|r| r.as_ref().unwrap().reused)
                    .count(),
                1
            );
        } else {
            assert!(
                results
                    .iter()
                    .any(|r| matches!(r, Err(Error::RangeConflict(_))))
            );
        }
        assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
    }
}
struct OtherKind(StopVerification<SqliteTasks>);
impl Contract for OtherKind {
    fn key(&self) -> CommandKey {
        CommandKey {
            operation: "test_other_verification".into(),
            ..self.0.key()
        }
    }
    fn normalize(&self, p: Value) -> Result<Value, Error> {
        self.0.normalize(p)
    }
    fn resolve(&self, p: &Value) -> Result<FrozenWork, Error> {
        self.0.resolve(p)
    }
}
#[test]
fn verification_serial_scope_is_shared_across_types_and_survives_cancel_and_restart() {
    let (_d, r) = db();
    let target = original_task(&r, "first");
    let ctl = control(&r, "new-run");
    let first = task(
        ctl.accept(
            "one",
            key(&r),
            json!({"target_task_id":target.task.task_id}),
        )
        .unwrap(),
    );
    let other = OtherKind(StopVerification::new(r.clone(), "new-run".into()).unwrap());
    let other_key = other.key();
    let other_ctl = TaskControl::new(r.clone(), "new-run".into(), vec![Box::new(other)]).unwrap();
    let cancelled = ctl
        .cancel(&first.task.task_id, first.task.state_revision)
        .unwrap();
    assert_eq!(cancelled.task.status, Status::CancelRequested);
    assert!(matches!(
        other_ctl.accept(
            "two",
            other_key.clone(),
            json!({"target_task_id":target.task.task_id})
        ),
        Err(Error::RangeConflict(_))
    ));
    r.reconcile("third-run").unwrap();
    assert!(matches!(
        other_ctl.accept(
            "two",
            other_key,
            json!({"target_task_id":target.task.task_id})
        ),
        Err(Error::RangeConflict(_))
    ));
    assert!(r.read_request("two").unwrap().is_none());
    assert_eq!(
        r.read_task(&target.task.task_id)
            .unwrap()
            .unwrap()
            .claims
            .len(),
        2
    );
    assert!(r.candidates("third-run").unwrap().is_empty());
}
#[test]
fn verification_corrupt_original_is_not_missing_or_permission_to_unlock() {
    let (_d, r) = db();
    let target = original_task(&r, "first");
    rusqlite::Connection::open(r.path())
        .unwrap()
        .execute(
            "DELETE FROM scope_occupancy WHERE task_id=?1",
            [&target.task.task_id],
        )
        .unwrap();
    assert!(matches!(
        control(&r, "new-run").accept(
            "inspect",
            key(&r),
            json!({"target_task_id":target.task.task_id})
        ),
        Err(Error::NeedsVerification(_))
    ));
    assert!(r.read_request("inspect").unwrap().is_none());
}

use x_data_mcp::task_verification::decision::{
    self, Conclusion, Observation, StopInspector, StopPlan, StopRepository, UnverifiedStopInspector,
};
struct FixtureStopped;
impl StopInspector for FixtureStopped {
    fn inspect(&self, target: &TaskSnapshot) -> Result<Observation, Error> {
        Ok(Observation::Stopped {
            snapshot_sha256: decision::snapshot_digest(target)?,
            evidence_ref: "fixture-only: controlled writer/effects".into(),
        })
    }
}
fn checking(r: &SqliteTasks, target: &TaskSnapshot, id: &str) -> TaskSnapshot {
    let accepted = task(
        control(r, "new-run")
            .accept(id, key(r), json!({"target_task_id":target.task.task_id}))
            .unwrap(),
    );
    let claimed = r
        .transition(&accepted.task.task_id, 0, "new-run", Change::Claim)
        .unwrap();
    r.transition(
        &claimed.task.task_id,
        claimed.task.state_revision,
        "new-run",
        Change::StartItem("stopped-writes".into()),
    )
    .unwrap()
}
fn plan(check: TaskSnapshot, target: TaskSnapshot, inspector: &dyn StopInspector) -> StopPlan {
    decision::prepare(
        check,
        target,
        "new-run",
        "fixture-owner",
        "2026-09-13T00:00:00Z".parse().unwrap(),
        inspector,
    )
    .unwrap()
}
#[test]
fn stopped_check_is_atomic_preserves_successes_and_retry_uses_new_task() {
    let (_d, r) = db();
    let old = original_task(&r, "first");
    let mut target = r
        .transition(&old.task.task_id, 0, "old-run", Change::Claim)
        .unwrap();
    target = r
        .transition(
            &target.task.task_id,
            target.task.state_revision,
            "old-run",
            Change::StartItem(target.items[0].spec.item_id.clone()),
        )
        .unwrap();
    target = r
        .transition(
            &target.task.task_id,
            target.task.state_revision,
            "old-run",
            Change::FinishItem(
                target.items[0].spec.item_id.clone(),
                ItemOutcome {
                    success: true,
                    error: None,
                    evidence: None,
                },
            ),
        )
        .unwrap();
    target = r
        .transition(
            &target.task.task_id,
            target.task.state_revision,
            "old-run",
            Change::StartItem(target.items[1].spec.item_id.clone()),
        )
        .unwrap();
    let check = checking(&r, &target, "inspection");
    let p = plan(check.clone(), target.clone(), &FixtureStopped);
    let receipt = r.commit_stop_check(&p).unwrap();
    assert_eq!(receipt.conclusion, Conclusion::Interrupted);
    let after = r.read_task(&target.task.task_id).unwrap().unwrap();
    assert_eq!(after.task.status, Status::Interrupted);
    assert!(after.claims.is_empty());
    assert_eq!(after.items[0], target.items[0]);
    assert_eq!(after.items[1].stage, Stage::Pending);
    assert_eq!(
        r.stop_receipt(&check.task.task_id).unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(r.commit_stop_check(&p).unwrap(), receipt);
    let business = TaskControl::new(
        r.clone(),
        "new-run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let retried = task(
        business
            .retry(
                &target.task.task_id,
                "explicit-retry",
                [target.items[1].spec.item_id.clone()].into(),
            )
            .unwrap(),
    );
    assert_eq!(
        retried.task.retry_of.as_deref(),
        Some(target.task.task_id.as_str())
    );
    assert_ne!(retried.task.task_id, target.task.task_id);
    let claims = retried.claims.clone();
    r.commit_stop_check(&p).unwrap();
    assert_eq!(
        r.read_task(&retried.task.task_id).unwrap().unwrap().claims,
        claims
    );
    assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), after);
    assert!(matches!(
        r.transition(
            &target.task.task_id,
            after.task.state_revision,
            "old-run",
            Change::StartItem(target.items[1].spec.item_id.clone())
        ),
        Err(Error::RevisionConflict)
    ));
    assert_eq!(r.reconcile("another-run").unwrap(), 1); // Only the new retry; never the old interrupted target.
}
#[test]
fn insufficient_stopping_evidence_completes_only_the_check_and_keeps_business_protection() {
    let (_d, r) = db();
    let target = original_task(&r, "first");
    let check = checking(&r, &target, "inspection");
    let p = plan(check.clone(), target.clone(), &UnverifiedStopInspector);
    let receipt = r.commit_stop_check(&p).unwrap();
    assert_eq!(receipt.conclusion, Conclusion::InsufficientEvidence);
    assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
    assert!(
        r.read_task(&check.task.task_id)
            .unwrap()
            .unwrap()
            .claims
            .is_empty()
    );
    let next = checking(&r, &target, "new-inspection");
    assert_ne!(next.task.task_id, check.task.task_id);
    assert_eq!(r.stop_receipt(&check.task.task_id).unwrap(), Some(receipt));
}
#[test]
fn stopped_plan_rejects_stale_proof_state_and_cancellation_without_partial_release() {
    struct Wrong;
    impl StopInspector for Wrong {
        fn inspect(&self, _: &TaskSnapshot) -> Result<Observation, Error> {
            Ok(Observation::Stopped {
                snapshot_sha256: "wrong".into(),
                evidence_ref: "fixture".into(),
            })
        }
    }
    let (_d, r) = db();
    let target = original_task(&r, "first");
    let check = checking(&r, &target, "inspection");
    assert!(
        decision::prepare(
            check.clone(),
            target.clone(),
            "new-run",
            "actor",
            chrono::Utc::now(),
            &Wrong
        )
        .is_err()
    );
    let p = plan(check.clone(), target.clone(), &FixtureStopped);
    let changed = r
        .transition(&target.task.task_id, 0, "old-run", Change::Claim)
        .unwrap();
    assert!(matches!(
        r.commit_stop_check(&p),
        Err(Error::RevisionConflict)
    ));
    assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), changed);
    let p = plan(check.clone(), changed, &FixtureStopped);
    let cancelled = control(&r, "new-run")
        .cancel(&check.task.task_id, check.task.state_revision)
        .unwrap();
    assert!(matches!(
        r.commit_stop_check(&p),
        Err(Error::RevisionConflict)
    ));
    assert_eq!(
        r.read_task(&check.task.task_id).unwrap().unwrap(),
        cancelled
    );
    assert!(r.stop_receipt(&check.task.task_id).unwrap().is_none());
}
#[test]
fn stopped_audit_insertion_failure_rolls_back_both_tasks_and_all_claims() {
    let (_d, r) = db();
    let target = original_task(&r, "first");
    let check = checking(&r, &target, "inspection");
    let p = plan(check.clone(), target.clone(), &FixtureStopped);
    let c = rusqlite::Connection::open(r.path()).unwrap();
    c.execute_batch("CREATE TRIGGER fail_stop BEFORE INSERT ON stopped_write_checks BEGIN SELECT RAISE(ABORT,'fixture before commit failure'); END;").unwrap();
    assert!(r.commit_stop_check(&p).is_err());
    assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
    assert_eq!(r.read_task(&check.task.task_id).unwrap().unwrap(), check);
    assert!(r.stop_receipt(&check.task.task_id).unwrap().is_none());
    c.execute_batch("DROP TRIGGER fail_stop").unwrap();
    r.commit_stop_check(&p).unwrap();
}
#[test]
fn stopped_receipts_and_normal_reads_reject_missing_or_changed_audit() {
    for delete in [true, false] {
        let (_d, r) = db();
        let target = original_task(&r, "first");
        let check = checking(&r, &target, "inspection");
        r.commit_stop_check(&plan(check.clone(), target.clone(), &FixtureStopped))
            .unwrap();
        let c = rusqlite::Connection::open(r.path()).unwrap();
        c.execute_batch(if delete {
            "DELETE FROM stopped_write_checks"
        } else {
            "UPDATE stopped_write_checks SET audit_json='{}'"
        })
        .unwrap();
        assert!(r.read_task(&target.task.task_id).is_err());
        assert!(r.read_task(&check.task.task_id).is_err());
        assert!(r.stop_receipt(&check.task.task_id).is_err());
    }
}
#[test]
fn stopped_check_busy_and_two_connection_commit_preserve_single_receipt() {
    let (_d, r) = db();
    let target = original_task(&r, "first");
    let check = checking(&r, &target, "inspection");
    let c = rusqlite::Connection::open(r.path()).unwrap();
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    let p = plan(check.clone(), target.clone(), &FixtureStopped);
    assert!(matches!(
        r.clone()
            .with_busy_timeout(std::time::Duration::from_millis(1))
            .commit_stop_check(&p),
        Err(Error::StorageUnavailable(_))
    ));
    c.execute_batch("ROLLBACK").unwrap();
    assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let r = r
                .clone()
                .with_busy_timeout(std::time::Duration::from_secs(5));
            let b = barrier.clone();
            let target = target.clone();
            let check = check.clone();
            std::thread::spawn(move || {
                let p = plan(check, target, &FixtureStopped);
                b.wait();
                r.commit_stop_check(&p).unwrap()
            })
        })
        .collect();
    let receipts: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(receipts[0], receipts[1]);
    assert_eq!(
        c.query_row("SELECT count(*) FROM stopped_write_checks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn stopped_schema_upgrade_uses_real_v6_ddl_preserves_bindings_and_rejects_broken_foreign_keys() {
    for broken in [false, true] {
        let (d, current) = db();
        let target = original_task(&current, "original");
        let path = d.path().join("v6.sqlite");
        let c = rusqlite::Connection::open(&path).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        for (index, sql) in [
            include_str!("../migrations/tasks/001.sql"),
            include_str!("../migrations/tasks/002.sql"),
            include_str!("../migrations/tasks/003_market.sql"),
            include_str!("../migrations/tasks/004_catalog.sql"),
            include_str!("../migrations/tasks/005_update_results.sql"),
            include_str!("../migrations/tasks/006_yearly_results.sql"),
        ]
        .into_iter()
        .enumerate()
        {
            c.execute_batch(sql).unwrap();
            c.execute(
                "INSERT INTO schema_migrations VALUES(?1,?2,'fixture')",
                rusqlite::params![
                    u32::try_from(index + 1).unwrap(),
                    x_data_mcp::hash::sha256(sql.as_bytes())
                ],
            )
            .unwrap();
        }
        c.pragma_update(None, "user_version", 6).unwrap();
        c.execute(
            "ATTACH DATABASE ?1 AS source",
            [current.path().to_str().unwrap()],
        )
        .unwrap();
        for table in [
            "tasks",
            "requests",
            "task_items",
            "task_scopes",
            "scope_occupancy",
        ] {
            let sql = if table == "tasks" {
                // The v6 tasks DDL predates the v15 aggregate counters;
                // copy only the columns a real v6 row carried.
                "INSERT INTO tasks SELECT task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of FROM source.tasks".to_string()
            } else {
                format!("INSERT INTO {table} SELECT * FROM source.{table}")
            };
            c.execute_batch(&sql).unwrap();
        }
        c.execute_batch("DETACH DATABASE source").unwrap();
        if broken {
            // Deliberately construct corruption outside the production Adapter.
            c.pragma_update(None, "foreign_keys", false).unwrap();
            c.execute("INSERT INTO task_scopes VALUES('nonexistent','orphan')", [])
                .unwrap();
        }
        let old = SqliteTasks::new(&path);
        assert!(old.read_request("original").is_err());
        assert_eq!(
            c.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            6
        );
        let upgraded = old.upgrade();
        assert_eq!(upgraded.is_err(), broken);
        if broken {
            assert_eq!(
                c.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
                    .unwrap(),
                6
            );
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='stopped_write_checks'",
                    [],
                    |r| r.get::<_, u32>(0)
                )
                .unwrap(),
                0
            );
        } else {
            assert_eq!(
                c.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
                    .unwrap(),
                x_data_mcp::sqlite_tasks::SCHEMA_VERSION
            );
            assert!(
                !c.prepare("PRAGMA foreign_key_check")
                    .unwrap()
                    .exists([])
                    .unwrap()
            );
            assert_eq!(
                old.read_task(&target.task.task_id).unwrap().unwrap(),
                target
            );
            let checking = checking(&old, &target, "inspection");
            old.commit_stop_check(&plan(checking, target, &FixtureStopped))
                .unwrap();
            old.upgrade().unwrap();
        }
    }
}

#[path = "support/market.rs"]
mod market_fixture;
#[test]
fn stopped_compound_task_retains_verified_published_daily_and_its_original_head() {
    use x_data_mcp::{
        market_data::Period,
        market_storage::{MarketRepository, Publication, PublishIntent, Publisher, Scope},
        sqlite_market::SqliteMarket,
    };
    let (_d, r) = db();
    let market = SqliteMarket::new(r.clone());
    let raw = market_fixture::samples()[0]["record"].clone();
    market.stage(&raw).unwrap();
    let business = TaskControl::new(
        r.clone(),
        "old-run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let target = task(
        business
            .accept(
                "compound",
                LegacyCollection.key(),
                json!({"symbols":["sh.600006"],"periods":["day","year"]}),
            )
            .unwrap(),
    );
    let target = r
        .transition(&target.task.task_id, 0, "old-run", Change::Claim)
        .unwrap();
    let day = target
        .items
        .iter()
        .find(|i| i.spec.target["period"] == "day")
        .map(|i| i.spec.item_id.clone())
        .expect("compound fixture must contain a daily item");
    let target = r
        .transition(
            &target.task.task_id,
            target.task.state_revision,
            "old-run",
            Change::StartItem(day.clone()),
        )
        .unwrap();
    let publisher = Publisher {
        repository: market.clone(),
    };
    let candidate = publisher
        .prepare(Publication {
            publication_id: "published-before-interruption".into(),
            version_id: "legacy-daily".into(),
            task_id: target.task.task_id.clone(),
            item_id: day,
            run_id: "old-run".into(),
            expected_revision: target.task.state_revision,
            intent: PublishIntent::Daily {
                expected_head: None,
            },
        })
        .unwrap();
    market.commit(&candidate).unwrap();
    let before = market.fixed("legacy-daily").unwrap();
    let scope = Scope {
        symbol: "sh.600006".into(),
        period: Period::Day,
    };
    let range = x_data_mcp::market_storage::Range {
        start: "2025-01-01".parse().unwrap(),
        end: "2025-12-31".parse().unwrap(),
        after: None,
        limit: 10,
    };
    let head = market
        .range(&scope, None, &range)
        .unwrap()
        .version
        .version_id;
    let target = r.read_task(&target.task.task_id).unwrap().unwrap();
    assert_eq!(target.task.status, Status::Running);
    let check = checking(&r, &target, "inspection");
    r.commit_stop_check(&plan(check, target.clone(), &FixtureStopped))
        .unwrap();
    assert_eq!(market.fixed("legacy-daily").unwrap().raw(), before.raw());
    assert_eq!(
        market
            .range(&scope, None, &range)
            .unwrap()
            .version
            .version_id,
        head
    );
    let after = r.read_task(&target.task.task_id).unwrap().unwrap();
    assert_eq!(after.task.status, Status::Interrupted);
    assert_eq!(
        after
            .items
            .iter()
            .filter(|i| i.outcome.as_ref().is_some_and(|o| o.success))
            .count(),
        1
    );
}

#[test]
fn stopped_plan_requires_frozen_contract_identity_and_verification_only_scope() {
    struct MustNotInspect;
    impl StopInspector for MustNotInspect {
        fn inspect(&self, _: &TaskSnapshot) -> Result<Observation, Error> {
            panic!("invalid execution contract must reject before external evidence inspection")
        }
    }
    let (_d, r) = db();
    let target = original_task(&r, "first");
    let check = checking(&r, &target, "inspection");
    for change in 0..4 {
        let mut changed = check.clone();
        match change {
            0 => changed.task.key.contract_version = "unknown".into(),
            1 => changed.task.work.scopes = target.task.work.scopes.clone(),
            2 => changed.items[0].spec.scope_keys = target.task.work.scopes.clone(),
            _ => changed.items[0].spec.frozen_inputs["accepted_run_id"] = json!("different-target"),
        }
        assert!(matches!(
            decision::prepare(
                changed,
                target.clone(),
                "new-run",
                "actor",
                chrono::Utc::now(),
                &MustNotInspect
            ),
            Err(Error::RevisionConflict)
        ));
    }
    assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
}

use x_data_mcp::task_verification::runner::{RunOutcome, StopRunner};
fn runner(r: &SqliteTasks) -> StopRunner<SqliteTasks> {
    StopRunner {
        repository: r.clone(),
        run_id: "new-run".into(),
        actor: "fixture-owner".into(),
    }
}
struct NeverInspect;
impl StopInspector for NeverInspect {
    fn inspect(&self, _: &TaskSnapshot) -> Result<Observation, Error> {
        panic!("must not inspect")
    }
}
#[test]
fn stopped_runner_executes_once_replays_receipt_and_defaults_to_insufficient() {
    for verified in [false, true] {
        let (_d, r) = db();
        let target = original_task(&r, "first");
        let check = task(
            control(&r, "new-run")
                .accept(
                    "inspection",
                    key(&r),
                    json!({"target_task_id":target.task.task_id}),
                )
                .unwrap(),
        );
        let inspector: &dyn StopInspector = if verified {
            &FixtureStopped
        } else {
            &UnverifiedStopInspector
        };
        let RunOutcome::Completed(receipt) =
            runner(&r).run(&check.task.task_id, inspector).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            receipt.conclusion,
            if verified {
                Conclusion::Interrupted
            } else {
                Conclusion::InsufficientEvidence
            }
        );
        let RunOutcome::Completed(replay) =
            runner(&r).run(&check.task.task_id, &NeverInspect).unwrap()
        else {
            panic!()
        };
        assert_eq!(receipt, replay);
        if !verified {
            assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
        }
    }
}
#[test]
fn stopped_runner_never_reexecutes_running_or_old_run_checks() {
    for running in [false, true] {
        let (_d, r) = db();
        let target = original_task(&r, "first");
        let check = if running {
            checking(&r, &target, "inspection")
        } else {
            task(
                control(&r, "new-run")
                    .accept(
                        "inspection",
                        key(&r),
                        json!({"target_task_id":target.task.task_id}),
                    )
                    .unwrap(),
            )
        };
        let mut worker = runner(&r);
        if !running {
            worker.run_id = "restarted".into();
        }
        assert!(matches!(
            worker.run(&check.task.task_id, &NeverInspect),
            Err(Error::NeedsVerification(_))
        ));
        assert_eq!(r.read_task(&check.task.task_id).unwrap().unwrap(), check);
        assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
    }
}
#[test]
fn stopped_runner_cancellation_before_or_during_inspection_does_not_touch_target() {
    struct CancelDuring {
        db: SqliteTasks,
        id: String,
    }
    impl StopInspector for CancelDuring {
        fn inspect(&self, target: &TaskSnapshot) -> Result<Observation, Error> {
            let s = self.db.read_task(&self.id)?.unwrap();
            self.db.transition(
                &self.id,
                s.task.state_revision,
                "new-run",
                Change::RequestCancel,
            )?;
            FixtureStopped.inspect(target)
        }
    }
    for during in [false, true] {
        let (_d, r) = db();
        let target = original_task(&r, "first");
        let check = task(
            control(&r, "new-run")
                .accept(
                    "inspection",
                    key(&r),
                    json!({"target_task_id":target.task.task_id}),
                )
                .unwrap(),
        );
        let cancel = CancelDuring {
            db: r.clone(),
            id: check.task.task_id.clone(),
        };
        if !during {
            control(&r, "new-run")
                .cancel(&check.task.task_id, check.task.state_revision)
                .unwrap();
        }
        let result = runner(&r)
            .run(
                &check.task.task_id,
                if during { &cancel } else { &NeverInspect },
            )
            .unwrap();
        let RunOutcome::Cancelled(s) = result else {
            panic!()
        };
        assert_eq!(s.task.status, Status::Cancelled);
        assert!(s.claims.is_empty());
        assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
        assert!(r.stop_receipt(&check.task.task_id).unwrap().is_none());
    }
}

#[path = "support/stop_fault.rs"]
mod stop_fault;
#[test]
fn stopped_runner_reconciles_committed_response_loss_and_keeps_unknown_execution_protected() {
    for commit in [true, false] {
        let (_d, r) = db();
        let target = original_task(&r, "first");
        let check = task(
            control(&r, "new-run")
                .accept(
                    "inspection",
                    key(&r),
                    json!({"target_task_id":target.task.task_id}),
                )
                .unwrap(),
        );
        let worker = StopRunner {
            repository: stop_fault::Fault {
                db: r.clone(),
                commit,
                mutation: None,
            },
            run_id: "new-run".into(),
            actor: "fixture-owner".into(),
        };
        let result = worker.run(&check.task.task_id, &FixtureStopped);
        if commit {
            let RunOutcome::Completed(receipt) = result.unwrap() else {
                panic!()
            };
            assert_eq!(r.stop_receipt(&check.task.task_id).unwrap(), Some(receipt));
        } else {
            assert!(matches!(result, Err(Error::OutcomeUnknown(_))));
            assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
            assert_eq!(
                r.read_task(&check.task.task_id).unwrap().unwrap().items[0].stage,
                Stage::Running
            );
            assert!(matches!(
                worker.run(&check.task.task_id, &NeverInspect),
                Err(Error::NeedsVerification(_))
            ));
        }
    }
}

#[test]
fn public_stopped_progress_preserves_conclusion_and_rechecks_audit_on_cursor() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let (_d, r) = db();
    let target = original_task(&r, "sh.600006");
    let check = task(
        control(&r, "new-run")
            .accept(
                "inspection",
                key(&r),
                json!({"target_task_id":target.task.task_id}),
            )
            .unwrap(),
    );
    runner(&r)
        .run(&check.task.task_id, &FixtureStopped)
        .unwrap();
    let api = DataApi::new(r.clone(), "new-run".into(), Access::local()).unwrap();
    let first = api
        .call(
            "get_task_progress",
            json!({"task_id":target.task.task_id,"page_size":1}),
        )
        .unwrap();
    assert_eq!(first["task"]["status"], "interrupted");
    assert_eq!(first["stopped_write_check"]["conclusion"], "interrupted");
    assert!(first["next_cursor"].is_string());
    rusqlite::Connection::open(r.path())
        .unwrap()
        .execute("DELETE FROM stopped_write_checks", [])
        .unwrap();
    assert!(
        api.call(
            "get_task_progress",
            json!({"task_id":target.task.task_id,"page_size":1,"cursor":first["next_cursor"]})
        )
        .is_err()
    );
}
#[test]
fn stopping_controls_and_task_facts_are_hidden_from_non_owner_context() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let (_d, r) = db();
    let target = original_task(&r, "first");
    let mut restricted = Access::local();
    restricted.securities = Some(Default::default());
    let api = DataApi::new(r.clone(), "new-run".into(), restricted).unwrap();
    assert!(
        !api.tools()
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "verify_stopped_writes")
    );
    assert!(
        api.call(
            "get_data_capabilities",
            json!({"operation":"verify_stopped_writes"})
        )
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(api.call("verify_stopped_writes",json!({"contract_version":"v1","request_id":"forbidden","target_task_id":target.task.task_id})).unwrap_err().code,"forbidden");
    assert!(r.read_request("forbidden").unwrap().is_none());
}

#[test]
fn already_interrupted_requests_bind_terminal_results_without_new_tasks_or_checks() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
        task_verification::existing,
    };
    let (_d, r) = db();
    let target = original_task(&r, "sh.600006");
    let check = task(
        control(&r, "new-run")
            .accept(
                "inspection",
                key(&r),
                json!({"target_task_id":target.task.task_id}),
            )
            .unwrap(),
    );
    runner(&r)
        .run(&check.task.task_id, &FixtureStopped)
        .unwrap();
    let before = r.read_task(&target.task.task_id).unwrap().unwrap();
    let ctl = control(&r, "new-run");
    let result = existing::accept(&ctl, "known-result", &target.task.task_id).unwrap();
    assert!(!result.reused);
    assert!(matches!(result.record.outcome, RequestOutcome::Terminal(_)));
    let replay = existing::accept(&ctl, "known-result", &target.task.task_id).unwrap();
    assert!(replay.reused);
    assert_eq!(replay.record, result.record);
    assert!(matches!(
        existing::accept(&ctl, "known-result", "missing-different-target"),
        Err(Error::RequestConflict)
    ));
    assert!(matches!(
        existing::accept(&ctl, "sh.600006", &target.task.task_id),
        Err(Error::RequestConflict)
    ));
    let api = DataApi::new(r.clone(), "new-run".into(), Access::local()).unwrap();
    let returned=api.call("verify_stopped_writes",json!({"contract_version":"v1","request_id":"public-known","target_task_id":target.task.task_id})).unwrap();
    assert_eq!(returned["bound"], true);
    assert!(returned["task"].is_null());
    assert_eq!(returned["terminal_result"]["kind"], "already_verified");
    assert_eq!(
        api.call(
            "get_request",
            json!({"contract_version":"v1","request_id":"public-known"})
        )
        .unwrap(),
        returned
    );
    assert_eq!(api.call("verify_stopped_writes",json!({"contract_version":"v1","request_id":"public-known","target_task_id":target.task.task_id})).unwrap(),returned);
    let caps = api
        .call(
            "get_data_capabilities",
            json!({"operation":"verify_stopped_writes"}),
        )
        .unwrap();
    jsonschema::validator_for(&caps["items"][0]["output_schema"])
        .unwrap()
        .validate(&returned)
        .unwrap();
    let c = rusqlite::Connection::open(r.path()).unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM tasks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), before);
    c.execute_batch("DELETE FROM stopped_write_checks").unwrap();
    assert!(r.read_request("known-result").is_err());
    assert!(
        api.call(
            "get_request",
            json!({"contract_version":"v1","request_id":"public-known"})
        )
        .is_err()
    );
}
#[test]
fn already_interrupted_request_two_connections_bind_one_terminal_and_never_release_again() {
    use x_data_mcp::task_verification::existing;
    let (_d, r) = db();
    let target = original_task(&r, "first");
    let check = checking(&r, &target, "inspection");
    r.commit_stop_check(&plan(check, target.clone(), &FixtureStopped))
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let r = r
                .clone()
                .with_busy_timeout(std::time::Duration::from_secs(5));
            let b = barrier.clone();
            let id = target.task.task_id.clone();
            std::thread::spawn(move || {
                b.wait();
                existing::accept(&control(&r, "new-run"), "same-result", &id).unwrap()
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results[0].record, results[1].record);
    assert_eq!(results.iter().filter(|a| a.reused).count(), 1);
}
#[test]
fn queued_inspection_rechecks_an_original_that_already_finished_without_inspecting() {
    let (_d, r) = db();
    let target = original_task(&r, "first");
    let contract = StopVerification::new(r.clone(), "new-run".into()).unwrap();
    let intent = contract
        .normalize(json!({"target_task_id":target.task.task_id}))
        .unwrap();
    let frozen = contract.resolve(&intent).unwrap();
    let first = checking(&r, &target, "first-check");
    r.commit_stop_check(&plan(first, target.clone(), &FixtureStopped))
        .unwrap();
    let after = r.read_task(&target.task.task_id).unwrap().unwrap();
    // The generic acceptance transaction may observe a newer target after the
    // application resolved this frozen identity: execution must recheck it.
    let second = task(
        r.accept(
            "accepted-race",
            &Command {
                key: key(&r),
                intent,
            },
            &frozen,
            "new-run",
            None,
        )
        .unwrap(),
    );
    let RunOutcome::Completed(receipt) =
        runner(&r).run(&second.task.task_id, &NeverInspect).unwrap()
    else {
        panic!()
    };
    assert_eq!(receipt.conclusion, Conclusion::AlreadyVerified);
    assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), after);
}

#[test]
fn verification_task_listing_filters_operation_and_excludes_it_for_non_owner() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let (_d, r) = db();
    let target = original_task(&r, "sh.600006");
    let check = task(
        control(&r, "new-run")
            .accept(
                "inspection",
                key(&r),
                json!({"target_task_id":target.task.task_id}),
            )
            .unwrap(),
    );
    let api = DataApi::new(r.clone(), "new-run".into(), Access::local()).unwrap();
    let listed = api
        .call("list_tasks", json!({"operation":"verify_stopped_writes"}))
        .unwrap();
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed["items"][0]["task_id"], check.task.task_id);
    let originals = api
        .call("list_tasks", json!({"operation":"start_collection"}))
        .unwrap();
    assert_eq!(originals["items"].as_array().unwrap().len(), 1);
    assert_eq!(originals["items"][0]["task_id"], target.task.task_id);
    let mut restricted = Access::local();
    restricted.securities = Some(
        [x_data_mcp::data_supply::wire::Security {
            exchange: x_data_mcp::data_supply::wire::Exchange::Sh,
            code: "600006".into(),
        }]
        .into(),
    );
    let view = DataApi::new(r, "new-run".into(), restricted).unwrap();
    assert!(
        view.call("list_tasks", json!({"operation":"verify_stopped_writes"}))
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        view.call("get_task_progress", json!({"task_id":check.task.task_id}))
            .unwrap_err()
            .code,
        "forbidden"
    );
}

#[test]
fn stopped_runner_rechecks_late_cancel_and_target_change_without_using_stale_approval() {
    for cancel in [false, true] {
        let (_d, r) = db();
        let target = original_task(&r, "first");
        let check = task(
            control(&r, "new-run")
                .accept(
                    "inspection",
                    key(&r),
                    json!({"target_task_id":target.task.task_id}),
                )
                .unwrap(),
        );
        let worker = StopRunner {
            repository: stop_fault::Fault {
                db: r.clone(),
                commit: false,
                mutation: Some(cancel),
            },
            run_id: "new-run".into(),
            actor: "fixture-owner".into(),
        };
        let result = worker.run(&check.task.task_id, &FixtureStopped).unwrap();
        if cancel {
            let RunOutcome::Cancelled(s) = result else {
                panic!()
            };
            assert_eq!(s.task.status, Status::Cancelled);
            assert_eq!(r.read_task(&target.task.task_id).unwrap().unwrap(), target);
        } else {
            let RunOutcome::Completed(receipt) = result else {
                panic!()
            };
            assert_eq!(receipt.conclusion, Conclusion::InsufficientEvidence);
            assert!(receipt.reason.contains("TARGET_CHANGED_DURING_CHECK"));
            let target = r.read_task(&target.task.task_id).unwrap().unwrap();
            assert_eq!(target.task.status, Status::Running);
            assert!(!target.claims.is_empty());
        }
    }
}

#[test]
fn never_started_sqlite_proof_rejects_claimed_changed_and_unknown_execution() {
    use x_data_mcp::task_verification::{
        decision::{Observation, StopInspector},
        never_started::NeverStarted,
    };
    let (_d, r) = db();
    let original = original_task(&r, "never-started");
    let inspector = NeverStarted {
        repository: r.clone(),
    };
    assert!(matches!(
        inspector.inspect(&original).unwrap(),
        Observation::Stopped { .. }
    ));
    let mut unknown = original.clone();
    unknown.task.key.contract_version = "future-unknown".into();
    assert!(matches!(
        inspector.inspect(&unknown).unwrap(),
        Observation::Insufficient { .. }
    ));
    let claimed = r
        .transition(&original.task.task_id, 0, "old-run", Change::Claim)
        .unwrap();
    assert!(matches!(
        inspector.inspect(&original),
        Err(Error::RevisionConflict)
    ));
    assert!(matches!(
        inspector.inspect(&claimed).unwrap(),
        Observation::Insufficient { .. }
    ));
}

#[test]
fn never_started_stop_commit_fences_the_original_claim_and_preserves_receipt() {
    use x_data_mcp::task_verification::{
        decision::{Conclusion, StopRepository},
        never_started::NeverStarted,
        runner::{RunOutcome, StopRunner},
    };
    let (_d, r) = db();
    let original = original_task(&r, "never-started-fence");
    let ctl = control(&r, "new-run");
    let check = task(
        ctl.accept(
            "check",
            key(&r),
            json!({"target_task_id":original.task.task_id}),
        )
        .unwrap(),
    );
    let runner = StopRunner {
        repository: r.clone(),
        run_id: "new-run".into(),
        actor: "test-owner".into(),
    };
    let RunOutcome::Completed(receipt) = runner
        .run(
            &check.task.task_id,
            &NeverStarted {
                repository: r.clone(),
            },
        )
        .unwrap()
    else {
        panic!("completed check required")
    };
    assert_eq!(receipt.conclusion, Conclusion::Interrupted);
    let second = SqliteTasks::new(r.path());
    assert!(matches!(
        second.transition(&original.task.task_id, 0, "old-run", Change::Claim),
        Err(Error::RevisionConflict)
    ));
    let stopped = second.read_task(&original.task.task_id).unwrap().unwrap();
    assert_eq!(stopped.task.status, Status::Interrupted);
    assert!(stopped.claims.is_empty());
    assert!(
        stopped
            .items
            .iter()
            .all(|i| i.stage == Stage::Pending && i.outcome.is_none())
    );
    assert_eq!(
        second.stop_receipt(&check.task.task_id).unwrap(),
        Some(receipt)
    );
}

#[test]
fn never_started_two_connections_allow_only_claim_or_interruption() {
    use x_data_mcp::task_verification::{
        decision::{StopRepository, prepare},
        never_started::NeverStarted,
    };
    for index in 0..8 {
        let (_d, r) = db();
        let original = original_task(&r, &format!("race-{index}"));
        let ctl = control(&r, "new-run");
        let check = task(
            ctl.accept(
                "check",
                key(&r),
                json!({"target_task_id":original.task.task_id}),
            )
            .unwrap(),
        );
        let check = r
            .transition(&check.task.task_id, 0, "new-run", Change::Claim)
            .unwrap();
        let check = r
            .transition(
                &check.task.task_id,
                check.task.state_revision,
                "new-run",
                Change::StartItem("stopped-writes".into()),
            )
            .unwrap();
        let plan = prepare(
            check,
            original.clone(),
            "new-run",
            "test-owner",
            chrono::Utc::now(),
            &NeverStarted {
                repository: r.clone(),
            },
        )
        .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let other = SqliteTasks::new(r.path()).with_busy_timeout(std::time::Duration::from_secs(5));
        let id = original.task.task_id.clone();
        let go = barrier.clone();
        let old = std::thread::spawn(move || {
            go.wait();
            other.transition(&id, 0, "old-run", Change::Claim)
        });
        barrier.wait();
        let committed = r
            .clone()
            .with_busy_timeout(std::time::Duration::from_secs(5))
            .commit_stop_check(&plan);
        let claimed = old.join().unwrap();
        match (committed, claimed) {
            (Ok(_), Err(Error::RevisionConflict)) => {
                assert_eq!(
                    r.read_task(&original.task.task_id)
                        .unwrap()
                        .unwrap()
                        .task
                        .status,
                    Status::Interrupted
                );
            }
            (Err(Error::RevisionConflict), Ok(_)) => {
                let target = r.read_task(&original.task.task_id).unwrap().unwrap();
                assert_eq!(target.task.status, Status::Running);
                assert_eq!(target.claims.len(), original.claims.len());
            }
            other => panic!("exactly one conditional transition must win: {other:?}"),
        }
    }
}

#[test]
fn never_started_rejects_durable_associations_even_with_pristine_task_fields() {
    use x_data_mcp::task_verification::{decision::StopInspector, never_started::NeverStarted};
    for sql in [
        "INSERT INTO task_publications VALUES(?1,'missing-version','missing-batch')",
        "INSERT INTO publication_batches VALUES('batch','missing-version',?1,'missing-item','{}','{}')",
        "INSERT INTO market_update_results VALUES('result',?1,'missing-item','missing-base','missing-result',1,'{}','digest','{}')",
        "INSERT INTO yearly_execution_results VALUES('result',?1,'missing-item','missing-base','missing-result',1,'{}','digest','{}')",
    ] {
        let (_d, r) = db();
        let original = original_task(&r, "unexpected-effects");
        let c = rusqlite::Connection::open(r.path()).unwrap();
        // Deliberately damaged fixture only. Production reads never disable FKs.
        c.pragma_update(None, "foreign_keys", false).unwrap();
        c.execute(sql, [&original.task.task_id]).unwrap();
        assert!(matches!(
            NeverStarted { repository: r }.inspect(&original),
            Err(Error::NeedsVerification(_))
        ));
    }
}

#[test]
fn publication_check_admission_is_independent_serialized_and_never_claims_original_work() {
    use x_data_mcp::publication_verification::admission::{PublicationVerification, command_key};
    let (_dir, r) = db();
    let original = original_task(&r, "publication-target");
    let check = TaskControl::new(
        r.clone(),
        "new-run".into(),
        vec![
            Box::new(PublicationVerification::new(r.clone(), "new-run".into()).unwrap()),
            Box::new(StopVerification::new(r.clone(), "new-run".into()).unwrap()),
        ],
    )
    .unwrap();
    let input = json!({"target_task_id":original.task.task_id});
    assert!(matches!(
        check.accept("not-started", command_key(), input.clone()),
        Err(Error::InvalidCommand(_))
    ));
    assert!(r.read_request("not-started").unwrap().is_none());
    let running = r
        .transition(&original.task.task_id, 0, "old-run", Change::Claim)
        .unwrap();
    let running = r
        .transition(
            &running.task.task_id,
            running.task.state_revision,
            "old-run",
            Change::StartItem(running.items[0].spec.item_id.clone()),
        )
        .unwrap();
    let same_run = TaskControl::new(
        r.clone(),
        "old-run".into(),
        vec![Box::new(
            PublicationVerification::new(r.clone(), "old-run".into()).unwrap(),
        )],
    )
    .unwrap();
    assert!(matches!(
        same_run.accept("normal", command_key(), input.clone()),
        Err(Error::InvalidCommand(_))
    ));
    let accepted = task(
        check
            .accept("pub-check", command_key(), input.clone())
            .unwrap(),
    );
    assert_eq!(accepted.items[0].spec.progress_unit, "evidence_check");
    assert_eq!(
        accepted.task.work.scopes,
        [verification_scope(&original.task.task_id)]
            .into_iter()
            .collect()
    );
    assert_eq!(
        r.read_task(&running.task.task_id).unwrap().unwrap(),
        running
    );
    assert!(
        check
            .accept("pub-check", command_key(), input.clone())
            .unwrap()
            .reused
    );
    assert!(matches!(
        check.accept(
            "pub-check",
            command_key(),
            json!({"target_task_id":"other"})
        ),
        Err(Error::RequestConflict)
    ));
    assert!(matches!(
        check.accept("stop-check", key(&r), input.clone()),
        Err(Error::RangeConflict(_))
    ));
    assert!(matches!(
        check.accept("pub-check", key(&r), input.clone()),
        Err(Error::RequestConflict)
    ));
    let cancelling = check.cancel(&accepted.task.task_id, 0).unwrap();
    assert!(matches!(
        check.accept("second", command_key(), input.clone()),
        Err(Error::RangeConflict(_))
    ));
    r.transition(
        &accepted.task.task_id,
        cancelling.task.state_revision,
        "new-run",
        Change::FinishCancellation,
    )
    .unwrap();
    assert_eq!(
        r.read_task(&running.task.task_id).unwrap().unwrap(),
        running
    );
    check
        .accept("stop-after-cancel", key(&r), input.clone())
        .unwrap();
    assert!(matches!(
        check.accept("publication-after-stop", command_key(), input),
        Err(Error::RangeConflict(_))
    ));
}

#[test]
fn publication_check_accepts_persisted_uncertainty_and_two_connections_replay_one_request() {
    use x_data_mcp::publication_verification::admission::{PublicationVerification, command_key};
    let (_dir, r) = db();
    let original = original_task(&r, "same-run-uncertainty");
    let s = r
        .transition(&original.task.task_id, 0, "old-run", Change::Claim)
        .unwrap();
    let s = r
        .transition(
            &s.task.task_id,
            s.task.state_revision,
            "old-run",
            Change::StartItem(s.items[0].spec.item_id.clone()),
        )
        .unwrap();
    let s = r
        .transition(
            &s.task.task_id,
            s.task.state_revision,
            "old-run",
            Change::RequireVerification {
                item_id: s.items[0].spec.item_id.clone(),
                reason: "unconfirmed commit".into(),
            },
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let db = SqliteTasks::new(r.path()).with_busy_timeout(std::time::Duration::from_secs(5));
        let gate = barrier.clone();
        let target = s.task.task_id.clone();
        handles.push(std::thread::spawn(move || {
            let ctl = TaskControl::new(
                db.clone(),
                "old-run".into(),
                vec![Box::new(
                    PublicationVerification::new(db, "old-run".into()).unwrap(),
                )],
            )
            .unwrap();
            gate.wait();
            ctl.accept(
                "concurrent-publication-check",
                command_key(),
                json!({"target_task_id":target}),
            )
            .unwrap()
        }));
    }
    let mut results = handles.into_iter().map(|h| h.join().unwrap());
    let first = results.next().unwrap();
    let second = results.next().unwrap();
    assert_ne!(first.reused, second.reused);
    assert_eq!(task(first).task.task_id, task(second).task.task_id);
    assert_eq!(r.read_task(&s.task.task_id).unwrap().unwrap(), s);
}

use x_data_mcp::task_verification::determinate::{Audited, Determinate};
fn determinate(r: &SqliteTasks) -> Determinate<SqliteTasks> {
    Determinate {
        repository: r.clone(),
    }
}
fn audited(r: &SqliteTasks) -> Audited<SqliteTasks> {
    Audited {
        repository: r.clone(),
    }
}
fn started_target(r: &SqliteTasks, id: &str, inflight: bool) -> TaskSnapshot {
    let old = original_task(r, id);
    let mut target = r
        .transition(&old.task.task_id, 0, "old-run", Change::Claim)
        .unwrap();
    target = r
        .transition(
            &target.task.task_id,
            target.task.state_revision,
            "old-run",
            Change::StartItem(target.items[0].spec.item_id.clone()),
        )
        .unwrap();
    target = r
        .transition(
            &target.task.task_id,
            target.task.state_revision,
            "old-run",
            Change::FinishItem(
                target.items[0].spec.item_id.clone(),
                ItemOutcome {
                    success: true,
                    error: None,
                    evidence: None,
                },
            ),
        )
        .unwrap();
    if inflight {
        target = r
            .transition(
                &target.task.task_id,
                target.task.state_revision,
                "old-run",
                Change::StartItem(target.items[1].spec.item_id.clone()),
            )
            .unwrap();
    }
    target
}
#[test]
fn determinate_check_interrupts_started_task_and_releases_claims() {
    let (_d, r) = db();
    let target = started_target(&r, "first", false);
    let check = task(
        control(&r, "new-run")
            .accept(
                "inspection",
                key(&r),
                json!({"target_task_id":target.task.task_id}),
            )
            .unwrap(),
    );
    let RunOutcome::Completed(receipt) = runner(&r)
        .run(&check.task.task_id, &determinate(&r))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(receipt.conclusion, Conclusion::Interrupted);
    let after = r.read_task(&target.task.task_id).unwrap().unwrap();
    assert_eq!(after.task.status, Status::Interrupted);
    assert!(after.claims.is_empty());
    assert_eq!(after.items[0], target.items[0]);
    assert_eq!(after.items[1].stage, Stage::Pending);
    let business = TaskControl::new(
        r.clone(),
        "new-run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let retried = task(
        business
            .retry(
                &target.task.task_id,
                "explicit-retry",
                [target.items[1].spec.item_id.clone()].into(),
            )
            .unwrap(),
    );
    assert_eq!(retried.items.len(), 1);
    assert_eq!(
        retried.task.retry_of.as_deref(),
        Some(target.task.task_id.as_str())
    );
}
#[test]
fn determinate_check_resolves_inflight_items_from_committed_receipts() {
    for committed in [false, true] {
        let (_d, r) = db();
        let target = started_target(&r, "first", true);
        if committed {
            let c = rusqlite::Connection::open(r.path()).unwrap();
            c.pragma_update(None, "foreign_keys", false).unwrap();
            c.execute(
                "INSERT INTO market_update_results VALUES(?1,?2,?3,'base','ver',0,'{}','x','{}')",
                rusqlite::params![
                    format!("pub-{}", target.task.task_id),
                    target.task.task_id,
                    target.items[1].spec.item_id
                ],
            )
            .unwrap();
        }
        let check = task(
            control(&r, "new-run")
                .accept(
                    "inspection",
                    key(&r),
                    json!({"target_task_id":target.task.task_id}),
                )
                .unwrap(),
        );
        let RunOutcome::Completed(receipt) = runner(&r)
            .run(&check.task.task_id, &determinate(&r))
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(receipt.conclusion, Conclusion::Interrupted);
        let after = r.read_task(&target.task.task_id).unwrap().unwrap();
        assert_eq!(after.task.status, Status::Interrupted);
        assert!(after.claims.is_empty());
        // Reset to pending so a retry either re-runs it or fails its base
        // precondition honestly; the receipt outcome is never rewritten.
        assert_eq!(after.items[1].stage, Stage::Pending);
    }
}
#[test]
fn determinate_check_refuses_pending_item_with_prior_start() {
    let (_d, r) = db();
    let target = started_target(&r, "first", false);
    rusqlite::Connection::open(r.path())
        .unwrap()
        .execute(
            "INSERT INTO task_item_execution_starts VALUES(?1,?2,'old-run',1,'2026-09-13T00:00:00Z')",
            rusqlite::params![target.task.task_id, target.items[1].spec.item_id],
        )
        .unwrap();
    let check = task(
        control(&r, "new-run")
            .accept(
                "inspection",
                key(&r),
                json!({"target_task_id":target.task.task_id}),
            )
            .unwrap(),
    );
    let RunOutcome::Completed(receipt) = runner(&r)
        .run(&check.task.task_id, &determinate(&r))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(receipt.conclusion, Conclusion::InsufficientEvidence);
    assert_eq!(
        r.read_task(&target.task.task_id)
            .unwrap()
            .unwrap()
            .task
            .status,
        Status::Running
    );
    assert!(
        !r.read_task(&target.task.task_id)
            .unwrap()
            .unwrap()
            .claims
            .is_empty()
    );
}
#[test]
fn determinate_check_covers_claimed_task_without_item_starts() {
    let (_d, r) = db();
    let old = original_task(&r, "first");
    let target = r
        .transition(&old.task.task_id, 0, "old-run", Change::Claim)
        .unwrap();
    let check = task(
        control(&r, "new-run")
            .accept(
                "inspection",
                key(&r),
                json!({"target_task_id":target.task.task_id}),
            )
            .unwrap(),
    );
    let RunOutcome::Completed(receipt) = runner(&r)
        .run(&check.task.task_id, &determinate(&r))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(receipt.conclusion, Conclusion::Interrupted);
}
#[test]
fn audited_inspector_covers_never_started_and_determinate_targets() {
    for started in [false, true] {
        let (_d, r) = db();
        let target = if started {
            started_target(&r, "first", false)
        } else {
            original_task(&r, "first")
        };
        let check = task(
            control(&r, "new-run")
                .accept(
                    "inspection",
                    key(&r),
                    json!({"target_task_id":target.task.task_id}),
                )
                .unwrap(),
        );
        let RunOutcome::Completed(receipt) =
            runner(&r).run(&check.task.task_id, &audited(&r)).unwrap()
        else {
            panic!()
        };
        assert_eq!(receipt.conclusion, Conclusion::Interrupted);
    }
}
