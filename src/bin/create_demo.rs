//! Explicit synthetic demo initialization; never called by normal server startup.
use serde_json::{Value, json};
use x_data_mcp::{
    catalog::*,
    market_storage::{
        MarketRepository, Publication, PublishIntent, Publisher, compat::legacy_digest,
    },
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
};
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn publish(
    store: &SqliteMarket,
    raw: &Value,
    intent: PublishIntent,
) -> Result<(), Box<dyn std::error::Error>> {
    store.stage(raw)?;
    let ctl = TaskControl::new(
        store.tasks.clone(),
        "fixture-demo".into(),
        vec![Box::new(LegacyCollection)],
    )?;
    let id = raw["version_id"].as_str().ok_or("fixture id")?;
    let accepted = ctl.accept(
        id,
        LegacyCollection.key(),
        json!({"symbols":[raw["symbol"]],"periods":[if raw["period"]=="Day"{"day"}else{"year"}]}),
    )?;
    let RequestOutcome::Task(t) = accepted.record.outcome else {
        return Err("fixture task binding".into());
    };
    let t = store
        .tasks
        .transition(&t.task.task_id, 0, "fixture-demo", Change::Claim)?;
    let t = store.tasks.transition(
        &t.task.task_id,
        t.task.state_revision,
        "fixture-demo",
        Change::StartItem(t.items[0].spec.item_id.clone()),
    )?;
    let plan = Publisher {
        repository: store.clone(),
    }
    .prepare(Publication {
        publication_id: format!("pub-{id}"),
        version_id: id.into(),
        task_id: t.task.task_id,
        item_id: t.items[0].spec.item_id.clone(),
        run_id: "fixture-demo".into(),
        expected_revision: t.task.state_revision,
        intent,
    })?;
    store.commit(&plan)?;
    Ok(())
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err("usage: create_demo <NEW fixture-database-path> (synthetic data only)".into());
    }
    let tasks = SqliteTasks::new(&args[0]);
    tasks.initialize()?;
    let store = SqliteMarket::new(tasks.clone());
    let mut daily: Value = serde_json::from_str(include_str!("../../fixtures/demo/daily.json"))?;
    daily["integrity_sha256"] = json!(legacy_digest(&daily)?);
    publish(
        &store,
        &daily,
        PublishIntent::Daily {
            expected_head: None,
        },
    )?;
    let mut year = daily;
    year["version_id"] = json!("demo-yearly");
    year["period"] = json!("Year");
    year["batch"]["period"] = json!("Year");
    let lineage = json!({"base_daily_version_id":"demo-daily","algorithm_version":"local-natural-year-v1","first_trading_date":"2020-12-30","last_trading_date":"2023-12-30"});
    year["yearly_derivation"] = lineage.clone();
    year["batch"]["provenance"]["yearly_derivation"] = lineage;
    year["analysis_coverage_v1"] = json!({"origin":"fixture","years":(2020..=2023).map(|year|json!({"year":year,"status":"complete","evidence_ref":"fixtures/demo/README.md: synthetic one-point yearly vector"})).collect::<Vec<_>>()});
    year["integrity_sha256"] = json!(legacy_digest(&year)?);
    publish(
        &store,
        &year,
        PublishIntent::CurrentHead {
            expected_daily_head: "demo-daily".into(),
            expected_yearly_head: None,
        },
    )?;
    let now =
        chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")?.with_timezone(&chrono::Utc);
    let source = "fixture:release-demo".to_string();
    x_data_mcp::sqlite_catalog::SqliteCatalog::new(tasks).publish_fixture(
        "demo-catalog",
        CatalogSnapshot {
            source: source.clone(),
            acquired_at: now,
            completeness: CompletenessEvidence::Complete { expected_count: 1 },
            records: vec![SecurityRecord {
                symbol: "600006".into(),
                name: "合成演示证券（非真实行情）".into(),
                exchange: Exchange::Shanghai,
                security_type: SecurityType::AShare,
                source_security_id: "demo-600006".into(),
                industry: IndustryInfo::Missing {
                    reason: "synthetic demo does not certify industry".into(),
                },
                source,
                acquired_at: now,
            }],
        },
        None,
    )?;
    println!(
        "{}",
        json!({"status":"FIXTURE_ONLY","version_ref":{"dataset":"market.yearly","contract_version":"v1","entity":{"exchange":"sh","code":"600006"},"version_id":"demo-yearly","integrity_sha256":year["integrity_sha256"]}})
    );
    Ok(())
}
