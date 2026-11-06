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
    // The writer stores the knowledge time the domain carries; it never fills
    // one in (XDM-157: the adapter resolves it from the snapshot store).
    assert!(stored.iter().all(|row| row.known_at.is_none()));
    let stored_registration = read_registration(&f.c)
        .unwrap()
        .expect("registration exists");
    assert_eq!(stored_registration.version_id, registration.version_id);
}

/// XDM-157: the knowledge time the adapter resolved survives the round trip,
/// and it is a *different* day from the effective one. The writer must not
/// derive `known_at` from `effective_from` (separation point 3).
#[test]
fn the_stored_row_keeps_the_knowledge_time_the_domain_carries() {
    let f = Fixture::new();
    let mut interval = row("000004", day(2001, 3, 7), None, "深安达");
    interval.known_at = Some(day(2026, 10, 31));
    replace_security_names(
        &f.c,
        &set(vec![interval]),
        f.binding,
        "task-1",
        "derived:security_names",
    )
    .unwrap()
    .expect("non-empty set registers");

    let stored = read_names(&f.c).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].known_at, Some(day(2026, 10, 31)));
    assert_eq!(stored[0].effective_from, day(2001, 3, 7));
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

// ------------------------------------------------ ETF subset (D-76 Q1) ---

use crate::security_master::names::{EtfNameChain, EtfNameSet, EtfNameStep, assemble_etf_names};

/// `(exchange, code, [(first observed, name)], ended)`.
type ChainSpec<'a> = (
    Exchange,
    &'a str,
    &'a [(NaiveDate, &'a str)],
    Option<NaiveDate>,
);

fn etf_set(chains: &[ChainSpec<'_>]) -> EtfNameSet {
    let chains: Vec<EtfNameChain> = chains
        .iter()
        .map(|(exchange, code, steps, ended)| EtfNameChain {
            exchange: *exchange,
            code: (*code).into(),
            steps: steps
                .iter()
                .map(|(observed, name)| EtfNameStep {
                    observed_from: *observed,
                    known_at: *observed,
                    name: (*name).into(),
                    source_record_id: format!("list:{observed}"),
                })
                .collect(),
            ended: *ended,
        })
        .collect();
    EtfNameSet {
        rows: assemble_etf_names(&chains).unwrap(),
        entities: chains
            .iter()
            .map(|chain| (chain.exchange, chain.code.clone()))
            .collect(),
    }
}

fn write(f: &Fixture, a_share: &NameSet, etf: EtfPart<'_>) -> Option<DerivedVersionRow> {
    replace_security_names_with(
        &f.c,
        a_share,
        etf,
        f.binding,
        "task",
        "derived:security_names",
    )
    .unwrap()
}

fn a_share_rows(f: &Fixture) -> Vec<crate::sqlite_market::security::SecurityNameRow> {
    read_names(&f.c)
        .unwrap()
        .into_iter()
        .filter(|row| row.source_kind != NameSourceKind::ExchangeSnapshot)
        .collect()
}

fn etf_rows(f: &Fixture) -> Vec<crate::sqlite_market::security::SecurityNameRow> {
    read_names(&f.c)
        .unwrap()
        .into_iter()
        .filter(|row| row.source_kind == NameSourceKind::ExchangeSnapshot)
        .collect()
}

fn a_share_set() -> NameSet {
    set(vec![
        row("000004", day(1999, 4, 27), Some(day(2001, 3, 7)), "深安达A"),
        row("000004", day(2001, 3, 7), None, "深安达"),
    ])
}

#[test]
fn writing_etf_names_leaves_the_a_share_rows_exactly_as_they_were() {
    let f = Fixture::new();
    write(&f, &a_share_set(), EtfPart::Keep);
    let before = a_share_rows(&f);
    assert_eq!(before.len(), 2);

    let etf = etf_set(&[
        (
            Exchange::Sh,
            "510300",
            &[(day(2026, 11, 5), "沪深300ETF华泰柏瑞")],
            None,
        ),
        (
            Exchange::Sz,
            "159915",
            &[
                (day(2026, 11, 5), "创业板ETF易方达"),
                (day(2026, 11, 9), "创业板ETF"),
            ],
            None,
        ),
    ]);
    let registration = write(&f, &a_share_set(), EtfPart::Replace(&etf)).unwrap();

    assert_eq!(
        a_share_rows(&f),
        before,
        "A-share rows are byte for byte the same"
    );
    let stored_etf = etf_rows(&f);
    assert_eq!(stored_etf.len(), 3);
    assert!(stored_etf.iter().all(|row| row.known_at.is_some()));
    // One registration for the whole table, counting both subsets.
    assert_eq!(registration.record_count, 5);
    assert_eq!(
        read_registration(&f.c).unwrap().unwrap().version_id,
        registration.version_id
    );
    let derived_rows: i64 =
        f.c.query_row(
            "SELECT COUNT(*) FROM derived_dataset_versions WHERE dataset_kind='security_names'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        derived_rows, 1,
        "a second dataset_key would make the reader refuse"
    );
}

#[test]
fn an_a_share_only_registration_has_no_etf_block_and_the_etf_one_does() {
    let f = Fixture::new();
    let plain = write(&f, &a_share_set(), EtfPart::Keep).unwrap();
    let (_, text) =
        evidence_store::read_evidence(&f.c, &plain.evidence_digest).expect("evidence reads");
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(value.get("etf").is_none(), "{value}");
    assert_eq!(value["sz_rows"], 2);

    let etf = etf_set(&[(Exchange::Sh, "510300", &[(day(2026, 11, 5), "甲ETF")], None)]);
    let with_etf = write(&f, &a_share_set(), EtfPart::Replace(&etf)).unwrap();
    let (_, text) =
        evidence_store::read_evidence(&f.c, &with_etf.evidence_digest).expect("evidence reads");
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["sz_rows"], 2, "ETF rows do not count as A-share rows");
    assert_eq!(value["etf"]["rows"], 1);
    assert_eq!(value["etf"]["algorithm_version"], "etf-names-v1");
    assert_eq!(value["etf"]["basis"], "fetch_time");
}

