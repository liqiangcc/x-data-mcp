use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use tempfile::tempdir;
use x_data_mcp::catalog::{
    CatalogRepository, CatalogService, CatalogSnapshot, CatalogSourceError, CompletenessEvidence,
    Exchange, IndustryInfo, JsonCatalogStore, RefreshStatus, RejectReason, SecurityCatalogSource,
    SecurityKey, SecurityRecord, SecurityType,
};

#[derive(Clone)]
struct FakeSource {
    response: SharedResponse,
}

type SharedResponse = Arc<Mutex<Result<CatalogSnapshot, CatalogSourceError>>>;
type FakeService = CatalogService<FakeSource, JsonCatalogStore>;

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
