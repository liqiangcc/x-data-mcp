//! PR-4: `replay --scope delisted_etf_names` end to end, in-crate.
//!
//! The fixture freezes synthetic announcement pages and a synthetic D-72
//! extraction CSV into a source store, writes a `security_master` with one
//! delisted ETF and A-share/ETF name rows through the production writers, then
//! checks that the explicit scope rebuilds only the `exchange_snapshot` rows.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::json;

use super::replay::{
    DELISTED_ETF_NAMES_NOT_READY, NOTHING_SELECTED, REBUILT_SECURITY_NAMES, ReplayReport,
    ReplayRequest, Scope, replay,
};
use crate::compact_market::{Digest, EvidenceKind, SourceBinding};
use crate::security_master::assembly::Assembly;
use crate::security_master::names::{
    EtfNameSet, NameCertainty, NameInterval, NameSet, NameSourceKind,
};
use crate::security_master::{
    DelistingReason, DelistingSource, Exchange, IssuerResolution, ListingSource,
    SecurityLifecycleFact,
};
use crate::source_store::{Registration, SourceStore, StoreKind};
use crate::sqlite_market::{evidence_store, security, security_names};
use crate::sqlite_tasks::SqliteTasks;

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

const GEN: &str = "gen-pr4";
const MANIFEST_SHA: &str = "0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c";

fn lifecycle(
    exchange: Exchange,
    code: &str,
    class: u8,
    effective_to: Option<NaiveDate>,
) -> SecurityLifecycleFact {
    SecurityLifecycleFact {
        exchange,
        code: code.into(),
        effective_from: day(2005, 2, 23),
        effective_to,
        instrument_class: class,
        issuer_key: None,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_date: Some(day(2005, 2, 23)),
        listing_date_tdx: None,
        listing_source: ListingSource::ExchangeList,
        b_listing_date: None,
        delisting_date: effective_to,
        delisting_date_tdx: None,
        delisting_source: if effective_to.is_some() {
            DelistingSource::ExchangeList
        } else {
            DelistingSource::Unknown
        },
        delisting_reason_class: if effective_to.is_some() {
            DelistingReason::Other
        } else {
            DelistingReason::Unknown
        },
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: false,
        delisting_conflict: false,
        known_at: Some(day(2005, 2, 23)),
    }
}

fn a_share_name() -> NameInterval {
    NameInterval {
        exchange: Exchange::Sz,
        code: "000001".into(),
        effective_from: day(2001, 3, 7),
        effective_to: None,
        name: "深发展A".into(),
        certainty: NameCertainty::Unbounded,
        known_at: None,
        source_kind: NameSourceKind::SzseNameHistory,
        source_record_id: Some("2001-03-07|000001|深发展A".into()),
    }
}

fn current_etf_name() -> NameInterval {
    NameInterval {
        exchange: Exchange::Sh,
        code: "510300".into(),
        effective_from: day(2026, 11, 5),
        effective_to: None,
        name: "沪深300ETF华泰柏瑞".into(),
        certainty: NameCertainty::Unbounded,
        known_at: Some(day(2026, 11, 5)),
        source_kind: NameSourceKind::ExchangeSnapshot,
        source_record_id: Some("sse_etf_list:aaaa".into()),
    }
}

fn szse_notice() -> Vec<u8> {
    json!({"announceCount": 1, "data": [{
        "title": "终止上市公告",
        "publishTime": "2019-06-01 09:00:00",
        "secCode": ["159503"],
        "secName": ["财富管理ETF"],
        "attachPath": "/terminate.pdf",
    }]})
    .to_string()
    .into_bytes()
}

fn sse_notice() -> Vec<u8> {
    json!({"pageHelp": {"pageNo": 1, "pageCount": null, "total": 0}, "result": [{
        "TITLE": "关于XX交易型开放式指数证券投资基金终止上市的公告",
        "SECURITY_CODE": "510070",
        "SSEDATE": "2019-06-01",
        "URL": "/sse/terminate.pdf",
    }]})
    .to_string()
    .into_bytes()
}

