//! M3 PR-4 (#446 §4.4, D-62): the `sh_sz_a_listed_ever` write scope.
//!
//! The resolution is exercised against a real compact database and a real
//! verified generation: master rows and series presence come from SQLite, the
//! member set from the inspected `.day` archive. The pure filter/budget tests
//! cover the boundaries without that fixture.
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;

use super::{CollectionContract, ListedEverCollectionContract};
use crate::acquire::{AcquireService, Source};
use crate::compact_market::EvidenceKind;
use crate::data_supply::Access;
use crate::data_supply::generation::{Bound, GenerationSource, SOURCE_GENERATION_KEY};
use crate::data_supply::wire::{CatalogScope, Dataset, Selection, WriteSelection};
use crate::sqlite_market::backend::MarketBackend;
use crate::sqlite_market::{evidence_store, series_commit};
use crate::sqlite_supply::SqliteSupply;
use crate::sqlite_tasks::SqliteTasks;
use crate::task_control::{CommandKey, Contract, RequestOutcome, Status, service::TaskControl};
use std::collections::BTreeSet;

const LISTED_EVER: &str = "sh_sz_a_listed_ever";

fn day_record(date: u32, ohlc: [u32; 4], amount: f32, volume: u32) -> [u8; 32] {
    let mut r = [0u8; 32];
    r[0..4].copy_from_slice(&date.to_le_bytes());
    for (i, w) in ohlc.iter().enumerate() {
        r[4 + i * 4..8 + i * 4].copy_from_slice(&w.to_le_bytes());
    }
    r[20..24].copy_from_slice(&amount.to_bits().to_le_bytes());
    r[24..28].copy_from_slice(&volume.to_le_bytes());
    r
}
fn day_record_r7(date: u32, ohlc: [u32; 4], amount: f32, volume: u32, r7: u32) -> [u8; 32] {
    let mut r = day_record(date, ohlc, amount, volume);
    r[28..32].copy_from_slice(&r7.to_le_bytes());
    r
}
fn gbbq_record(market: u8, code: &str, date_raw: u32, category: u8) -> [u8; 29] {
    gbbq_factor_record(market, code, date_raw, category, [0.0; 4])
}
fn gbbq_factor_record(
    market: u8,
    code: &str,
    date_raw: u32,
    category: u8,
    c: [f32; 4],
) -> [u8; 29] {
    let mut clear = [0u8; 29];
    clear[0] = market;
    clear[1..1 + code.len()].copy_from_slice(code.as_bytes());
    clear[8..12].copy_from_slice(&date_raw.to_le_bytes());
    clear[12] = category;
    for (index, value) in c.iter().enumerate() {
        clear[13 + index * 4..17 + index * 4].copy_from_slice(&value.to_le_bytes());
    }
    crate::gbbq::encrypt_record(&clear)
}
fn gbbq_member(records: &[[u8; 29]]) -> Vec<u8> {
    let mut bytes = (records.len() as u32).to_le_bytes().to_vec();
    for r in records {
        bytes.extend_from_slice(r);
    }
    bytes
}
fn write_zip(path: &Path, members: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut w = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in members {
        w.start_file(*name, options).unwrap();
        w.write_all(bytes).unwrap();
    }
    w.finish().unwrap();
}
/// `yyyymmdd` for a day relative to today: the acquisition probe requires
/// the freshest sampled bar to sit inside its tolerance window.
fn day_u32(days_ago: u64) -> u32 {
    let day = crate::calendar::shanghai_today() - chrono::Days::new(days_ago);
    day.format("%Y%m%d").to_string().parse().unwrap()
}
fn days() -> Vec<u8> {
    [
        day_record(day_u32(2), [1128, 1130, 1125, 1129], 0.0, 100),
        day_record(day_u32(1), [1129, 1132, 1128, 1131], 0.0, 110),
        day_record(day_u32(0), [1131, 1135, 1130, 1134], 0.0, 120),
    ]
    .concat()
}
fn one_day() -> Vec<u8> {
    day_record(day_u32(0), [1128, 1130, 1125, 1129], 0.0, 100).to_vec()
}
/// ETF 3dp bars: raw integers are 厘 (41.780 元 = 41_780), never 分.
fn etf_days() -> Vec<u8> {
    [
        day_record(day_u32(2), [41680, 41820, 41600, 41780], 0.0, 10_000),
        day_record(day_u32(1), [41780, 41900, 41700, 41860], 0.0, 11_000),
        day_record(day_u32(0), [41860, 42000, 41800, 41950], 0.0, 12_000),
    ]
    .concat()
}
/// Hand-recorded ETF bar (`sz159338` anchor): volume_raw is hands, so the
/// source value is 42,937,950 and the true share count is ×100. Amount is the
/// same 4.293795e9 shares at ~1.15 元, inside the day's raw/1000 range.
fn etf_hand_days() -> Vec<u8> {
    [
        day_record(day_u32(1), [1140, 1160, 1130, 1150], 0.0, 1_000),
        day_record_r7(
            day_u32(0),
            [1150, 1160, 1140, 1158],
            4_937_864_250.0,
            42_937_950,
            0xC364_0053,
        ),
    ]
    .concat()
}
/// Delisted ETF tail: the final two bars have zero volume and zero amount
/// and must stay in the series byte-for-byte (D-72).
fn etf_delisted_days() -> Vec<u8> {
    [
        day_record(day_u32(4), [2000, 2050, 1990, 2040], 2_040_000.0, 1_000),
        day_record(day_u32(3), [2040, 2060, 2030, 2050], 0.0, 0),
        day_record(day_u32(2), [2050, 2050, 2050, 2050], 0.0, 0),
    ]
    .concat()
}