#[test]
fn an_a_share_write_keeps_the_etf_rows_when_the_etf_source_did_not_answer() {
    let f = Fixture::new();
    let etf = etf_set(&[(Exchange::Sh, "510300", &[(day(2026, 11, 5), "甲ETF")], None)]);
    write(&f, &a_share_set(), EtfPart::Replace(&etf));
    let etf_before = etf_rows(&f);
    assert_eq!(etf_before.len(), 1);

    // The ETF source failed or is pending: Keep. The A-share set changes.
    let changed = set(vec![row("000004", day(2001, 3, 7), None, "深安达新")]);
    write(&f, &changed, EtfPart::Keep);
    assert_eq!(
        etf_rows(&f),
        etf_before,
        "a failed ETF source never deletes names"
    );
    assert_eq!(a_share_rows(&f).len(), 1);
    assert_eq!(a_share_rows(&f)[0].name, "深安达新");
}

#[test]
fn a_replace_swaps_the_etf_subset_and_a_new_name_is_a_new_interval() {
    let f = Fixture::new();
    let first = etf_set(&[(Exchange::Sh, "510300", &[(day(2026, 11, 5), "甲ETF")], None)]);
    write(&f, &a_share_set(), EtfPart::Replace(&first));
    let second = etf_set(&[(
        Exchange::Sh,
        "510300",
        &[(day(2026, 11, 5), "甲ETF"), (day(2026, 11, 9), "乙ETF")],
        None,
    )]);
    write(&f, &a_share_set(), EtfPart::Replace(&second));
    let stored = etf_rows(&f);
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].name, "甲ETF");
    assert_eq!(stored[0].effective_to, Some(day(2026, 11, 9)));
    assert_eq!(stored[0].certainty, NameCertainty::Interval);
    assert_eq!(stored[1].name, "乙ETF");
    assert_eq!(stored[1].effective_to, None);
}

