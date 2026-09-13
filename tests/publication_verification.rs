//! Publication inspection fixtures use actual SQLite, never a live market source.
use serde_json::{Value, json};
use x_data_mcp::{
    market_storage::{MarketRepository, Publication, PublishIntent, Publisher},
    publication_verification::{
        Evidence, EvidenceRepository,
        admission::{PublicationVerification, command_key},
        decision::{AuditStore, Conclusion},
        runner::{Outcome, Runner},
    },
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Change, Contract, RequestOutcome, Stage, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
};

#[test]
fn publication_check_preserves_mixed_published_running_and_never_started_items() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(dir.path().join("mixed.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let control = TaskControl::new(
        tasks.clone(),
        "original-run".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let RequestOutcome::Task(original) = control
        .accept(
            "mixed-original",
            LegacyCollection.key(),
            json!({"symbols":["sh.600006","sh.600007"],"periods":["day","year"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let original = tasks
        .transition(&original.task.task_id, 0, "original-run", Change::Claim)
        .unwrap();
    let day = original
        .items
        .iter()
        .find(|i| i.spec.target == json!({"symbol":"sh.600006","period":"day"}))
        .unwrap()
        .spec
        .item_id
        .clone();
    let year = original
        .items
        .iter()
        .find(|i| i.spec.target == json!({"symbol":"sh.600006","period":"year"}))
        .unwrap()
        .spec
        .item_id
        .clone();
    let original = tasks
        .transition(
            &original.task.task_id,
            original.task.state_revision,
            "original-run",
            Change::StartItem(day.clone()),
        )
        .unwrap();
    let records: Value =
        serde_json::from_str(include_str!("fixtures/issue79/legacy-records.json")).unwrap();
    market.stage(&records[0]["record"]).unwrap();
    let plan = (Publisher {
        repository: market.clone(),
    })
    .prepare(Publication {
        publication_id: "mixed-daily".into(),
        version_id: "legacy-daily".into(),
        task_id: original.task.task_id.clone(),
        item_id: day.clone(),
        run_id: "original-run".into(),
        expected_revision: original.task.state_revision,
        intent: PublishIntent::Daily {
            expected_head: None,
        },
    })
    .unwrap();
    market.commit(&plan).unwrap();
    let original = tasks.read_task(&original.task.task_id).unwrap().unwrap();
    let original = tasks
        .transition(
            &original.task.task_id,
            original.task.state_revision,
            "original-run",
            Change::StartItem(year.clone()),
        )
        .unwrap();
    let facts = market.publication_facts(&original.task.task_id).unwrap();
    assert_eq!(facts.task, original);
    assert_eq!(
        facts
            .items
            .iter()
            .filter(|f| matches!(f.evidence, Evidence::Committed { .. }))
            .count(),
        1
    );
    assert_eq!(
        facts
            .items
            .iter()
            .filter(|f| matches!(f.evidence, Evidence::Missing))
            .count(),
        3
    );
    assert_eq!(
        original
            .items
            .iter()
            .filter(|i| i.stage == Stage::Pending)
            .count(),
        2
    );
    assert_eq!(
        original
            .items
            .iter()
            .filter(|i| i.stage == Stage::Running)
            .count(),
        1
    );
    let check_control = TaskControl::new(
        tasks.clone(),
        "inspection-run".into(),
        vec![Box::new(
            PublicationVerification::new(tasks.clone(), "inspection-run".into()).unwrap(),
        )],
    )
    .unwrap();
    let RequestOutcome::Task(check) = check_control
        .accept(
            "mixed-inspection",
            command_key(),
            json!({"target_task_id":original.task.task_id}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let Outcome::Completed(receipt) = (Runner {
        tasks: tasks.clone(),
        evidence: market.clone(),
        run_id: "inspection-run".into(),
        actor: "test-owner".into(),
    })
    .run(&check.task.task_id)
    .unwrap() else {
        panic!()
    };
    assert_eq!(receipt.conclusion, Conclusion::InsufficientEvidence);
    assert_eq!(receipt.missing_receipt_item_ids.len(), 3);
    assert!(receipt.missing_receipt_item_ids.contains(&year));
    assert!(!receipt.missing_receipt_item_ids.contains(&day));
    assert!(receipt.reason.contains("absence does not prove failure"));
    assert_eq!(
        market.check_receipt(&check.task.task_id).unwrap(),
        Some(receipt)
    );
    assert_eq!(
        tasks.read_task(&original.task.task_id).unwrap().unwrap(),
        original
    );
    assert_eq!(original.claims.len(), 4);
    assert_eq!(
        market.fixed("legacy-daily").unwrap().raw(),
        &records[0]["record"]
    );
}
