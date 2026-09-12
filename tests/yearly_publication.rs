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
