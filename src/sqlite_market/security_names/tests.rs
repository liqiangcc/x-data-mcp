//! `security_name_history` writer tests (issue-308 S-6 / #446 PR-12).
//!
//! The writer is driven directly with a domain [`NameSet`]; source parsing has
//! its own fixture tests in `official_security::security_names`. These tests
//! connect the stored rows to the read model and to the dataset registration,
//! including the "an inferred market row is refused" and "empty removes both"
//! cases.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::compact_market::{Digest, EvidenceKind, SourceBinding};
use crate::security_master::coverage::CoverageWindow;
use crate::security_master::names::{NameCertainty, NameSet, NameSourceKind};
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

fn row(code: &str, from: NaiveDate, to: Option<NaiveDate>, name: &str) -> NameInterval {
    NameInterval {
        exchange: Exchange::Sz,
        code: code.into(),
        effective_from: from,
        effective_to: to,
        name: name.into(),
        certainty: if to.is_some() {
            NameCertainty::Interval
        } else {
            NameCertainty::Unbounded
        },
        known_at: None,
        source_kind: NameSourceKind::SzseNameHistory,
        source_record_id: Some(format!("{code}|{from}|{name}")),
    }
}

fn set(rows: Vec<NameInterval>) -> NameSet {
    NameSet {
        rows,
        coverage: vec![
            CoverageWindow {
                source_kind: "szse_name_history".into(),
                exchange: Exchange::Sz,
                from: day(1998, 4, 28),
                to: day(2026, 9, 28),
                observed: true,
                rows: 3,
                reason: "fixture".into(),
            },
            CoverageWindow {
                source_kind: "unknown".into(),
                exchange: Exchange::Sh,
                from: day(1994, 1, 3),
                to: day(2026, 10, 30),
                observed: false,
                rows: 0,
                reason: "NOT_AVAILABLE fixture".into(),
            },
        ],
        pending_sources: Vec::new(),
    }
}

#[test]
fn round_trip_keeps_provenance_and_registers_counts() {
    let f = Fixture::new();
    let expected = set(vec![
        row("000004", day(1999, 4, 27), Some(day(2001, 3, 7)), "深安达A"),
        row("000004", day(2001, 3, 7), None, "深安达"),
    ]);
    let registration = replace_security_names(
        &f.c,
        &expected,
        f.binding,
        "task-1",
        "derived:security_names",
    )
    .unwrap()
    .expect("non-empty set registers");
    assert_eq!(registration.record_count, 2);
    assert_eq!(
        registration.first_day,
        compact_market::epoch_day(day(1999, 4, 27))
    );
    assert_eq!(
        registration.last_day,
        compact_market::epoch_day(day(2001, 3, 7))
    );
    let stored = read_names(&f.c).unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].name, "深安达A");
    assert_eq!(stored[0].certainty, NameCertainty::Interval);
    assert_eq!(stored[1].certainty, NameCertainty::Unbounded);
    assert_eq!(stored[0].source_kind, NameSourceKind::SzseNameHistory);
    // The SZSE table carries no publication time, so known_at stays NULL.
    assert!(stored.iter().all(|row| row.known_at.is_none()));
    let stored_registration = read_registration(&f.c)
        .unwrap()
        .expect("registration exists");
    assert_eq!(stored_registration.version_id, registration.version_id);
}

#[test]
fn empty_set_removes_rows_and_registration() {
    let f = Fixture::new();
    let expected = set(vec![row("000004", day(2001, 3, 7), None, "深安达")]);
    replace_security_names(
        &f.c,
        &expected,
        f.binding,
        "task-1",
        "derived:security_names",
    )
    .unwrap()
    .expect("first write registers");

    let empty = NameSet {
        rows: Vec::new(),
        coverage: Vec::new(),
        pending_sources: Vec::new(),
    };
    assert!(
        replace_security_names(&f.c, &empty, f.binding, "task-2", "derived:security_names")
            .unwrap()
            .is_none()
    );
    assert!(read_names(&f.c).unwrap().is_empty());
    assert!(read_registration(&f.c).unwrap().is_none());
}

#[test]
fn overlapping_intervals_are_refused_before_delete() {
    let f = Fixture::new();
    let first = set(vec![row(
        "000004",
        day(1999, 4, 27),
        Some(day(2001, 3, 7)),
        "深安达A",
    )]);
    replace_security_names(&f.c, &first, f.binding, "task-1", "derived:security_names")
        .unwrap()
        .expect("first write registers");

    let overlap = set(vec![
        row("000004", day(1999, 4, 27), Some(day(2001, 3, 7)), "深安达A"),
        row("000004", day(2000, 1, 1), None, "深安达"),
    ]);
    assert!(
        replace_security_names(
            &f.c,
            &overlap,
            f.binding,
            "task-2",
            "derived:security_names"
        )
        .is_err()
    );
    assert_eq!(read_names(&f.c).unwrap().len(), 1);
}

#[test]
fn a_row_for_a_market_without_a_dated_source_is_refused() {
    let f = Fixture::new();
    let mut name = row("600001", day(2001, 3, 7), None, "沪某股份");
    name.exchange = Exchange::Sh;
    let error = replace_security_names(
        &f.c,
        &set(vec![name]),
        f.binding,
        "task-1",
        "derived:security_names",
    )
    .unwrap_err();
    assert!(error.to_string().contains("admitted dated name source"));
    assert!(read_names(&f.c).unwrap().is_empty());
    assert!(read_registration(&f.c).unwrap().is_none());
}