fn extraction_csv(pdf_sha: &str) -> Vec<u8> {
    format!(
        "entity,delisting_date,announcement_date,source_id,doc_url,doc_sha256,page_no,snippet,\
         extractor,extracted_at,confidence,last_operating_day,registration_date,short_name,\
         short_name_page_no,short_name_snippet,short_name_extractor\n\
         sh.510070,2019-06-05,2019-06-01,sse_fund_delist_ann,https://example.invalid/a.pdf,\
         {pdf_sha},1,场内简称民企ETF,etf-short-name-v1,2026-11-06T02:00:00Z,high,,,民企ETF,1,\
         场内简称民企ETF,etf-short-name-v1\n"
    )
    .into_bytes()
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    c: Connection,
    store: SourceStore,
    binding: Digest,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let db = dir.path().join("market.db");
        SqliteTasks::new(&db).initialize_compact().unwrap();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
        let c = Connection::open(&db).unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        let generation = evidence_store::put_evidence(
            &c,
            EvidenceKind::Generation,
            &json!({"generation_id": GEN, "manifest_sha256": MANIFEST_SHA}),
        )
        .unwrap();
        let binding = evidence_store::put_source_binding(
            &c,
            &SourceBinding {
                generation_id: GEN.into(),
                generation_manifest_sha256: [0x0d; 32],
                day_member_sha256: [0x0e; 32],
                security_event_sha256: [0x0f; 32],
                evidence_digest: generation,
            },
        )
        .unwrap();
        let store = SourceStore::new(dir.path().join("source-store"));
        Self {
            _dir: dir,
            db,
            c,
            store,
            binding,
        }
    }

    fn register(
        &self,
        source_id: &str,
        kind: StoreKind,
        bytes: &[u8],
        acquired_at: &str,
        imported_at: Option<&str>,
        parser: Option<&str>,
    ) -> String {
        let mut reg = Registration::new(
            source_id,
            kind,
            format!("synthetic:{source_id}"),
            acquired_at,
        );
        reg.parser_version = parser.map(str::to_string);
        reg.imported_at = imported_at.map(str::to_string);
        if kind == StoreKind::ArchiveImport {
            reg.import_batch = Some("synthetic-batch".into());
            reg.basis_status = Some("approved_source".into());
            reg.evidence_grade = Some("A".into());
        }
        self.store.register(bytes, &reg).unwrap().sha256
    }

    fn freeze_sources(&self) {
        self.register(
            "szse_etf_notice_ann",
            StoreKind::HttpFetch,
            &szse_notice(),
            "2026-11-06T02:00:00Z",
            None,
            None,
        );
        self.register(
            "sse_fund_delist_ann",
            StoreKind::HttpFetch,
            &sse_notice(),
            "2026-11-06T02:00:00Z",
            None,
            None,
        );
        let pdf = b"%PDF-1.4 synthetic";
        let sha = self.register(
            crate::official_security::etf_delisting::PDF_SOURCE_ID,
            StoreKind::ArchiveImport,
            pdf,
            "2019-01-01T00:00:00Z",
            Some("2026-11-06T02:00:00Z"),
            Some("pdf-v1"),
        );
        self.register(
            crate::official_security::etf_delisting::EXTRACT_SOURCE_ID,
            StoreKind::ArchiveImport,
            &extraction_csv(&sha),
            "2019-01-01T00:00:00Z",
            Some("2026-11-06T02:00:00Z"),
            Some(crate::official_security::etf_delisting::EXTRACT_PARSER_VERSION),
        );
    }

    /// A database with a delisted SZ ETF, a delisted SH ETF, a current ETF and
    /// an A-share. Names: one A-share row + one current ETF row.
    fn write_inputs(&self, with_master: bool) {
        if with_master {
            security::replace_security_master(
                &self.c,
                &Assembly {
                    rows: vec![
                        lifecycle(Exchange::Sz, "159503", 2, Some(day(2019, 6, 20))),
                        lifecycle(Exchange::Sh, "510070", 2, Some(day(2019, 6, 20))),
                        lifecycle(Exchange::Sh, "510300", 2, None),
                        lifecycle(Exchange::Sz, "000001", 0, None),
                    ],
                    migrations: Vec::new(),
                    conflicts: Vec::new(),
                    pending_sources: Vec::new(),
                    unresolved: Vec::new(),
                    exchange_as_of: None,
                },
                self.binding,
                "task-master",
                "derived:security_master",
            )
            .unwrap();
        }
        security_names::replace_security_names_with(
            &self.c,
            &NameSet {
                rows: vec![a_share_name()],
                coverage: Vec::new(),
                pending_sources: Vec::new(),
            },
            security_names::EtfPart::Replace(&EtfNameSet {
                rows: vec![current_etf_name()],
                entities: std::collections::BTreeSet::from([(Exchange::Sh, "510300".into())]),
            }),
            self.binding,
            "task-names",
            "derived:security_names",
        )
        .unwrap();
    }

    fn request(&self, verify_only: bool, with_db: bool) -> ReplayRequest<'_> {
        ReplayRequest::new(
            Some(Scope::DelistedEtfNames),
            verify_only,
            with_db.then_some(self.db.as_path()),
        )
    }
}