/// Mirror snapshot: five codes with bars, two members without any bar, and
/// enough filler members that the two empty ones stay outside the
/// acquisition probe's deterministic 64-member sample.
fn write_mirror(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let tdxhy = b"1|600101|T090302|||X6102\r\n";
    write_zip(
        &dir.join("base.zip"),
        &[
            ("tdxhy.cfg", tdxhy.as_slice()),
            ("infoharbor_ex.code", b"600101|name|kw\r\n".as_slice()),
        ],
    );
    let records = [
        gbbq_record(1, "600101", 20240101, 1),
        gbbq_record(1, "600102", 20240101, 1),
        gbbq_record(1, "600103", 20240101, 1),
        gbbq_record(1, "600104", 20240101, 1),
        gbbq_record(1, "600105", 20240101, 1),
        // M3-2 §12 provenance (synthetic records, not exchange bytes):
        // sz159919 2025-03-31 cat1 c1=0.661; sz159901 2021-04-12 cat11 c3=2.
        gbbq_factor_record(0, "159919", 20250331, 1, [0.661, 0.0, 0.0, 0.0]),
        gbbq_factor_record(0, "159901", 20210412, 11, [0.0, 0.0, 2.0, 0.0]),
        gbbq_factor_record(0, "159901", 20101122, 11, [0.0, 0.0, 5.0, 0.0]),
    ];
    write_zip(
        &dir.join("gbbq.zip"),
        &[("gbbq", gbbq_member(&records).as_slice())],
    );
    let full = days();
    let short = one_day();
    let empty: &[u8] = &[];
    let fillers: Vec<String> = (201..=270)
        .map(|code| format!("sh\\lday\\sh600{code:03}.day"))
        .collect();
    let mut members: Vec<(&str, &[u8])> = vec![
        ("sh\\lday\\sh600101.day", full.as_slice()),
        ("sh\\lday\\sh600102.day", full.as_slice()),
        ("sh\\lday\\sh600103.day", short.as_slice()),
        ("sh\\lday\\sh600104.day", short.as_slice()),
        ("sh\\lday\\sh600105.day", short.as_slice()),
    ];
    for name in &fillers {
        members.push((name.as_str(), short.as_slice()));
    }
    members.push(("sh\\lday\\sh600901.day", empty));
    members.push(("sz\\lday\\sz000101.day", empty));
    let etf = etf_days();
    let etf_hand = etf_hand_days();
    let etf_delisted = etf_delisted_days();
    members.push(("sh\\lday\\sh510050.day", etf.as_slice()));
    members.push(("sz\\lday\\sz159919.day", etf.as_slice()));
    members.push(("sz\\lday\\sz159901.day", etf.as_slice()));
    members.push(("sz\\lday\\sz159338.day", etf_hand.as_slice()));
    members.push(("sh\\lday\\sh510070.day", etf_delisted.as_slice()));
    write_zip(&dir.join("hsjday.zip"), &members);
}
fn drive(service: &AcquireService) {
    for _ in 0..16 {
        if service.worker_once().unwrap().is_none() {
            break;
        }
    }
}
fn activate(root: &Path, mirror: &Path) -> String {
    let service = AcquireService::open(root, Source::Mirror(mirror.into())).unwrap();
    let acceptance = service.acquire_generation("req-gen").unwrap();
    drive(&service);
    let generation_id = service
        .task(&acceptance.task_id)
        .unwrap()
        .generation_id
        .unwrap();
    let reply = service
        .verify_generation("req-gen-verify", &generation_id)
        .unwrap();
    assert_eq!(reply.state, "consistency_verified", "{reply:?}");
    let activated = service
        .activate_generation("req-gen-activate", &generation_id, None)
        .unwrap();
    assert_eq!(activated.state, "active", "{activated:?}");
    generation_id
}

