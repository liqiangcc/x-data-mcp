use tempfile::tempdir;
use x_data_mcp::{
    market_data::{Adjustment, Period},
    task::{CONTRACT_VERSION, TaskAcceptanceService, TaskError, TaskStore},
};

fn service() -> (TaskAcceptanceService, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let service = TaskAcceptanceService::new(TaskStore::new(dir.path()));
    (service, dir)
}

#[test]
fn normalizes_and_reuses_same_request() {
    let (service, _) = service();
    let first = service
        .accept(
            "r1",
            &["600000", "000001", "000001"],
            &[Period::Year, Period::Day, Period::Day],
            Adjustment::Forward,
        )
        .unwrap();
    let second = service
        .accept(
            "r1",
            &["000001", "600000"],
            &[Period::Day, Period::Year],
            Adjustment::Forward,
        )
        .unwrap();
    assert_eq!(first.task_id, second.task_id);
    assert!(!first.reused);
    assert!(second.reused);
    assert_eq!(first.binding.symbols, vec!["000001", "600000"]);
    assert_eq!(first.binding.periods, vec![Period::Day, Period::Year]);
    assert_eq!(first.binding.frozen_range.len(), 4);
    assert_eq!(first.binding.contract_version, CONTRACT_VERSION);
}

#[test]
fn changed_parameters_conflict_without_mutating_record() {
    let (service, _) = service();
    let first = service
        .accept("r1", &["000001"], &[Period::Day], Adjustment::Forward)
        .unwrap();
    assert!(matches!(
        service.accept("r1", &["600000"], &[Period::Day], Adjustment::Forward),
        Err(TaskError::RequestConflict { .. })
    ));
    assert_eq!(
        service.store().get("r1").unwrap().unwrap().task_id,
        first.task_id
    );
}

#[test]
fn different_request_ids_create_different_tasks() {
    let (service, _) = service();
    let first = service
        .accept("r1", &["000001"], &[Period::Day], Adjustment::Forward)
        .unwrap();
    let second = service
        .accept("r2", &["600000"], &[Period::Day], Adjustment::Forward)
        .unwrap();
    assert_ne!(first.task_id, second.task_id);
}

#[test]
fn rebuilt_service_recovers_persistent_binding() {
    let dir = tempdir().unwrap();
    let first = TaskAcceptanceService::new(TaskStore::new(dir.path()))
        .accept(
            "r1",
            &["000001", "600000"],
            &[Period::Year],
            Adjustment::Forward,
        )
        .unwrap();
    let rebuilt = TaskAcceptanceService::new(TaskStore::new(dir.path()));
    let second = rebuilt
        .accept(
            "r1",
            &["600000", "000001", "000001"],
            &[Period::Year, Period::Year],
            Adjustment::Forward,
        )
        .unwrap();
    assert_eq!(second.task_id, first.task_id);
    assert!(second.reused);
    assert_eq!(rebuilt.store().list().unwrap().len(), 1);
}

#[test]
fn failed_acceptance_cannot_be_reused_without_complete_state_or_allow_overlap() {
    for blocked in [
        "task-bindings.json",
        "task-executions.json",
        "task-occupancy.json",
    ] {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join(blocked)).unwrap();
        let service = TaskAcceptanceService::new(TaskStore::new(dir.path()));
        assert!(
            service
                .accept("r1", &["000001"], &[Period::Day], Adjustment::Forward)
                .is_err()
        );
        std::fs::remove_dir(dir.path().join(blocked)).unwrap();
        let restarted = TaskAcceptanceService::new(TaskStore::new(dir.path()));
        let accepted = restarted
            .accept("r1", &["000001"], &[Period::Day], Adjustment::Forward)
            .unwrap();
        assert!(
            !accepted.reused,
            "a failed acceptance left a reusable binding at {blocked}"
        );
        assert!(
            restarted
                .store()
                .get_execution(&accepted.task_id)
                .unwrap()
                .is_some()
        );
        assert_eq!(restarted.store().list_occupancy().unwrap().len(), 1);
        assert!(matches!(
            restarted.accept("r2", &["000001"], &[Period::Day], Adjustment::Forward),
            Err(TaskError::RangeConflict { .. })
        ));
    }
}

#[test]
fn incomplete_legacy_binding_needs_verification_and_reserves_its_scope() {
    let (service, dir) = service();
    let accepted = service
        .accept("r1", &["000001"], &[Period::Day], Adjustment::Forward)
        .unwrap();
    // Reproduce an old partial-write artifact without upgrading it to success.
    std::fs::remove_file(dir.path().join("task-state.json")).unwrap();
    std::fs::write(
        dir.path().join("task-bindings.json"),
        serde_json::to_vec(&vec![accepted.binding]).unwrap(),
    )
    .unwrap();
    let restarted = TaskAcceptanceService::new(TaskStore::new(dir.path()));
    assert!(matches!(
        restarted.accept("r1", &["000001"], &[Period::Day], Adjustment::Forward),
        Err(TaskError::Storage(_))
    ));
    assert!(matches!(
        restarted.accept("r2", &["000001"], &[Period::Day], Adjustment::Forward),
        Err(TaskError::RangeConflict { .. })
    ));
    assert!(!dir.path().join("task-state.json").exists());
}

#[test]
fn unreadable_committed_state_must_not_fall_back_to_empty_legacy_state() {
    let (service, dir) = service();
    service
        .accept("r1", &["000001"], &[Period::Day], Adjustment::Forward)
        .unwrap();
    std::fs::write(dir.path().join("task-state.json"), b"{truncated").unwrap();
    let restarted = TaskAcceptanceService::new(TaskStore::new(dir.path()));
    assert!(matches!(
        restarted.accept("r2", &["000001"], &[Period::Day], Adjustment::Forward),
        Err(TaskError::Storage(_))
    ));
}
