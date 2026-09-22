//! Resolver tests over real store state: real catalog publish, real legacy
//! collection acceptance (which writes the execution-start marker), and real
//! daily publication. Only proven-gap rows are written through the fact API;
//! nothing here fabricates selection inputs.
use super::*;
use crate::{
    catalog::{
        CatalogSnapshot, CompletenessEvidence, Exchange as CatalogExchange, IndustryInfo,
        SecurityRecord, SecurityType,
    },
    market_storage::{
        GapCovering, MarketRepository, ProvenGap, Publication, PublishIntent, Publisher,
    },
    market_update::publication::RangeRepository,
    sqlite_catalog::SqliteCatalog,
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
};
use chrono::{NaiveDate, Utc};
use rusqlite::Connection;
use serde_json::{Value, json};
/// After the fixture record's last bar (2025-12-31).
const TODAY: &str = "2026-01-15";
fn today() -> NaiveDate {
    TODAY.parse().unwrap()
}
fn open(dir: &tempfile::TempDir) -> (SqliteTasks, SqliteCatalog, SqliteMarket) {
    let tasks = SqliteTasks::new(dir.path().join("t.sqlite"));
    tasks.initialize().unwrap();
    (
        tasks.clone(),
        SqliteCatalog::new(tasks.clone()),
        SqliteMarket::new(tasks),
    )
}
fn publish_catalog(catalog: &SqliteCatalog, id: &str, codes: &[&str]) {
    publish_catalog_expected(catalog, id, codes, None);
}
fn publish_catalog_expected(
    catalog: &SqliteCatalog,
    id: &str,
    codes: &[&str],
    expected: Option<&str>,
) {
    let records = codes
        .iter()
        .map(|code| SecurityRecord {
            symbol: (*code).into(),
            name: (*code).into(),
            exchange: CatalogExchange::Shanghai,
            security_type: SecurityType::AShare,
            source_security_id: (*code).into(),
            industry: IndustryInfo::Missing {
                reason: "fixture".into(),
            },
            source: "fixture:catalog".into(),
            acquired_at: Utc::now(),
        })
        .collect::<Vec<_>>();
    catalog
        .publish_fixture(
            id,
            CatalogSnapshot {
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
/// Real daily publication for `sh.<code>`: legacy collection acceptance
/// (writes the daily execution-start marker), claim, start, publish, head
/// promotion. Bars reuse the fixture record's dates unchanged.
fn publish_daily(db: &SqliteMarket, ctl: &TaskControl<SqliteTasks>, code: &str, seq: usize) {
    let symbol = format!("sh.{code}");
    let version_id = format!("daily-{code}");
    let samples: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/issue79/legacy-records.json"
    ))
    .unwrap();
    let mut raw = samples[0]["record"].clone();
    raw["version_id"] = json!(version_id);
    raw["symbol"] = json!(symbol);
    raw["batch"]["symbol"] = json!(symbol);
    raw["batch"]["provenance"]["symbol"] = json!(symbol);
    raw["integrity_sha256"] = json!(crate::market_storage::compat::legacy_digest(&raw).unwrap());
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
        .transition(&s.task.task_id, 0, "run", Change::Claim)
        .unwrap();
    let s = db
        .tasks
        .transition(
            &s.task.task_id,
            s.task.state_revision,
            "run",
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
        run_id: "run".into(),
        expected_revision: s.task.state_revision,
        intent: PublishIntent::Daily {
            expected_head: None,
        },
    })
    .unwrap();
    db.commit(&plan).unwrap();
}
/// Marker without publication: real legacy acceptance + claim + start, no
/// commit, so the symbol is ever-started but has no effective head.
fn start_only(db: &SqliteMarket, ctl: &TaskControl<SqliteTasks>, code: &str) {
    let accepted = ctl
        .accept(
            &format!("req-start-{code}"),
            LegacyCollection.key(),
            json!({"symbols":[format!("sh.{code}")],"periods":["day"]}),
        )
        .unwrap();
    let RequestOutcome::Task(s) = accepted.record.outcome else {
        panic!()
    };
    let s = db
        .tasks
        .transition(&s.task.task_id, 0, "run", Change::Claim)
        .unwrap();
    db.tasks
        .transition(
            &s.task.task_id,
            s.task.state_revision,
            "run",
            Change::StartItem(s.items[0].spec.item_id.clone()),
        )
        .unwrap();
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
fn resolve(
    db: &SqliteMarket,
    op: DefaultOperation,
    today: NaiveDate,
) -> Result<DefaultWork, SelectionError> {
    let c = Connection::open(db.tasks.path()).unwrap();
    resolve_default_work_on(&c, op, today)
}
/// Acceptance control carrying the default contracts plus the legacy
/// collection the fixtures need for execution-start markers.
fn acceptance_ctl(
    tasks: &SqliteTasks,
    db: &SqliteMarket,
    clock: fn() -> NaiveDate,
) -> TaskControl<SqliteTasks> {
    TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![
            Box::new(LegacyCollection),
            Box::new(DefaultRangeContract {
                operation: crate::market_update::tasks::Operation::Update,
                resolver: db.clone(),
                today: clock,
            }),
            Box::new(DefaultRangeContract {
                operation: crate::market_update::tasks::Operation::Backfill,
                resolver: db.clone(),
                today: clock,
            }),
        ],
    )
    .unwrap()
}
fn default_key(op: crate::market_update::tasks::Operation, db: &SqliteMarket) -> task::CommandKey {
    DefaultRangeContract {
        operation: op,
        resolver: db.clone(),
        today,
    }
    .key()
}
fn occupancy_and_request_rows(db: &SqliteMarket, request_id: &str) -> (i64, i64) {
    let c = Connection::open(db.tasks.path()).unwrap();
    let requests: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM requests WHERE request_id=?1",
            [request_id],
            |r| r.get(0),
        )
        .unwrap();
    let occupancy: i64 = c
        .query_row("SELECT COUNT(*) FROM scope_occupancy", [], |r| r.get(0))
        .unwrap();
    (requests, occupancy)
}
fn intent() -> Value {
    json!({"selection":{"kind":"default"}})
}
#[test]
fn update_freezes_refresh_range_base_identity_and_gap_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let ctl = TaskControl::new(tasks, "run".into(), vec![Box::new(LegacyCollection)]).unwrap();
    publish_daily(&db, &ctl, "600006", 0);
    db.record_proven_gap(&gap("g1", "sh.600006", "2025-02-01", "2025-02-10"))
        .unwrap();
    let mut second = gap("g2", "sh.600006", "2025-01-05", "2025-01-06");
    second.evidence_digest = "f".repeat(64);
    db.record_proven_gap(&second).unwrap();
    let work = resolve(&db, DefaultOperation::Update, today()).unwrap();
    assert_eq!(work.selection.securities, ["sh.600006"]);
    assert_eq!(work.items.len(), 1);
    let item = &work.items[0];
    assert_eq!(item.base_version_id, "daily-600006");
    assert_eq!(item.base_integrity_sha256.len(), 64);
    // refresh range spans [base.first_date, injected today]; the fixture
    // record's first bar is 2024-12-31.
    assert_eq!(
        item.refresh_range,
        Some(FrozenRange {
            start_date: "2024-12-31".parse().unwrap(),
            end_date: today(),
        })
    );
    // gap provenance preserved per gap, ordered, never coalesced into refresh.
    assert_eq!(item.proven_gap_ranges.len(), 2);
    assert_eq!(item.proven_gap_ranges[0].gap_id, "g2");
    assert_eq!(item.proven_gap_ranges[0].evidence_digest, "f".repeat(64));
    assert_eq!(
        item.proven_gap_ranges[0].range,
        FrozenRange {
            start_date: "2025-01-05".parse().unwrap(),
            end_date: "2025-01-06".parse().unwrap()
        }
    );
    assert_eq!(item.proven_gap_ranges[1].gap_id, "g1");
    assert_eq!(item.proven_gap_ranges[1].evidence_digest, "e".repeat(64));
}
#[test]
fn covered_gaps_leave_the_open_set() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let ctl = TaskControl::new(tasks, "run".into(), vec![Box::new(LegacyCollection)]).unwrap();
    publish_daily(&db, &ctl, "600006", 0);
    db.record_proven_gap(&gap("g1", "sh.600006", "2025-02-01", "2025-02-10"))
        .unwrap();
    db.record_proven_gap(&gap("g2", "sh.600006", "2025-01-05", "2025-01-06"))
        .unwrap();
    db.cover_gap(&GapCovering {
        gap_id: "g1".into(),
        version_id: "daily-600006".into(),
        covered_at: Utc::now(),
    })
    .unwrap();
    let work = resolve(&db, DefaultOperation::Update, today()).unwrap();
    assert_eq!(work.items[0].proven_gap_ranges.len(), 1);
    assert_eq!(work.items[0].proven_gap_ranges[0].gap_id, "g2");
}
#[test]
fn missing_or_corrupt_head_fails_the_whole_batch() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006", "600007"]);
    let ctl = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    publish_daily(&db, &ctl, "600006", 0);
    start_only(&db, &ctl, "600007");
    assert!(matches!(
        resolve(&db, DefaultOperation::Update, today()),
        Err(SelectionError::Invalid(_))
    ));
    // the still-valid head does not rescue the batch: no silent skip
    let tasks2 = SqliteTasks::new(dir.path().join("t2.sqlite"));
    tasks2.initialize().unwrap();
    let catalog2 = SqliteCatalog::new(tasks2.clone());
    let db2 = SqliteMarket::new(tasks2);
    publish_catalog(&catalog2, "c1", &["600006"]);
    let ctl2 = TaskControl::new(
        db2.tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    publish_daily(&db2, &ctl2, "600006", 0);
    Connection::open(db2.tasks.path())
        .unwrap()
        .execute(
            "UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','0')",
            [],
        )
        .unwrap();
    assert!(matches!(
        resolve(&db2, DefaultOperation::Update, today()),
        Err(SelectionError::Invalid(_))
    ));
}
#[test]
fn refresh_range_guards_are_fail_closed() {
    let t = today();
    assert!(refresh_range("s", None, Some(t), t).is_err());
    assert!(refresh_range("s", Some(t), None, t).is_err());
    assert!(
        refresh_range(
            "s",
            Some("2025-02-01".parse().unwrap()),
            Some("2025-01-01".parse().unwrap()),
            t
        )
        .is_err()
    );
    assert!(
        refresh_range(
            "s",
            Some("2025-01-01".parse().unwrap()),
            Some("2026-02-01".parse().unwrap()),
            t
        )
        .is_err()
    );
    assert_eq!(
        refresh_range(
            "s",
            Some("2025-01-01".parse().unwrap()),
            Some("2026-02-01".parse().unwrap()),
            "2026-02-01".parse().unwrap()
        )
        .unwrap()
        .end_date,
        "2026-02-01".parse().unwrap()
    );
}
#[test]
fn base_last_date_after_acceptance_day_rejects() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let ctl = TaskControl::new(tasks, "run".into(), vec![Box::new(LegacyCollection)]).unwrap();
    publish_daily(&db, &ctl, "600006", 0);
    // the fixture record's last bar is 2025-12-31; sampling 2025-12-30 as the
    // acceptance date makes the stored head look future-dated
    assert!(matches!(
        resolve(&db, DefaultOperation::Update, "2025-12-30".parse().unwrap()),
        Err(SelectionError::Invalid(_))
    ));
}
#[test]
fn future_gap_end_rejects_the_batch() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let ctl = TaskControl::new(tasks, "run".into(), vec![Box::new(LegacyCollection)]).unwrap();
    publish_daily(&db, &ctl, "600006", 0);
    db.record_proven_gap(&gap("g1", "sh.600006", "2026-02-01", "2026-02-10"))
        .unwrap();
    assert!(matches!(
        resolve(&db, DefaultOperation::Update, today()),
        Err(SelectionError::Invalid(_))
    ));
    assert!(matches!(
        resolve(&db, DefaultOperation::Backfill, today()),
        Err(SelectionError::Invalid(_))
    ));
}
#[test]
fn selection_budget_is_0_to_100_without_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let (_tasks, catalog, db) = open(&dir);
    // no catalog head at all -> invalid facts, not an empty set
    assert!(matches!(
        resolve(&db, DefaultOperation::Update, today()),
        Err(SelectionError::Invalid(_))
    ));
    publish_catalog(&catalog, "c1", &["600006"]);
    // catalog members without ever-started markers -> EmptySelection
    assert!(matches!(
        resolve(&db, DefaultOperation::Update, today()),
        Err(SelectionError::EmptySelection)
    ));
    for n in 100..=MAX_DEFAULT_SECURITIES + 1 {
        let dir = tempfile::tempdir().unwrap();
        let (tasks, catalog, db) = open(&dir);
        let ctl = TaskControl::new(tasks, "run".into(), vec![Box::new(LegacyCollection)]).unwrap();
        let codes: Vec<String> = (0..n).map(|i| format!("6{i:05}")).collect();
        let refs: Vec<&str> = codes.iter().map(String::as_str).collect();
        publish_catalog(&catalog, "c1", &refs);
        for (i, code) in codes.iter().enumerate() {
            publish_daily(&db, &ctl, code, i);
        }
        let work = resolve(&db, DefaultOperation::Update, today());
        if n <= MAX_DEFAULT_SECURITIES {
            assert_eq!(work.unwrap().items.len(), n);
        } else {
            assert!(matches!(work, Err(SelectionError::Invalid(_))));
        }
    }
}
#[test]
fn backfill_consumes_only_open_gaps_and_fails_closed_without_any() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006", "600007"]);
    let ctl = TaskControl::new(tasks, "run".into(), vec![Box::new(LegacyCollection)]).unwrap();
    publish_daily(&db, &ctl, "600006", 0);
    publish_daily(&db, &ctl, "600007", 1);
    // no open gaps at all -> no items, fail closed
    assert!(matches!(
        resolve(&db, DefaultOperation::Backfill, today()),
        Err(SelectionError::EmptySelection)
    ));
    db.record_proven_gap(&gap("g1", "sh.600006", "2025-02-01", "2025-02-10"))
        .unwrap();
    let work = resolve(&db, DefaultOperation::Backfill, today()).unwrap();
    assert_eq!(work.items.len(), 1);
    let item = &work.items[0];
    assert_eq!(item.symbol, "sh.600006");
    assert_eq!(item.refresh_range, None);
    assert_eq!(item.proven_gap_ranges.len(), 1);
    assert_eq!(item.proven_gap_ranges[0].gap_id, "g1");
    // update still emits both securities with refresh ranges
    let update = resolve(&db, DefaultOperation::Update, today()).unwrap();
    assert_eq!(update.items.len(), 2);
    assert!(update.items.iter().all(|i| i.refresh_range.is_some()));
}
#[test]
fn same_snapshot_and_clock_replays_identical_work() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let ctl = TaskControl::new(tasks, "run".into(), vec![Box::new(LegacyCollection)]).unwrap();
    publish_daily(&db, &ctl, "600006", 0);
    db.record_proven_gap(&gap("g1", "sh.600006", "2025-02-01", "2025-02-10"))
        .unwrap();
    let c = Connection::open(db.tasks.path()).unwrap();
    let first = resolve_default_work_on(&c, DefaultOperation::Update, today()).unwrap();
    let second = resolve_default_work_on(&c, DefaultOperation::Update, today()).unwrap();
    assert_eq!(first, second);
}
#[test]
fn update_acceptance_freezes_minimal_intent_and_both_provenance_classes() {
    use crate::market_update::tasks::{DEFAULT_CONTRACT_VERSION, Operation};
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let setup = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    publish_daily(&db, &setup, "600006", 0);
    db.record_proven_gap(&gap("g1", "sh.600006", "2025-02-01", "2025-02-10"))
        .unwrap();
    let ctl = acceptance_ctl(&tasks, &db, today);
    let accepted = ctl
        .accept("r-default", default_key(Operation::Update, &db), intent())
        .unwrap();
    assert!(!accepted.reused);
    // The persisted command stays the minimal deterministic intent: resolved
    // securities, ranges and evidence never enter request identity.
    assert_eq!(accepted.record.command.intent, intent());
    let RequestOutcome::Task(snap) = &accepted.record.outcome else {
        panic!()
    };
    assert_eq!(snap.task.key.contract_version, DEFAULT_CONTRACT_VERSION);
    assert_eq!(snap.task.key.dataset, "market.daily");
    assert_eq!(snap.items.len(), 1);
    // Item scope/occupancy identical to explicit daily: symbol x day.
    assert_eq!(snap.items[0].spec.item_id, scope_id("sh.600006", "day"));
    let f = &snap.items[0].spec.frozen_inputs;
    // Class 1: selection/catalog/execution-start provenance is stored once
    // in the task's shared evidence; the item carries only its digest marker.
    let ev = &f["selection_evidence"];
    let digest = ev["evidence_digest"].as_str().unwrap();
    assert_eq!(digest.len(), 64);
    let shared = snap.task.work.shared_evidence.as_ref().unwrap();
    assert_eq!(shared["catalog_version_id"], json!("c1"));
    assert_eq!(shared["catalog_digest"].as_str().unwrap().len(), 64);
    assert_eq!(shared["securities"], json!(["sh.600006"]));
    assert!(shared["starts"]["sh.600006"].is_array());
    assert_eq!(shared["evidence_digest"], json!(digest));
    // Verified base identity.
    assert_eq!(f["base_version_id"], json!("daily-600006"));
    assert_eq!(f["base_integrity_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(
        f["refresh_range"],
        json!({"start_date":"2024-12-31","end_date":TODAY})
    );
    // Class 2: per-gap provenance stays a distinct list.
    assert_eq!(
        f["proven_gap_ranges"],
        json!([{
            "gap_id":"g1",
            "range":{"start_date":"2025-02-01","end_date":"2025-02-10"},
            "evidence_digest":"e".repeat(64)
        }])
    );
}
#[test]
fn acceptance_rejects_caller_overrides_and_shape_violations() {
    use crate::market_update::tasks::Operation;
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let setup = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    publish_daily(&db, &setup, "600006", 0);
    let ctl = acceptance_ctl(&tasks, &db, today);
    let key = default_key(Operation::Update, &db);
    for (i, bad) in [
        json!({"selection":{"kind":"default","base_version_id":"x"}}),
        json!({"selection":{"kind":"default","start_date":"2025-01-01"}}),
        json!({"selection":{"kind":"default","end_date":"2025-12-31"}}),
        json!({"selection":{"kind":"default","gap_ids":["g1"]}}),
        json!({"selection":{"kind":"default"},"base_version_id":"x"}),
        json!({"selection":{"kind":"explicit","securities":[]}}),
        json!({"selection":{"kind":"catalog","version_id":"c1"}}),
        json!({"selection":{"kind":"future"}}),
        json!({}),
    ]
    .into_iter()
    .enumerate()
    {
        let request_id = format!("bad-{i}");
        assert!(matches!(
            ctl.accept(&request_id, key.clone(), bad),
            Err(task::Error::InvalidCommand(_))
        ));
        assert!(ctl.repository.read_request(&request_id).unwrap().is_none());
    }
}
#[test]
fn replay_returns_original_task_without_recompute() {
    use crate::market_update::tasks::Operation;
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let setup = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    publish_daily(&db, &setup, "600006", 0);
    db.record_proven_gap(&gap("g1", "sh.600006", "2025-02-01", "2025-02-10"))
        .unwrap();
    let ctl = acceptance_ctl(&tasks, &db, today);
    let key = default_key(Operation::Update, &db);
    let first = ctl.accept("r1", key.clone(), intent()).unwrap();
    let RequestOutcome::Task(before) = &first.record.outcome else {
        panic!()
    };
    let frozen_before = before.items[0].spec.frozen_inputs.clone();
    // Catalog head, gap set and selection facts change after acceptance.
    publish_catalog_expected(&catalog, "c2", &["600006", "600008"], Some("c1"));
    db.record_proven_gap(&gap("g9", "sh.600006", "2025-03-01", "2025-03-02"))
        .unwrap();
    let replay = ctl.accept("r1", key.clone(), intent()).unwrap();
    assert!(replay.reused);
    let RequestOutcome::Task(after) = &replay.record.outcome else {
        panic!()
    };
    assert_eq!(after.task.task_id, before.task.task_id);
    assert_eq!(after.items[0].spec.frozen_inputs, frozen_before);
    // Same request_id with different arguments conflicts.
    assert!(matches!(
        ctl.accept("r1", default_key(Operation::Backfill, &db), intent()),
        Err(task::Error::RequestConflict)
    ));
    assert!(matches!(
        ctl.accept("r1", Operation::Update.key(), json!({"items":[]})),
        Err(task::Error::RequestConflict)
    ));
}
#[test]
fn backfill_acceptance_creates_only_gap_items_without_refresh_range() {
    use crate::market_update::tasks::Operation;
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006", "600007"]);
    let setup = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    publish_daily(&db, &setup, "600006", 0);
    publish_daily(&db, &setup, "600007", 1);
    db.record_proven_gap(&gap("g1", "sh.600006", "2025-02-01", "2025-02-10"))
        .unwrap();
    let ctl = acceptance_ctl(&tasks, &db, today);
    let accepted = ctl
        .accept("r-bf", default_key(Operation::Backfill, &db), intent())
        .unwrap();
    let RequestOutcome::Task(snap) = &accepted.record.outcome else {
        panic!()
    };
    assert_eq!(snap.items.len(), 1);
    assert_eq!(snap.items[0].spec.item_id, scope_id("sh.600006", "day"));
    let f = &snap.items[0].spec.frozen_inputs;
    assert!(f.get("refresh_range").is_none());
    assert_eq!(f["proven_gap_ranges"].as_array().unwrap().len(), 1);
    assert_eq!(f["proven_gap_ranges"][0]["gap_id"], json!("g1"));
    assert_eq!(f["base_version_id"], json!("daily-600006"));
}
#[test]
fn zero_gap_backfill_rejects_before_persistence() {
    use crate::market_update::tasks::Operation;
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let setup = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    publish_daily(&db, &setup, "600006", 0);
    let ctl = acceptance_ctl(&tasks, &db, today);
    let (requests, occupancy) = occupancy_and_request_rows(&db, "r-empty");
    assert!(matches!(
        ctl.accept("r-empty", default_key(Operation::Backfill, &db), intent()),
        Err(task::Error::InvalidCommand(_))
    ));
    assert_eq!(
        occupancy_and_request_rows(&db, "r-empty"),
        (requests, occupancy)
    );
}
#[test]
fn over_budget_default_rejects_before_persistence() {
    use crate::market_update::tasks::Operation;
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    let setup = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let codes: Vec<String> = (0..=MAX_DEFAULT_SECURITIES)
        .map(|i| format!("6{i:05}"))
        .collect();
    let refs: Vec<&str> = codes.iter().map(String::as_str).collect();
    publish_catalog(&catalog, "c1", &refs);
    for (i, code) in codes.iter().enumerate() {
        publish_daily(&db, &setup, code, i);
    }
    let ctl = acceptance_ctl(&tasks, &db, today);
    let (requests, occupancy) = occupancy_and_request_rows(&db, "r-big");
    assert!(matches!(
        ctl.accept("r-big", default_key(Operation::Update, &db), intent()),
        Err(task::Error::InvalidCommand(_))
    ));
    assert_eq!(
        occupancy_and_request_rows(&db, "r-big"),
        (requests, occupancy)
    );
}
#[test]
fn future_resolution_rejects_before_persistence() {
    use crate::market_update::tasks::Operation;
    fn early() -> NaiveDate {
        "2025-12-30".parse().unwrap()
    }
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let setup = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    publish_daily(&db, &setup, "600006", 0);
    // The fixture head's last bar (2025-12-31) is beyond this clock's
    // acceptance date: the whole batch rejects before any rows persist.
    let ctl = acceptance_ctl(&tasks, &db, early);
    let (requests, occupancy) = occupancy_and_request_rows(&db, "r-future");
    assert!(matches!(
        ctl.accept("r-future", default_key(Operation::Update, &db), intent()),
        Err(task::Error::InvalidCommand(_))
    ));
    assert_eq!(
        occupancy_and_request_rows(&db, "r-future"),
        (requests, occupancy)
    );
}
#[test]
fn official_provider_keeps_default_execution_fail_closed() {
    use crate::market_update::{
        runner::{ItemExecution, RangeRunner},
        tasks::Operation,
    };
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let setup = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    publish_daily(&db, &setup, "600006", 0);
    let ctl = acceptance_ctl(&tasks, &db, today);
    let accepted = ctl
        .accept("r-run", default_key(Operation::Update, &db), intent())
        .unwrap();
    let RequestOutcome::Task(snap) = &accepted.record.outcome else {
        panic!()
    };
    let task_id = snap.task.task_id.clone();
    // The runner accepts the default contract, but the official provider is
    // still fail-closed: acquisition returns SOURCE_NOT_VERIFIED, so the
    // item finishes failed with no version, no receipt and no covering.
    let runner = RangeRunner {
        repository: db.clone(),
        run_id: "run".into(),
    };
    let ItemExecution::NotPublished(after) = runner
        .run_item(
            &task_id,
            &snap.items[0].spec.item_id,
            &crate::market_update::runner::OfficialRangeProvider,
        )
        .unwrap()
    else {
        panic!()
    };
    let outcome = after.items[0].outcome.as_ref().unwrap();
    assert!(!outcome.success);
    assert!(
        outcome
            .error
            .as_ref()
            .unwrap()
            .contains("SOURCE_NOT_VERIFIED")
    );
    assert!(
        db.item_receipt(&task_id, &snap.items[0].spec.item_id)
            .unwrap()
            .is_none()
    );
    // The finished-failed task is no longer dispatchable.
    assert!(matches!(
        crate::data_supply::worker::run_once(
            &ctl,
            &crate::sqlite_market::backend::MarketBackend::Legacy(db.clone()),
            None,
            None
        ),
        Ok(None)
    ));
}
#[test]
fn explicit_daily_acceptance_still_uses_the_explicit_contract() {
    use crate::market_update::tasks::{CONTRACT_VERSION, Operation, RangeContract};
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let ctl = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![
            Box::new(LegacyCollection),
            Box::new(RangeContract {
                operation: Operation::Update,
                repository: db.clone(),
                today,
            }),
        ],
    )
    .unwrap();
    publish_daily(&db, &ctl, "600006", 0);
    let accepted = ctl
        .accept(
            "r-explicit",
            Operation::Update.key(),
            json!({"items":[{
                "symbol":"sh.600006",
                "base_version_id":"daily-600006",
                "start_date":"2025-12-01",
                "end_date":"2025-12-31"
            }]}),
        )
        .unwrap();
    let RequestOutcome::Task(snap) = &accepted.record.outcome else {
        panic!()
    };
    assert_eq!(snap.task.key.contract_version, CONTRACT_VERSION);
    assert!(snap.items[0].spec.frozen_inputs["range"].is_object());
}
/// #262 ECS catch: the `Bound` decorator stamps `source_generation_id`
/// into every resolved item's frozen inputs at acceptance; the runner's
/// strict frozen decode must admit that field (the binding itself is
/// enforced at session open) while still rejecting unknown fields.
#[test]
fn generation_binding_stamp_survives_default_frozen_parse() {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, catalog, db) = open(&dir);
    publish_catalog(&catalog, "c1", &["600006"]);
    let ctl0 = TaskControl::new(
        tasks.clone(),
        "run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    publish_daily(&db, &ctl0, "600006", 0);
    let ctl = acceptance_ctl(&tasks, &db, today);
    let accepted = ctl
        .accept(
            "r-bound",
            default_key(crate::market_update::tasks::Operation::Update, &db),
            intent(),
        )
        .unwrap();
    let RequestOutcome::Task(snap) = &accepted.record.outcome else {
        panic!()
    };
    let mut frozen = snap.items[0].spec.frozen_inputs.clone();
    frozen["source_generation_id"] = json!("gen-1");
    crate::market_update::tasks::parse_default_frozen(&frozen).unwrap();
    frozen["surprise"] = json!(1);
    assert!(crate::market_update::tasks::parse_default_frozen(&frozen).is_err());
}
