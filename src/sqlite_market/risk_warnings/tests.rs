//! `risk_warning_intervals` writer tests (issue-308 / #446 PR-10).
//!
//! The writer is driven directly with a domain [`RiskWarningSet`]; source
//! parsing has its own fixture tests in `official_security::risk_warnings`.
//! These tests connect the stored rows to the read model and to the dataset
//! registration, including the "empty is not evidence" case.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::compact_market::{Digest, EvidenceKind, SourceBinding};
use crate::security_master::risk_warnings::{CoverageWindow, RiskWarningSet};
use crate::security_master::{Exchange, RiskStatus, RiskWarningInterval};
use crate::sqlite_tasks::SqliteTasks;

const CREATED_AT: &str = "2026-10-30T00:00:00Z";

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    c: Connection,
    binding: Digest,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/dev/shm")
            .unwrap();
        let path: PathBuf = dir.path().join("market.db");
        SqliteTasks::new(&path).initialize_compact().unwrap();
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        let generation = evidence_store::put_evidence(
            &c,
            EvidenceKind::Generation,
            &json!({"generation_id": "gen-1", "created_at": CREATED_AT}),
        )
        .unwrap();
        let binding = evidence_store::put_source_binding(
            &c,
            &SourceBinding {
                generation_id: "gen-1".into(),
                generation_manifest_sha256: [0x0b; 32],
                day_member_sha256: [0x0c; 32],
                security_event_sha256: [0x0e; 32],
                evidence_digest: generation,
            },
        )
        .unwrap();
        Self {
            _dir: dir,
            c,
            binding,
        }
    }
}

fn row(
    exchange: Exchange,
    code: &str,
    from: NaiveDate,
    to: Option<NaiveDate>,
    status: RiskStatus,
) -> RiskWarningInterval {
    RiskWarningInterval {
        exchange,
        code: code.into(),
        effective_from: from,
        effective_to: to,
        raw_status_code: if status == RiskStatus::DelistingRiskWarning {
            "*ST".into()
        } else {
            "ST".into()
        },
        status,
        announced_at: (exchange == Exchange::Sh).then(|| from.to_string()),
        known_at: (exchange == Exchange::Sh).then_some(from),
        source_kind: if exchange == Exchange::Sz {
            "szse_name_history".into()
        } else {
            "exchange_announcement".into()
        },
        source_record_id: Some(format!("{}|{code}|{from}", exchange.as_str())),
        source_document_sha256: Some("a".repeat(64)),
    }
}

fn set(rows: Vec<RiskWarningInterval>) -> RiskWarningSet {
    RiskWarningSet {
        rows,
        coverage: vec![
            CoverageWindow {
                source_kind: "szse_name_history".into(),
                exchange: Exchange::Sz,
                from: day(1998, 4, 28),
                to: day(2026, 9, 28),
                observed: true,
                rows: 1,
                reason: "fixture".into(),
            },
            CoverageWindow {
                source_kind: "exchange_announcement".into(),
                exchange: Exchange::Sh,
                from: day(2000, 1, 1),
                to: day(2000, 3, 31),
                observed: false,
                rows: 0,
                reason: "unverified_zero fixture".into(),
            },
        ],
        pending_sources: Vec::new(),
    }
}

#[test]
fn round_trip_keeps_provenance_and_registers_counts() {
    let f = Fixture::new();
    let rows = vec![
        row(
            Exchange::Sz,
            "000004",
            day(1999, 4, 27),
            Some(day(2001, 3, 7)),
            RiskStatus::OtherRiskWarning,
        ),
        row(
            Exchange::Sh,
            "600001",
            day(2024, 5, 6),
            None,
            RiskStatus::OtherRiskWarning,
        ),
    ];
    let expected = set(rows);
    let registration = replace_risk_warnings(
        &f.c,
        &expected,
        f.binding,
        "task-1",
        "derived:risk_warnings",
    )
    .unwrap()
    .expect("non-empty set registers");
    assert_eq!(registration.record_count, 2);
    let stored = read_risk_warnings(&f.c).unwrap();
    let mut expected_rows = expected.rows.clone();
    expected_rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
    assert_eq!(stored, expected_rows);
    let stored_registration = read_registration(&f.c)
        .unwrap()
        .expect("registration exists");
    assert_eq!(stored_registration.version_id, registration.version_id);
}

#[test]
fn empty_set_removes_rows_and_registration() {
    let f = Fixture::new();
    let expected = set(vec![row(
        Exchange::Sz,
        "000004",
        day(1999, 4, 27),
        Some(day(2001, 3, 7)),
        RiskStatus::OtherRiskWarning,
    )]);
    replace_risk_warnings(
        &f.c,
        &expected,
        f.binding,
        "task-1",
        "derived:risk_warnings",
    )
    .unwrap()
    .expect("first write registers");
    assert!(!read_risk_warnings(&f.c).unwrap().is_empty());

    let empty = RiskWarningSet {
        rows: Vec::new(),
        coverage: Vec::new(),
        pending_sources: Vec::new(),
    };
    assert!(
        replace_risk_warnings(&f.c, &empty, f.binding, "task-2", "derived:risk_warnings")
            .unwrap()
            .is_none()
    );
    assert!(read_risk_warnings(&f.c).unwrap().is_empty());
    assert!(read_registration(&f.c).unwrap().is_none());
}

#[test]
fn overlapping_intervals_are_refused_before_delete() {
    let f = Fixture::new();
    let first = set(vec![row(
        Exchange::Sz,
        "000004",
        day(1999, 4, 27),
        Some(day(2001, 3, 7)),
        RiskStatus::OtherRiskWarning,
    )]);
    replace_risk_warnings(&f.c, &first, f.binding, "task-1", "derived:risk_warnings")
        .unwrap()
        .expect("first write registers");

    let overlap = set(vec![
        row(
            Exchange::Sz,
            "000004",
            day(1999, 4, 27),
            Some(day(2001, 3, 7)),
            RiskStatus::OtherRiskWarning,
        ),
        row(
            Exchange::Sz,
            "000004",
            day(2000, 1, 1),
            None,
            RiskStatus::DelistingRiskWarning,
        ),
    ]);
    assert!(
        replace_risk_warnings(&f.c, &overlap, f.binding, "task-2", "derived:risk_warnings")
            .is_err()
    );
    assert_eq!(read_risk_warnings(&f.c).unwrap(), first.rows);
}