struct Fixture {
    backend: MarketBackend,
    generation: GenerationSource,
    generation_id: String,
    master_version: String,
    _dirs: (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
}

fn fixture() -> Fixture {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path());
    let root = tempfile::tempdir().unwrap();
    let generation_id = activate(root.path(), mirror.path());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    SqliteTasks::new(&path).initialize_compact().unwrap();
    set_private(&path);
    let backend = MarketBackend::open(&SqliteTasks::new(&path)).unwrap();
    let master_version = seed_master(&backend);
    seed_series(&backend, "sh.600101");
    Fixture {
        backend,
        generation: GenerationSource::new(root.path()),
        generation_id,
        master_version,
        _dirs: (mirror, root, dir),
    }
}

/// Seven `security_master` rows (all delisted except sh.600101) + one
/// registration row. Direct SQL is intentional: the writer path is M2's, not
/// what this test proves.
fn seed_master(backend: &MarketBackend) -> String {
    let MarketBackend::Compact(repo) = backend else {
        panic!("fixture must open the compact backend");
    };
    let version = "07".repeat(32);
    repo.tx(|c| {
        let binding = series_commit::put_generation_binding(
            c,
            "gen-1",
            &"0b".repeat(32),
            &"0c".repeat(32),
            &"0e".repeat(32),
            &json!({"snapshot_inputs": []}),
        )?;
        let evidence =
            evidence_store::put_evidence(c, EvidenceKind::Continuity, &json!({"mode": "test"}))?;
        let entities = [
            ("sh", "600101"),
            ("sh", "600102"),
            ("sh", "600103"),
            ("sh", "600104"),
            ("sh", "600105"),
            ("sh", "600901"),
            ("sz", "000101"),
        ];
        for (exchange, code) in entities {
            let (from, to) = if code == "600101" {
                (0i64, None)
            } else {
                (0i64, Some(20000i64))
            };
            c.execute(
                "INSERT INTO security_master(exchange,code,effective_from,effective_to,\
                 instrument_class,issuer_resolution,listing_date,listing_source,\
                 delisting_date,delisting_source,delisting_reason_class,listing_conflict,\
                 delisting_conflict,source_binding_digest,evidence_digest) \
                 VALUES(?1,?2,?3,?4,0,'code_date',?3,'tdx_ssdate',?4,'tdx_arrangement',\
                 'other',0,0,?5,?6)",
                rusqlite::params![
                    exchange,
                    code,
                    from,
                    to,
                    binding.as_slice(),
                    evidence.as_slice()
                ],
            )
            .unwrap();
        }
        // M3-2 PR-B ETF fixtures: three current class=2 members with `.day`,
        // one delisted member (D-72 close, tail zero bars), and one list-only
        // member with no `.day` that must be reported absent.
        for (exchange, code, to, delisting_source) in [
            ("sh", "510050", None, "unknown"),
            ("sz", "159919", None, "unknown"),
            ("sz", "159901", None, "unknown"),
            ("sz", "159338", None, "unknown"),
            ("sh", "510070", Some(20000i64), "exchange_list"),
            ("sh", "589001", None, "unknown"),
        ] {
            c.execute(
                "INSERT INTO security_master(exchange,code,effective_from,effective_to,\
                 instrument_class,issuer_resolution,listing_date,listing_source,\
                 delisting_date,delisting_source,delisting_reason_class,listing_conflict,\
                 delisting_conflict,source_binding_digest,evidence_digest) \
                 VALUES(?1,?2,0,?3,2,'code_date',0,'exchange_list',?3,?4,'other',0,0,?5,?6)",
                rusqlite::params![
                    exchange,
                    code,
                    to,
                    delisting_source,
                    binding.as_slice(),
                    evidence.as_slice()
                ],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO derived_dataset_versions(dataset_kind,dataset_key,version_id,\
             content_digest,digest_format,record_count,first_day,last_day,algorithm_version,\
             source_binding_digest,evidence_digest,task_id,item_id,created_at,updated_at) \
             VALUES('security_master','a_share',?1,?2,'security-master-v1',7,0,20000,'test',\
             ?3,?4,'t','i','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            rusqlite::params![
                version,
                [7u8; 32].as_slice(),
                binding.as_slice(),
                evidence.as_slice()
            ],
        )
        .unwrap();
        Ok::<(), crate::market_storage::Error>(())
    })
    .unwrap();
    version
}

fn seed_series(backend: &MarketBackend, entity: &str) {
    let MarketBackend::Compact(repo) = backend else {
        panic!("fixture must open the compact backend");
    };
    repo.tx(|c| {
        let binding = series_commit::put_generation_binding(
            c,
            "gen-1",
            &"0b".repeat(32),
            &"0c".repeat(32),
            &"0e".repeat(32),
            &json!({"snapshot_inputs": []}),
        )?;
        let evidence =
            evidence_store::put_evidence(c, EvidenceKind::Continuity, &json!({"mode": "test"}))?;
        c.execute(
            "INSERT INTO market_series(series_key,entity,period,adjustment,instrument_class,\
             price_scale,version_id,content_digest,digest_format,record_count,first_day,last_day,\
             coverage_status,source_binding_digest,evidence_digest,last_event_seq,created_at,\
             updated_at) VALUES(1,?1,1,0,0,2,?2,?3,'series-fold-v2',1,0,1,1,?4,?5,1,\
             '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            rusqlite::params![
                entity,
                "11".repeat(32),
                [0x11u8; 32].as_slice(),
                binding.as_slice(),
                evidence.as_slice()
            ],
        )
        .unwrap();
        Ok::<(), crate::market_storage::Error>(())
    })
    .unwrap();
}

