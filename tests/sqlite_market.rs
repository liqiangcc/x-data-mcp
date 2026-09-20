mod day_file {
    pub use x_data_mcp::hash::sha256;
}
use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::{Value, json};
use x_data_mcp::{
    market_data::Period,
    market_storage::{
        Error, MarketRepository, Publication, PublishIntent, PublishPlan, Publisher, Range, Scope,
        compat,
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
fn range() -> Range {
    Range {
        start: "2025-01-01".parse().unwrap(),
        end: "2025-12-31".parse().unwrap(),
        after: None,
        limit: 1,
    }
}
fn scope() -> Scope {
    Scope {
        symbol: "sh.600006".into(),
        period: Period::Day,
    }
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
#[test]
fn frozen_old_digest_matches_independent_literals_and_preserves_missing_null() {
    let fixtures = samples();
    assert_ne!(fixtures[0]["expected"], fixtures[1]["expected"]);
    for sample in fixtures {
        let raw = &sample["record"];
        assert_eq!(frozen_legacy::digest(raw), sample["expected"]);
        assert_eq!(compat::legacy_digest(raw).unwrap(), sample["expected"]);
        assert_eq!(compat::check(raw).unwrap().raw(), raw);
        let (_dir, store) = db();
        if raw["period"] == "Year" {
            published_fixture(&store, &samples()[0]["record"]);
        }
        store.stage(raw).unwrap();
        assert_eq!(
            store
                .candidate(raw["version_id"].as_str().unwrap())
                .unwrap()
                .raw(),
            raw
        );
    }
}
#[test]
fn verified_sql_range_is_ordered_precise_and_fixed() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let page = store.range(&scope(), None, &range()).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].open.to_string(), "10.000000000000000001");
    assert_eq!(page.rows[0].volume, None);
    assert_eq!(
        page.rows[0].date,
        "2025-01-02".parse::<NaiveDate>().unwrap()
    );
    let mut next = range();
    next.after = page.next_after;
    let page2 = store
        .range(&scope(), Some(&page.version.version_id), &next)
        .unwrap();
    assert_eq!(
        page2.rows[0].date,
        "2025-12-31".parse::<NaiveDate>().unwrap()
    );
    assert_eq!(page2.next_after, None);
}
#[test]
fn compat_bar_metadata_and_evidence_tampering_reject_without_fallback() {
    for sql in [
        "UPDATE data_versions SET envelope_json=json_set(envelope_json,'$.source','altered')",
        "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','10.06') WHERE trade_date='2025-01-02'",
        "UPDATE data_versions SET source='altered'",
        "UPDATE version_evidence SET value='USD' WHERE field='price_currency'",
        "UPDATE data_versions SET content_digest='wrong'",
    ] {
        let (_dir, store) = db();
        published_fixture(&store, &samples()[0]["record"]);
        Connection::open(store.tasks.path())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        assert!(
            matches!(
                store.range(&scope(), Some("legacy-daily"), &range()),
                Err(Error::Integrity(_))
            ),
            "{sql}"
        );
        assert!(store.range(&scope(), None, &range()).is_err());
    }
}
#[test]
fn missing_digest_or_unverified_input_is_never_promoted_by_staging() {
    let (_dir, store) = db();
    let mut raw = samples()[0]["record"].clone();
    raw.as_object_mut().unwrap().remove("integrity_sha256");
    assert!(store.stage(&raw).is_err());
    raw["batch"]["coverage"]["status"] = json!("Unknown");
    raw["integrity_sha256"] = json!(frozen_legacy::digest(&raw));
    assert!(store.stage(&raw).is_err());
    assert!(matches!(store.fixed("legacy-daily"), Err(Error::NotFound)));
}
fn yearly(id: &str, base: &str) -> Value {
    let mut raw = samples()[2]["record"].clone();
    raw["version_id"] = json!(id);
    raw["yearly_derivation"]["base_daily_version_id"] = json!(base);
    raw["batch"]["provenance"]["yearly_derivation"]["base_daily_version_id"] = json!(base);
    raw["integrity_sha256"] = json!(frozen_legacy::digest(&raw));
    raw
}
fn current_head(db: &SqliteMarket, period: Period) -> Option<String> {
    let mut scope = scope();
    scope.period = period;
    match db.range(&scope, None, &range()) {
        Ok(page) => Some(page.version.version_id),
        Err(Error::NotFound) => None,
        Err(e) => panic!("{e}"),
    }
}
#[test]
fn historical_fixed_base_publishes_but_never_promotes_even_after_daily_advances() {
    let (_dir, store) = db();
    let daily = samples()[0]["record"].clone();
    published_fixture(&store, &daily);
    let d2 = revised(&daily, "daily-2");
    let plan = prepare(
        &store,
        &d2,
        PublishIntent::Daily {
            expected_head: Some("legacy-daily".into()),
        },
    );
    store.commit(&plan).unwrap();
    let current = prepare(
        &store,
        &yearly("current-year", "daily-2"),
        PublishIntent::CurrentHead {
            expected_daily_head: "daily-2".into(),
            expected_yearly_head: None,
        },
    );
    store.commit(&current).unwrap();
    let historical = prepare(
        &store,
        &yearly("historical-year", "legacy-daily"),
        PublishIntent::HistoricalFixedBase,
    );
    assert!(historical.head_promotion().is_none());
    assert!(historical.head_preconditions().is_empty());
    store.commit(&historical).unwrap();
    assert_eq!(
        current_head(&store, Period::Year).as_deref(),
        Some("current-year")
    );
    assert_eq!(
        current_head(&store, Period::Day).as_deref(),
        Some("daily-2")
    );
    let mut scope = scope();
    scope.period = Period::Year;
    let mut r = range();
    r.start = "2025-06-01".parse().unwrap();
    r.end = "2025-06-30".parse().unwrap();
    let page = store.range(&scope, Some("historical-year"), &r).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(
        page.rows[0].date,
        "2025-12-31".parse::<NaiveDate>().unwrap()
    );
    assert_eq!(
        page.version
            .yearly_derivation
            .unwrap()
            .base_daily_version_id,
        "legacy-daily"
    );
    assert_eq!(store.versions(&scope).unwrap().len(), 2);
}
#[test]
fn current_yearly_conflicts_when_daily_or_yearly_head_advances() {
    for daily_changes in [true, false] {
        let (_dir, store) = db();
        let daily = samples()[0]["record"].clone();
        published_fixture(&store, &daily);
        let plan = prepare(
            &store,
            &yearly("losing-year", "legacy-daily"),
            PublishIntent::CurrentHead {
                expected_daily_head: "legacy-daily".into(),
                expected_yearly_head: None,
            },
        );
        if daily_changes {
            let other = prepare(
                &store,
                &revised(&daily, "daily-2"),
                PublishIntent::Daily {
                    expected_head: Some("legacy-daily".into()),
                },
            );
            store.commit(&other).unwrap();
        } else {
            store
                .stage(&yearly("winning-year", "legacy-daily"))
                .unwrap();
            let other = Publisher {
                repository: store.clone(),
            }
            .prepare(Publication {
                publication_id: "other".into(),
                version_id: "winning-year".into(),
                ..plan.publication().clone()
            })
            .unwrap();
            store.commit(&other).unwrap();
        }
        assert!(matches!(store.commit(&plan), Err(Error::Conflict(_))));
        assert!(
            store
                .receipt(&plan.publication().publication_id)
                .unwrap()
                .is_none()
        );
        assert!(matches!(store.fixed("losing-year"), Err(Error::NotFound)));
        assert_eq!(
            current_head(&store, Period::Year),
            if daily_changes {
                None
            } else {
                Some("winning-year".into())
            }
        );
    }
}
#[test]
fn two_real_connections_compete_for_expected_head_without_last_writer_wins() {
    use std::sync::{Arc, Barrier};
    let (_dir, store) = db();
    let daily = samples()[0]["record"].clone();
    let first = prepare(
        &store,
        &daily,
        PublishIntent::Daily {
            expected_head: None,
        },
    );
    let other_raw = revised(&daily, "competitor");
    store.stage(&other_raw).unwrap();
    let second = Publisher {
        repository: store.clone(),
    }
    .prepare(Publication {
        publication_id: "competitor-pub".into(),
        version_id: "competitor".into(),
        ..first.publication().clone()
    })
    .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [first, second]
        .into_iter()
        .map(|plan| {
            let db = SqliteMarket::new(
                store
                    .tasks
                    .clone()
                    .with_busy_timeout(std::time::Duration::from_secs(3)),
            );
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                db.commit(&plan)
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::Conflict(_))))
            .count(),
        1
    );
}
#[test]
fn head_task_and_batch_write_failures_roll_back_every_publication_fact() {
    for trigger in [
        "CREATE TRIGGER fail BEFORE INSERT ON publication_batches BEGIN SELECT RAISE(ABORT,'batch fail'); END;",
        "CREATE TRIGGER fail BEFORE UPDATE ON tasks BEGIN SELECT RAISE(ABORT,'task fail'); END;",
        "CREATE TRIGGER fail BEFORE UPDATE ON effective_heads BEGIN SELECT RAISE(ABORT,'head fail'); END;",
        "CREATE TRIGGER fail BEFORE UPDATE ON effective_heads BEGIN SELECT RAISE(IGNORE); END;",
    ] {
        let (_dir, store) = db();
        let raw = samples()[0]["record"].clone();
        published_fixture(&store, &raw);
        let plan = prepare(
            &store,
            &revised(&raw, "new"),
            PublishIntent::Daily {
                expected_head: Some("legacy-daily".into()),
            },
        );
        let before = store.task(&plan.publication().task_id).unwrap();
        Connection::open(store.tasks.path())
            .unwrap()
            .execute_batch(trigger)
            .unwrap();
        assert!(store.commit(&plan).is_err());
        assert_eq!(
            current_head(&store, Period::Day).as_deref(),
            Some("legacy-daily")
        );
        assert!(
            store
                .receipt(&plan.publication().publication_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.task(&plan.publication().task_id).unwrap(), before);
        assert!(matches!(store.fixed("new"), Err(Error::NotFound)));
    }
}
#[test]
fn cancellation_revision_and_occupancy_changes_block_publication() {
    for missing_claim in [false, true] {
        let (_dir, store) = db();
        let raw = samples()[0]["record"].clone();
        let plan = prepare(
            &store,
            &raw,
            PublishIntent::Daily {
                expected_head: None,
            },
        );
        if missing_claim {
            Connection::open(store.tasks.path())
                .unwrap()
                .execute("DELETE FROM scope_occupancy", [])
                .unwrap();
        } else {
            let p = plan.publication();
            store
                .tasks
                .transition(
                    &p.task_id,
                    p.expected_revision,
                    "run",
                    Change::RequestCancel,
                )
                .unwrap();
        }
        assert!(store.commit(&plan).is_err());
        assert!(
            store
                .receipt(&plan.publication().publication_id)
                .unwrap()
                .is_none()
        );
        assert!(matches!(store.fixed("legacy-daily"), Err(Error::NotFound)));
    }
}
#[test]
fn base_missing_corrupt_identity_period_and_adjustment_are_not_head_conflicts() {
    for scenario in ["missing", "corrupt", "identity", "period", "adjustment"] {
        let (_dir, store) = db();
        let raw = samples()[0]["record"].clone();
        published_fixture(&store, &raw);
        let mut y = yearly("bad-year", "legacy-daily");
        match scenario {
            "missing" => {
                y = yearly("bad-year", "absent");
            }
            "corrupt" => {
                Connection::open(store.tasks.path())
                    .unwrap()
                    .execute(
                        "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','99')",
                        [],
                    )
                    .unwrap();
            }
            "identity" => {
                y["symbol"] = json!("sz.000001");
                y["batch"]["symbol"] = json!("sz.000001");
                y["batch"]["provenance"]["symbol"] = json!("sz.000001");
            }
            "period" => {
                let p = prepare(
                    &store,
                    &yearly("base-year", "legacy-daily"),
                    PublishIntent::HistoricalFixedBase,
                );
                store.commit(&p).unwrap();
                y = yearly("bad-year", "base-year");
            }
            "adjustment" => {
                Connection::open(store.tasks.path())
                    .unwrap()
                    .execute(
                        "UPDATE version_evidence SET value='raw' WHERE field='adjustment'",
                        [],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        y["integrity_sha256"] = json!(frozen_legacy::digest(&y));
        assert!(matches!(store.stage(&y), Err(Error::Base(_))), "{scenario}");
    }
}
#[test]
fn yearly_lineage_projection_tampering_and_forged_published_flag_are_rejected() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    let plan = prepare(
        &store,
        &yearly("y", "legacy-daily"),
        PublishIntent::HistoricalFixedBase,
    );
    store.commit(&plan).unwrap();
    Connection::open(store.tasks.path())
        .unwrap()
        .execute("UPDATE yearly_lineage SET algorithm_version='forged'", [])
        .unwrap();
    assert!(store.fixed("y").is_err());
    let other = revised(&samples()[0]["record"], "forged-state");
    store.stage(&other).unwrap();
    Connection::open(store.tasks.path())
        .unwrap()
        .execute(
            "UPDATE data_versions SET state='published' WHERE version_id='forged-state'",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.fixed("forged-state"),
        Err(Error::Integrity(_))
    ));
}
#[test]
fn fixed_paging_ignores_new_head_preserves_conditions_and_rejects_later_damage() {
    use x_data_mcp::{market_storage::query::Pages, query::MarketQuery};
    let (_dir, store) = db();
    let raw = samples()[0]["record"].clone();
    published_fixture(&store, &raw);
    let pages = Pages::new(store.clone());
    let now = "2026-01-02T00:00:00Z".parse().unwrap();
    let query = MarketQuery {
        exchange: "sh".into(),
        code: "600006".into(),
        period: "day".into(),
        start_date: "2024-01-01".parse().unwrap(),
        end_date: "2026-12-31".parse().unwrap(),
        version_id: None,
        page_size: Some(1),
        cursor: None,
    };
    let first = pages.market(query.clone(), now).unwrap();
    let mut next = query.clone();
    next.cursor = first.next_cursor;
    let plan = prepare(
        &store,
        &revised(&raw, "new-head"),
        PublishIntent::Daily {
            expected_head: Some("legacy-daily".into()),
        },
    );
    store.commit(&plan).unwrap();
    let second = pages.market(next.clone(), chrono::Utc::now()).unwrap();
    assert_eq!(second.query_time, now);
    assert_eq!(second.data.unwrap().version.version_id, "legacy-daily");
    let mut changed = next.clone();
    changed.page_size = Some(2);
    assert!(matches!(pages.market(changed, now), Err(Error::Invalid(_))));
    assert!(Pages::new(store.clone()).market(next.clone(), now).is_err());
    Connection::open(store.tasks.path())
        .unwrap()
        .execute(
            "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.open','0') WHERE segment_id IN (SELECT segment_id FROM version_segments WHERE version_id='legacy-daily')",
            [],
        )
        .unwrap();
    assert!(matches!(pages.market(next, now), Err(Error::Integrity(_))));
    // new-head shares legacy-daily's identical bars via a clipped segment ref:
    // damage to the shared immutable segment is detected on every version
    // that references it, including the effective head.
    assert!(matches!(
        store.range(&scope(), None, &range()),
        Err(Error::Integrity(_))
    ));
}
#[test]
fn version_list_freezes_members_and_tie_sort_order() {
    use x_data_mcp::{market_storage::query::Pages, query::VersionQuery};
    let (_dir, store) = db();
    let raw = samples()[0]["record"].clone();
    published_fixture(&store, &raw);
    let plan = prepare(
        &store,
        &revised(&raw, "a-version"),
        PublishIntent::Daily {
            expected_head: Some("legacy-daily".into()),
        },
    );
    store.commit(&plan).unwrap();
    let pages = Pages::new(store.clone());
    let now = chrono::Utc::now();
    let query = VersionQuery {
        exchange: "sh".into(),
        code: "600006".into(),
        period: "day".into(),
        page_size: Some(1),
        cursor: None,
    };
    let first = pages.versions(query.clone(), now).unwrap();
    assert_eq!(first.versions[0].version_id, "a-version");
    let plan = prepare(
        &store,
        &revised(&raw, "0-new"),
        PublishIntent::Daily {
            expected_head: Some("a-version".into()),
        },
    );
    store.commit(&plan).unwrap();
    let mut next = query;
    next.cursor = first.next_cursor;
    let second = pages.versions(next.clone(), now).unwrap();
    assert_eq!(second.versions[0].version_id, "legacy-daily");
    assert!(second.next_cursor.is_none());
    Connection::open(store.tasks.path()).unwrap().execute("UPDATE version_evidence SET value='raw' WHERE version_id='legacy-daily' AND field='adjustment'",[]).unwrap();
    assert!(pages.versions(next, now).is_err());
}
#[test]
fn composite_task_keeps_protection_until_last_publication_commits() {
    let (_dir, store) = db();
    let raw = samples()[0]["record"].clone();
    store.stage(&raw).unwrap();
    let ctl = TaskControl::new(
        store.tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let a = ctl
        .accept(
            "compound",
            LegacyCollection.key(),
            json!({"symbols":["sh.600006"],"periods":["day","year"]}),
        )
        .unwrap();
    let RequestOutcome::Task(t) = a.record.outcome else {
        panic!()
    };
    let task = store
        .tasks
        .transition(&t.task.task_id, 0, "run", Change::Claim)
        .unwrap();
    let daily_item = task
        .items
        .iter()
        .find(|i| i.spec.target["period"] == "day")
        .unwrap()
        .spec
        .item_id
        .clone();
    let task = store
        .tasks
        .transition(
            &task.task.task_id,
            1,
            "run",
            Change::StartItem(daily_item.clone()),
        )
        .unwrap();
    let p = Publication {
        publication_id: "compound-day".into(),
        version_id: "legacy-daily".into(),
        task_id: task.task.task_id.clone(),
        item_id: daily_item,
        run_id: "run".into(),
        expected_revision: task.task.state_revision,
        intent: PublishIntent::Daily {
            expected_head: None,
        },
    };
    let plan = Publisher {
        repository: store.clone(),
    }
    .prepare(p)
    .unwrap();
    store.commit(&plan).unwrap();
    let task = store.task(&task.task.task_id).unwrap();
    assert_eq!(task.task.status, x_data_mcp::task_control::Status::Running);
    assert_eq!(task.claims.len(), 2);
    let year_item = task
        .items
        .iter()
        .find(|i| i.spec.target["period"] == "year")
        .unwrap()
        .spec
        .item_id
        .clone();
    let task = store
        .tasks
        .transition(
            &task.task.task_id,
            task.task.state_revision,
            "run",
            Change::StartItem(year_item.clone()),
        )
        .unwrap();
    store
        .stage(&yearly("compound-year", "legacy-daily"))
        .unwrap();
    let p = Publication {
        publication_id: "compound-year-pub".into(),
        version_id: "compound-year".into(),
        task_id: task.task.task_id.clone(),
        item_id: year_item,
        run_id: "run".into(),
        expected_revision: task.task.state_revision,
        intent: PublishIntent::CurrentHead {
            expected_daily_head: "legacy-daily".into(),
            expected_yearly_head: None,
        },
    };
    let plan = Publisher {
        repository: store.clone(),
    }
    .prepare(p)
    .unwrap();
    store.commit(&plan).unwrap();
    let task = store.task(&task.task.task_id).unwrap();
    assert_eq!(
        task.task.status,
        x_data_mcp::task_control::Status::Succeeded
    );
    assert!(task.claims.is_empty());
}
#[test]
fn fixed_base_change_after_prepare_is_rejected_without_recomputing_or_rebinding() {
    let (_dir, store) = db();
    let raw = samples()[0]["record"].clone();
    published_fixture(&store, &raw);
    let plan = prepare(
        &store,
        &yearly("y", "legacy-daily"),
        PublishIntent::HistoricalFixedBase,
    );
    // Independently altered compat/projection with a matching new digest still violates the sealed input.
    let mut changed = raw;
    changed["batch"]["coverage"]["reason"] = json!("changed after validation");
    changed["integrity_sha256"] = json!(frozen_legacy::digest(&changed));
    Connection::open(store.tasks.path()).unwrap().execute("UPDATE data_versions SET envelope_json=json_set(envelope_json,'$.batch.coverage.reason','changed after validation','$.integrity_sha256',?1),content_digest=?1,coverage_reason='changed after validation' WHERE version_id='legacy-daily'",rusqlite::params![changed["integrity_sha256"].as_str().unwrap()]).unwrap();
    assert!(store.fixed("legacy-daily").is_ok());
    assert!(matches!(store.commit(&plan), Err(Error::Base(_))));
    assert!(
        store
            .receipt(&plan.publication().publication_id)
            .unwrap()
            .is_none()
    );
}
#[test]
fn damaged_published_state_or_missing_version_is_not_reported_as_no_data() {
    use x_data_mcp::{market_storage::query::Pages, query::MarketQuery};
    for sql in [
        "UPDATE data_versions SET state='validated'",
        "PRAGMA foreign_keys=OFF; DELETE FROM data_versions;",
    ] {
        let (_dir, store) = db();
        published_fixture(&store, &samples()[0]["record"]);
        Connection::open(store.tasks.path())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        assert!(matches!(
            store.fixed("legacy-daily"),
            Err(Error::Integrity(_))
        ));
        assert!(matches!(store.versions(&scope()), Err(Error::Integrity(_))));
        let args = MarketQuery {
            exchange: "sh".into(),
            code: "600006".into(),
            period: "day".into(),
            start_date: range().start,
            end_date: range().end,
            version_id: None,
            page_size: Some(1),
            cursor: None,
        };
        assert!(matches!(
            Pages::new(store).market(args, chrono::Utc::now()),
            Err(Error::Integrity(_))
        ));
    }
}

#[test]
fn publication_receipt_cannot_alias_another_valid_publication() {
    let (_dir, store) = db();
    let raw = samples()[0]["record"].clone();
    published_fixture(&store, &raw);
    let next = prepare(
        &store,
        &revised(&raw, "next"),
        PublishIntent::Daily {
            expected_head: Some("legacy-daily".into()),
        },
    );
    store.commit(&next).unwrap();
    let c = Connection::open(store.tasks.path()).unwrap();
    c.execute("UPDATE publication_batches SET receipt_json=(SELECT receipt_json FROM publication_batches WHERE publication_id='pub-next') WHERE publication_id='pub-legacy-daily'", []).unwrap();
    assert!(matches!(
        store.receipt("pub-legacy-daily"),
        Err(Error::Integrity(_))
    ));
    assert!(store.receipt("pub-next").unwrap().is_some());
}

#[test]
fn publication_item_projection_tampering_is_rejected() {
    let (_dir, store) = db();
    published_fixture(&store, &samples()[0]["record"]);
    Connection::open(store.tasks.path())
        .unwrap()
        .execute(
            "UPDATE publication_batches SET command_json=json_set(command_json, '$.item_id', 'unrelated-item')",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.fixed("legacy-daily"),
        Err(Error::Integrity(_))
    ));
    assert!(matches!(
        store.receipt("pub-legacy-daily"),
        Err(Error::Integrity(_))
    ));
}
#[test]
fn segmented_storage_shares_unchanged_bars_and_materializes_only_delta() {
    let (_dir, store) = db();
    let raw = samples()[0]["record"].clone();
    published_fixture(&store, &raw);
    let mut r2 = raw.clone();
    r2["version_id"] = json!("daily-2");
    r2["records"][1]["close"] = json!("10.06");
    r2["batch"]["bars"][1]["close"] = json!("10.06");
    r2["integrity_sha256"] = json!(frozen_legacy::digest(&r2));
    let plan = prepare(
        &store,
        &r2,
        PublishIntent::Daily {
            expected_head: Some("legacy-daily".into()),
        },
    );
    store.commit(&plan).unwrap();
    let c = Connection::open(store.tasks.path()).unwrap();
    // Only the changed bar was materialized: 3 legacy bars + 1 delta row.
    let bar_rows: i64 = c
        .query_row("SELECT count(*) FROM segment_bars", [], |r| r.get(0))
        .unwrap();
    assert_eq!(bar_rows, 4);
    let segments: i64 = c
        .query_row("SELECT count(*) FROM bar_segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(segments, 2);
    // daily-2 = ref(bar0) + new(bar1) + ref(bar2).
    let refs: Vec<(i64, i64, String, String)> = c
        .prepare(
            "SELECT segment_id,seq,clip_start,clip_end FROM version_segments WHERE version_id='daily-2' ORDER BY seq",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let legacy_seg: i64 = c
        .query_row(
            "SELECT segment_id FROM version_segments WHERE version_id='legacy-daily'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(refs.len(), 3);
    assert_eq!(refs[0].0, legacy_seg);
    assert_eq!(refs[2].0, legacy_seg);
    assert_ne!(refs[1].0, legacy_seg);
    // Rebuilt version reads verify against the recorded projection.
    store.fixed("daily-2").unwrap();
    let mut r = range();
    r.start = "2024-01-01".parse().unwrap();
    r.limit = 10;
    let page = store.range(&scope(), Some("daily-2"), &r).unwrap();
    assert_eq!(page.rows.len(), 3);
    assert_eq!(page.rows[1].close, "10.06".parse().unwrap());
}
