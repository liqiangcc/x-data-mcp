mod day_file {
    pub use x_data_mcp::hash::sha256;
}
use chrono::Utc;
use rusqlite::Connection;
use serde_json::{Value, json};
use x_data_mcp::{
    market_data::Period,
    market_storage::{
        Error, MarketRepository, Publication, PublishIntent, PublishPlan, Publisher, Range, Scope,
    },
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
};

mod frozen_legacy {
    use serde_json::Value;
    use std::io;
    // Only the crate path is supplied locally; body below is verbatim fixed old code.

    include!("fixtures/issue79/legacy_record_digest.rs.txt");
    pub fn digest(v: &Value) -> String {
        record_digest(v).unwrap()
    }
}
fn samples() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/issue79/legacy-records.json")).unwrap()
}
fn db() -> (tempfile::TempDir, SqliteMarket) {
    let dir = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(dir.path().join("market.sqlite"));
    tasks.initialize().unwrap();
    (dir, SqliteMarket::new(tasks))
}
fn prepare(db: &SqliteMarket, raw: &Value, intent: PublishIntent) -> PublishPlan {
    db.stage(raw).unwrap();
    let ctl = TaskControl::new(
        db.tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let accepted=ctl.accept(raw["version_id"].as_str().unwrap(),LegacyCollection.key(),json!({"symbols":[raw["symbol"]],"periods":[if raw["period"]=="Day" {"day"}else{"year"}]})).unwrap();
    let RequestOutcome::Task(task) = accepted.record.outcome else {
        panic!()
    };
    let task = db
        .tasks
        .transition(&task.task.task_id, 0, "run", Change::Claim)
        .unwrap();
    let task = db
        .tasks
        .transition(
            &task.task.task_id,
            task.task.state_revision,
            "run",
            Change::StartItem(task.items[0].spec.item_id.clone()),
        )
        .unwrap();
    Publisher {
        repository: db.clone(),
    }
    .prepare(Publication {
        publication_id: format!("pub-{}", raw["version_id"].as_str().unwrap()),
        version_id: raw["version_id"].as_str().unwrap().into(),
        task_id: task.task.task_id,
        item_id: task.items[0].spec.item_id.clone(),
        run_id: "run".into(),
        expected_revision: task.task.state_revision,
        intent,
    })
    .unwrap()
}
fn published_fixture(db: &SqliteMarket, raw: &Value) {
    let plan = prepare(
        db,
        raw,
        PublishIntent::Daily {
            expected_head: None,
        },
    );
    db.commit(&plan).unwrap();
}
fn revised(raw: &Value, id: &str) -> Value {
    let mut r = raw.clone();
    r["version_id"] = json!(id);
    r["integrity_sha256"] = json!(frozen_legacy::digest(&r));
    r
}
use x_data_mcp::{
    market_data::{CoverageEvidence, CoverageStatus},
    yearly_publication::{Derivation, DerivationEvidence},
};
fn inputs(store: &SqliteMarket, version: &str) -> (Derivation, DerivationEvidence) {
    let base = store.fixed("legacy-daily").unwrap();
    let request = Derivation {
        symbol: "sh.600006".into(),
        base_daily_version_id: "legacy-daily".into(),
        version_id: version.into(),
        years: [2025].into(),
        derived_at: "2026-01-02T00:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap(),
    };
    let evidence = DerivationEvidence {
        base_integrity_sha256: base.raw()["integrity_sha256"].as_str().unwrap().into(),
        years: request.years.clone(),
        as_of: "2026-01-02".parse().unwrap(),
        coverage: CoverageEvidence {
            status: CoverageStatus::Complete,
            reason: "FIXTURE: selected retained input only; not listing history".into(),
        },
    };
    (request, evidence)
}
fn command(store: &SqliteMarket, id: &str, intent: PublishIntent) -> Publication {
    let ctl = TaskControl::new(
        store.tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let result = ctl
        .accept(
            id,
            LegacyCollection.key(),
            json!({"symbols":["sh.600006"],"periods":["year"]}),
        )
        .unwrap();
    let RequestOutcome::Task(task) = result.record.outcome else {
        panic!()
    };
    let task = store
        .tasks
        .transition(&task.task.task_id, 0, "run", Change::Claim)
        .unwrap();
    let task = store
        .tasks
        .transition(
            &task.task.task_id,
            task.task.state_revision,
            "run",
            Change::StartItem(task.items[0].spec.item_id.clone()),
        )
        .unwrap();
    Publication {
        publication_id: format!("pub-{id}"),
        version_id: id.into(),
        task_id: task.task.task_id,
        item_id: task.items[0].spec.item_id.clone(),
        run_id: "run".into(),
        expected_revision: task.task.state_revision,
        intent,
    }
}
#[test]
fn local_candidate_publishes_and_queries_fixed_year_with_original_daily_untouched() {
    let (_dir, store) = db();
    let original = samples()[0]["record"].clone();
    published_fixture(&store, &original);
    let publisher = Publisher {
        repository: store.clone(),
    };
    let (request, evidence) = inputs(&store, "local-year");
    let candidate = publisher.derive_yearly(&request, &evidence).unwrap();
    assert_eq!(candidate.raw()["records"][0]["close"], "11.75");
    assert_eq!(
        candidate.raw()["yearly_derivation"]["first_trading_date"],
        "2025-01-02"
    );
    assert_eq!(
        candidate.raw()["yearly_derivation"]["last_trading_date"],
        "2025-12-31"
    );
    assert_eq!(
        candidate.raw()["local_yearly_derivation_v1"]["annual"]["records"][0]["year_unfinished"],
        false
    );
    assert!(candidate.raw().get("analysis_coverage_v1").is_none());
    let cmd = command(
        &store,
        "local-year",
        PublishIntent::CurrentHead {
            expected_daily_head: "legacy-daily".into(),
            expected_yearly_head: None,
        },
    );
    let plan = publisher.prepare_derived_yearly(cmd, &candidate).unwrap();
    let receipt = store.commit(&plan).unwrap();
    assert_eq!(
        store.receipt(&receipt.publication_id).unwrap(),
        Some(receipt)
    );
    let page = store
        .range(
            &Scope {
                symbol: request.symbol,
                period: Period::Year,
            },
            None,
            &Range {
                start: "2025-01-01".parse().unwrap(),
                end: "2025-12-31".parse().unwrap(),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.version.version_id, "local-year");
    assert_eq!(page.rows.len(), 1);
    assert_eq!(store.fixed("legacy-daily").unwrap().raw(), &original);
}
#[test]
fn coverage_binding_and_base_identity_reject_before_staging() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let publisher = Publisher {
        repository: store.clone(),
    };
    for case in 0..8 {
        let (mut request, mut evidence) = inputs(&store, "rejected");
        match case {
            0 => evidence.coverage.status = CoverageStatus::Unknown,
            1 => evidence.coverage.reason.clear(),
            2 => evidence.base_integrity_sha256 = "wrong".into(),
            3 => evidence.years = [2024].into(),
            4 => evidence.as_of = "2026-01-03".parse().unwrap(),
            5 => request.symbol = "sz.000001".into(),
            6 => request.base_daily_version_id = "missing".into(),
            _ => {
                request.years = [2023].into();
                evidence.years = request.years.clone();
            }
        }
        assert!(
            publisher.derive_yearly(&request, &evidence).is_err(),
            "case {case}"
        );
        assert!(store.candidate("rejected").is_err());
    }
}
#[test]
fn historical_base_can_publish_after_daily_advances_but_current_intent_conflicts() {
    for historical in [true, false] {
        let (_dir, store) = db();
        let original = samples()[0]["record"].clone();
        published_fixture(&store, &original);
        let publisher = Publisher {
            repository: store.clone(),
        };
        let (request, evidence) = inputs(&store, "derived");
        let candidate = publisher.derive_yearly(&request, &evidence).unwrap();
        let cmd = command(
            &store,
            "derived",
            if historical {
                PublishIntent::HistoricalFixedBase
            } else {
                PublishIntent::CurrentHead {
                    expected_daily_head: "legacy-daily".into(),
                    expected_yearly_head: None,
                }
            },
        );
        let plan = publisher.prepare_derived_yearly(cmd, &candidate).unwrap();
        let newer = revised(&original, "new-daily");
        let daily_plan = prepare(
            &store,
            &newer,
            PublishIntent::Daily {
                expected_head: Some("legacy-daily".into()),
            },
        );
        store.commit(&daily_plan).unwrap();
        if historical {
            store.commit(&plan).unwrap();
            assert_eq!(store.fixed("derived").unwrap().raw(), candidate.raw());
            let count: i64 = Connection::open(store.tasks.path())
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM effective_heads WHERE period='year'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 0);
        } else {
            assert!(matches!(store.commit(&plan), Err(Error::Conflict(_))));
            assert!(store.fixed("derived").is_err());
        }
    }
}
#[test]
fn fixed_base_damage_after_derivation_never_substitutes_another_version() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let publisher = Publisher {
        repository: store.clone(),
    };
    let (request, evidence) = inputs(&store, "derived");
    let candidate = publisher.derive_yearly(&request, &evidence).unwrap();
    let cmd = command(&store, "derived", PublishIntent::HistoricalFixedBase);
    Connection::open(store.tasks.path())
        .unwrap()
        .execute_batch("UPDATE daily_bars SET close='0' WHERE version_id='legacy-daily'")
        .unwrap();
    assert!(matches!(
        publisher.prepare_derived_yearly(cmd, &candidate),
        Err(Error::Base(_))
    ));
    assert!(store.candidate("derived").is_err());
}