fn control(f: &Fixture) -> TaskControl<SqliteTasks> {
    let tasks = f.backend.tasks();
    let supply = SqliteSupply {
        market: f.backend.clone(),
    };
    TaskControl::new(
        tasks,
        "run-109".into(),
        vec![
            Box::new(CollectionContract {
                dataset: Dataset::Daily,
                repository: supply.clone(),
            }) as Box<dyn Contract>,
            Box::new(Bound {
                inner: Box::new(ListedEverCollectionContract {
                    repository: supply,
                    generation: Some(f.generation.clone()),
                }),
                source: f.generation.clone(),
            }) as Box<dyn Contract>,
        ],
    )
    .unwrap()
}

fn payload(version: &str, selection: Value) -> Value {
    json!({"dataset": "market.daily", "contract_version": version, "selection": selection})
}

fn task_id(accepted: &crate::task_control::Acceptance) -> String {
    let RequestOutcome::Task(task) = &accepted.record.outcome else {
        panic!("expected task outcome");
    };
    task.task.task_id.clone()
}

fn assert_scope_rejected(error: impl std::fmt::Display, needle: &str) {
    let text = error.to_string();
    assert!(text.contains(needle), "{text}");
}

#[test]
fn scope_value_decodes_only_as_the_listed_ever_variant() {
    let w: WriteSelection =
        serde_json::from_value(json!({"kind":"scope","scope":LISTED_EVER})).unwrap();
    assert!(matches!(
        w,
        WriteSelection::Scope {
            scope: CatalogScope::ShSzAListedEver
        }
    ));
    assert_eq!(
        serde_json::to_value(CatalogScope::CurrentShSzA).unwrap(),
        json!("current_sh_sz_a")
    );
}

