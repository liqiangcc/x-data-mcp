#![allow(dead_code)]
use serde_json::{Value, json};
use x_data_mcp::{
    market_data::Period,
    market_storage::{
        MarketRepository, Publication, PublishIntent, PublishPlan, Publisher, Range, Scope,
    },
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
};
mod frozen_legacy {
    pub fn digest(v: &serde_json::Value) -> String {
        x_data_mcp::market_storage::compat::legacy_digest(v).unwrap()
    }
}
pub fn samples() -> Vec<Value> {
    serde_json::from_str(include_str!("../fixtures/issue79/legacy-records.json")).unwrap()
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
pub fn prepare(db: &SqliteMarket, raw: &Value, intent: PublishIntent) -> PublishPlan {
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
pub fn published_fixture(db: &SqliteMarket, raw: &Value) {
    let plan = prepare(
        db,
        raw,
        PublishIntent::Daily {
            expected_head: None,
        },
    );
    db.commit(&plan).unwrap();
}
pub fn revised(raw: &Value, id: &str) -> Value {
    let mut r = raw.clone();
    r["version_id"] = json!(id);
    r["integrity_sha256"] = json!(frozen_legacy::digest(&r));
    r
}
/// Snapshot every stored version's full compatibility record before a
/// simulated schema downgrade; `restore_compat` writes them back into the
/// renamed `compat_json` column so an explicit upgrade replays the real
/// migration chain.
pub fn stored_compat(db: &SqliteMarket) -> Vec<(String, String)> {
    let c = rusqlite::Connection::open(db.tasks.path()).unwrap();
    let mut stmt = c
        .prepare("SELECT version_id FROM data_versions ORDER BY rowid")
        .unwrap();
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(stmt);
    ids.into_iter()
        .map(|id| {
            (
                id.clone(),
                serde_json::to_string(db.candidate(&id).unwrap().raw()).unwrap(),
            )
        })
        .collect()
}
pub fn restore_compat(db: &SqliteMarket, raws: &[(String, String)]) {
    let c = rusqlite::Connection::open(db.tasks.path()).unwrap();
    for (id, raw) in raws {
        c.execute(
            "UPDATE data_versions SET compat_json=?1 WHERE version_id=?2",
            rusqlite::params![raw, id],
        )
        .unwrap();
    }
}