#[test]
fn selected_historical_year_cannot_discard_other_years_from_effective_head() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let publisher = Publisher {
        repository: store.clone(),
    };
    let (mut request, mut evidence) = inputs(&store, "both-years");
    request.years = [2024, 2025].into();
    evidence.years = request.years.clone();
    let all = publisher.derive_yearly(&request, &evidence).unwrap();
    let cmd = command(
        &store,
        "both-years",
        PublishIntent::CurrentHead {
            expected_daily_head: "legacy-daily".into(),
            expected_yearly_head: None,
        },
    );
    let plan = publisher.prepare_derived_yearly(cmd, &all).unwrap();
    store.commit(&plan).unwrap();
    let (request, evidence) = inputs(&store, "selected-year");
    let selected = publisher.derive_yearly(&request, &evidence).unwrap();
    let cmd = command(
        &store,
        "selected-year",
        PublishIntent::CurrentHead {
            expected_daily_head: "legacy-daily".into(),
            expected_yearly_head: Some("both-years".into()),
        },
    );
    assert!(matches!(
        publisher.prepare_derived_yearly(cmd.clone(), &selected),
        Err(Error::Conflict(_))
    ));
    assert!(store.candidate("selected-year").is_err());
    let historical = Publication {
        intent: PublishIntent::HistoricalFixedBase,
        ..cmd
    };
    let plan = publisher
        .prepare_derived_yearly(historical, &selected)
        .unwrap();
    store.commit(&plan).unwrap();
    let page = store
        .range(
            &Scope {
                symbol: request.symbol,
                period: Period::Year,
            },
            None,
            &Range {
                start: "2024-01-01".parse().unwrap(),
                end: "2025-12-31".parse().unwrap(),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.version.version_id, "both-years");
    assert_eq!(page.rows.len(), 2);
    assert_eq!(
        store
            .fixed("selected-year")
            .unwrap()
            .version()
            .records
            .len(),
        1
    );
}