#[test]
fn v1_rejects_the_scope_selection() {
    let (backend, _dir) = market();
    let supply = SqliteSupply { market: backend };
    let v1 = CollectionContract {
        dataset: Dataset::Daily,
        repository: supply.clone(),
    };
    let error = v1
        .normalize(payload("v1", json!({"kind":"scope","scope":LISTED_EVER})))
        .unwrap_err();
    assert_scope_rejected(error, "requires contract_version v2");
}

#[test]
fn catalog_write_rejects_the_scope_instead_of_degrading() {
    let (backend, _dir) = market();
    let catalog = CollectionContract {
        dataset: Dataset::Catalog,
        repository: SqliteSupply { market: backend },
    };
    let error = catalog
        .normalize(json!({
            "dataset":"securities.catalog",
            "contract_version":"v1",
            "selection":{"scope":LISTED_EVER}
        }))
        .unwrap_err();
    assert_scope_rejected(error, "current_sh_sz_a");
}

#[test]
fn catalog_read_rejects_the_scope() {
    let error = crate::data_supply::query::entities(
        Dataset::Catalog,
        &Selection::Catalog {
            scope: CatalogScope::ShSzAListedEver,
        },
        &Access::local(),
    )
    .unwrap_err();
    assert_scope_rejected(error, "current_sh_sz_a");
}

#[test]
fn etf_scopes_are_wire_values_but_catalog_rejects_them() {
    // The new scopes are part of the wire enum (v10) and resolve from the main
    // table for `market.daily`; the catalog read/write entries must reject them
    // instead of degrading to the current A-share subset.
    assert_eq!(
        serde_json::to_value(CatalogScope::ShSzEtfCurrent).unwrap(),
        json!("sh_sz_etf_current")
    );
    assert_eq!(
        serde_json::to_value(CatalogScope::ShSzEtfListedEver).unwrap(),
        json!("sh_sz_etf_listed_ever")
    );
    let error = crate::data_supply::query::entities(
        Dataset::Catalog,
        &Selection::Catalog {
            scope: CatalogScope::ShSzEtfCurrent,
        },
        &Access::local(),
    )
    .unwrap_err();
    assert_scope_rejected(error, "current_sh_sz_a");
}

#[test]
fn v2_scope_collection_accepts_the_etf_scopes() {
    let (backend, _dir) = market();
    let contract = ListedEverCollectionContract {
        repository: SqliteSupply { market: backend },
        generation: None,
    };
    for scope in ["sh_sz_etf_current", "sh_sz_etf_listed_ever"] {
        let value = contract
            .normalize(json!({
                "dataset": "market.daily",
                "request_id": "etf-scope",
                "contract_version": "v2",
                "selection": {"kind": "scope", "scope": scope}
            }))
            .unwrap();
        assert_eq!(value["selection"]["scope"], json!(scope));
    }
}

#[test]
fn resolution_budget_is_1_to_500() {
    assert!(ListedEverCollectionContract::budget(1).is_ok());
    assert!(ListedEverCollectionContract::budget(500).is_ok());
    let zero = ListedEverCollectionContract::budget(0).unwrap_err();
    assert_scope_rejected(zero, "empty");
    let over = ListedEverCollectionContract::budget(501).unwrap_err();
    assert_scope_rejected(over, "1..500");
}

fn fact(
    exchange: crate::security_master::Exchange,
    code: &str,
    class: u8,
) -> crate::security_master::SecurityLifecycleFact {
    use crate::security_master::{
        DelistingReason, DelistingSource, IssuerResolution, ListingSource,
    };
    crate::security_master::SecurityLifecycleFact {
        exchange,
        code: code.into(),
        effective_from: chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
        effective_to: None,
        instrument_class: class,
        issuer_key: None,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_date: None,
        listing_date_tdx: None,
        listing_source: ListingSource::Unknown,
        b_listing_date: None,
        delisting_date: None,
        delisting_date_tdx: None,
        delisting_source: DelistingSource::Unknown,
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: false,
        delisting_conflict: false,
        known_at: None,
    }
}

#[test]
fn candidates_are_ordinary_a_shares_without_a_series() {
    use crate::security_master::Exchange;
    let rows = [
        fact(Exchange::Sh, "600001", 0),
        fact(Exchange::Sh, "600002", 0),
        fact(Exchange::Sh, "600003", 1),
        fact(Exchange::Sz, "000004", 0),
        fact(Exchange::Bj, "830001", 0),
    ];
    let has_series = BTreeSet::from(["sh.600002".to_string()]);
    let got =
        ListedEverCollectionContract::candidates(&rows, &has_series, CatalogScope::ShSzAListedEver);
    assert_eq!(
        got,
        BTreeSet::from(["sh.600001".to_string(), "sz.000004".to_string()])
    );
}

