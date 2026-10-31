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
fn gbbq_record(market: u8, code: &str, date_raw: u32, category: u8) -> [u8; 29] {
    let mut clear = [0u8; 29];
    clear[0] = market;
    clear[1..1 + code.len()].copy_from_slice(code.as_bytes());
    clear[8..12].copy_from_slice(&date_raw.to_le_bytes());
    clear[12] = category;
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
    let got = ListedEverCollectionContract::candidates(&rows, &has_series);
    assert_eq!(
        got,
        BTreeSet::from(["sh.600001".to_string(), "sz.000004".to_string()])
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