use x_data_mcp::{market_update::tasks::Operation, yearly_publication::tasks::YearlyContract};
fn yearly_control(store: &SqliteMarket) -> TaskControl<SqliteTasks> {
    TaskControl::new(
        store.tasks.clone(),
        "run".into(),
        vec![Box::new(YearlyContract {
            operation: Operation::Update,
            repository: store.clone(),
            today: "2026-09-12".parse().unwrap(),
        })],
    )
    .unwrap()
}
fn yearly_key(store: &SqliteMarket) -> x_data_mcp::task_control::CommandKey {
    YearlyContract {
        operation: Operation::Update,
        repository: store.clone(),
        today: "2026-09-12".parse().unwrap(),
    }
    .key()
}
fn yearly_payload(years: Value) -> Value {
    json!({"items":[{"symbol":"sh.600006","base_daily_version_id":"legacy-daily","years":years,"intent":"HistoricalFixedBase"}]})
}
fn task_of(
    acceptance: x_data_mcp::task_control::Acceptance,
) -> Box<x_data_mcp::task_control::TaskSnapshot> {
    let RequestOutcome::Task(task) = acceptance.record.outcome else {
        panic!()
    };
    task
}
#[test]
fn yearly_acceptance_normalizes_freezes_and_retains_global_id_and_scope_boundaries() {
    use x_data_mcp::task_control::Error as TaskError;
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let ctl = yearly_control(&store);
    let task = task_of(
        ctl.accept(
            "years",
            yearly_key(&store),
            yearly_payload(json!([2025, 2024, 2025])),
        )
        .unwrap(),
    );
    assert_eq!(
        task.items[0].spec.frozen_inputs["years"],
        json!([2024, 2025])
    );
    assert_eq!(
        task.items[0].spec.frozen_inputs["base_integrity_sha256"],
        store.fixed("legacy-daily").unwrap().raw()["integrity_sha256"]
    );
    let mut duplicate = yearly_payload(json!([2024, 2025]));
    let repeated = duplicate["items"][0].clone();
    duplicate["items"].as_array_mut().unwrap().push(repeated);
    assert!(
        ctl.accept("years", yearly_key(&store), duplicate)
            .unwrap()
            .reused
    );
    assert!(matches!(
        ctl.accept("years", yearly_key(&store), yearly_payload(json!([2025]))),
        Err(TaskError::RequestConflict)
    ));
    assert!(matches!(
        ctl.accept(
            "different-year",
            yearly_key(&store),
            yearly_payload(json!([2024]))
        ),
        Err(TaskError::RangeConflict(_))
    ));
    assert!(ctl.read_request("different-year").unwrap().is_none());
    let cancelled = ctl
        .cancel(&task.task.task_id, task.task.state_revision)
        .unwrap();
    assert!(matches!(
        ctl.retry(
            &task.task.task_id,
            "before-stop",
            [task.items[0].spec.item_id.clone()].into()
        ),
        Err(TaskError::RangeConflict(_))
    ));
    assert!(ctl.read_request("before-stop").unwrap().is_none());
    let cancelled = store
        .tasks
        .transition(
            &task.task.task_id,
            cancelled.task.state_revision,
            "run",
            Change::FinishCancellation,
        )
        .unwrap();
    let retried = task_of(
        ctl.retry(
            &task.task.task_id,
            "retry-years",
            [task.items[0].spec.item_id.clone()].into(),
        )
        .unwrap(),
    );
    assert!(cancelled.task.status.terminal());
    assert_eq!(retried.items[0].spec, task.items[0].spec);
    Connection::open(store.tasks.path())
        .unwrap()
        .execute_batch("UPDATE daily_bars SET close='0' WHERE version_id='legacy-daily'")
        .unwrap();
    assert!(
        ctl.accept(
            "years",
            yearly_key(&store),
            yearly_payload(json!([2024, 2025]))
        )
        .unwrap()
        .reused
    );
}
#[test]
fn yearly_invalid_or_future_selection_never_creates_a_request() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let ctl = yearly_control(&store);
    for (i, payload) in [
        yearly_payload(json!([])),
        yearly_payload(json!([2027])),
        yearly_payload(json!([2023])),
        json!({"items":[],"force":true}),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("invalid-{i}");
        assert!(ctl.accept(&id, yearly_key(&store), payload).is_err());
        assert!(ctl.read_request(&id).unwrap().is_none());
    }
}
#[test]
fn yearly_same_request_two_connections_create_one_task() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let ctl = yearly_control(&store);
                barrier.wait();
                task_of(
                    ctl.accept(
                        "race-years",
                        yearly_key(&store),
                        yearly_payload(json!([2025])),
                    )
                    .unwrap(),
                )
                .task
                .task_id
                .clone()
            })
        })
        .collect();
    let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(ids[0], ids[1]);
}
#[test]
fn yearly_publication_is_bound_to_accepted_years_base_and_intent() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let ctl = yearly_control(&store);
    let task = task_of(
        ctl.accept(
            "year-task",
            yearly_key(&store),
            yearly_payload(json!([2025])),
        )
        .unwrap(),
    );
    let task = store
        .tasks
        .transition(
            &task.task.task_id,
            task.task.state_revision,
            "run",
            Change::Claim,
        )
        .unwrap();
    let task = store
        .tasks
        .transition(
            &task.task.task_id,
            task.task.state_revision,
            "run",
            Change::StartItem(task.items[0].spec.item_id.clone()),
        )
        .unwrap();
    let publisher = Publisher {
        repository: store.clone(),
    };
    let (mut request, mut evidence) = inputs(&store, "accepted-year");
    let valid = publisher.derive_yearly(&request, &evidence).unwrap();
    let cmd = Publication {
        publication_id: "accepted-result".into(),
        version_id: "accepted-year".into(),
        task_id: task.task.task_id,
        item_id: task.items[0].spec.item_id.clone(),
        run_id: "run".into(),
        expected_revision: task.task.state_revision,
        intent: PublishIntent::HistoricalFixedBase,
    };
    let changed = Publication {
        intent: PublishIntent::CurrentHead {
            expected_daily_head: "legacy-daily".into(),
            expected_yearly_head: None,
        },
        ..cmd.clone()
    };
    assert!(matches!(
        publisher.prepare_derived_yearly(changed, &valid),
        Err(Error::Conflict(_))
    ));
    request.years = [2024, 2025].into();
    evidence.years = request.years.clone();
    let widened = publisher.derive_yearly(&request, &evidence).unwrap();
    assert!(matches!(
        publisher.prepare_derived_yearly(cmd.clone(), &widened),
        Err(Error::Conflict(_))
    ));
    assert!(store.candidate("accepted-year").is_err());
    let plan = publisher.prepare_derived_yearly(cmd, &valid).unwrap();
    store.commit(&plan).unwrap();
    assert_eq!(
        store
            .fixed("accepted-year")
            .unwrap()
            .version()
            .records
            .len(),
        1
    );
}