#[test]
fn the_same_etf_write_twice_is_idempotent() {
    let f = Fixture::new();
    let etf = etf_set(&[
        (Exchange::Sh, "510300", &[(day(2026, 11, 5), "甲ETF")], None),
        (Exchange::Sz, "159915", &[(day(2026, 11, 5), "乙ETF")], None),
    ]);
    let first = write(&f, &a_share_set(), EtfPart::Replace(&etf)).unwrap();
    let rows_first = read_names(&f.c).unwrap();
    let second = write(&f, &a_share_set(), EtfPart::Replace(&etf)).unwrap();
    assert_eq!(read_names(&f.c).unwrap(), rows_first);
    assert_eq!(first.version_id, second.version_id);
    assert_eq!(first.content_digest, second.content_digest);
}

#[test]
fn an_etf_only_table_registers_and_an_empty_one_removes_the_registration() {
    let f = Fixture::new();
    let empty = NameSet {
        rows: Vec::new(),
        coverage: Vec::new(),
        pending_sources: Vec::new(),
    };
    let etf = etf_set(&[(Exchange::Sh, "510300", &[(day(2026, 11, 5), "甲ETF")], None)]);
    assert!(write(&f, &empty, EtfPart::Replace(&etf)).is_some());
    assert!(read_registration(&f.c).unwrap().is_some());
    let none = EtfNameSet {
        rows: Vec::new(),
        entities: Default::default(),
    };
    assert!(write(&f, &empty, EtfPart::Replace(&none)).is_none());
    assert!(read_names(&f.c).unwrap().is_empty());
    assert!(read_registration(&f.c).unwrap().is_none());
}

#[test]
fn an_invalid_etf_set_is_refused_before_any_delete() {
    let f = Fixture::new();
    let good = etf_set(&[(Exchange::Sh, "510300", &[(day(2026, 11, 5), "甲ETF")], None)]);
    write(&f, &a_share_set(), EtfPart::Replace(&good));
    let before = read_names(&f.c).unwrap();

    // An ETF row whose entity is not declared: the whole write is refused.
    let mut bad = etf_set(&[(Exchange::Sh, "510310", &[(day(2026, 11, 5), "乙ETF")], None)]);
    bad.entities.clear();
    assert!(
        replace_security_names_with(
            &f.c,
            &a_share_set(),
            EtfPart::Replace(&bad),
            f.binding,
            "task",
            "derived:security_names"
        )
        .is_err()
    );
    assert_eq!(read_names(&f.c).unwrap(), before, "nothing was deleted");
}

/// The A-share set itself is validated on the A-share path before the ETF part
/// is looked at: an `exchange_snapshot` row smuggled into it is refused.
#[test]
fn an_etf_row_cannot_enter_through_the_a_share_set() {
    let f = Fixture::new();
    let etf = etf_set(&[(Exchange::Sh, "510300", &[(day(2026, 11, 5), "甲ETF")], None)]);
    let smuggled = set(etf.rows.clone());
    let error = replace_security_names_with(
        &f.c,
        &smuggled,
        EtfPart::Replace(&etf),
        f.binding,
        "task",
        "derived:security_names",
    )
    .unwrap_err();
    assert!(error.to_string().contains("admitted dated name source"));
    assert!(read_names(&f.c).unwrap().is_empty());
}

/// D-76 Q7: the research export keeps `security_names.parquet` to the SZ
/// A-share history. Its row source drops ETF rows and its manifest entry
/// counts only the rows the file holds, while the registration keeps the
/// whole-table `version_id`.
#[test]
fn the_research_export_sees_only_the_a_share_names() {
    use crate::research::{DerivedDataset, ExportRequest, read};

    let f = Fixture::new();
    let etf = etf_set(&[(Exchange::Sh, "510300", &[(day(2026, 11, 5), "甲ETF")], None)]);
    let registration = write(&f, &a_share_set(), EtfPart::Replace(&etf)).unwrap();
    assert_eq!(registration.record_count, 3);

    let request = ExportRequest::default();
    let rows = read::security_name_rows(&f.c, &request).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| row.source_kind == NameSourceKind::SzseNameHistory)
    );

    let entry = read::panel_version(
        &f.c,
        DerivedDataset::SecurityNames,
        names::DATASET_KIND,
        names::DATASET_KEY,
    )
    .unwrap()
    .expect("registered");
    assert_eq!(
        entry.record_count, 2,
        "the file's row count, not the table's"
    );
    assert_eq!(entry.version_id, registration.version_id);
}