fn scope(report: &ReplayReport) -> &super::replay::ScopeReport {
    report
        .scopes
        .iter()
        .find(|scope| scope.scope == "delisted_etf_names")
        .unwrap()
}

fn stored(fixture: &Fixture) -> Vec<NameInterval> {
    security::read_security_names(&fixture.c)
        .unwrap()
        .iter()
        .map(|row| row.to_interval().unwrap())
        .collect()
}

#[test]
fn the_explicit_scope_is_not_part_of_all() {
    assert!(!Scope::ALL.contains(&Scope::DelistedEtfNames));
    assert_eq!(Scope::ALL.len(), 6);
    assert_eq!(
        Scope::parse("delisted_etf_names"),
        Some(Scope::DelistedEtfNames)
    );
    assert_eq!(Scope::DelistedEtfNames.name(), "delisted_etf_names");
}

#[test]
fn without_a_database_the_scope_is_not_ready_and_writes_nothing() {
    let fixture = Fixture::new();
    fixture.freeze_sources();
    fixture.write_inputs(true);
    let before = stored(&fixture);
    let report = replay(&fixture.store, &fixture.request(false, false)).unwrap();
    assert_eq!(report.status, DELISTED_ETF_NAMES_NOT_READY);
    assert_eq!(scope(&report).status, DELISTED_ETF_NAMES_NOT_READY);
    assert!(scope(&report).not_ready_reason.is_some());
    assert_eq!(stored(&fixture), before, "no row may be written");
}

#[test]
fn a_database_without_class_two_master_rows_is_not_ready() {
    let fixture = Fixture::new();
    fixture.freeze_sources();
    // Only A-share name rows; no security_master at all.
    security_names::replace_security_names_with(
        &fixture.c,
        &NameSet {
            rows: vec![a_share_name()],
            coverage: Vec::new(),
            pending_sources: Vec::new(),
        },
        security_names::EtfPart::Keep,
        fixture.binding,
        "task-names",
        "derived:security_names",
    )
    .unwrap();
    let before = stored(&fixture);
    let report = replay(&fixture.store, &fixture.request(false, true)).unwrap();
    assert_eq!(report.status, DELISTED_ETF_NAMES_NOT_READY);
    assert_eq!(stored(&fixture), before);
}

#[test]
fn an_empty_store_selects_nothing_and_does_not_fail_the_explicit_scope() {
    let fixture = Fixture::new();
    fixture.write_inputs(true);
    let report = replay(&fixture.store, &fixture.request(false, true)).unwrap();
    assert_eq!(scope(&report).status, "store_empty");
    assert_eq!(scope(&report).rebuilt, NOTHING_SELECTED);
}

