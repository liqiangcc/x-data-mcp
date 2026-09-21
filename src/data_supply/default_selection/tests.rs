//! Resolver tests: real store + real acceptance/transition paths where the
//! contract family matters, raw seeding only for families the resolver must
//! ignore or for corrupt-row fixtures.
use super::*;
use crate::{
    catalog::{
        CompletenessEvidence, Exchange as CatalogExchange, IndustryInfo, SecurityRecord,
        SecurityType,
    },
    data_supply::{collect::CollectionContract, wire::Dataset},
    sqlite_catalog::{SqliteCatalog, selection_facts_on},
    sqlite_market::SqliteMarket,
    sqlite_supply::SqliteSupply,
    sqlite_tasks::{SqliteTasks, check_schema},
    task_control::{
        self as task, Change, CommandKey, Contract, RequestOutcome, Status, TaskRepository,
        legacy::LegacyCollection, service::TaskControl,
    },
    task_verification::{
        StopVerification,
        decision::{self, Observation, StopInspector, StopRepository},
    },
};
use chrono::Utc;
use rusqlite::{Connection, params};
use serde_json::json;
use std::collections::BTreeSet;
fn open(dir: &tempfile::TempDir) -> (SqliteTasks, SqliteCatalog) {
    let tasks = SqliteTasks::new(dir.path().join("t.sqlite"));
    tasks.initialize().unwrap();
    let catalog = SqliteCatalog::new(tasks.clone());
    (tasks, catalog)
}
fn member(code: &str, exchange: CatalogExchange) -> SecurityRecord {
    SecurityRecord {
        symbol: code.into(),
        name: code.into(),
        exchange,
        security_type: SecurityType::AShare,
        source_security_id: format!("src-{code}"),
        industry: IndustryInfo::Missing {
            reason: "fixture".into(),
        },
        source: "fixture:catalog".into(),
        acquired_at: Utc::now(),
    }
}
fn publish(
    catalog: &SqliteCatalog,
    id: &str,
    members: &[(&str, CatalogExchange)],
    expected: Option<&str>,
) {
    let records = members
        .iter()
        .map(|(code, e)| member(code, e.clone()))
        .collect::<Vec<_>>();
    catalog
        .publish_fixture(
            id,
            crate::catalog::CatalogSnapshot {
                source: "fixture:catalog".into(),
                acquired_at: Utc::now(),
                completeness: CompletenessEvidence::Complete {
                    expected_count: records.len(),
                },
                records,
            },
            expected,
        )
        .unwrap();
}
fn ctl(db: &SqliteTasks, run: &str) -> TaskControl<SqliteTasks> {
    TaskControl::new(
        db.clone(),
        run.into(),
        vec![
            Box::new(CollectionContract {
                dataset: Dataset::Daily,
                repository: SqliteSupply {
                    market: SqliteMarket::new(db.clone()),
                },
            }),
            Box::new(LegacyCollection),
        ],
    )
    .unwrap()
}
fn accept_typed(
    c: &TaskControl<SqliteTasks>,
    request: &str,
    codes: &[(&str, &str)],
) -> (String, Vec<String>) {
    let key = CommandKey {
        operation: "start_collection".into(),
        dataset: "market.daily".into(),
        contract_version: "v1".into(),
    };
    let securities: Vec<Value> = codes
        .iter()
        .map(|(ex, code)| json!({"exchange":ex,"code":code}))
        .collect();
    let RequestOutcome::Task(t) = c
        .accept(
            request,
            key,
            json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":securities}}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    (
        t.task.task_id.clone(),
        t.items.iter().map(|i| i.spec.item_id.clone()).collect(),
    )
}
fn accept_legacy(
    c: &TaskControl<SqliteTasks>,
    request: &str,
    symbols: &[&str],
    periods: &[&str],
) -> (String, Vec<String>) {
    let RequestOutcome::Task(t) = c
        .accept(
            request,
            LegacyCollection.key(),
            json!({"symbols":symbols,"periods":periods}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    (
        t.task.task_id.clone(),
        t.items.iter().map(|i| i.spec.item_id.clone()).collect(),
    )
}
fn start(tasks: &SqliteTasks, run: &str, task_id: &str, item_id: &str) {
    let s = tasks.transition(task_id, 0, run, Change::Claim).unwrap();
    tasks
        .transition(
            task_id,
            s.task.state_revision,
            run,
            Change::StartItem(item_id.into()),
        )
        .unwrap();
}
/// Seed a bound task row + finished item + start marker without acceptance:
/// owner non-null, succeeded+success consistent, and a digest-matched request
/// row (flattened `retry_task` key when retry_of is set) so the acceptance
/// orphan/binding checks keep passing. Only for contract families the resolver
/// must ignore, or corrupt-row fixtures.
fn seed_marker(
    db: &SqliteTasks,
    task_id: &str,
    key_json: &str,
    item_id: &str,
    retry_of: Option<&str>,
) {
    let c = Connection::open(db.path()).unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    // frozen_work.items must contain the exact ItemSpec the task_items row
    // decodes to: bound-task reads cross-check membership.
    let spec = json!({"item_id":item_id,"target":{},"progress_unit":"u","frozen_inputs":{},"scope_keys":["s"]});
    let work = json!({"items":[spec],"scopes":["s"]}).to_string();
    c.execute(
        "INSERT INTO tasks VALUES(?1,?2,'succeeded',3,'seed-run','seed-run','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z',?3,'{}',?4,1,0,0,1,1,NULL)",
        params![task_id, key_json, work, retry_of],
    )
    .unwrap();
    // unparseable keys (corrupt fixtures) cannot produce a consistent request
    // row; those fixtures never call accept, so the orphan sweep never runs
    if let Ok(mut key) = serde_json::from_str::<Value>(key_json) {
        if retry_of.is_some() {
            key["operation"] = json!("retry_task");
        }
        let command = json!({"key":key,"intent":{}}).to_string();
        c.execute(
            "INSERT INTO requests VALUES(?1,?2,?3,'task',?4,NULL)",
            params![
                format!("seedreq-{task_id}"),
                command,
                crate::hash::sha256(command.as_bytes()),
                task_id
            ],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO task_items VALUES(?1,?2,'{}','u','{}','[\"s\"]','finished','{\"success\":true}')",
        params![task_id, item_id],
    )
    .unwrap();
    c.execute("INSERT INTO task_scopes VALUES(?1,'s')", params![task_id])
        .unwrap();
    c.execute(
        "INSERT INTO task_item_execution_starts VALUES(?1,?2,'seed-run',2,'2026-01-01T00:00:00Z')",
        params![task_id, item_id],
    )
    .unwrap();
}
const TYPED_KEY: &str =
    "{\"operation\":\"start_collection\",\"dataset\":\"market.daily\",\"contract_version\":\"v1\"}";
fn resolve(catalog: &SqliteCatalog) -> Result<DefaultDailySelection, SelectionError> {
    resolve_default_daily(catalog)
}
#[test]
fn typed_selection_is_catalog_intersect_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[
            ("600006", CatalogExchange::Shanghai),
            ("600007", CatalogExchange::Shanghai),
            ("000001", CatalogExchange::Shenzhen),
        ],
        None,
    );
    let c = ctl(&tasks, "run-a");
    let (ta, ia) = accept_typed(&c, "req-a", &[("sh", "600006")]);
    start(&tasks, "run-a", &ta, &ia[0]);
    let (tb, ib) = accept_typed(&c, "req-b", &[("sh", "600007")]);
    start(&tasks, "run-a", &tb, &ib[0]);
    let s = resolve(&catalog).unwrap();
    assert_eq!(s.securities, vec!["sh.600006", "sh.600007"]);
    assert_eq!(s.catalog_version_id, "v1");
    assert!(!s.catalog_digest.is_empty());
    assert_eq!(s.starts["sh.600006"][0].task_id, ta);
    assert_eq!(s.starts["sh.600006"][0].item_id, ia[0]);
    assert_eq!(s.starts["sh.600006"][0].run_id, "run-a");
}
#[test]
fn legacy_day_counts_and_legacy_year_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[
            ("600006", CatalogExchange::Shanghai),
            ("600007", CatalogExchange::Shanghai),
        ],
        None,
    );
    let c = ctl(&tasks, "run-a");
    let (ta, ia) = accept_legacy(&c, "req-a", &["sh.600006"], &["day"]);
    start(&tasks, "run-a", &ta, &ia[0]);
    let (tb, ib) = accept_legacy(&c, "req-b", &["sh.600007"], &["day", "year"]);
    let year = ib.iter().find(|i| i.contains("\"year\"")).unwrap();
    start(&tasks, "run-a", &tb, year);
    let s = resolve(&catalog).unwrap();
    assert_eq!(s.securities, vec!["sh.600006"]);
}
#[test]
fn typed_non_day_marker_is_inconsistency() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[("600006", CatalogExchange::Shanghai)],
        None,
    );
    seed_marker(
        &tasks,
        "t1",
        TYPED_KEY,
        "[\"market\",\"sh.600006\",\"year\"]",
        None,
    );
    assert!(resolve(&catalog).is_err());
}
#[test]
fn retry_of_collection_counts_but_retry_of_update_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[
            ("600006", CatalogExchange::Shanghai),
            ("600007", CatalogExchange::Shanghai),
        ],
        None,
    );
    let c = ctl(&tasks, "run-a");
    // original task cancelled before its item ever started
    let (orig, items) = accept_typed(&c, "req-orig", &[("sh", "600006")]);
    let item = items[0].clone();
    let s = tasks.read_task(&orig).unwrap().unwrap();
    let s = tasks
        .transition(&orig, s.task.state_revision, "run-a", Change::RequestCancel)
        .unwrap();
    let s = tasks
        .transition(
            &orig,
            s.task.state_revision,
            "run-a",
            Change::FinishCancellation,
        )
        .unwrap();
    assert_eq!(s.task.status, Status::Cancelled);
    // first real execution happens inside the retry task
    let RequestOutcome::Task(rt) = c
        .retry(&orig, "req-retry", BTreeSet::from([item.clone()]))
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    start(&tasks, "run-a", &rt.task.task_id, &item);
    // a retry task flattened to update_data must not count
    seed_marker(
        &tasks,
        "upd-parent",
        "{\"operation\":\"update_data\",\"dataset\":\"market.daily\",\"contract_version\":\"explicit-range-task-v1\"}",
        "[\"market\",\"sh.600007\",\"day\"]",
        None,
    );
    seed_marker(
        &tasks,
        "upd-retry",
        "{\"operation\":\"update_data\",\"dataset\":\"market.daily\",\"contract_version\":\"explicit-range-task-v1\"}",
        "[\"market\",\"sh.600007\",\"day\"]",
        Some("upd-parent"),
    );
    let s = resolve(&catalog).unwrap();
    assert_eq!(s.securities, vec!["sh.600006"]);
    let proof = &s.starts["sh.600006"][0];
    assert_eq!(proof.task_id, rt.task.task_id);
    assert_eq!(proof.retry_of.as_deref(), Some(orig.as_str()));
}
#[test]
fn other_contract_families_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[
            ("600006", CatalogExchange::Shanghai),
            ("600007", CatalogExchange::Shanghai),
            ("600008", CatalogExchange::Shanghai),
            ("600009", CatalogExchange::Shanghai),
        ],
        None,
    );
    seed_marker(
        &tasks,
        "t-upd",
        "{\"operation\":\"update_data\",\"dataset\":\"market.daily\",\"contract_version\":\"explicit-range-task-v1\"}",
        "[\"market\",\"sh.600006\",\"day\"]",
        None,
    );
    seed_marker(
        &tasks,
        "t-yearly",
        "{\"operation\":\"start_collection\",\"dataset\":\"market.yearly\",\"contract_version\":\"v1\"}",
        "[\"market\",\"sh.600007\",\"year\"]",
        None,
    );
    seed_marker(
        &tasks,
        "t-verify",
        "{\"operation\":\"verify_stopped_writes\",\"dataset\":\"task_verification\",\"contract_version\":\"v1\"}",
        "stopped-writes",
        None,
    );
    assert!(matches!(
        resolve(&catalog),
        Err(SelectionError::EmptySelection)
    ));
    let c = ctl(&tasks, "run-a");
    let (ta, ia) = accept_typed(&c, "req-a", &[("sh", "600009")]);
    start(&tasks, "run-a", &ta, &ia[0]);
    assert_eq!(resolve(&catalog).unwrap().securities, vec!["sh.600009"]);
}
#[test]
fn outside_catalog_and_unstarted_members_are_excluded() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[
            ("600006", CatalogExchange::Shanghai),
            ("600007", CatalogExchange::Shanghai),
        ],
        None,
    );
    let c = ctl(&tasks, "run-a");
    let (ta, ia) = accept_typed(&c, "req-a", &[("sh", "600006")]);
    start(&tasks, "run-a", &ta, &ia[0]);
    // started security outside the catalog must not enter the selection
    let (tx, ix) = accept_typed(&c, "req-x", &[("sz", "000002")]);
    start(&tasks, "run-a", &tx, &ix[0]);
    let s = resolve(&catalog).unwrap();
    assert_eq!(s.securities, vec!["sh.600006"]);
    assert!(!s.starts.contains_key("sz.000002"));
    assert!(!s.starts.contains_key("sh.600007"));
}
#[test]
fn failed_cancelled_and_interrupted_tasks_keep_their_marker() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[
            ("600006", CatalogExchange::Shanghai),
            ("600007", CatalogExchange::Shanghai),
            ("600008", CatalogExchange::Shanghai),
        ],
        None,
    );
    let c = ctl(&tasks, "run-a");
    // failed
    let (tf, it) = accept_typed(&c, "req-f", &[("sh", "600006")]);
    start(&tasks, "run-a", &tf, &it[0]);
    let s = tasks.read_task(&tf).unwrap().unwrap();
    let s = tasks
        .transition(
            &tf,
            s.task.state_revision,
            "run-a",
            Change::FinishItem(
                it[0].clone(),
                task::ItemOutcome {
                    success: false,
                    error: Some("fixture".into()),
                    evidence: None,
                },
            ),
        )
        .unwrap();
    let s = tasks
        .transition(&tf, s.task.state_revision, "run-a", Change::FinishExecution)
        .unwrap();
    assert_eq!(s.task.status, Status::Failed);
    // cancelled while running
    let (tc, ic) = accept_typed(&c, "req-c", &[("sh", "600007")]);
    start(&tasks, "run-a", &tc, &ic[0]);
    let s = tasks.read_task(&tc).unwrap().unwrap();
    let s = tasks
        .transition(&tc, s.task.state_revision, "run-a", Change::RequestCancel)
        .unwrap();
    let s = tasks
        .transition(
            &tc,
            s.task.state_revision,
            "run-a",
            Change::FinishItem(
                ic[0].clone(),
                task::ItemOutcome {
                    success: false,
                    error: Some("stopped".into()),
                    evidence: None,
                },
            ),
        )
        .unwrap();
    let s = tasks
        .transition(
            &tc,
            s.task.state_revision,
            "run-a",
            Change::FinishCancellation,
        )
        .unwrap();
    assert_eq!(s.task.status, Status::Cancelled);
    // verified interruption resets the running item back to pending
    let (ti, ii) = accept_typed(&c, "req-i", &[("sh", "600008")]);
    start(&tasks, "run-a", &ti, &ii[0]);
    let target = tasks.read_task(&ti).unwrap().unwrap();
    let contract = StopVerification::new(tasks.clone(), "new".into()).unwrap();
    let key = contract.key();
    let ctl2 = TaskControl::new(tasks.clone(), "new".into(), vec![Box::new(contract)]).unwrap();
    let RequestOutcome::Task(check) = ctl2
        .accept("check", key, json!({"target_task_id": ti}))
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let s = tasks
        .transition(&check.task.task_id, 0, "new", Change::Claim)
        .unwrap();
    let s = tasks
        .transition(
            &check.task.task_id,
            s.task.state_revision,
            "new",
            Change::StartItem("stopped-writes".into()),
        )
        .unwrap();
    struct Stopped;
    impl StopInspector for Stopped {
        fn inspect(&self, s: &task::TaskSnapshot) -> Result<Observation, task::Error> {
            Ok(Observation::Stopped {
                snapshot_sha256: decision::snapshot_digest(s)?,
                evidence_ref: "fixture".into(),
            })
        }
    }
    let plan = decision::prepare(
        s,
        target,
        "new",
        "actor",
        "2026-09-13T00:00:00Z".parse().unwrap(),
        &Stopped,
    )
    .unwrap();
    tasks.commit_stop_check(&plan).unwrap();
    let after = tasks.read_task(&ti).unwrap().unwrap();
    assert_eq!(after.task.status, Status::Interrupted);
    assert_eq!(after.items[0].stage, task::Stage::Pending);
    let s = resolve(&catalog).unwrap();
    assert_eq!(s.securities, vec!["sh.600006", "sh.600007", "sh.600008"]);
}
#[test]
fn upgraded_v9_tasks_without_markers_do_not_count() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");
    const V9: [&str; 9] = [
        include_str!("../../../migrations/tasks/001.sql"),
        include_str!("../../../migrations/tasks/002.sql"),
        include_str!("../../../migrations/tasks/003_market.sql"),
        include_str!("../../../migrations/tasks/004_catalog.sql"),
        include_str!("../../../migrations/tasks/005_update_results.sql"),
        include_str!("../../../migrations/tasks/006_yearly_results.sql"),
        include_str!("../../../migrations/tasks/007_stopped_checks.sql"),
        include_str!("../../../migrations/tasks/008_publication_checks.sql"),
        include_str!("../../../migrations/tasks/009_verification_inspections.sql"),
    ];
    {
        let c = Connection::open(&path).unwrap();
        c.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        for (i, sql) in V9.iter().enumerate() {
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
        // pre-upgrade first-collection task that finished a day item: no marker exists
        let spec = json!({"item_id":"[\"market\",\"sh.600007\",\"day\"]","target":{},"progress_unit":"u","frozen_inputs":{},"scope_keys":["s"]});
        let work = json!({"items":[spec],"scopes":["s"]}).to_string();
        c.execute(
            "INSERT INTO tasks VALUES('v9-task',?1,'succeeded',3,'old-run','old-run','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z',?2,'{}',NULL)",
            params![TYPED_KEY, work],
        )
        .unwrap();
        let command = json!({"key":serde_json::from_str::<Value>(TYPED_KEY).unwrap(),"intent":{}})
            .to_string();
        c.execute(
            "INSERT INTO requests VALUES('v9-req',?1,?2,'task','v9-task',NULL)",
            params![command, crate::hash::sha256(command.as_bytes())],
        )
        .unwrap();
        c.execute(
            "INSERT INTO task_items VALUES('v9-task','[\"market\",\"sh.600007\",\"day\"]','{}','u','{}','[\"s\"]','finished','{\"success\":true}')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO task_scopes VALUES('v9-task','s')", [])
            .unwrap();
    }
    let tasks = SqliteTasks::new(&path);
    tasks.upgrade().unwrap();
    let catalog = SqliteCatalog::new(tasks.clone());
    publish(
        &catalog,
        "v1",
        &[
            ("600006", CatalogExchange::Shanghai),
            ("600007", CatalogExchange::Shanghai),
        ],
        None,
    );
    let c = ctl(&tasks, "run-a");
    let (ta, ia) = accept_typed(&c, "req-a", &[("sh", "600006")]);
    start(&tasks, "run-a", &ta, &ia[0]);
    // the v9-era finished item proves completion, never start: excluded
    assert_eq!(resolve(&catalog).unwrap().securities, vec!["sh.600006"]);
}
#[test]
fn corrupt_command_key_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[("600006", CatalogExchange::Shanghai)],
        None,
    );
    seed_marker(
        &tasks,
        "bad",
        "{corrupt",
        "[\"market\",\"sh.600006\",\"day\"]",
        None,
    );
    assert!(resolve(&catalog).is_err());
}
#[test]
fn malformed_item_id_fails_closed() {
    for bad in [
        "not-json",
        "[\"market\",\"sh.600006\"]",
        "[\"catalog\",\"sh.600006\",\"day\"]",
        "[\"market\",\"sh.600006\",\"week\"]",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (tasks, catalog) = open(&dir);
        publish(
            &catalog,
            "v1",
            &[("600006", CatalogExchange::Shanghai)],
            None,
        );
        seed_marker(&tasks, "bad", TYPED_KEY, bad, None);
        assert!(resolve(&catalog).is_err(), "{bad}");
    }
}
#[test]
fn missing_or_corrupt_catalog_fails_closed() {
    // no catalog head at all
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    let c = ctl(&tasks, "run-a");
    let (ta, ia) = accept_typed(&c, "req-a", &[("sh", "600006")]);
    start(&tasks, "run-a", &ta, &ia[0]);
    assert!(resolve(&catalog).is_err());
    // corrupted catalog payload fails integrity inside the same snapshot
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[("600006", CatalogExchange::Shanghai)],
        None,
    );
    Connection::open(tasks.path())
        .unwrap()
        .execute("UPDATE catalog_versions SET compat_json='{}'", [])
        .unwrap();
    assert!(resolve(&catalog).is_err());
}
#[test]
fn facts_come_from_one_snapshot_while_another_connection_commits() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[("600006", CatalogExchange::Shanghai)],
        None,
    );
    let c = ctl(&tasks, "run-a");
    let (ta, ia) = accept_typed(&c, "req-a", &[("sh", "600006")]);
    start(&tasks, "run-a", &ta, &ia[0]);
    let mut c1 = tasks.connect(true).unwrap();
    let tx = c1.transaction().unwrap();
    check_schema(&tx, true).unwrap();
    let before = selection_facts_on(&tx).unwrap();
    // a second connection switches the head and adds a marker mid-snapshot
    publish(
        &catalog,
        "v2",
        &[
            ("600006", CatalogExchange::Shanghai),
            ("600007", CatalogExchange::Shanghai),
        ],
        Some("v1"),
    );
    seed_marker(
        &tasks,
        "t2",
        TYPED_KEY,
        "[\"market\",\"sh.600007\",\"day\"]",
        None,
    );
    let inside = selection_facts_on(&tx).unwrap();
    assert_eq!(before, inside);
    assert_eq!(inside.catalog_head.as_deref(), Some("v1"));
    assert_eq!(inside.starts.len(), 1);
    tx.commit().unwrap();
    let fresh = catalog.selection_facts().unwrap();
    assert_eq!(fresh.catalog_head.as_deref(), Some("v2"));
    assert_eq!(fresh.starts.len(), 2);
}
#[test]
fn evidence_is_deterministic_and_covers_every_proof() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[("600006", CatalogExchange::Shanghai)],
        None,
    );
    let c = ctl(&tasks, "run-a");
    // two qualifying proofs for the same security: original cancelled, retry started
    let (orig, items) = accept_typed(&c, "req-1", &[("sh", "600006")]);
    let item = items[0].clone();
    let s = tasks.read_task(&orig).unwrap().unwrap();
    let s = tasks
        .transition(&orig, s.task.state_revision, "run-a", Change::RequestCancel)
        .unwrap();
    tasks
        .transition(
            &orig,
            s.task.state_revision,
            "run-a",
            Change::FinishCancellation,
        )
        .unwrap();
    let RequestOutcome::Task(r1) = c
        .retry(&orig, "req-r1", BTreeSet::from([item.clone()]))
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    start(&tasks, "run-a", &r1.task.task_id, &item);
    // retry task itself finishes failed, then a second retry starts again
    let s = tasks.read_task(&r1.task.task_id).unwrap().unwrap();
    let s = tasks
        .transition(
            &r1.task.task_id,
            s.task.state_revision,
            "run-a",
            Change::FinishItem(
                item.clone(),
                task::ItemOutcome {
                    success: false,
                    error: Some("again".into()),
                    evidence: None,
                },
            ),
        )
        .unwrap();
    let s = tasks
        .transition(
            &r1.task.task_id,
            s.task.state_revision,
            "run-a",
            Change::FinishExecution,
        )
        .unwrap();
    assert_eq!(s.task.status, Status::Failed);
    let RequestOutcome::Task(r2) = c
        .retry(&r1.task.task_id, "req-r2", BTreeSet::from([item.clone()]))
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    start(&tasks, "run-a", &r2.task.task_id, &item);
    let a = resolve(&catalog).unwrap();
    let b = resolve(&catalog).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.securities, vec!["sh.600006"]);
    let proofs = &a.starts["sh.600006"];
    assert_eq!(proofs.len(), 2);
    let ids: BTreeSet<&str> = proofs.iter().map(|p| p.task_id.as_str()).collect();
    assert_eq!(
        ids,
        BTreeSet::from([r1.task.task_id.as_str(), r2.task.task_id.as_str()])
    );
    let parents: BTreeSet<Option<&str>> = proofs.iter().map(|p| p.retry_of.as_deref()).collect();
    assert_eq!(
        parents,
        BTreeSet::from([Some(orig.as_str()), Some(r1.task.task_id.as_str())])
    );
    let mut sorted = proofs.clone();
    sorted.sort();
    assert_eq!(*proofs, sorted);
}
#[test]
fn resolution_is_read_only_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog) = open(&dir);
    publish(
        &catalog,
        "v1",
        &[("600006", CatalogExchange::Shanghai)],
        None,
    );
    let c = ctl(&tasks, "run-a");
    let (ta, ia) = accept_typed(&c, "req-a", &[("sh", "600006")]);
    start(&tasks, "run-a", &ta, &ia[0]);
    let count = |table: &str| -> i64 {
        Connection::open(tasks.path())
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    let tables = [
        "tasks",
        "task_items",
        "requests",
        "scope_occupancy",
        "task_item_execution_starts",
    ];
    let before: Vec<i64> = tables.iter().map(|t| count(t)).collect();
    let _ = resolve(&catalog).unwrap();
    let after: Vec<i64> = tables.iter().map(|t| count(t)).collect();
    assert_eq!(before, after);
}