#[test]
fn etf_scopes_admit_class_two_only() {
    use crate::security_master::Exchange;
    let current = fact(Exchange::Sh, "510050", 2);
    let delisted = {
        let mut row = fact(Exchange::Sh, "510070", 2);
        row.effective_to = Some(chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap());
        row.delisting_source = crate::security_master::DelistingSource::ExchangeList;
        row
    };
    let rows = [
        fact(Exchange::Sh, "600001", 0),
        fact(Exchange::Sh, "000001", 1),
        current.clone(),
        delisted.clone(),
    ];
    let has_series = BTreeSet::new();
    assert_eq!(
        ListedEverCollectionContract::candidates(&rows, &has_series, CatalogScope::ShSzEtfCurrent),
        BTreeSet::from(["sh.510050".to_string()])
    );
    assert_eq!(
        ListedEverCollectionContract::candidates(
            &rows,
            &has_series,
            CatalogScope::ShSzEtfListedEver
        ),
        BTreeSet::from(["sh.510050".to_string(), "sh.510070".to_string()])
    );
    let existing = BTreeSet::from(["sh.510050".to_string()]);
    assert_eq!(
        ListedEverCollectionContract::candidates(&rows, &existing, CatalogScope::ShSzEtfListedEver),
        BTreeSet::from(["sh.510070".to_string()])
    );
}

