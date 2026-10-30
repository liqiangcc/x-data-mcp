//! `market.industry_assignments` read-model fixtures (#446 §14 PR-7).
//!
//! The database is produced through PR-6's real writer: `rebuild` computes the
//! intervals from snapshot facts, then `sqlite_market::industry::replace_industry`
//! stores them. The tests below therefore exercise the production interval
//! computation and the production read model, not hand-written rows.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::NaiveDate;
use rusqlite::Connection;

use super::{
    DATASET_INDUSTRY_ASSIGNMENTS, IndustryAssignment, IndustryReport, IndustryRequest,
    UnknownReason, assignments, dataset_identity,
};
use crate::data_supply::Access;
use crate::data_supply::query::{QueryPlan, ReadRepository};
use crate::data_supply::wire::{
    Dataset, FactValue, Filter, ReadContractVersion, Row, Security, Selection, VersionChoice,
};
use crate::industry::{IndustryMember, SnapshotFacts, SnapshotRecord, SourceKind, rebuild};
use crate::research::temporal::{Temporal, TemporalReport};
use crate::sqlite_market::backend::MarketBackend;
use crate::sqlite_market::industry as store;
use crate::sqlite_supply::SqliteSupply;
use crate::sqlite_tasks::SqliteTasks;

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn dictionary() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("X5001".to_owned(), "软件服务".to_owned()),
        ("X5002".to_owned(), "硬件设备".to_owned()),
        ("X5003".to_owned(), "新材料".to_owned()),
    ])
}

fn snapshot(
    label: &str,
    published: NaiveDate,
    known_at: &str,
    members: &[(&str, &str, &str)],
) -> SnapshotFacts {
    let seed = format!("{label}:{published}:{members:?}");
    let digest = crate::hash::sha256(seed.as_bytes());
    SnapshotFacts {
        record: SnapshotRecord {
            member_sha256: digest.clone(),
            store_sha256: digest,
            base_zip_sha256: None,
            generation_manifest_sha256: None,
            source_kind: SourceKind::Generation,
            publisher_last_modified: Some(published.to_string()),
            observed_at: Some(known_at.to_owned()),
            ingested_at: known_at.to_owned(),
            imported_at: None,
            evidence_grade: "A".to_owned(),
            parser_version: "test".to_owned(),
            row_count: members.len() as i64,
            scope_member_count: members.len() as i64,
            dictionary_sha256: None,
            notes: None,
        },
        members: members
            .iter()
            .map(|(exchange, code, industry)| IndustryMember {
                exchange: (*exchange).to_owned(),
                code: (*code).to_owned(),
                industry_code: (*industry).to_owned(),
                t_code: format!("T{industry}"),
                level: 2,
            })
            .collect(),
        dictionary: dictionary(),
        dictionary_present: true,
    }
}

fn snapshots() -> Vec<SnapshotFacts> {
    vec![
        snapshot(
            "s1",
            day(2026, 9, 30),
            "2026-09-30T02:00:00Z",
            &[
                ("sz", "000001", "X5001"),
                ("sh", "600006", "X5001"),
                ("sz", "000004", "X5001"),
            ],
        ),
        snapshot(
            "s2",
            day(2026, 10, 12),
            "2026-10-12T02:00:00Z",
            &[
                ("sz", "000001", "X5002"),
                ("sh", "600006", "X5001"),
                ("sz", "000004", "X5001"),
            ],
        ),
        snapshot(
            "s3",
            day(2026, 10, 16),
            "2026-10-16T02:00:00Z",
            &[("sz", "000001", "X5002"), ("sh", "600006", "X5001")],
        ),
        snapshot(
            "s4",
            day(2026, 10, 19),
            "2026-10-19T02:00:00Z",
            &[
                ("sz", "000001", "X5002"),
                ("sh", "600006", "X5001"),
                ("sz", "000005", "X5003"),
            ],
        ),
        // A second observation of the same mapping must not open a new interval.
        snapshot(
            "s5",
            day(2026, 10, 24),
            "2026-10-24T02:00:00Z",
            &[
                ("sz", "000001", "X5002"),
                ("sh", "600006", "X5001"),
                ("sz", "000005", "X5003"),
            ],
        ),
        snapshot(
            "s6",
            day(2026, 10, 28),
            "2026-10-28T02:00:00Z",
            &[("sz", "000001", "X5002"), ("sz", "000005", "X5003")],
        ),
    ]
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Fixture {
    fn new(with_snapshots: bool) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("industry-read-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("market.db");
        SqliteTasks::new(&path).initialize_compact().unwrap();
        // The compact backend requires a private runtime file; the default
        // umask in CI leaves the fresh database group/other-readable.
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        if with_snapshots {
            let connection = Connection::open(&path).unwrap();
            let rebuild = rebuild(&snapshots()).unwrap();
            store::replace_industry(&connection, &rebuild).unwrap();
        }
        Self { _dir: dir, path }
    }

    fn connection(&self) -> Connection {
        Connection::open(&self.path).unwrap()
    }

    fn supply(&self) -> SqliteSupply {
        SqliteSupply {
            market: MarketBackend::open(&SqliteTasks::new(&self.path)).unwrap(),
        }
    }
}

fn assigned<'a>(report: &'a IndustryReport, entity: &str) -> &'a super::AssignedIndustry {
    let row = report
        .assignments
        .iter()
        .find(|row| row.entity() == entity)
        .unwrap();
    match row {
        IndustryAssignment::Assigned(row) => row,
        IndustryAssignment::Unknown(row) => {
            panic!("{entity} was unknown: {}", row.reason.as_str())
        }
    }
}

