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
        serde_json::from_str(include_str!("../fixtures/issue79/legacy-records.json")).unwrap();
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
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let mut access = Access::local();
    access.principal = "restricted-reader".into();
    let reader = DataApi::new(tasks.clone(), "reader".into(), access).unwrap();
    for corrupt in [false, true] {
        if corrupt {
            rusqlite::Connection::open(tasks.path())
                .unwrap()
                .execute("UPDATE publication_checks SET audit_sha256='bad'", [])
                .unwrap();
        }
        assert_eq!(
            reader
                .call("get_task_progress", json!({"task_id":check.task.task_id}))
                .unwrap_err()
                .code,
            "forbidden"
        );
        assert!(
            reader
                .call(
                    "list_tasks",
                    json!({"operation":"verify_publication_result"})
                )
                .unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn malformed_evidence_port_result_cannot_leave_check_looking_normally_running() {
    // This is an application-port fault, not a claim that SQLite returned these facts.
    struct IncompletePort {
        market: SqliteMarket,
        reads: std::cell::Cell<usize>,
    }
    impl EvidenceRepository for IncompletePort {
        fn publication_facts(
            &self,
            id: &str,
        ) -> Result<x_data_mcp::publication_verification::Facts, x_data_mcp::market_storage::Error>
        {
            self.reads.set(self.reads.get() + 1);
            let mut facts = self.market.publication_facts(id)?;
            facts.items.clear();
            Ok(facts)
        }
    }
    impl AuditStore for IncompletePort {
        fn commit_check(
            &self,
            _: &x_data_mcp::publication_verification::decision::Plan,
        ) -> Result<
            x_data_mcp::publication_verification::decision::Receipt,
            x_data_mcp::market_storage::Error,
        > {
            panic!("invalid evidence must not reach audit commit");
        }
        fn check_receipt(
            &self,
            id: &str,
        ) -> Result<
            Option<x_data_mcp::publication_verification::decision::Receipt>,
            x_data_mcp::market_storage::Error,
        > {
            self.market.check_receipt(id)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(dir.path().join("invalid-port.sqlite"));
    tasks.initialize().unwrap();
    let old = TaskControl::new(
        tasks.clone(),
        "old".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let RequestOutcome::Task(original) = old
        .accept(
            "original",
            LegacyCollection.key(),
            json!({"symbols":["sh.600006"],"periods":["day"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let original = tasks
        .transition(&original.task.task_id, 0, "old", Change::Claim)
        .unwrap();
    let original = tasks
        .transition(
            &original.task.task_id,
            original.task.state_revision,
            "old",
            Change::StartItem(original.items[0].spec.item_id.clone()),
        )
        .unwrap();
    let control = TaskControl::new(
        tasks.clone(),
        "check".into(),
        vec![Box::new(
            PublicationVerification::new(tasks.clone(), "check".into()).unwrap(),
        )],
    )
    .unwrap();
    let RequestOutcome::Task(check) = control
        .accept(
            "inspect",
            command_key(),
            json!({"target_task_id":original.task.task_id}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let runner = Runner {
        tasks: tasks.clone(),
        evidence: IncompletePort {
            market: SqliteMarket::new(tasks.clone()),
            reads: std::cell::Cell::new(0),
        },
        run_id: "check".into(),
        actor: "test-owner".into(),
    };
    assert!(runner.run(&check.task.task_id).is_err());
    let unresolved = tasks.read_task(&check.task.task_id).unwrap().unwrap();
    assert_eq!(
        unresolved.task.status,
        x_data_mcp::task_control::Status::NeedsVerification
    );
    assert!(unresolved.execution_uncertainty().unwrap().is_some());
    assert!(!unresolved.claims.is_empty());
    assert!(
        runner
            .evidence
            .check_receipt(&check.task.task_id)
            .unwrap()
            .is_none()
    );
    assert!(runner.run(&check.task.task_id).is_err());
    assert_eq!(runner.evidence.reads.get(), 1);
    assert_eq!(
        tasks.read_task(&original.task.task_id).unwrap().unwrap(),
        original
    );
}
