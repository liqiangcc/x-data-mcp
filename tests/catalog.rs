use std::{
    io,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use tempfile::tempdir;
use x_data_mcp::catalog::{
    CatalogError, CatalogRepository, CatalogService, CatalogSnapshot, CatalogSourceError,
    CatalogVersion, CompletenessEvidence, Exchange, IndustryInfo, JsonCatalogStore, RefreshStatus,
    RejectReason, SecurityCatalogSource, SecurityKey, SecurityRecord, SecurityType,
};

#[derive(Clone)]
struct FakeSource {
    response: SharedResponse,
}

type SharedResponse = Arc<Mutex<Result<CatalogSnapshot, CatalogSourceError>>>;
type FakeService = CatalogService<FakeSource, JsonCatalogStore>;
type CatalogState = Arc<Mutex<Option<CatalogVersion>>>;

#[derive(Clone)]
struct FailingRepository {
    current: CatalogState,
    fail_publish: bool,
}

impl CatalogRepository for FailingRepository {
    fn latest(&self) -> Result<Option<CatalogVersion>, io::Error> {
        Ok(self.current.lock().unwrap().clone())
    }

    fn publish_atomic(&self, version: &CatalogVersion) -> Result<(), io::Error> {
        if self.fail_publish {
            return Err(io::Error::other("injected publish failure"));
        }
        *self.current.lock().unwrap() = Some(version.clone());
        Ok(())
    }
}

impl SecurityCatalogSource for FakeSource {
    fn fetch(&self) -> Result<CatalogSnapshot, CatalogSourceError> {
        self.response.lock().unwrap().clone()
    }
}

fn record(
    symbol: &str,
    exchange: Exchange,
    source_security_id: &str,
    industry: IndustryInfo,
) -> SecurityRecord {
    SecurityRecord {
        symbol: symbol.into(),
        name: format!("security-{symbol}"),
        exchange,
        security_type: SecurityType::AShare,
        source_security_id: source_security_id.into(),
        industry,
        source: "fake-catalog".into(),
        acquired_at: DateTime::from_timestamp(1_000, 0).unwrap(),
    }
}

fn assigned() -> IndustryInfo {
    IndustryInfo::Assigned {
        code: "BK0001".into(),
        name: "银行Ⅱ".into(),
        scheme: None,
    }
}

fn snapshot(records: Vec<SecurityRecord>, completeness: CompletenessEvidence) -> CatalogSnapshot {
    CatalogSnapshot {
        source: "fake-catalog".into(),
        acquired_at: Utc::now(),
        completeness,
        records,
    }
}

fn make_service(
    response: Result<CatalogSnapshot, CatalogSourceError>,
) -> (FakeService, SharedResponse, tempfile::TempDir) {
    let state = Arc::new(Mutex::new(response));
    let dir = tempdir().unwrap();
    let service = CatalogService::new(
        FakeSource {
            response: state.clone(),
        },
        JsonCatalogStore::new(dir.path()),
    );
    (service, state, dir)
}

#[test]
fn publishes_explicit_scope_and_restart_reads_latest_effective_version() {
    let (service, _, dir) = make_service(Ok(snapshot(
        vec![record(
            "000001",
            Exchange::Shenzhen,
            "vendor-sz-000001",
            assigned(),
        )],
        CompletenessEvidence::Complete { expected_count: 1 },
    )));

    let result = service.refresh().unwrap();
    assert_eq!(result.status, RefreshStatus::Published);
    let published = service.current().unwrap().unwrap();
    assert_eq!(published.records[0].exchange, Exchange::Shenzhen);
    assert_eq!(published.records[0].source_security_id, "vendor-sz-000001");
    assert!(dir.path().join("catalog.effective.json").is_file());

    let restarted = JsonCatalogStore::new(dir.path());
    assert_eq!(restarted.latest().unwrap(), Some(published));
}

#[test]
fn first_unknown_or_failed_refresh_is_not_ready_and_does_not_write_empty_catalog() {
    let (service, state, dir) = make_service(Ok(snapshot(
        Vec::new(),
        CompletenessEvidence::Unknown {
            reason: "source did not provide a stable total".into(),
        },
    )));
    let result = service.refresh().unwrap();
    assert_eq!(result.status, RefreshStatus::NotReady);
    assert!(matches!(
        result.reason,
        Some(RejectReason::Incomplete { .. })
    ));
    assert!(service.current().unwrap().is_none());
    assert!(!dir.path().join("catalog.effective.json").exists());

    *state.lock().unwrap() = Err(CatalogSourceError::Unavailable("offline".into()));
    assert_eq!(service.refresh().unwrap().status, RefreshStatus::NotReady);
    assert!(!dir.path().join("catalog.effective.json").exists());
}

#[test]
fn rejected_refresh_keeps_old_bytes_and_restart_reads_old_version() {
    let base = snapshot(
        vec![record(
            "000001",
            Exchange::Shenzhen,
            "vendor-sz-000001",
            assigned(),
        )],
        CompletenessEvidence::Complete { expected_count: 1 },
    );
    let (service, state, dir) = make_service(Ok(base));
    service.refresh().unwrap();
    let old = service.current().unwrap().unwrap();
    let old_bytes = std::fs::read(dir.path().join("catalog.effective.json")).unwrap();

    *state.lock().unwrap() = Ok(snapshot(
        vec![record(
            "000001",
            Exchange::Shenzhen,
            "vendor-sz-000001",
            assigned(),
        )],
        CompletenessEvidence::Incomplete {
            reason: "second page missing".into(),
        },
    ));
    let result = service.refresh().unwrap();
    assert_eq!(result.status, RefreshStatus::Rejected);
    assert_eq!(result.retained_version_id, Some(old.version_id.clone()));
    assert_eq!(service.current().unwrap(), Some(old.clone()));
    assert_eq!(
        std::fs::read(dir.path().join("catalog.effective.json")).unwrap(),
        old_bytes
    );

    *state.lock().unwrap() = Err(CatalogSourceError::Unavailable("offline".into()));
    assert_eq!(service.refresh().unwrap().status, RefreshStatus::Rejected);
    let restarted = JsonCatalogStore::new(dir.path());
    assert_eq!(restarted.latest().unwrap(), Some(old));
}

#[test]
fn missing_and_conflicted_industry_are_retained_and_labeled() {
    let (service, _, _) = make_service(Ok(snapshot(
        vec![
            record(
                "000001",
                Exchange::Shenzhen,
                "vendor-sz-000001",
                IndustryInfo::Missing {
                    reason: "industry endpoint unavailable".into(),
                },
            ),
            record(
                "600000",
                Exchange::Shanghai,
                "vendor-sh-600000",
                IndustryInfo::Conflicted {
                    reason: "two level-2 assignments returned".into(),
                },
            ),
        ],
        CompletenessEvidence::Complete { expected_count: 2 },
    )));
    service.refresh().unwrap();
    let current = service.current().unwrap().unwrap();
    assert!(matches!(
        current.records[0].industry,
        IndustryInfo::Missing { .. }
    ));
    assert!(matches!(
        current.records[1].industry,
        IndustryInfo::Conflicted { .. }
    ));
}

#[test]
fn member_disappearance_rejects_whole_candidate_without_partial_publish() {
    let (service, state, _) = make_service(Ok(snapshot(
        vec![
            record("000001", Exchange::Shenzhen, "vendor-sz-000001", assigned()),
            record("600000", Exchange::Shanghai, "vendor-sh-600000", assigned()),
        ],
        CompletenessEvidence::Complete { expected_count: 2 },
    )));
    service.refresh().unwrap();
    let old = service.current().unwrap().unwrap();
    *state.lock().unwrap() = Ok(snapshot(
        vec![record(
            "000001",
            Exchange::Shenzhen,
            "vendor-sz-000001",
            assigned(),
        )],
        CompletenessEvidence::Complete { expected_count: 1 },
    ));
    let result = service.refresh().unwrap();
    assert_eq!(result.status, RefreshStatus::Rejected);
    assert_eq!(
        result.reason,
        Some(RejectReason::MemberDisappeared {
            members: vec![SecurityKey {
                exchange: Exchange::Shanghai,
                symbol: "600000".into(),
            }],
        })
    );
    assert_eq!(service.current().unwrap(), Some(old));
}

#[test]
fn duplicate_identity_mapping_and_out_of_scope_records_reject() {
    let duplicate = vec![
        record("000001", Exchange::Shenzhen, "same-source-id", assigned()),
        record("600000", Exchange::Shanghai, "same-source-id", assigned()),
    ];
    let (service, _, _) = make_service(Ok(snapshot(
        duplicate,
        CompletenessEvidence::Complete { expected_count: 2 },
    )));
    let result = service.refresh().unwrap();
    assert!(matches!(
        result.reason,
        Some(RejectReason::IdentityConflict { .. })
    ));
    assert_eq!(result.status, RefreshStatus::NotReady);

    let (service, _, _) = make_service(Ok(snapshot(
        vec![record(
            "000001",
            Exchange::Other("unknown".into()),
            "vendor-unknown",
            assigned(),
        )],
        CompletenessEvidence::Complete { expected_count: 1 },
    )));
    assert!(matches!(
        service.refresh().unwrap().reason,
        Some(RejectReason::InvalidRecord { .. })
    ));

    let mut non_stock = record("000002", Exchange::Shenzhen, "vendor-sz-000002", assigned());
    non_stock.security_type = SecurityType::Other("fund".into());
    let (service, _, _) = make_service(Ok(snapshot(
        vec![non_stock],
        CompletenessEvidence::Complete { expected_count: 1 },
    )));
    assert!(matches!(
        service.refresh().unwrap().reason,
        Some(RejectReason::InvalidRecord { .. })
    ));
}

#[test]
fn same_key_with_different_source_ids_is_an_identity_conflict() {
    let mut second = record(
        "000001",
        Exchange::Shenzhen,
        "vendor-sz-000001-v2",
        assigned(),
    );
    second.name = "security-000001-renamed".into();
    let (service, _, _) = make_service(Ok(snapshot(
        vec![
            record("000001", Exchange::Shenzhen, "vendor-sz-000001", assigned()),
            second,
        ],
        CompletenessEvidence::Complete { expected_count: 2 },
    )));
    assert!(matches!(
        service.refresh().unwrap().reason,
        Some(RejectReason::IdentityConflict { .. })
    ));
}

#[test]
fn same_key_with_same_source_id_remains_a_duplicate() {
    let duplicate = record("000001", Exchange::Shenzhen, "vendor-sz-000001", assigned());
    let (service, _, _) = make_service(Ok(snapshot(
        vec![duplicate.clone(), duplicate],
        CompletenessEvidence::Complete { expected_count: 2 },
    )));
    assert!(matches!(
        service.refresh().unwrap().reason,
        Some(RejectReason::DuplicateSecurity { key })
            if key == SecurityKey {
                exchange: Exchange::Shenzhen,
                symbol: "000001".into()
            }
    ));
}

#[test]
fn required_identity_fields_reject_without_publishing() {
    let mut missing_name = record("000001", Exchange::Shenzhen, "vendor-sz-000001", assigned());
    missing_name.name.clear();
    let (service, _, _) = make_service(Ok(snapshot(
        vec![missing_name],
        CompletenessEvidence::Complete { expected_count: 1 },
    )));
    assert!(matches!(
        service.refresh().unwrap().reason,
        Some(RejectReason::InvalidRecord { message }) if message == "name must not be empty"
    ));

    let (service, _, _) = make_service(Ok(snapshot(
        vec![record("000001", Exchange::Shenzhen, "", assigned())],
        CompletenessEvidence::Complete { expected_count: 1 },
    )));
    assert!(matches!(
        service.refresh().unwrap().reason,
        Some(RejectReason::InvalidRecord { message })
            if message == "source_security_id must not be empty"
    ));
}

#[test]
fn expected_count_mismatch_and_complete_empty_snapshot_reject() {
    let (service, _, _) = make_service(Ok(snapshot(
        vec![record(
            "000001",
            Exchange::Shenzhen,
            "vendor-sz-000001",
            assigned(),
        )],
        CompletenessEvidence::Complete { expected_count: 2 },
    )));
    assert!(matches!(
        service.refresh().unwrap().reason,
        Some(RejectReason::Incomplete { .. })
    ));

    let (service, _, _) = make_service(Ok(snapshot(
        Vec::new(),
        CompletenessEvidence::Complete { expected_count: 0 },
    )));
    assert_eq!(
        service.refresh().unwrap().reason,
        Some(RejectReason::EmptySnapshot)
    );
}

#[test]
fn publish_repository_failure_returns_storage_error_and_keeps_old_version() {
    let current = Arc::new(Mutex::new(None));
    let initial = CatalogService::new(
        FakeSource {
            response: Arc::new(Mutex::new(Ok(snapshot(
                vec![record(
                    "000001",
                    Exchange::Shenzhen,
                    "vendor-sz-000001",
                    assigned(),
                )],
                CompletenessEvidence::Complete { expected_count: 1 },
            )))),
        },
        FailingRepository {
            current: current.clone(),
            fail_publish: false,
        },
    );
    initial.refresh().unwrap();
    let old = current.lock().unwrap().clone().unwrap();

    let failing = CatalogService::new(
        FakeSource {
            response: Arc::new(Mutex::new(Ok(snapshot(
                vec![record(
                    "000001",
                    Exchange::Shenzhen,
                    "vendor-sz-000001",
                    assigned(),
                )],
                CompletenessEvidence::Complete { expected_count: 1 },
            )))),
        },
        FailingRepository {
            current: current.clone(),
            fail_publish: true,
        },
    );
    let error = failing.refresh().unwrap_err();
    assert!(
        matches!(error, CatalogError::Storage(ref error) if error.to_string() == "injected publish failure")
    );
    assert_eq!(failing.current().unwrap(), Some(old.clone()));
    assert_eq!(current.lock().unwrap().as_ref(), Some(&old));
}