fn unknown<'a>(report: &'a IndustryReport, entity: &str) -> &'a super::UnknownIndustry {
    let row = report
        .assignments
        .iter()
        .find(|row| row.entity() == entity)
        .unwrap();
    match row {
        IndustryAssignment::Unknown(row) => row,
        IndustryAssignment::Assigned(_) => panic!("{entity} was assigned"),
    }
}

fn report(as_of: NaiveDate, entity: &str) -> IndustryReport {
    let fixture = Fixture::new(true);
    let connection = fixture.connection();
    assignments(
        &connection,
        &IndustryRequest::for_entities(as_of, [entity.to_owned()]),
    )
    .unwrap()
}

/// The ten `as_of` cases from §14 PR-7: coverage start, unchanged spans,
/// reclassification, removal, addition and the open final interval.
#[test]
fn ten_as_of_cases_cover_assignment_reclassification_and_gaps() {
    struct Case {
        as_of: NaiveDate,
        entity: &'static str,
        status: &'static str,
        industry: Option<&'static str>,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
        reason: Option<UnknownReason>,
    }
    let cases = [
        Case {
            as_of: day(2026, 9, 29),
            entity: "sz.000001",
            status: "unknown",
            industry: None,
            from: None,
            to: None,
            reason: Some(UnknownReason::BeforeCoverage),
        },
        Case {
            as_of: day(2026, 9, 30),
            entity: "sz.000001",
            status: "assigned",
            industry: Some("X5001"),
            from: Some(day(2026, 9, 30)),
            to: Some(day(2026, 10, 12)),
            reason: None,
        },
        Case {
            as_of: day(2026, 10, 11),
            entity: "sz.000001",
            status: "assigned",
            industry: Some("X5001"),
            from: Some(day(2026, 9, 30)),
            to: Some(day(2026, 10, 12)),
            reason: None,
        },
        Case {
            as_of: day(2026, 10, 12),
            entity: "sz.000001",
            status: "assigned",
            industry: Some("X5002"),
            from: Some(day(2026, 10, 12)),
            to: None,
            reason: None,
        },
        Case {
            as_of: day(2026, 10, 15),
            entity: "sz.000001",
            status: "assigned",
            industry: Some("X5002"),
            from: Some(day(2026, 10, 12)),
            to: None,
            reason: None,
        },
        Case {
            as_of: day(2026, 10, 16),
            entity: "sz.000004",
            status: "unknown",
            industry: None,
            from: None,
            to: None,
            reason: Some(UnknownReason::MissingInSnapshot),
        },
        Case {
            as_of: day(2026, 10, 18),
            entity: "sz.000005",
            status: "unknown",
            industry: None,
            from: None,
            to: None,
            reason: Some(UnknownReason::MissingInSnapshot),
        },
        Case {
            as_of: day(2026, 10, 19),
            entity: "sz.000005",
            status: "assigned",
            industry: Some("X5003"),
            from: Some(day(2026, 10, 19)),
            to: None,
            reason: None,
        },
        Case {
            as_of: day(2026, 10, 28),
            entity: "sh.600006",
            status: "unknown",
            industry: None,
            from: None,
            to: None,
            reason: Some(UnknownReason::MissingInSnapshot),
        },
        Case {
            as_of: day(2026, 10, 30),
            entity: "sz.000001",
            status: "assigned",
            industry: Some("X5002"),
            from: Some(day(2026, 10, 12)),
            to: None,
            reason: None,
        },
    ];
    for case in cases {
        let report = report(case.as_of, case.entity);
        assert_eq!(report.as_of, case.as_of);
        assert_eq!(report.assignments.len(), 1);
        match (&report.assignments[0], case.status) {
            (IndustryAssignment::Assigned(row), "assigned") => {
                assert_eq!(row.industry_code, case.industry.unwrap());
                assert_eq!(Some(row.effective_from), case.from);
                assert_eq!(row.effective_to, case.to);
                assert!(row.known_at <= case.as_of);
                assert_eq!(row.scheme, crate::industry::SCHEME);
                assert_eq!(row.level, 2);
                assert_eq!(row.name_status, "resolved");
                assert!(row.name.is_some());
                assert_eq!(row.effective_from_basis, "publisher_last_modified");
                assert_eq!(row.evidence_grade, "A");
                assert_eq!(row.source_snapshot_sha256.len(), 64);
            }
            (IndustryAssignment::Unknown(row), "unknown") => {
                assert_eq!(Some(row.reason), case.reason);
            }
            (row, expected) => panic!("{} was {}, expected {expected}", row.entity(), row.status()),
        }
    }
}