#[test]
fn etf_listed_ever_collects_raw_daily_and_reports_absent() {
    let f = fixture();
    let control = control(&f);
    let accepted = control
        .accept(
            "req-etf-listed-ever",
            CommandKey {
                operation: "start_collection".into(),
                dataset: "market.daily".into(),
                contract_version: "v2".into(),
            },
            payload(
                "v2",
                json!({"kind":"scope","scope":"sh_sz_etf_listed_ever"}),
            ),
        )
        .unwrap();
    let id = task_id(&accepted);
    let snapshot = control.read_task(&id).unwrap();
    let symbols: Vec<String> = snapshot
        .items
        .iter()
        .map(|item| item.spec.target["symbol"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        symbols,
        vec![
            "sh.510050",
            "sh.510070",
            "sz.159338",
            "sz.159901",
            "sz.159919"
        ]
    );
    for item in &snapshot.items {
        assert_eq!(
            item.spec.frozen_inputs["scope"],
            json!("sh_sz_etf_listed_ever")
        );
        assert_eq!(item.spec.frozen_inputs["absent_count"], json!(1));
        assert_eq!(
            item.spec.frozen_inputs["absent"],
            json!([{"entity":"sh.589001","reason":"source_member_absent"}])
        );
    }

    let done = crate::data_supply::worker::run_once(
        &control,
        &f.backend,
        None,
        Some(&f.generation),
        crate::source_store::Policy::Optional,
    )
    .unwrap()
    .unwrap();
    assert_eq!(done.task.status, Status::Succeeded, "{done:?}");

    let facts = f
        .backend
        .read(|c| {
            use crate::market_data::{Adjustment, Period};
            let etf = crate::sqlite_market::series::read_series_adjusted(
                c,
                "sh.510050",
                Period::Day,
                Adjustment::Raw,
            )?
            .unwrap();
            let etf_bars = crate::sqlite_market::series::all_bars(c, etf.series_key)?;
            let hand = crate::sqlite_market::series::read_series_adjusted(
                c,
                "sz.159338",
                Period::Day,
                Adjustment::Raw,
            )?
            .unwrap();
            let hand_bars = crate::sqlite_market::series::all_bars(c, hand.series_key)?;
            let delisted = crate::sqlite_market::series::read_series_adjusted(
                c,
                "sh.510070",
                Period::Day,
                Adjustment::Raw,
            )?
            .unwrap();
            let delisted_bars =
                crate::sqlite_market::series::all_bars(c, delisted.series_key)?;
            let quantum: Option<i64> = c
                .query_row(
                    "SELECT volume_quantum FROM series_bars WHERE series_key=?1 ORDER BY trade_day DESC LIMIT 1",
                    rusqlite::params![hand.series_key],
                    |row| row.get(0),
                )
                .unwrap();
            Ok((
                (
                    etf.instrument_class,
                    etf.price_scale,
                    etf_bars.last().unwrap().close_units,
                ),
                (
                    hand_bars.last().unwrap().volume,
                    quantum,
                ),
                delisted_bars.len(),
                crate::sqlite_market::derived::read_adjustment_events(c, "sz.159919")?,
                crate::sqlite_market::derived::read_adjustment_events(c, "sz.159901")?,
            ))
        })
        .unwrap();
    assert_eq!(facts.0, (2, 3, 41_950));
    assert_eq!(facts.1, (Some(4_293_795_000), Some(100)));
    assert_eq!(facts.2, 3, "delisted tail zero bars are preserved");
    assert_eq!(facts.3.len(), 1);
    assert_eq!(facts.3[0].category, 1);
    assert_eq!(
        facts.3[0].factors[0],
        "0.661".parse::<rust_decimal::Decimal>().unwrap()
    );
    assert_eq!(
        facts.3[0].basis_status,
        crate::market_data::FactorBasisStatus::Verified
    );
    assert_eq!(facts.4.len(), 2);
    assert!(facts.4.iter().all(|event| event.category == 11));
    assert_eq!(
        facts
            .4
            .iter()
            .map(|event| event.factors[2].to_string())
            .collect::<Vec<_>>(),
        vec!["2".to_string(), "5".to_string()]
    );
    assert!(
        facts
            .4
            .iter()
            .all(|event| { event.basis_status == crate::market_data::FactorBasisStatus::Pending })
    );
}

#[test]
fn listed_ever_scope_resolves_master_intersection_and_skips_existing_heads() {
    let f = fixture();
    let control = control(&f);
    let selection = json!({"kind":"scope","scope":LISTED_EVER});
    let accepted = control
        .accept(
            "req-listed-ever",
            CommandKey {
                operation: "start_collection".into(),
                dataset: "market.daily".into(),
                contract_version: "v2".into(),
            },
            payload("v2", selection.clone()),
        )
        .unwrap();
    let id = task_id(&accepted);
    let snapshot = control.read_task(&id).unwrap();
    let symbols: Vec<String> = snapshot
        .items
        .iter()
        .map(|item| item.spec.target["symbol"].as_str().unwrap().to_string())
        .collect();
    // sh.600101 already has a series; sh.600901/sz.000101 have no bar.
    assert_eq!(
        symbols,
        vec!["sh.600102", "sh.600103", "sh.600104", "sh.600105"]
    );
    for item in &snapshot.items {
        assert_eq!(item.spec.frozen_inputs["scope"], json!(LISTED_EVER));
        assert_eq!(
            item.spec.frozen_inputs["security_master_version_id"],
            json!(f.master_version)
        );
        assert_eq!(
            item.spec.frozen_inputs["source_generation_id"],
            json!(f.generation_id)
        );
        assert_eq!(
            item.spec.frozen_inputs[SOURCE_GENERATION_KEY],
            json!(f.generation_id)
        );
        assert_eq!(
            item.spec.frozen_inputs["skipped_no_bars"],
            json!(["sh.600901", "sz.000101"])
        );
    }

    // The v2 task executes through the same runner as v1.
    let done = crate::data_supply::worker::run_once(
        &control,
        &f.backend,
        None,
        Some(&f.generation),
        crate::source_store::Policy::Optional,
    )
    .unwrap()
    .unwrap();
    assert_eq!(done.task.status, Status::Succeeded, "{done:?}");

    // Every resolved entity now has a head, so the same scope is a no-op.
    let error = control
        .accept(
            "req-listed-ever-again",
            CommandKey {
                operation: "start_collection".into(),
                dataset: "market.daily".into(),
                contract_version: "v2".into(),
            },
            payload("v2", selection),
        )
        .unwrap_err();
    assert_scope_rejected(error, "empty");
}

fn set_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn market() -> (MarketBackend, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    SqliteTasks::new(&path).initialize_compact().unwrap();
    set_private(&path);
    let backend = MarketBackend::open(&SqliteTasks::new(&path)).unwrap();
    (backend, dir)
}