/// The core equivalence test: a writing replay adds exactly the two delisted
/// names, leaves the A-share row and the current ETF row byte-identical, and a
/// following `--verify-only` says `match`.
#[test]
fn replay_rebuilds_delisted_names_and_keeps_a_share_and_current_etf_rows() {
    let fixture = Fixture::new();
    fixture.freeze_sources();
    fixture.write_inputs(true);
    let before = stored(&fixture);

    let report = replay(&fixture.store, &fixture.request(false, true)).unwrap();
    assert_eq!(report.status, "ok", "{:?}", report.problems);
    let the_scope = scope(&report);
    assert_eq!(the_scope.status, "ok");
    assert_eq!(the_scope.rebuilt, REBUILT_SECURITY_NAMES);
    assert_eq!(the_scope.database, Some("match"));
    assert_eq!(the_scope.etf_rows, Some(2));
    let evidence = the_scope.delisted_etf_names.as_ref().unwrap();
    assert_eq!(evidence["targets"], 2); // only the two with effective_to (the current ETF is not delisted)
    assert_eq!(evidence["rows"], 2);

    let after = stored(&fixture);
    // A-share row unchanged.
    assert!(after.iter().any(|row| row == &a_share_name()));
    // Current ETF row unchanged.
    assert!(after.iter().any(|row| row == &current_etf_name()));
    // Exactly two new delisted rows.
    let added: Vec<&NameInterval> = after.iter().filter(|row| !before.contains(row)).collect();
    assert_eq!(added.len(), 2);
    let sz = added.iter().find(|row| row.code == "159503").unwrap();
    assert_eq!(sz.name, "财富管理ETF");
    assert_eq!(sz.effective_from, day(2019, 6, 1));
    assert_eq!(sz.effective_to, Some(day(2019, 6, 20)));
    assert_eq!(sz.known_at, Some(day(2026, 11, 6)));
    assert_eq!(
        sz.source_record_id.as_deref(),
        Some("szse_etf_notice_ann:/terminate.pdf")
    );
    let sh = added.iter().find(|row| row.code == "510070").unwrap();
    assert_eq!(sh.name, "民企ETF");
    assert_eq!(sh.effective_from, day(2019, 6, 1));
    assert_eq!(sh.effective_to, Some(day(2019, 6, 20)));
    assert_eq!(sh.known_at, Some(day(2026, 11, 6)));
    assert_eq!(
        sh.source_record_id.as_deref(),
        Some("sse_fund_delist_ann:/sse/terminate.pdf")
    );

    // Replay equivalence: --verify-only over the same store matches, and a
    // second writing replay is idempotent.
    let verify = replay(&fixture.store, &fixture.request(true, true)).unwrap();
    assert_eq!(verify.status, "ok");
    assert_eq!(scope(&verify).database, Some("match"));
    let again = replay(&fixture.store, &fixture.request(false, true)).unwrap();
    assert_eq!(again.status, "ok");
    assert_eq!(
        stored(&fixture),
        after,
        "a second replay must not change a row"
    );
}

/// Drift: changing a stored delisted row must be detected by --verify-only.
#[test]
fn verify_only_reports_drift_when_a_delisted_name_is_changed() {
    let fixture = Fixture::new();
    fixture.freeze_sources();
    fixture.write_inputs(true);
    replay(&fixture.store, &fixture.request(false, true)).unwrap();
    fixture
        .c
        .execute(
            "UPDATE security_name_history SET name='改过的名字' WHERE code='159503'",
            [],
        )
        .unwrap();
    let report = replay(&fixture.store, &fixture.request(true, true)).unwrap();
    assert_eq!(report.status, "drift");
    assert_eq!(scope(&report).database, Some("drift"));
    assert!(!scope(&report).drift.is_empty());
}

/// Live/replay equivalence.
///
/// The daily item's rule (`delisted_names_from_store` and
/// `merge_delisted_names`, which `etf_list::run_names` calls) and the
/// explicit replay scope are both pure functions of the same store bytes and
/// the same database state, so they must produce the same ETF rows.
#[test]
fn the_daily_merge_rule_and_the_replay_scope_produce_the_same_rows() {
    let fixture = Fixture::new();
    fixture.freeze_sources();
    fixture.write_inputs(true);
    // Compute the daily item's answer from the same pure functions, using the
    // `security_master` rows this fixture wrote (no second database lease).
    let (announcements, extractions, arrivals) =
        crate::official_security::etf_delisting::read_names_source(&fixture.store).unwrap();
    let master = security::read_security_master(&fixture.c).unwrap();
    let targets: Vec<_> = master
        .iter()
        .filter(|row| row.instrument_class == 2)
        .filter_map(|row| {
            row.effective_to.map(
                |end| crate::official_security::etf_delisting::DelistedEtfTarget {
                    exchange: row.exchange,
                    code: row.code.clone(),
                    effective_to: end,
                },
            )
        })
        .collect();
    let mut delisted = crate::official_security::etf_delisting::assemble_delisted_etf_names(
        &targets,
        &announcements,
        &extractions,
        &arrivals,
    );
    let current = crate::security_master::names::EtfNameSet {
        rows: vec![current_etf_name()],
        entities: std::collections::BTreeSet::from([(Exchange::Sh, "510300".into())]),
    };
    let expected =
        crate::official_security::etf_delisting::merge_delisted_names(current, &mut delisted)
            .unwrap();

    let report = replay(&fixture.store, &fixture.request(false, true)).unwrap();
    assert_eq!(report.status, "ok", "{:?}", report.problems);
    let stored_etf: Vec<NameInterval> = stored(&fixture)
        .into_iter()
        .filter(|row| row.source_kind == NameSourceKind::ExchangeSnapshot)
        .collect();
    assert_eq!(stored_etf, expected.rows);
}