#[test]
fn known_at_excludes_later_rows_and_counts_them() {
    let fixture = Fixture::new(true);
    let connection = fixture.connection();
    let cutoff = day(2026, 10, 18);
    let report = assignments(
        &connection,
        &IndustryRequest::for_entities(day(2026, 10, 19), ["sz.000005".to_owned()])
            .with_known_at(cutoff),
    )
    .unwrap();
    assert_eq!(
        unknown(&report, "sz.000005").reason,
        UnknownReason::NotKnownAt
    );
    let temporal = report
        .temporal
        .dataset(DATASET_INDUSTRY_ASSIGNMENTS)
        .expect("industry assignments are observed");
    assert_eq!(temporal.rows, 1);
    assert_eq!(temporal.rows_with_known_at, 1);
    assert_eq!(temporal.unknown_known_at_rows, 0);
    assert_eq!(temporal.excluded_known_at_rows, 1);

    // The same row is visible on its own day.
    let report = assignments(
        &connection,
        &IndustryRequest::for_entities(day(2026, 10, 19), ["sz.000005".to_owned()])
            .with_known_at(day(2026, 10, 19)),
    )
    .unwrap();
    assert_eq!(assigned(&report, "sz.000005").industry_code, "X5003");
}

/// The temporal layer owns the NULL rule; this pins the same vocabulary for
/// this dataset: a NULL row is dropped under a cutoff and counted, never
/// treated as old or new. The production table's `known_at` is NOT NULL, so
/// the fixture uses the report directly.
#[test]
fn null_known_at_is_excluded_and_counted() {
    let mut report = TemporalReport::new(Temporal {
        as_of: day(2026, 10, 19),
        known_at: Some(day(2026, 10, 18)),
    });
    assert!(!report.observe(DATASET_INDUSTRY_ASSIGNMENTS, None));
    assert_eq!(report.unknown_known_at_rows(), 1);
    assert_eq!(
        report.datasets_without_known_at(),
        vec![DATASET_INDUSTRY_ASSIGNMENTS]
    );
    let observed = report
        .dataset(DATASET_INDUSTRY_ASSIGNMENTS)
        .expect("dataset was observed");
    assert_eq!(observed.unknown_known_at_rows, 1);
    assert_eq!(observed.excluded_known_at_rows, 1);
}

#[test]
fn an_empty_interval_table_answers_before_coverage() {
    let fixture = Fixture::new(false);
    let connection = fixture.connection();
    let report = assignments(
        &connection,
        &IndustryRequest::for_entities(day(2026, 10, 19), ["sz.000001".to_owned()]),
    )
    .unwrap();
    assert_eq!(report.coverage_start, None);
    assert_eq!(
        unknown(&report, "sz.000001").reason,
        UnknownReason::BeforeCoverage
    );
}