use x_data_mcp::yearly_publication::results::{YearlyRepository, YearlyResultPlan};
fn result_plan(
    store: &SqliteMarket,
    id: &str,
    prior: Option<&str>,
    coverage: &str,
) -> YearlyResultPlan {
    let intent = PublishIntent::CurrentHead {
        expected_daily_head: "legacy-daily".into(),
        expected_yearly_head: prior.map(str::to_owned),
    };
    let mut payload = yearly_payload(json!([2025]));
    payload["items"][0]["intent"] = json!(intent);
    let ctl = yearly_control(store);
    let task = task_of(ctl.accept(id, yearly_key(store), payload).unwrap());
    let task = store
        .tasks
        .transition(
            &task.task.task_id,
            task.task.state_revision,
            "run",
            Change::Claim,
        )
        .unwrap();
    let task = store
        .tasks
        .transition(
            &task.task.task_id,
            task.task.state_revision,
            "run",
            Change::StartItem(task.items[0].spec.item_id.clone()),
        )
        .unwrap();
    let (mut request, mut evidence) = inputs(store, id);
    if id.starts_with("reuse") {
        request.derived_at += chrono::Duration::days(1);
        evidence.as_of = evidence.as_of.succ_opt().unwrap();
    }
    evidence.coverage.reason = coverage.into();
    let publisher = Publisher {
        repository: store.clone(),
    };
    let candidate = publisher.derive_yearly(&request, &evidence).unwrap();
    publisher
        .prepare_yearly_result(
            Publication {
                publication_id: format!("result-{id}"),
                version_id: id.into(),
                task_id: task.task.task_id,
                item_id: task.items[0].spec.item_id.clone(),
                run_id: "run".into(),
                expected_revision: task.task.state_revision,
                intent,
            },
            &candidate,
            prior,
        )
        .unwrap()
}
#[test]
fn yearly_no_change_reuses_version_atomically_but_coverage_changes_publish() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let first = result_plan(&store, "first-yearly", None, "fixture range");
    assert!(!first.is_no_change());
    store.commit_yearly(&first).unwrap();
    let original = store.fixed("first-yearly").unwrap().raw().clone();
    let reuse = result_plan(
        &store,
        "reuse-yearly",
        Some("first-yearly"),
        "fixture range",
    );
    assert!(reuse.is_no_change());
    assert!(store.candidate("reuse-yearly").is_err());
    let receipt = store.commit_yearly(&reuse).unwrap();
    assert_eq!(receipt.version_id, "first-yearly");
    assert_eq!(store.commit_yearly(&reuse).unwrap(), receipt);
    assert_eq!(
        store.yearly_receipt("result-reuse-yearly").unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        store
            .yearly_item_receipt(&reuse.command().task_id, &reuse.command().item_id)
            .unwrap(),
        Some(receipt)
    );
    assert!(
        store
            .tasks
            .read_task(&reuse.command().task_id)
            .unwrap()
            .unwrap()
            .claims
            .is_empty()
    );
    let changed = result_plan(
        &store,
        "new-coverage",
        Some("first-yearly"),
        "fixture newly verified coverage",
    );
    assert!(!changed.is_no_change());
    store.commit_yearly(&changed).unwrap();
    assert_eq!(store.fixed("first-yearly").unwrap().raw(), &original);
    assert_eq!(
        store
            .versions(&Scope {
                symbol: "sh.600006".into(),
                period: Period::Year
            })
            .unwrap()
            .len(),
        2
    );
    let count: i64 = Connection::open(store.tasks.path())
        .unwrap()
        .query_row(
            "SELECT count(*) FROM task_publications WHERE task_id=?1",
            [&reuse.command().task_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    Connection::open(store.tasks.path()).unwrap().execute_batch("UPDATE yearly_execution_results SET audit_json='{}' WHERE result_id='result-reuse-yearly'").unwrap();
    assert!(matches!(
        store.yearly_receipt("result-reuse-yearly"),
        Err(Error::Integrity(_))
    ));
}
#[test]
fn yearly_reuse_head_conflict_preserves_task_and_old_result() {
    let (_dir, store) = db();
    let original = samples()[0]["record"].clone();
    published_fixture(&store, &original);
    let first = result_plan(&store, "first-yearly", None, "fixture");
    store.commit_yearly(&first).unwrap();
    let reuse = result_plan(&store, "reuse", Some("first-yearly"), "fixture");
    let daily = revised(&original, "advanced-daily");
    let plan = prepare(
        &store,
        &daily,
        PublishIntent::Daily {
            expected_head: Some("legacy-daily".into()),
        },
    );
    store.commit(&plan).unwrap();
    assert!(matches!(
        store.commit_yearly(&reuse),
        Err(Error::Conflict(_))
    ));
    assert!(store.yearly_receipt("result-reuse").unwrap().is_none());
    let task = store
        .tasks
        .read_task(&reuse.command().task_id)
        .unwrap()
        .unwrap();
    assert!(!task.claims.is_empty());
    assert_eq!(task.task.state_revision, reuse.command().expected_revision);
    assert!(store.fixed("first-yearly").is_ok());
}
#[test]
fn yearly_result_schema_requires_explicit_upgrade_and_read_does_not_upgrade() {
    let (_dir, store) = db();
    let c = Connection::open(store.tasks.path()).unwrap();
    c.execute_batch("DROP TABLE yearly_execution_results; DELETE FROM schema_migrations WHERE version=6; PRAGMA user_version=5;").unwrap();
    assert!(store.yearly_receipt("absent").is_err());
    let version: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 5);
    store.tasks.upgrade().unwrap();
    assert!(store.yearly_receipt("absent").unwrap().is_none());
    let version: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 6);
}
