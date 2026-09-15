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
            None,
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
        .execute("UPDATE daily_bars SET close='0'", [])
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