#[test]
fn dataset_identity_is_stable_and_names_the_coverage_start() {
    let fixture = Fixture::new(true);
    let connection = fixture.connection();
    let first = dataset_identity(&connection).unwrap();
    let second = dataset_identity(&connection).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.coverage_start, Some(day(2026, 9, 30)));
    assert_eq!(first.version_id.len(), 64);
    assert_eq!(first.snapshot_rows, 6);
    assert!(first.assignment_rows > 0);
    assert!(first.latest_ingested_at.is_some());
}

fn run_query(
    fixture: &Fixture,
    entity: &str,
    as_of: NaiveDate,
) -> crate::data_supply::query::QuerySnapshot {
    let supply = fixture.supply();
    let access = Access::local();
    let query = crate::data_supply::wire::Query {
        dataset: Dataset::IndustryAssignments,
        contract_version: ReadContractVersion::V2,
        selection: Selection::Single {
            entity: Security::parse(entity).unwrap(),
        },
        filter: Filter::Daily {
            start_date: day(2026, 9, 30),
            end_date: day(2026, 10, 31),
            as_of: Some(as_of),
        },
        version: VersionChoice::LatestPerEntity {},
        page_size: 100,
        cursor: None,
        fields: None,
        known_at: None,
    };
    let mut plan = QueryPlan::new(query, &access).unwrap();
    plan.resolve(&supply, &access).unwrap();
    supply.query(&plan, &access).unwrap()
}

fn cell_text(row: &Row, field: &str) -> Option<String> {
    let Row::Fact { cells, .. } = row else {
        panic!("industry row is not a fact")
    };
    match cells.get(field) {
        Some(FactValue::Text(value)) => Some(value.clone()),
        Some(FactValue::Null) | None => None,
        Some(other) => panic!("{field} has unexpected value {other:?}"),
    }
}

#[test]
fn the_v8_branch_projects_assigned_and_unknown_rows() {
    let fixture = Fixture::new(true);

    let snapshot = run_query(&fixture, "sz.000001", day(2026, 10, 16));
    assert_eq!(snapshot.manifest.len(), 1);
    assert_eq!(
        snapshot.manifest[0].status,
        crate::data_supply::wire::ManifestStatus::Present
    );
    assert!(snapshot.manifest[0].version_ref.is_some());
    assert_eq!(
        snapshot.manifest[0]
            .metadata
            .as_ref()
            .unwrap()
            .coverage_status,
        "partial"
    );
    assert_eq!(snapshot.rows.len(), 1);
    assert_eq!(
        cell_text(&snapshot.rows[0], "status").as_deref(),
        Some("assigned")
    );
    assert_eq!(
        cell_text(&snapshot.rows[0], "industry_code").as_deref(),
        Some("X5002")
    );
    assert_eq!(
        cell_text(&snapshot.rows[0], "name").as_deref(),
        Some("硬件设备")
    );
    assert_eq!(snapshot.temporal.as_ref().unwrap().as_of, day(2026, 10, 16));

    let snapshot = run_query(&fixture, "sz.000004", day(2026, 10, 16));
    assert_eq!(snapshot.rows.len(), 1);
    assert_eq!(
        cell_text(&snapshot.rows[0], "status").as_deref(),
        Some("unknown")
    );
    assert_eq!(
        cell_text(&snapshot.rows[0], "reason").as_deref(),
        Some("missing_in_snapshot")
    );
    assert!(cell_text(&snapshot.rows[0], "industry_code").is_none());

    let Row::Fact { day: row_day, .. } = &snapshot.rows[0] else {
        unreachable!()
    };
    assert_eq!(*row_day, day(2026, 10, 16));
}

#[test]
fn a_v1_request_for_the_new_dataset_is_rejected() {
    let fixture = Fixture::new(true);
    let supply = fixture.supply();
    let access = Access::local();
    let query = crate::data_supply::wire::Query {
        dataset: Dataset::IndustryAssignments,
        contract_version: ReadContractVersion::V1,
        selection: Selection::Single {
            entity: Security::parse("sz.000001").unwrap(),
        },
        filter: Filter::Daily {
            start_date: day(2026, 9, 30),
            end_date: day(2026, 10, 31),
            as_of: Some(day(2026, 10, 16)),
        },
        version: VersionChoice::LatestPerEntity {},
        page_size: 100,
        cursor: None,
        fields: None,
        known_at: None,
    };
    let error = QueryPlan::new(query, &access).err().unwrap();
    assert!(error.to_string().contains("requires contract_version v2"));
    let _ = supply;
}
