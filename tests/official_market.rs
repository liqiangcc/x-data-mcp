//! `TdxMarketSource` / `TdxDailyRunner` — official-file snapshot source for
//! market.daily collection. Synthetic fixtures cover fail-closed behavior;
//! the retained gen-2 snapshot (when present) covers the end-to-end oracle.
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{io::Write, path::Path, str::FromStr};
use x_data_mcp::{
    data_supply::{collect::CollectionContract, wire::Dataset},
    gap_verification::{GapTarget, SourceGapVerifier, SubjectBinding, decide_gap},
    gbbq,
    market_data::{Adjustment, CoverageStatus, MarketDataBatch, Period, SourceError},
    market_storage::{MarketRepository, PublishIntent, Range, Scope, compat},
    market_update::runner::ItemExecution,
    official_market::{
        DailyCollectionRepository, TdxBackfillProvider, TdxDailyRunner, TdxGapVerifier,
        TdxMarketSource, TdxRepublishProvider,
    },
    publish::{EffectiveVersion, ValidationSummary},
    sqlite_market::SqliteMarket,
    sqlite_supply::SqliteSupply,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Contract, RequestOutcome, Stage, Status, TaskRepository, legacy::LegacyCollection,
        service::TaskControl,
    },
};
#[path = "support/market.rs"]
mod market;

fn now() -> DateTime<Utc> {
    "2026-09-12T00:00:00Z".parse().unwrap()
}
fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}
fn sha(bytes: &[u8]) -> String {
    x_data_mcp::day_file::sha256(bytes)
}

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

fn gbbq_record(market: u8, code: &str, date_raw: u32, category: u8, c: [f32; 4]) -> [u8; 29] {
    let mut clear = [0u8; 29];
    clear[0] = market;
    clear[1..1 + code.len()].copy_from_slice(code.as_bytes());
    clear[8..12].copy_from_slice(&date_raw.to_le_bytes());
    clear[12] = category;
    for (i, v) in c.iter().enumerate() {
        clear[13 + i * 4..17 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    gbbq::encrypt_record(&clear)
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

struct Snapshot {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    manifest: Vec<u8>,
}

/// Minimal inspected snapshot: three role-bound archives with caller-chosen
/// member payloads. Member lists may be empty (absent-member fixtures) or
/// carry malformed bytes (corruption fixtures).
fn snapshot(
    day_members: &[(&str, &[u8])],
    gbbq_members: &[(&str, &[u8])],
    base_members: &[(&str, &[u8])],
) -> Snapshot {
    let dir = tempfile::tempdir().unwrap();
    let roles = [
        (
            "base_industry",
            "base.zip",
            "http://www.tdx.com.cn/products/data/data/dbf/base.zip",
            base_members,
        ),
        (
            "day",
            "hsjday.zip",
            "https://data.tdx.com.cn/vipdoc/hsjday.zip",
            day_members,
        ),
        (
            "gbbq",
            "gbbq.zip",
            "http://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
            gbbq_members,
        ),
    ];
    let mut files = Vec::new();
    for (role, name, url, members) in roles {
        let path = dir.path().join(name);
        write_zip(&path, members);
        let bytes = std::fs::read(&path).unwrap();
        files.push(json!({
            "role": role,
            "relative_path": name,
            "source_url": url,
            "acquisition_started_at": "2026-09-11T01:00:00Z",
            "acquisition_completed_at": "2026-09-11T02:00:00Z",
            "last_modified": null,
            "etag": null,
            "bytes": bytes.len(),
            "sha256": sha(&bytes),
        }));
    }
    let manifest =
        serde_json::to_vec(&json!({"schema":"x-data-mcp.official-file-input.v1","files":files}))
            .unwrap();
    Snapshot {
        root: dir.path().to_path_buf(),
        manifest,
        _dir: dir,
    }
}

fn make_source(snap: &Snapshot) -> TdxMarketSource {
    TdxMarketSource::inspect(&snap.root, &snap.manifest, now()).unwrap()
}

fn base_members() -> Vec<(&'static str, &'static [u8])> {
    vec![("base.dbf", b"unused-by-daily-source".as_slice())]
}

/// Valid fixture: three bars for sh.600006 and one category-1 GBBQ event
/// (2025-07-16 allotment C4=5 → divisor (10+0+5)/10 = 1.5).
fn valid_snapshot() -> Snapshot {
    let bars: Vec<u8> = [
        day_record(20250715, [1128, 1130, 1125, 1129], 0.0, 100),
        day_record(20250716, [1129, 1132, 1128, 1131], 0.0, 110),
        day_record(20250717, [1131, 1135, 1130, 1134], 0.0, 120),
    ]
    .concat();
    let events = gbbq_member(&[gbbq_record(1, "600006", 20250716, 1, [0.0, 0.0, 0.0, 5.0])]);
    snapshot(
        &[("sh\\lday\\sh600006.day", bars.as_slice())],
        &[("gbbq", events.as_slice())],
        &base_members(),
    )
}

/// Rebuild a snapshot's gbbq archive with new member bytes and re-pin the
/// manifest to the new archive digest (still inspect()-valid).
fn regbbq(snap: &mut Snapshot, members: &[(&str, &[u8])]) {
    write_zip(&snap.root.join("gbbq.zip"), members);
    let bytes = std::fs::read(snap.root.join("gbbq.zip")).unwrap();
    let mut manifest: Value = serde_json::from_slice(&snap.manifest).unwrap();
    for f in manifest["files"].as_array_mut().unwrap() {
        if f["role"].as_str() == Some("gbbq") {
            f["bytes"] = json!(bytes.len());
            f["sha256"] = json!(sha(&bytes));
        }
    }
    snap.manifest = serde_json::to_vec(&manifest).unwrap();
}

fn reday(snap: &mut Snapshot, members: &[(&str, &[u8])]) {
    write_zip(&snap.root.join("hsjday.zip"), members);
    let bytes = std::fs::read(snap.root.join("hsjday.zip")).unwrap();
    let mut manifest: Value = serde_json::from_slice(&snap.manifest).unwrap();
    for f in manifest["files"].as_array_mut().unwrap() {
        if f["role"].as_str() == Some("day") {
            f["bytes"] = json!(bytes.len());
            f["sha256"] = json!(sha(&bytes));
        }
    }
    snap.manifest = serde_json::to_vec(&manifest).unwrap();
}

#[test]
fn uninspected_or_tampered_snapshot_is_rejected() {
    let snap = valid_snapshot();
    // Manifest bytes tampered after pinning.
    let mut bad = snap.manifest.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert!(matches!(
        TdxMarketSource::inspect(&snap.root, &bad, now()),
        Err(SourceError::NotVerified(_))
    ));
    // Archive bytes differing from the pinned sha256.
    std::fs::write(snap.root.join("gbbq.zip"), b"tampered").unwrap();
    assert!(matches!(
        TdxMarketSource::inspect(&snap.root, &snap.manifest, now()),
        Err(SourceError::NotVerified(_))
    ));
}

#[test]
fn materializes_qfq_daily_with_snapshot_bound_provenance() {
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let batch = source.materialize_daily("sh.600006").unwrap();
    assert_eq!(batch.symbol, "sh.600006");
    assert_eq!(batch.period, Period::Day);
    assert_eq!(batch.adjustment, Adjustment::Forward);
    assert_eq!(batch.bars.len(), 3);
    // Event 20250716 (divisor 1.5) adjusts only strictly earlier bars.
    assert_eq!(batch.bars[0].close, dec("7.53")); // 11.29/1.5 = 7.5266..→7.53
    assert_eq!(batch.bars[1].close, dec("11.31"));
    assert_eq!(batch.bars[2].close, dec("11.34"));
    // #239-anchored units: raw volume (shares) / amount (yuan) promoted
    // verbatim; only prices are QFQ-adjusted.
    assert_eq!(batch.bars[0].volume, Some(dec("100")));
    assert_eq!(batch.bars[1].volume, Some(dec("110")));
    assert_eq!(batch.bars[2].volume, Some(dec("120")));
    assert!(batch.bars.iter().all(|b| b.amount == Some(dec("0"))));
    // #244: eighth .day word preserved verbatim as internal evidence.
    assert!(batch.bars.iter().all(|b| b.r7_raw == Some(0)));
    assert_eq!(
        batch.unit_currency.volume_unit.value.as_deref(),
        Some("share")
    );
    assert!(batch.unit_currency.volume_unit.is_confirmed());
    assert!(batch.unit_currency.amount_currency.is_confirmed());
    assert!(batch.unit_currency.amount_unit.is_confirmed());
    assert_eq!(batch.coverage.status, CoverageStatus::Complete);
    assert!(batch.adjustment_evidence.is_confirmed());
    assert_eq!(batch.adjustment_evidence.value.as_deref(), Some("qfq"));
    let inputs = batch.provenance.snapshot_inputs.as_ref().unwrap();
    assert_eq!(inputs.len(), 2);
    let manifest: Value = serde_json::from_slice(&snap.manifest).unwrap();
    for input in inputs {
        let role = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["role"].as_str() == Some(&input.role))
            .unwrap();
        assert_eq!(input.archive_sha256, role["sha256"].as_str().unwrap());
        assert_eq!(input.member_sha256.len(), 64);
    }
    assert!(
        batch.coverage.reason.contains("NOT_PRODUCTION_READY"),
        "published evidence must keep the not-production-ready marking"
    );
}

#[test]
fn unsupported_symbol_rejected_and_non_category1_never_applied() {
    let snap = valid_snapshot();
    let source = make_source(&snap);
    assert!(matches!(
        source.materialize_daily("bj.600006"),
        Err(SourceError::Unsupported(_))
    ));
    assert!(matches!(
        source.materialize_daily("sh600006"),
        Err(SourceError::Unsupported(_))
    ));
    // A non-category-1 record for the same code is excluded, never applied:
    // C4=99 would collapse prices ~10x if it were (incorrectly) interpreted.
    let mut snap2 = valid_snapshot();
    let events = gbbq_member(&[
        gbbq_record(1, "600006", 20250716, 1, [0.0, 0.0, 0.0, 5.0]),
        gbbq_record(1, "600006", 20250715, 2, [0.0, 0.0, 0.0, 99.0]),
    ]);
    regbbq(&mut snap2, &[("gbbq", events.as_slice())]);
    let batch = make_source(&snap2).materialize_daily("sh.600006").unwrap();
    assert_eq!(batch.bars[0].close, dec("7.53"));
    assert!(
        batch
            .adjustment_evidence
            .reason
            .contains("1 non-category-1 records excluded")
    );
    // Other markets' records never leak into this security's chain.
    let mut snap3 = valid_snapshot();
    let events = gbbq_member(&[
        gbbq_record(1, "600006", 20250716, 1, [0.0, 0.0, 0.0, 5.0]),
        gbbq_record(0, "600006", 20250715, 1, [0.0, 0.0, 0.0, 99.0]),
    ]);
    regbbq(&mut snap3, &[("gbbq", events.as_slice())]);
    let batch = make_source(&snap3).materialize_daily("sh.600006").unwrap();
    assert_eq!(batch.bars[0].close, dec("7.53"));
}

#[test]
fn absent_or_corrupt_members_fail_closed() {
    // Missing .day member for the requested security.
    let events = gbbq_member(&[]);
    let snap = snapshot(
        &[("sh\\lday\\sh600006.day", b"irrelevant".as_slice())],
        &[("gbbq", events.as_slice())],
        &base_members(),
    );
    assert!(matches!(
        make_source(&snap).materialize_daily("sz.000001"),
        Err(SourceError::Invocation(_))
    ));
    // Missing gbbq member.
    let bars = day_record(20250715, [1, 2, 1, 2], 0.0, 1).to_vec();
    let snap = snapshot(
        &[("sh\\lday\\sh600006.day", bars.as_slice())],
        &[("other", b"x".as_slice())],
        &base_members(),
    );
    assert!(matches!(
        make_source(&snap).materialize_daily("sh.600006"),
        Err(SourceError::Invocation(_))
    ));
    // Truncated .day member (not a multiple of 32).
    let snap = snapshot(
        &[("sh\\lday\\sh600006.day", b"short".as_slice())],
        &[("gbbq", events.as_slice())],
        &base_members(),
    );
    assert!(matches!(
        make_source(&snap).materialize_daily("sh.600006"),
        Err(SourceError::InvalidResponse(_))
    ));
    // Malformed gbbq member (length/count mismatch).
    let snap = snapshot(
        &[("sh\\lday\\sh600006.day", bars.as_slice())],
        &[("gbbq", b"\x02\x00\x00\x00truncated".as_slice())],
        &base_members(),
    );
    assert!(matches!(
        make_source(&snap).materialize_daily("sh.600006"),
        Err(SourceError::InvalidResponse(_))
    ));
    // .day member with an invalid calendar date.
    let mut snap = snapshot(
        &[("sh\\lday\\sh600006.day", bars.as_slice())],
        &[("gbbq", events.as_slice())],
        &base_members(),
    );
    reday(&mut snap, &[("sh\\lday\\sh600006.day", &[0u8; 32])]);
    assert!(matches!(
        make_source(&snap).materialize_daily("sh.600006"),
        Err(SourceError::InvalidResponse(_))
    ));
    // Archive that is not a zip at all.
    let mut snap = snapshot(
        &[("sh\\lday\\sh600006.day", bars.as_slice())],
        &[("gbbq", events.as_slice())],
        &base_members(),
    );
    std::fs::write(snap.root.join("hsjday.zip"), b"not a zip").unwrap();
    let bytes = std::fs::read(snap.root.join("hsjday.zip")).unwrap();
    let mut manifest: Value = serde_json::from_slice(&snap.manifest).unwrap();
    for f in manifest["files"].as_array_mut().unwrap() {
        if f["role"].as_str() == Some("day") {
            f["bytes"] = json!(bytes.len());
            f["sha256"] = json!(sha(&bytes));
        }
    }
    snap.manifest = serde_json::to_vec(&manifest).unwrap();
    assert!(matches!(
        make_source(&snap).materialize_daily("sh.600006"),
        Err(SourceError::Invocation(_))
    ));
}

// ---------------------------------------------------------------------------
// Runner/publication tests over the sqlite governed path.
// ---------------------------------------------------------------------------

struct Rig {
    task_id: String,
    item_id: String,
}

fn accept_daily(market: &SqliteMarket, run: &str, exchange: &str, code: &str) -> Rig {
    let supply = SqliteSupply {
        market: market.clone(),
    };
    let control = TaskControl::new(
        market.tasks.clone(),
        run.into(),
        vec![Box::new(CollectionContract {
            dataset: Dataset::Daily,
            repository: supply.clone(),
        })],
    )
    .unwrap();
    let key = CollectionContract {
        dataset: Dataset::Daily,
        repository: supply,
    }
    .key();
    let request = control
        .accept(
            "req-1",
            key,
            json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":exchange,"code":code}]}}),
        )
        .unwrap();
    let RequestOutcome::Task(task) = request.record.outcome else {
        panic!("collection must accept");
    };
    Rig {
        task_id: task.task.task_id.clone(),
        item_id: task.items[0].spec.item_id.clone(),
    }
}

fn opened(dir: &tempfile::TempDir) -> SqliteMarket {
    let tasks = SqliteTasks::new(dir.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    SqliteMarket::new(tasks)
}

#[test]
fn runner_publishes_daily_through_governed_path() {
    let dir = tempfile::tempdir().unwrap();
    let market = opened(&dir);
    let r = accept_daily(&market, "run-a", "sh", "600006");
    let snap = valid_snapshot();
    let src = make_source(&snap);
    let runner = TdxDailyRunner {
        source: &src,
        session: None,
        repository: market.clone(),
        run_id: "run-a".into(),
        published_at: now(),
    };
    let out = runner.run_item(&r.task_id, &r.item_id).unwrap();
    let ItemExecution::Published(receipt) = out else {
        panic!("expected publication")
    };
    let task = market.task(&r.task_id).unwrap();
    assert!(task.task.status.terminal());
    assert!(task.items[0].outcome.as_ref().unwrap().success);
    let scope = Scope {
        symbol: "sh.600006".into(),
        period: Period::Day,
    };
    assert_eq!(
        market.effective_head(&scope).unwrap().as_deref(),
        Some(receipt.version_id.as_str())
    );
    let page = market
        .range(
            &scope,
            None,
            &Range {
                start: chrono::NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
                end: chrono::NaiveDate::from_ymd_opt(2100, 1, 1).unwrap(),
                after: None,
                limit: 1000,
            },
        )
        .unwrap();
    assert_eq!(page.rows.len(), 3);
    assert_eq!(page.rows[0].close, dec("7.53"));
    // Snapshot input digests are bound into the stored record provenance.
    let stored = market.fixed(&receipt.version_id).unwrap();
    let inputs = stored.raw()["batch"]["provenance"]["snapshot_inputs"]
        .as_array()
        .unwrap();
    assert_eq!(inputs.len(), 2);
    // Replay is idempotent: the committed receipt is returned, never re-run.
    let again = runner.run_item(&r.task_id, &r.item_id).unwrap();
    assert!(matches!(
        again,
        ItemExecution::Published(r2) if r2.publication_id == receipt.publication_id
    ));
}

#[test]
fn worker_dispatch_routes_daily_collection_to_installed_source() {
    let dir = tempfile::tempdir().unwrap();
    let market = opened(&dir);
    let supply = SqliteSupply {
        market: market.clone(),
    };
    let control = TaskControl::new(
        market.tasks.clone(),
        "run-d".into(),
        vec![Box::new(CollectionContract {
            dataset: Dataset::Daily,
            repository: supply.clone(),
        })],
    )
    .unwrap();
    control
        .accept(
            "req-w",
            CollectionContract {
                dataset: Dataset::Daily,
                repository: supply,
            }
            .key(),
            json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600006"}]}}),
        )
        .unwrap();
    let snap = valid_snapshot();
    let src = make_source(&snap);
    let done = x_data_mcp::data_supply::worker::run_once(&control, &market, Some(&src))
        .unwrap()
        .unwrap();
    assert_eq!(done.task.status, Status::Succeeded);
    assert!(
        market
            .effective_head(&Scope {
                symbol: "sh.600006".into(),
                period: Period::Day,
            })
            .unwrap()
            .is_some()
    );
    // A second collection task for the now-published scope fails closed.
    let r2 = accept_daily(&market, "run-d", "sh", "600006");
    let runner = TdxDailyRunner {
        source: &src,
        session: None,
        repository: market.clone(),
        run_id: "run-d".into(),
        published_at: now(),
    };
    let out = runner.run_item(&r2.task_id, &r2.item_id).unwrap();
    assert!(matches!(out, ItemExecution::NotPublished(_)));
    let task = market.task(&r2.task_id).unwrap();
    let error = task.items[0]
        .outcome
        .as_ref()
        .unwrap()
        .error
        .clone()
        .unwrap();
    assert!(error.contains("update_data"), "unexpected: {error}");
}

#[test]
fn source_failure_finishes_item_failed_and_preserves_prior_state() {
    let dir = tempfile::tempdir().unwrap();
    let market = opened(&dir);
    let r = accept_daily(&market, "run-b", "sh", "600006");
    // Snapshot without the requested .day member.
    let events = gbbq_member(&[]);
    let snap = snapshot(&[], &[("gbbq", events.as_slice())], &base_members());
    let src = make_source(&snap);
    let runner = TdxDailyRunner {
        source: &src,
        session: None,
        repository: market.clone(),
        run_id: "run-b".into(),
        published_at: now(),
    };
    let out = runner.run_item(&r.task_id, &r.item_id).unwrap();
    assert!(matches!(out, ItemExecution::NotPublished(_)));
    let task = market.task(&r.task_id).unwrap();
    assert!(task.task.status.terminal());
    assert_eq!(task.items[0].stage, Stage::Finished);
    let outcome = task.items[0].outcome.as_ref().unwrap();
    assert!(!outcome.success);
    assert!(outcome.error.as_deref().unwrap().contains("member absent"));
    let scope = Scope {
        symbol: "sh.600006".into(),
        period: Period::Day,
    };
    assert!(market.effective_head(&scope).unwrap().is_none());
    assert!(market.versions(&scope).unwrap().is_empty());
}

#[test]
fn non_daily_collection_items_remain_blocked() {
    // Catalog dispatch consults X_DATA_MCP_OFFICIAL_* env; serialize against
    // the env-mutating install tests below.
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(dir.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let supply = SqliteSupply {
        market: market.clone(),
    };
    let control = TaskControl::new(
        tasks,
        "run-e".into(),
        vec![
            Box::new(CollectionContract {
                dataset: Dataset::Catalog,
                repository: supply.clone(),
            }),
            Box::new(LegacyCollection),
        ],
    )
    .unwrap();
    // Catalog task stays on BlockedSource even when a source is installed.
    let cat = control
        .accept(
            "req-cat",
            CollectionContract {
                dataset: Dataset::Catalog,
                repository: supply.clone(),
            }
            .key(),
            json!({"dataset":"securities.catalog","contract_version":"v1","selection":{"scope":"current_sh_sz_a"}}),
        )
        .unwrap();
    let RequestOutcome::Task(cat_task) = cat.record.outcome else {
        panic!()
    };
    let snap = valid_snapshot();
    let src = make_source(&snap);
    x_data_mcp::data_supply::worker::run_once(&control, &market, Some(&src)).unwrap();
    let task = market.task(&cat_task.task.task_id).unwrap();
    assert!(
        task.items[0]
            .outcome
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("SOURCE_NOT_VERIFIED")
    );
    // Legacy compound collection likewise keeps the refusal.
    let legacy = control
        .accept(
            "req-legacy",
            LegacyCollection.key(),
            json!({"symbols":["sh.600006"],"periods":["day"],"adjustment":"qfq"}),
        )
        .unwrap();
    let RequestOutcome::Task(l_task) = legacy.record.outcome else {
        panic!()
    };
    x_data_mcp::data_supply::worker::run_once(&control, &market, Some(&src)).unwrap();
    let task = market.task(&l_task.task.task_id).unwrap();
    assert!(
        task.items[0]
            .outcome
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("SOURCE_NOT_VERIFIED")
    );
}

// ---------------------------------------------------------------------------
// Retained gen-2 snapshot end-to-end (skipped when the fixed input directory
// is absent — CI does not carry it; Box holds the pinned bytes).
// ---------------------------------------------------------------------------

fn retained_snapshot() -> Option<Snapshot> {
    let retained = Path::new("/home/box/retained-inputs");
    let inputs = [
        (
            "official-base.zip",
            "base.zip",
            "base_industry",
            "http://www.tdx.com.cn/products/data/data/dbf/base.zip",
        ),
        (
            "official-hsjday.zip",
            "hsjday.zip",
            "day",
            "https://data.tdx.com.cn/vipdoc/hsjday.zip",
        ),
        (
            "official-gbbq.zip",
            "gbbq.zip",
            "gbbq",
            "http://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
        ),
    ];
    if inputs.iter().any(|(src, ..)| !retained.join(src).is_file()) {
        return None;
    }
    // Hard links stay on the same filesystem, so inspect()'s canonicalized
    // path check sees real files under the snapshot root.
    let dir = tempfile::tempdir_in("/home/box").unwrap();
    let mut files = Vec::new();
    for (src, name, role, url) in inputs {
        let path = dir.path().join(name);
        std::fs::hard_link(retained.join(src), &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        files.push(json!({
            "role": role,
            "relative_path": name,
            "source_url": url,
            "acquisition_started_at": "2026-09-11T01:00:00Z",
            "acquisition_completed_at": "2026-09-11T02:00:00Z",
            "last_modified": null,
            "etag": null,
            "bytes": bytes.len(),
            "sha256": sha(&bytes),
        }));
    }
    let manifest =
        serde_json::to_vec(&json!({"schema":"x-data-mcp.official-file-input.v1","files":files}))
            .unwrap();
    Some(Snapshot {
        root: dir.path().to_path_buf(),
        manifest,
        _dir: dir,
    })
}

#[test]
fn retained_gen2_snapshot_reproduces_frozen_oracle_end_to_end() {
    let Some(snap) = retained_snapshot() else {
        eprintln!("retained-inputs absent; retained gen-2 e2e skipped");
        return;
    };
    let src = make_source(&snap);
    let regression: Value = serde_json::from_str(include_str!(
        "../evidence/issue-54-qfq-contract-regression/result.json"
    ))
    .unwrap();
    for oracle in regression["client_oracle_bars"].as_array().unwrap() {
        let symbol = oracle["security"].as_str().unwrap();
        let batch = src.materialize_daily(symbol).unwrap();
        let date = chrono::NaiveDate::parse_from_str(oracle["date"].as_str().unwrap(), "%Y-%m-%d")
            .unwrap();
        let bar = batch
            .bars
            .iter()
            .find(|b| b.date == date)
            .unwrap_or_else(|| panic!("{symbol} missing oracle date {date}"));
        for (field, value) in [
            ("O", bar.open),
            ("H", bar.high),
            ("L", bar.low),
            ("C", bar.close),
        ] {
            assert_eq!(
                value,
                dec(oracle["fields"][field]["observed"].as_str().unwrap()),
                "{symbol} {date} {field}"
            );
        }
    }
    // End-to-end through the governed publication path: collect sz.000002
    // and read the promoted head back through the store.
    let dir = tempfile::tempdir().unwrap();
    let market = opened(&dir);
    let r = accept_daily(&market, "run-retained", "sz", "000002");
    let runner = TdxDailyRunner {
        source: &src,
        session: None,
        repository: market.clone(),
        run_id: "run-retained".into(),
        published_at: now(),
    };
    let out = runner.run_item(&r.task_id, &r.item_id).unwrap();
    let ItemExecution::Published(receipt) = out else {
        let task = market.task(&r.task_id).unwrap();
        panic!(
            "expected publication; item outcome: {:?}",
            task.items[0].outcome
        )
    };
    let scope = Scope {
        symbol: "sz.000002".into(),
        period: Period::Day,
    };
    let page = market
        .range(
            &scope,
            None,
            &Range {
                start: chrono::NaiveDate::from_ymd_opt(1999, 12, 1).unwrap(),
                end: chrono::NaiveDate::from_ymd_opt(2000, 2, 1).unwrap(),
                after: None,
                limit: 1000,
            },
        )
        .unwrap();
    let bar = page
        .rows
        .iter()
        .find(|b| b.date == chrono::NaiveDate::from_ymd_opt(2000, 1, 10).unwrap())
        .unwrap();
    assert_eq!(bar.close, dec("-8.06"));
    assert_eq!(bar.open, dec("-8.14"));
    let record = market.fixed(&receipt.version_id).unwrap();
    let inputs = record.raw()["batch"]["provenance"]["snapshot_inputs"]
        .as_array()
        .unwrap();
    assert_eq!(inputs.len(), 2);
    assert!(inputs.iter().all(|i| {
        i["archive_sha256"].as_str().unwrap().len() == 64
            && i["member_sha256"].as_str().unwrap().len() == 64
    }));
}

// ---------------------------------------------------------------------------
// Service install hook: X_DATA_MCP_OFFICIAL_MANIFEST + X_DATA_MCP_OFFICIAL_DIR
// ---------------------------------------------------------------------------

/// Env mutation is process-global; serialize the install tests (and the
/// catalog-dispatch test above, which reads the same variables per tick).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct EnvReset;
impl EnvReset {
    fn install(manifest: &Path, root: &Path) -> Self {
        unsafe {
            std::env::set_var("X_DATA_MCP_OFFICIAL_MANIFEST", manifest);
            std::env::set_var("X_DATA_MCP_OFFICIAL_DIR", root);
        }
        Self
    }
    fn absent() -> Self {
        unsafe {
            std::env::remove_var("X_DATA_MCP_OFFICIAL_MANIFEST");
            std::env::remove_var("X_DATA_MCP_OFFICIAL_DIR");
        }
        Self
    }
}
impl Drop for EnvReset {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("X_DATA_MCP_OFFICIAL_MANIFEST");
            std::env::remove_var("X_DATA_MCP_OFFICIAL_DIR");
        }
    }
}

fn blocked_outcome(error: Option<String>) {
    assert!(
        error.as_deref().unwrap().contains("SOURCE_NOT_VERIFIED"),
        "expected source refusal, got {error:?}"
    );
}

#[test]
fn service_env_install_publishes_daily_and_derives_yearly() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(snap) = retained_snapshot() else {
        eprintln!("retained-inputs absent; env install e2e skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let manifest_path = dir.path().join("snapshot-manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    let _env = EnvReset::install(&manifest_path, &snap.root);

    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    // The hook lives in DataApi::new: no with_market_source call here.
    let api = DataApi::new(tasks, "run-env".into(), Access::local()).unwrap();
    let accepted = api
        .call(
            "start_collection",
            json!({"request_id":"daily-1","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sz","code":"000002"}]}}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let task = market.task(accepted["task_id"].as_str().unwrap()).unwrap();
    assert!(task.items[0].outcome.as_ref().unwrap().success);

    let scope = Scope {
        symbol: "sz.000002".into(),
        period: Period::Day,
    };
    let head = market
        .effective_head(&scope)
        .unwrap()
        .expect("published daily head");

    // Public read-back: the frozen oracle bar is served by the service query.
    let page = api
        .call(
            "query_data",
            json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sz","code":"000002"}},"filter":{"kind":"daily","start_date":"2000-01-10","end_date":"2000-01-10"},"version":{"kind":"latest_per_entity"},"page_size":10,"cursor":null}),
        )
        .unwrap();
    let record = &page["records"][0];
    assert_eq!(record["date"], "2000-01-10");
    assert_eq!(record["close"], "-8.06");
    // Derived yearly through the real worker path: update_data/market.yearly
    // → YearlyRunner + OfficialYearlyVerifier (env-installed source). One
    // complete natural year plus the still-running one.
    let yearly = api
        .call(
            "update_data",
            json!({"dataset":"market.yearly","request_id":"yr-1","contract_version":"v1","items":[{"entity":{"exchange":"sz","code":"000002"},"base_daily_version_id":head,"years":[2000,2026],"publication":{"mode":"current_head","expected_yearly_head":null}}]}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let task = market.task(yearly["task_id"].as_str().unwrap()).unwrap();
    assert!(
        task.items[0].outcome.as_ref().unwrap().success,
        "yearly item failed: {:?}",
        task.items[0].outcome
    );
    let yearly_scope = Scope {
        symbol: "sz.000002".into(),
        period: Period::Year,
    };
    let yearly_head = market
        .effective_head(&yearly_scope)
        .unwrap()
        .expect("yearly head");
    let record = market.fixed(&yearly_head).unwrap();
    let annual = &record.raw()["local_yearly_derivation_v1"]["annual"];
    assert_eq!(annual["base_daily_version_id"], json!(head));
    let records = annual["records"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    let (finished, running) = (&records[0], &records[1]);
    assert_eq!(finished["lineage"]["base_daily_version_id"], json!(head));
    assert_eq!(
        finished["lineage"]["algorithm_version"],
        "local-natural-year-v1"
    );
    assert_eq!(finished["year_unfinished"], false);
    assert_eq!(running["year_unfinished"], true);

    // Read-back through the public query equals an independent recompute of
    // the same fixed daily base.
    let expected = x_data_mcp::yearly::aggregate(
        market.fixed(&head).unwrap().version(),
        Some(&[2000, 2026].into()),
        chrono::Utc::now()
            .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .date_naive(),
    )
    .unwrap();
    let page = api
        .call(
            "query_data",
            json!({"dataset":"market.yearly","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sz","code":"000002"}},"filter":{"kind":"yearly","start_year":2000,"end_year":2026},"version":{"kind":"latest_per_entity"},"page_size":10,"cursor":null}),
        )
        .unwrap();
    let rows = page["records"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for (row, annual) in rows.iter().zip(expected.records.iter()) {
        assert_eq!(row["date"], json!(annual.bar.date.to_string()));
        assert_eq!(row["open"], json!(annual.bar.open.to_string()));
        assert_eq!(row["high"], json!(annual.bar.high.to_string()));
        assert_eq!(row["low"], json!(annual.bar.low.to_string()));
        assert_eq!(row["close"], json!(annual.bar.close.to_string()));
    }

    // Same frozen inputs again: the result reuses the version; the scope
    // keeps exactly one published yearly version.
    let again = api
        .call(
            "update_data",
            json!({"dataset":"market.yearly","request_id":"yr-2","contract_version":"v1","items":[{"entity":{"exchange":"sz","code":"000002"},"base_daily_version_id":head,"years":[2000,2026],"publication":{"mode":"current_head","expected_yearly_head":yearly_head}}]}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let task = market.task(again["task_id"].as_str().unwrap()).unwrap();
    assert!(task.items[0].outcome.as_ref().unwrap().success);
    assert_eq!(market.versions(&yearly_scope).unwrap().len(), 1);
    assert_eq!(
        market.effective_head(&yearly_scope).unwrap().as_deref(),
        Some(yearly_head.as_str())
    );
}

#[test]
fn service_without_env_keeps_daily_blocked() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvReset::absent();
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let api = DataApi::new(tasks, "run".into(), Access::local()).unwrap();
    let accepted = api
        .call(
            "start_collection",
            json!({"request_id":"daily","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600006"}]}}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let task = market.task(accepted["task_id"].as_str().unwrap()).unwrap();
    blocked_outcome(task.items[0].outcome.as_ref().unwrap().error.clone());
}

#[test]
fn service_env_with_uninspectable_manifest_stays_blocked() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let manifest_path = dir.path().join("manifest.json");
    std::fs::write(&manifest_path, b"{\"schema\":\"garbage\"}").unwrap();
    let _env = EnvReset::install(&manifest_path, dir.path());
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let api = DataApi::new(tasks, "run".into(), Access::local()).unwrap();
    let accepted = api
        .call(
            "start_collection",
            json!({"request_id":"daily","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600006"}]}}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let task = market.task(accepted["task_id"].as_str().unwrap()).unwrap();
    blocked_outcome(task.items[0].outcome.as_ref().unwrap().error.clone());
}

#[test]
fn service_yearly_fail_closed_paths() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(snap) = retained_snapshot() else {
        eprintln!("retained-inputs absent; yearly fail-closed test skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let manifest_path = dir.path().join("snapshot-manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    let env = EnvReset::install(&manifest_path, &snap.root);

    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let api = DataApi::new(tasks, "run-env".into(), Access::local()).unwrap();
    let accepted = api
        .call(
            "start_collection",
            json!({"request_id":"daily-1","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sz","code":"000002"}]}}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let task = market.task(accepted["task_id"].as_str().unwrap()).unwrap();
    assert!(task.items[0].outcome.as_ref().unwrap().success);
    let head = market
        .effective_head(&Scope {
            symbol: "sz.000002".into(),
            period: Period::Day,
        })
        .unwrap()
        .expect("published daily head");

    let yearly_request = |request_id: &str, base: &Value, years: &[i32]| json!({"dataset":"market.yearly","request_id":request_id,"contract_version":"v1","items":[{"entity":{"exchange":"sz","code":"000002"},"base_daily_version_id":base,"years":years,"publication":{"mode":"current_head","expected_yearly_head":null}}]});
    let item_error = |task_id: &str| {
        market.task(task_id).unwrap().items[0]
            .outcome
            .as_ref()
            .unwrap()
            .error
            .clone()
            .unwrap()
    };

    // A base version that does not exist is refused at acceptance.
    assert!(
        api.call(
            "update_data",
            yearly_request("yr-absent", &json!("no-such-version"), &[2000])
        )
        .is_err(),
        "unknown base must not be accepted"
    );

    // A requested year outside the fixed base range fails closed (the
    // acceptance coverage pre-check reports the missing year; the verifier's
    // own re-aggregation would reject it at run time the same way).
    let missing = api
        .call(
            "update_data",
            yearly_request("yr-missing", &json!(head), &[1989]),
        )
        .map(|_| ())
        .unwrap_err();
    assert!(
        missing.message.contains("absent"),
        "expected MissingYear refusal, got {missing:?}"
    );

    // An accepted base whose durable record no longer matches its pinned
    // digest fails closed before any derivation.
    let tampered = api
        .call(
            "update_data",
            yearly_request("yr-tampered", &json!(head), &[2000]),
        )
        .unwrap();
    rusqlite::Connection::open(market.tasks.path())
        .unwrap()
        .execute(
            "UPDATE data_versions SET content_digest='tampered' WHERE version_id=?1",
            [&head],
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let error = item_error(tampered["task_id"].as_str().unwrap());
    assert!(
        error.contains("INTEGRITY"),
        "expected integrity refusal, got {error:?}"
    );
    rusqlite::Connection::open(market.tasks.path())
        .unwrap()
        .execute(
            "UPDATE data_versions SET content_digest=json_extract(compat_json,'$.integrity_sha256') WHERE version_id=?1",
            [&head],
        )
        .unwrap();

    // Without the official-file source the verifier gate refuses.
    drop(env);
    let noenv = api
        .call(
            "update_data",
            yearly_request("yr-noenv", &json!(head), &[2000]),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let error = item_error(noenv["task_id"].as_str().unwrap());
    assert!(
        error.contains("SOURCE_NOT_VERIFIED")
            && error.contains("official-file source not installed"),
        "expected source-install refusal, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// update_data market.daily against the installed snapshot source (#183):
// TdxRangeProvider delivers the accepted range as a verified segment.
// ---------------------------------------------------------------------------

#[test]
fn range_provider_delivers_segment_with_snapshot_basis() {
    let dir = tempfile::tempdir().unwrap();
    let market = opened(&dir);
    let r = accept_daily(&market, "run-a", "sh", "600006");
    let snap = valid_snapshot();
    let src = make_source(&snap);
    let runner = TdxDailyRunner {
        source: &src,
        session: None,
        repository: market.clone(),
        run_id: "run-a".into(),
        published_at: now(),
    };
    let ItemExecution::Published(receipt) = runner.run_item(&r.task_id, &r.item_id).unwrap() else {
        panic!("expected publication")
    };
    let base = market.fixed(&receipt.version_id).unwrap();
    let provider = x_data_mcp::official_market::TdxRangeProvider { source: &src };

    let range = x_data_mcp::market_update::DateRange {
        start: chrono::NaiveDate::from_ymd_opt(2025, 7, 16).unwrap(),
        end: chrono::NaiveDate::from_ymd_opt(2025, 7, 16).unwrap(),
    };
    let acquired =
        x_data_mcp::market_update::runner::RangeProvider::acquire(&provider, &base, &range, "cand")
            .unwrap();
    assert_eq!(acquired.batch.bars.len(), 1);
    assert_eq!(acquired.batch.bars[0].date, range.start);
    assert!(acquired.combined.is_none());
    // The segment keeps the inspected-snapshot coverage claim unchanged so an
    // identical range reports no metadata change.
    assert_eq!(
        serde_json::to_value(&acquired.batch.coverage).unwrap(),
        base.raw()["batch"]["coverage"]
    );
    let evidence = &acquired.evidence;
    assert_eq!(evidence.base_version_id, receipt.version_id);
    assert_eq!(
        evidence.base_integrity_sha256,
        base.raw()["integrity_sha256"].as_str().unwrap()
    );
    assert_eq!(
        evidence.candidate_sha256,
        x_data_mcp::market_update::candidate_digest(&acquired.batch).unwrap()
    );
    assert_eq!(evidence.range, range);
    assert!(evidence.same_qfq_basis_ref.contains(src.manifest_sha256()));
    assert!(evidence.revision_ref.is_none());

    // A range the snapshot does not cover fails closed, never empty-approves.
    let outside = x_data_mcp::market_update::DateRange {
        start: chrono::NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
        end: chrono::NaiveDate::from_ymd_opt(2000, 1, 2).unwrap(),
    };
    assert!(matches!(
        x_data_mcp::market_update::runner::RangeProvider::acquire(
            &provider, &base, &outside, "cand"
        ),
        Err(SourceError::InvalidResponse(_))
    ));
}

#[test]
fn service_env_install_update_data_no_change_and_fail_closed() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(snap) = retained_snapshot() else {
        eprintln!("retained-inputs absent; update e2e skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let manifest_path = dir.path().join("snapshot-manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();

    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let scope = Scope {
        symbol: "sz.000002".into(),
        period: Period::Day,
    };

    // Publish the daily base directly (explicit source, no env) so the no-env
    // leg can observe a real fixed base being refused.
    let r = accept_daily(&market, "run-a", "sz", "000002");
    let src = make_source(&snap);
    let runner = TdxDailyRunner {
        source: &src,
        session: None,
        repository: market.clone(),
        run_id: "run-a".into(),
        published_at: now(),
    };
    let ItemExecution::Published(receipt) = runner.run_item(&r.task_id, &r.item_id).unwrap() else {
        panic!("expected publication")
    };
    let head = market.effective_head(&scope).unwrap().unwrap();
    assert_eq!(head, receipt.version_id);

    let update_request = |request_id: &str| json!({"dataset":"market.daily","request_id":request_id,"contract_version":"v1","items":[{"entity":{"exchange":"sz","code":"000002"},"base_version_id":head,"start_date":"2000-01-10","end_date":"2000-01-10"}]});
    let item_error = |task_id: &str| {
        market.task(task_id).unwrap().items[0]
            .outcome
            .as_ref()
            .unwrap()
            .error
            .clone()
            .unwrap()
    };

    // No env: the installed source is absent, so update keeps the refusal.
    let _absent = EnvReset::absent();
    let api = DataApi::new(tasks.clone(), "run".into(), Access::local()).unwrap();
    let upd = api.call("update_data", update_request("u-noenv")).unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    blocked_outcome(Some(item_error(upd["task_id"].as_str().unwrap())));

    // env installed: the real provider answers an identical segment → the
    // update is a verified no-change bound to the fixed base.
    drop(_absent);
    let _env = EnvReset::install(&manifest_path, &snap.root);
    let api = DataApi::new(tasks.clone(), "run-env".into(), Access::local()).unwrap();
    let upd = api.call("update_data", update_request("u-1")).unwrap();
    let task_id = upd["task_id"].as_str().unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let task = market.task(task_id).unwrap();
    assert!(
        task.items[0].outcome.as_ref().unwrap().success,
        "update item failed: {:?}",
        task.items[0].outcome
    );
    let progress = api
        .call("get_task_progress", json!({"task_id":task_id}))
        .unwrap();
    let result = &progress["range_results"][0];
    assert_eq!(result["no_change"], json!(true));
    assert_eq!(result["base_version"]["version_id"], json!(head));
    assert_eq!(result["result_version"]["version_id"], json!(head));
    assert_eq!(market.versions(&scope).unwrap().len(), 1);
    assert_eq!(
        market.effective_head(&scope).unwrap().as_deref(),
        Some(head.as_str())
    );

    // Same frozen inputs under a new request id reuse the receipt and never
    // create a second version.
    let again = api.call("update_data", update_request("u-2")).unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let task = market.task(again["task_id"].as_str().unwrap()).unwrap();
    assert!(task.items[0].outcome.as_ref().unwrap().success);
    assert_eq!(market.versions(&scope).unwrap().len(), 1);
    assert_eq!(
        market.effective_head(&scope).unwrap().as_deref(),
        Some(head.as_str())
    );

    // backfill never borrows the refresh provider: gap-only semantics differ,
    // so it keeps the blocked source refusal even with env installed.
    let bf = api.call("backfill_data", update_request("b-1")).unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    blocked_outcome(Some(item_error(bf["task_id"].as_str().unwrap())));

    // An accepted base whose durable record no longer matches its pinned
    // digest fails closed before acquisition.
    let upd = api
        .call("update_data", update_request("u-tampered"))
        .unwrap();
    rusqlite::Connection::open(market.tasks.path())
        .unwrap()
        .execute(
            "UPDATE data_versions SET content_digest='tampered' WHERE version_id=?1",
            [&head],
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let error = item_error(upd["task_id"].as_str().unwrap());
    assert!(
        error.contains("INTEGRITY"),
        "expected integrity refusal, got {error:?}"
    );
}

// ---------- #187 Phase B: TdxGapVerifier ----------

fn day(d: u32) -> NaiveDate {
    NaiveDate::parse_from_str(&d.to_string(), "%Y%m%d").unwrap()
}

/// Publish-ready record JSON for a materialized batch, matching the shape
/// `TdxDailyRunner` commits (version fields + `batch` + legacy digest).
fn subject_raw(version_id: &str, batch: &MarketDataBatch) -> Value {
    let mut raw = serde_json::to_value(EffectiveVersion {
        version_id: version_id.into(),
        symbol: batch.symbol.clone(),
        period: batch.period,
        adjustment: batch.adjustment,
        source: batch.provenance.source.clone(),
        records: batch.bars.clone(),
        published_at: now(),
        validation: ValidationSummary {
            record_count: batch.bars.len(),
            unique_record_count: batch.bars.len(),
            duplicate_count: 0,
        },
        unit_currency: batch.unit_currency.clone(),
        adjustment_evidence: batch.adjustment_evidence.clone(),
        yearly_derivation: None,
    })
    .unwrap();
    raw["batch"] = serde_json::to_value(batch).unwrap();
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    raw
}

fn subject_binding(raw: &Value) -> SubjectBinding {
    SubjectBinding {
        version_id: raw["version_id"].as_str().unwrap().into(),
        integrity_sha256: raw["integrity_sha256"].as_str().unwrap().into(),
    }
}

fn fresh_market() -> (tempfile::TempDir, SqliteTasks, SqliteMarket) {
    let d = tempfile::tempdir().unwrap();
    let t = SqliteTasks::new(d.path().join("db"));
    t.initialize().unwrap();
    (d, t.clone(), SqliteMarket::new(t))
}

fn verifier<'a>(
    source: &'a TdxMarketSource,
    market: &SqliteMarket,
) -> TdxGapVerifier<'a, SqliteMarket> {
    TdxGapVerifier {
        source,
        repository: market.clone(),
        observed_at: now(),
    }
}

fn target(d: u32) -> GapTarget {
    GapTarget {
        symbol: "sh.600006".into(),
        start_date: day(d),
        end_date: day(d),
    }
}

#[test]
fn gap_verifier_confirms_missing_day_against_pinned_snapshot() {
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let (_d, t, market) = fresh_market();
    // Controlled local-gap subject: the real materialized bars minus the
    // target day, declared provenance still binding the same pinned
    // archive+member digests.
    let mut batch = source.materialize_daily("sh.600006").unwrap();
    batch.bars.retain(|b| b.date != day(20250716));
    // Honest label: the storage contract requires status Complete, so the
    // reason discloses the withheld bar and denies complete history.
    batch.coverage.reason = "acceptance local-gap subject: real .day member bars with 2025-07-16 withheld; not a complete source history".into();
    let raw = subject_raw("subject-gap", &batch);
    market::published_fixture(&market, &raw);

    let v = verifier(&source, &market);
    let basis = v.verify(&target(20250716), &subject_binding(&raw)).unwrap();
    assert_eq!(v.verifier_id(), "tdx-official-gap-verifier-v1");
    assert_eq!(basis.conclusion, "confirmed_missing_range");
    assert_eq!(basis.symbol, "sh.600006");
    assert_eq!(basis.start_date, day(20250716));
    assert_eq!(basis.subject_version_id, "subject-gap");
    assert_eq!(
        basis.subject_version_digest,
        raw["integrity_sha256"].as_str().unwrap()
    );
    assert_eq!(basis.verifier_id, v.verifier_id());
    assert_eq!(basis.basis_sha256, basis.content_digest());
    // The artifact binds the .day member digest; the manifest and gbbq
    // member are bound inside the artifact identity.
    let inputs = batch.provenance.snapshot_inputs.as_ref().unwrap();
    let day_input = inputs.iter().find(|i| i.role == "day").unwrap();
    let gbbq_input = inputs.iter().find(|i| i.role == "gbbq").unwrap();
    assert_eq!(basis.artifact_sha256, day_input.member_sha256);
    assert!(basis.artifact_identity.contains(&day_input.member_sha256));
    assert!(basis.artifact_identity.contains(&gbbq_input.member_sha256));
    assert!(basis.artifact_identity.contains(source.manifest_sha256()));
    // The decision commits through the guarded producer and writes the gap.
    decide_gap(
        &market,
        &v,
        &target(20250716),
        &subject_binding(&raw),
        "d-gap",
        now(),
    )
    .unwrap();
    let c = rusqlite::Connection::open(t.path()).unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM proven_gaps", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn gap_verifier_source_limitation_and_no_local_gap_write_no_gap() {
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let (_d, t, market) = fresh_market();
    let batch = source.materialize_daily("sh.600006").unwrap();
    let raw = subject_raw("subject-full", &batch);
    market::published_fixture(&market, &raw);
    let subject = subject_binding(&raw);
    let v = verifier(&source, &market);

    // Source member exists but lacks the target day -> decision only.
    let basis = v.verify(&target(20250718), &subject).unwrap();
    assert_eq!(basis.conclusion, "source_limitation");
    decide_gap(&market, &v, &target(20250718), &subject, "d-limit", now()).unwrap();
    // Subject already carries the day -> decision only.
    let basis = v.verify(&target(20250716), &subject).unwrap();
    assert_eq!(basis.conclusion, "no_local_gap");
    decide_gap(&market, &v, &target(20250716), &subject, "d-local", now()).unwrap();

    let c = rusqlite::Connection::open(t.path()).unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM gap_verification_decisions", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        2
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM proven_gaps", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn gap_verifier_fails_closed_on_basis_digest_or_conflict() {
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let (_d, _t, market) = fresh_market();
    let batch = source.materialize_daily("sh.600006").unwrap();
    let v = verifier(&source, &market);
    let err = |e: x_data_mcp::market_storage::Error| {
        matches!(e, x_data_mcp::market_storage::Error::BasisNotVerified(_))
    };

    // Subject minted from another snapshot generation: declared member
    // digests differ from the installed source's actual member bytes.
    let mut other = batch.clone();
    for i in other.provenance.snapshot_inputs.as_mut().unwrap() {
        i.member_sha256 = "0".repeat(64);
    }
    let raw = subject_raw("subject-other-snapshot", &other);
    market::published_fixture(&market, &raw);
    assert!(err(v
        .verify(&target(20250716), &subject_binding(&raw))
        .unwrap_err()));

    // Overlapping value conflict on a shared date -> integrity doubt, not a gap.
    let mut conflict = batch.clone();
    conflict.bars[0].close = dec("7.52"); // pinned source says 7.53
    let raw = subject_raw("subject-conflict", &conflict);
    market
        .commit(&market::prepare(
            &market,
            &raw,
            PublishIntent::Daily {
                expected_head: Some("subject-other-snapshot".into()),
            },
        ))
        .unwrap();
    assert!(err(v
        .verify(&target(20250717), &subject_binding(&raw))
        .unwrap_err()));

    // Non-official subject (legacy fixture, different source/provenance).
    let raw = market::samples()[0]["record"].clone();
    market
        .commit(&market::prepare(
            &market,
            &raw,
            PublishIntent::Daily {
                expected_head: Some("subject-conflict".into()),
            },
        ))
        .unwrap();
    assert!(err(v
        .verify(&target(20250717), &subject_binding(&raw))
        .unwrap_err()));

    // Frozen binding digest that does not match the stored record.
    let mut bad = subject_binding(&raw);
    bad.integrity_sha256 = "f".repeat(64);
    assert!(err(v.verify(&target(20250717), &bad).unwrap_err()));

    // Multi-day targets are outside this bounded slice.
    let wide = GapTarget {
        symbol: "sh.600006".into(),
        start_date: day(20250715),
        end_date: day(20250717),
    };
    assert!(err(v.verify(&wide, &subject_binding(&raw)).unwrap_err()));
}

#[test]
fn service_verify_data_gap_uses_installed_snapshot_verifier() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let mut batch = source.materialize_daily("sh.600006").unwrap();
    batch.bars.retain(|b| b.date != day(20250716));
    batch.coverage.reason = "acceptance local-gap subject: real .day member bars with 2025-07-16 withheld; not a complete source history".into();
    market::published_fixture(&market, &subject_raw("subject-gap", &batch));

    let manifest_path = db.path().join("manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    let _env = EnvReset::install(&manifest_path, &snap.root);
    let api = DataApi::new(tasks.clone(), "run".into(), Access::local()).unwrap();
    let accepted = api
        .call(
            "verify_data_gap",
            json!({"contract_version":"v1","request_id":"g1","target":{"symbol":"sh.600006","period":"day","start_date":"2025-07-16","end_date":"2025-07-16"}}),
        )
        .unwrap();
    let worked = api.call("run_worker_once", json!({})).unwrap();
    assert_eq!(worked["task_id"], accepted["task_id"]);
    let done = tasks
        .read_task(accepted["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert!(done.items[0].outcome.as_ref().unwrap().success);
    let c = rusqlite::Connection::open(tasks.path()).unwrap();
    let (conclusion, verifier_id): (String, String) = c
        .query_row(
            "SELECT conclusion,verifier_id FROM gap_verification_decisions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(conclusion, "confirmed_missing_range");
    assert_eq!(verifier_id, "tdx-official-gap-verifier-v1");
    assert_eq!(
        c.query_row("SELECT count(*) FROM proven_gaps", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

// ---------- #187 Phase C: TdxBackfillProvider ----------

fn publish_catalog(tasks: &SqliteTasks, codes: &[&str]) {
    use x_data_mcp::{
        catalog::{
            CatalogSnapshot, CompletenessEvidence, Exchange, IndustryInfo, SecurityRecord,
            SecurityType,
        },
        sqlite_catalog::SqliteCatalog,
    };
    let records = codes
        .iter()
        .map(|code| SecurityRecord {
            symbol: (*code).into(),
            name: (*code).into(),
            exchange: Exchange::Shanghai,
            security_type: SecurityType::AShare,
            source_security_id: (*code).into(),
            industry: IndustryInfo::Missing {
                reason: "fixture".into(),
            },
            source: "fixture:catalog".into(),
            acquired_at: now(),
        })
        .collect();
    SqliteCatalog::new(tasks.clone())
        .publish_fixture(
            "catalog-1",
            CatalogSnapshot {
                source: "fixture:catalog".into(),
                acquired_at: now(),
                completeness: CompletenessEvidence::Complete {
                    expected_count: codes.len(),
                },
                records,
            },
            None,
        )
        .unwrap();
}

#[test]
fn backfill_provider_builds_combined_candidate_for_frozen_gap() {
    use x_data_mcp::market_update::runner::RangeProvider;
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let (_d, _t, market) = fresh_market();
    let full = source.materialize_daily("sh.600006").unwrap();
    // Controlled-gap base: full member history minus the target day,
    // declared snapshot binding intact.
    let mut gapped = full.clone();
    gapped.bars.retain(|b| b.date != day(20250716));
    gapped.coverage.reason = "acceptance local-gap subject: real .day member bars with 2025-07-16 withheld; not a complete source history".into();
    let raw = subject_raw("base-gap", &gapped);
    market::published_fixture(&market, &raw);
    let base = market.fixed("base-gap").unwrap();
    let provider = TdxBackfillProvider {
        source: &source,
        published_at: now(),
    };
    let range = x_data_mcp::market_update::DateRange {
        start: day(20250716),
        end: day(20250716),
    };
    let acq = provider.acquire(&base, &range, "cand-1").unwrap();
    // The segment is exactly the real source bar inside the frozen range.
    assert_eq!(acq.batch.bars, [full.bars[1].clone()]);
    let whole = acq.combined.expect("changed backfill needs a candidate");
    assert_eq!(whole.record["version_id"], json!("cand-1"));
    // Merged record = full source history; honest Complete claim because
    // the merge equals the materialized member bytes.
    let combined_batch: MarketDataBatch =
        serde_json::from_value(whole.record["batch"].clone()).unwrap();
    assert_eq!(combined_batch.bars, full.bars);
    assert_eq!(combined_batch.coverage.status, CoverageStatus::Complete);
    let version: EffectiveVersion = serde_json::from_value(whole.record.clone()).unwrap();
    assert_eq!(version.records, combined_batch.bars);
    // Evidence bindings.
    assert_eq!(
        whole.evidence.base_integrity_sha256,
        raw["integrity_sha256"].as_str().unwrap()
    );
    assert_eq!(
        whole.evidence.segment_sha256,
        x_data_mcp::market_update::candidate_digest(&acq.batch).unwrap()
    );
    assert_eq!(
        whole.evidence.combined_integrity_sha256,
        whole.record["integrity_sha256"].as_str().unwrap()
    );
    let inputs = combined_batch.provenance.snapshot_inputs.as_ref().unwrap();
    for sha in [
        source.manifest_sha256(),
        inputs[0].member_sha256.as_str(),
        inputs[1].member_sha256.as_str(),
    ] {
        assert!(whole.evidence.complete_coverage_ref.contains(sha));
    }
    assert_eq!(acq.evidence.range, range);
    assert!(
        acq.evidence
            .revision_ref
            .as_deref()
            .unwrap()
            .contains("base-gap")
    );
    // The built record passes the governed stage/check path.
    market.stage(&whole.record).unwrap();
    assert_eq!(
        market.candidate("cand-1").unwrap().version().records.len(),
        3
    );

    // A base whose only hole is partially inside the range keeps its own
    // coverage claim (merge does not equal full history).
    let mut partial = full.clone();
    partial.bars.retain(|b| b.date != day(20250715));
    let raw = subject_raw("base-partial", &partial);
    market
        .commit(&market::prepare(
            &market,
            &raw,
            PublishIntent::Daily {
                expected_head: Some("base-gap".into()),
            },
        ))
        .unwrap();
    let base = market.fixed("base-partial").unwrap();
    let acq = provider.acquire(&base, &range, "cand-2").unwrap();
    let combined_batch: MarketDataBatch =
        serde_json::from_value(acq.combined.unwrap().record["batch"].clone()).unwrap();
    assert_eq!(combined_batch.bars.len(), 2);
    assert_eq!(combined_batch.coverage, partial.coverage);

    // A base that does not bind this snapshot generation fails closed.
    let mut other = full.clone();
    for i in other.provenance.snapshot_inputs.as_mut().unwrap() {
        i.member_sha256 = "0".repeat(64);
    }
    let raw = subject_raw("base-other-snapshot", &other);
    market
        .commit(&market::prepare(
            &market,
            &raw,
            PublishIntent::Daily {
                expected_head: Some("base-partial".into()),
            },
        ))
        .unwrap();
    let base = market.fixed("base-other-snapshot").unwrap();
    assert!(matches!(
        provider.acquire(&base, &range, "cand-3"),
        Err(SourceError::NotVerified(_))
    ));
    // An empty in-range segment cannot fill anything.
    let base = market.fixed("base-partial").unwrap();
    let empty = x_data_mcp::market_update::DateRange {
        start: day(20250720),
        end: day(20250720),
    };
    assert!(matches!(
        provider.acquire(&base, &empty, "cand-4"),
        Err(SourceError::InvalidResponse(_))
    ));

    // An overlapping in-range bar the source contradicts is a basis
    // conflict — the provider fails closed instead of overwriting it.
    let mut conflict = full.clone();
    for b in &mut conflict.bars {
        if b.date == day(20250716) {
            // A flat shifted bar stays inside low/high bounds while still
            // contradicting the source values.
            b.open = dec("999.99");
            b.high = dec("999.99");
            b.low = dec("999.99");
            b.close = dec("999.99");
        }
    }
    let raw = subject_raw("base-conflict", &conflict);
    market
        .commit(&market::prepare(
            &market,
            &raw,
            PublishIntent::Daily {
                expected_head: Some("base-other-snapshot".into()),
            },
        ))
        .unwrap();
    let base = market.fixed("base-conflict").unwrap();
    assert!(matches!(
        provider.acquire(&base, &range, "cand-5"),
        Err(SourceError::NotVerified(_))
    ));
}

#[test]
fn service_backfill_data_default_restores_proven_gap() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let full = source.materialize_daily("sh.600006").unwrap();
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    publish_catalog(&tasks, &["600006"]);
    let mut gapped = full.clone();
    gapped.bars.retain(|b| b.date != day(20250716));
    gapped.coverage.reason = "acceptance local-gap subject: real .day member bars with 2025-07-16 withheld; not a complete source history".into();
    let subject = subject_raw("subject-gap", &gapped);
    market::published_fixture(&market, &subject);

    let manifest_path = db.path().join("manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    let _env = EnvReset::install(&manifest_path, &snap.root);
    let api = DataApi::new(tasks.clone(), "run".into(), Access::local()).unwrap();
    // 1. verify_data_gap proves the local gap.
    api.call(
        "verify_data_gap",
        json!({"contract_version":"v1","request_id":"g1","target":{"symbol":"sh.600006","period":"day","start_date":"2025-07-16","end_date":"2025-07-16"}}),
    )
    .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    // 2. backfill_data under the default selector freezes exactly that gap.
    let accepted = api
        .call(
            "backfill_data",
            json!({"request_id":"b1","dataset":"market.daily","contract_version":"v1","selection":{"kind":"default"}}),
        )
        .unwrap();
    assert_eq!(accepted["contract_version"], json!("default-range-task-v1"));
    let task_id = accepted["task_id"].as_str().unwrap().to_owned();
    let snap_task = tasks.read_task(&task_id).unwrap().unwrap();
    let f = &snap_task.items[0].spec.frozen_inputs;
    assert_eq!(f["proven_gap_ranges"].as_array().unwrap().len(), 1);
    assert_eq!(
        f["proven_gap_ranges"][0]["range"]["start_date"],
        json!("2025-07-16")
    );
    // 3. Worker run: the installed snapshot fills the frozen gap.
    let worked = api.call("run_worker_once", json!({})).unwrap();
    assert_eq!(worked["task_id"], json!(task_id));
    let done = tasks.read_task(&task_id).unwrap().unwrap();
    assert!(done.items[0].outcome.as_ref().unwrap().success);
    let c = rusqlite::Connection::open(tasks.path()).unwrap();
    let new_head: String = c
        .query_row(
            "SELECT version_id FROM effective_heads WHERE symbol='sh.600006' AND period='day'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(new_head, "subject-gap");
    // The merged version restored the real source bar; outside-range bars
    // preserved verbatim; old version retained.
    let merged = market.fixed(&new_head).unwrap();
    assert_eq!(merged.version().records, full.bars);
    assert_eq!(merged.version().records[1], full.bars[1]);
    market.fixed("subject-gap").unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM data_versions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    // The covering binds the new version; the open gap is gone while the
    // historical decision/proven-gap rows stay auditable.
    let (gap_id, covered_version): (String, String) = c
        .query_row("SELECT gap_id,version_id FROM gap_coverings", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(covered_version, new_head);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM proven_gaps WHERE gap_id=?1",
            [&gap_id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM gap_verification_decisions", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    // Availability reports no open confirmed gap for the covered range.
    let availability = api
        .call(
            "get_data_availability",
            json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},"filter":{"kind":"daily","start_date":"2025-07-15","end_date":"2025-07-17"},"version":{"kind":"latest_per_entity"}}),
        )
        .unwrap();
    assert_eq!(
        availability["items"][0]["confirmed_gaps"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // Replay: the same request reuses the task; no second version/covering.
    let replay = api
        .call(
            "backfill_data",
            json!({"request_id":"b1","dataset":"market.daily","contract_version":"v1","selection":{"kind":"default"}}),
        )
        .unwrap();
    assert_eq!(replay["reused"], json!(true));
    assert_eq!(replay["task_id"], json!(task_id));
    assert_eq!(
        c.query_row("SELECT count(*) FROM gap_coverings", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );

    // Regression: update_data still routes to TdxRangeProvider, not the
    // blocked provider — the merged head's coverage text differs from the
    // materialized segment's, so prepare honestly lands on
    // NEEDS_VERIFICATION (refresh carries no combined evidence) instead of
    // SOURCE_NOT_VERIFIED or a publish. No third version appears.
    let upd = api
        .call(
            "update_data",
            json!({"dataset":"market.daily","request_id":"u1","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":new_head,"start_date":"2025-07-16","end_date":"2025-07-16"}]}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let upd_task = tasks
        .read_task(upd["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let outcome = upd_task.items[0].outcome.as_ref().unwrap();
    assert!(!outcome.success);
    let err = outcome.error.as_deref().unwrap();
    assert!(err.contains("NEEDS_VERIFICATION"), "unexpected: {err}");
    assert!(!err.contains("SOURCE_NOT_VERIFIED"), "unexpected: {err}");
    assert_eq!(
        c.query_row("SELECT count(*) FROM data_versions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn service_backfill_explicit_range_stays_blocked_with_installed_source() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let batch = source.materialize_daily("sh.600006").unwrap();
    market::published_fixture(&market, &subject_raw("base-full", &batch));
    let manifest_path = db.path().join("manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    let _env = EnvReset::install(&manifest_path, &snap.root);
    let api = DataApi::new(tasks.clone(), "run".into(), Access::local()).unwrap();
    let accepted = api
        .call(
            "backfill_data",
            json!({"request_id":"bx1","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"base-full","start_date":"2025-07-15","end_date":"2025-07-17"}]}),
        )
        .unwrap();
    assert_eq!(
        accepted["contract_version"],
        json!("explicit-range-task-v1")
    );
    let worked = api.call("run_worker_once", json!({})).unwrap();
    assert_eq!(worked["task_id"], accepted["task_id"]);
    let done = tasks
        .read_task(accepted["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let outcome = done.items[0].outcome.as_ref().unwrap();
    assert!(!outcome.success);
    assert!(
        outcome
            .error
            .as_deref()
            .unwrap()
            .contains("SOURCE_NOT_VERIFIED")
    );
    let c = rusqlite::Connection::open(tasks.path()).unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM data_versions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn service_verify_data_gap_stale_head_fails_closed() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let full = source.materialize_daily("sh.600006").unwrap();
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    publish_catalog(&tasks, &["600006"]);
    let mut gapped = full.clone();
    gapped.bars.retain(|b| b.date != day(20250716));
    gapped.coverage.reason = "acceptance local-gap subject: real .day member bars with 2025-07-16 withheld; not a complete source history".into();
    market::published_fixture(&market, &subject_raw("subject-gap", &gapped));

    let manifest_path = db.path().join("manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    let _env = EnvReset::install(&manifest_path, &snap.root);
    let api = DataApi::new(tasks.clone(), "run".into(), Access::local()).unwrap();
    // Acceptance freezes subject-gap as the verification subject.
    let accepted = api
        .call(
            "verify_data_gap",
            json!({"contract_version":"v1","request_id":"g-stale","target":{"symbol":"sh.600006","period":"day","start_date":"2025-07-16","end_date":"2025-07-16"}}),
        )
        .unwrap();
    let task_id = accepted["task_id"].as_str().unwrap().to_owned();

    // The effective head moves through the governed publish path before
    // the worker runs: the #155 in-transaction stale guard must refuse.
    let mut v2 = full.clone();
    v2.bars.retain(|b| b.date != day(20250715));
    v2.coverage.reason = "acceptance local-gap subject: real .day member bars with 2025-07-15 withheld; not a complete source history".into();
    market
        .commit(&market::prepare(
            &market,
            &subject_raw("subject-gap-v2", &v2),
            PublishIntent::Daily {
                expected_head: Some("subject-gap".into()),
            },
        ))
        .unwrap();

    api.call("run_worker_once", json!({})).unwrap();
    let done = tasks.read_task(&task_id).unwrap().unwrap();
    let outcome = done.items[0].outcome.as_ref().unwrap();
    assert!(!outcome.success);
    assert!(
        outcome
            .error
            .as_deref()
            .unwrap()
            .contains("subject head moved"),
        "unexpected error: {:?}",
        outcome.error
    );
    let c = rusqlite::Connection::open(tasks.path()).unwrap();
    for table in ["gap_verification_decisions", "proven_gaps"] {
        assert_eq!(
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table} must stay empty on stale-head refusal"
        );
    }
}

#[test]
fn service_no_env_verify_backfill_update_all_fail_closed() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let full = source.materialize_daily("sh.600006").unwrap();
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    publish_catalog(&tasks, &["600006"]);
    let mut gapped = full.clone();
    gapped.bars.retain(|b| b.date != day(20250716));
    gapped.coverage.reason = "acceptance local-gap subject: real .day member bars with 2025-07-16 withheld; not a complete source history".into();
    market::published_fixture(&market, &subject_raw("subject-gap", &gapped));

    // With the source installed, one real verification proves the gap so
    // the later no-env backfill has a frozen gap to refuse against.
    let manifest_path = db.path().join("manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    {
        let _env = EnvReset::install(&manifest_path, &snap.root);
        let api = DataApi::new(tasks.clone(), "run-env".into(), Access::local()).unwrap();
        api.call(
            "verify_data_gap",
            json!({"contract_version":"v1","request_id":"g1","target":{"symbol":"sh.600006","period":"day","start_date":"2025-07-16","end_date":"2025-07-16"}}),
        )
        .unwrap();
        api.call("run_worker_once", json!({})).unwrap();
    }

    let _absent = EnvReset::absent();
    let api = DataApi::new(tasks.clone(), "run-noenv".into(), Access::local()).unwrap();
    let fail = |name: &str, args: Value| -> String {
        let accepted = api.call(name, args).unwrap();
        api.call("run_worker_once", json!({})).unwrap();
        let done = tasks
            .read_task(accepted["task_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        let outcome = done.items[0].outcome.as_ref().unwrap();
        assert!(!outcome.success, "{name} unexpectedly succeeded");
        outcome.error.clone().unwrap()
    };

    // verify_data_gap without an installed source keeps the #155
    // unverified-verifier refusal; no new decision or gap row.
    let err = fail(
        "verify_data_gap",
        json!({"contract_version":"v1","request_id":"g2","target":{"symbol":"sh.600006","period":"day","start_date":"2025-07-16","end_date":"2025-07-16"}}),
    );
    assert!(err.contains("source-gap verifier"), "unexpected: {err}");

    // backfill_data accepts the frozen open gap but execution stays on the
    // blocked provider — no version and no covering.
    let err = fail(
        "backfill_data",
        json!({"request_id":"b1","dataset":"market.daily","contract_version":"v1","selection":{"kind":"default"}}),
    );
    assert!(
        err.contains("SOURCE_NOT_VERIFIED") || err.contains("NOT_PRODUCTION_READY"),
        "unexpected: {err}"
    );

    // update_data keeps OfficialRangeProvider without an installed source.
    let err = fail(
        "update_data",
        json!({"dataset":"market.daily","request_id":"u1","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"subject-gap","start_date":"2025-07-15","end_date":"2025-07-16"}]}),
    );
    assert!(err.contains("SOURCE_NOT_VERIFIED"), "unexpected: {err}");

    let c = rusqlite::Connection::open(tasks.path()).unwrap();
    for (table, want) in [
        ("gap_verification_decisions", 1),
        ("proven_gaps", 1),
        ("data_versions", 1),
        ("gap_coverings", 0),
    ] {
        assert_eq!(
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            want,
            "{table} count drifted"
        );
    }
}

#[test]
fn seed_gap_subject_tool_is_honest_and_fails_closed() {
    use std::process::Command;
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(snap) = retained_snapshot() else {
        eprintln!("retained-inputs absent; seed-tool test skipped");
        return;
    };
    let source = make_source(&snap);
    let full = source.materialize_daily("sz.000002").unwrap();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("m.sqlite");
    let tasks = SqliteTasks::new(&db_path);
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let manifest_path = db.path().join("manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    let _env = EnvReset::install(&manifest_path, &snap.root);
    let bin = env!("CARGO_BIN_EXE_seed_gap_subject");

    // Honest construction: real acceptance, real publish, verbatim disclosure.
    let out = Command::new(bin)
        .arg(&db_path)
        .args(["--symbol", "sz.000002", "--drop-date", "2000-01-10"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seeded: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(seeded["reused"], json!(false));
    assert_eq!(seeded["bars"], json!(full.bars.len() - 1));
    let subject_id = seeded["version_id"].as_str().unwrap().to_string();
    let task_id = seeded["task_id"].as_str().unwrap().to_string();
    let item_id = seeded["item_id"].as_str().unwrap().to_string();

    // Real task + receipt: the subject is bound to a genuinely accepted
    // LegacyCollection task, not a raw insert.
    let task = market.task(&task_id).unwrap();
    assert!(task.items[0].outcome.as_ref().unwrap().success);
    let receipt = market
        .collection_receipt(&task_id, &item_id)
        .unwrap()
        .expect("collection receipt");
    assert_eq!(receipt.version_id, subject_id);

    // Exactly one real bar withheld; the stored subject carries the
    // verbatim disclosure reason and the source snapshot binding.
    let subject = market.fixed(&subject_id).unwrap();
    assert_eq!(subject.version().records.len(), full.bars.len() - 1);
    assert!(
        !subject
            .version()
            .records
            .iter()
            .any(|r| r.date.to_string() == "2000-01-10")
    );
    let batch = &subject.raw()["batch"];
    assert_eq!(batch["coverage"]["status"], json!("Complete"));
    let reason = batch["coverage"]["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("ACCEPTANCE-LOCAL-GAP-SUBJECT:")
            && reason.contains("2000-01-10")
            && reason.contains("not complete source history"),
        "unexpected reason: {reason}"
    );
    assert_eq!(
        batch["provenance"]["snapshot_inputs"],
        serde_json::to_value(full.provenance.snapshot_inputs.as_ref().unwrap()).unwrap()
    );

    // Idempotent re-seed replays the task instead of fabricating a second.
    let out = Command::new(bin)
        .arg(&db_path)
        .args(["--symbol", "sz.000002", "--drop-date", "2000-01-10"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let again: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(again["reused"], json!(true));
    assert_eq!(again["version_id"], json!(subject_id));

    // A withheld date absent from the member is refused, not fabricated.
    let other_path = db.path().join("other.sqlite");
    SqliteTasks::new(&other_path).initialize().unwrap();
    let out = Command::new(bin)
        .arg(&other_path)
        .args(["--symbol", "sz.000002", "--drop-date", "1989-01-01"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no bar"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Without an installed source the tool refuses before touching the db.
    let out = Command::new(bin)
        .arg(db.path().join("noenv.sqlite"))
        .args(["--symbol", "sz.000002"])
        .env_remove("X_DATA_MCP_OFFICIAL_MANIFEST")
        .env_remove("X_DATA_MCP_OFFICIAL_DIR")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("source not installed"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // --source fixture variant: real bars under a fixture label; the
    // verifier's source-identity gate must fail closed on it.
    let db2 = tempfile::tempdir().unwrap();
    let db2_path = db2.path().join("m.sqlite");
    let tasks2 = SqliteTasks::new(&db2_path);
    tasks2.initialize().unwrap();
    let out = Command::new(bin)
        .arg(&db2_path)
        .args([
            "--symbol",
            "sz.000002",
            "--drop-date",
            "2000-01-10",
            "--source",
            "fixture",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fixture seed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fixture_seeded: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        fixture_seeded["source_label"],
        json!("fixture:acceptance-187")
    );
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let api2 = DataApi::new(tasks2.clone(), "run-fixture".into(), Access::local()).unwrap();
    let accepted = api2
        .call(
            "verify_data_gap",
            json!({"contract_version":"v1","request_id":"fx1","target":{"symbol":"sz.000002","period":"day","start_date":"2000-01-10","end_date":"2000-01-10"}}),
        )
        .unwrap();
    api2.call("run_worker_once", json!({})).unwrap();
    let done = tasks2
        .read_task(accepted["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let outcome = done.items[0].outcome.as_ref().unwrap();
    assert!(!outcome.success, "fixture subject must not verify");
    let c2 = rusqlite::Connection::open(tasks2.path()).unwrap();
    for table in ["gap_verification_decisions", "proven_gaps"] {
        assert_eq!(
            c2.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table} must stay empty on identity-mismatch refusal"
        );
    }
}

#[test]
fn seeded_gap_subject_drives_retained_backfill_e2e() {
    use std::process::Command;
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(snap) = retained_snapshot() else {
        eprintln!("retained-inputs absent; seeded backfill e2e skipped");
        return;
    };
    let source = make_source(&snap);
    let full = source.materialize_daily("sz.000002").unwrap();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("m.sqlite");
    let tasks = SqliteTasks::new(&db_path);
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let manifest_path = db.path().join("manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    let _env = EnvReset::install(&manifest_path, &snap.root);

    // Seed the controlled local-gap subject through the acceptance bin
    // (real task acceptance + governed publish), then run the real
    // verify -> backfill chain against it.
    let out = Command::new(env!("CARGO_BIN_EXE_seed_gap_subject"))
        .arg(&db_path)
        .args(["--symbol", "sz.000002", "--drop-date", "2000-01-10"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seeded: Value = serde_json::from_slice(&out.stdout).unwrap();
    let subject_id = seeded["version_id"].as_str().unwrap().to_string();

    let api = DataApi::new(tasks.clone(), "run".into(), Access::local()).unwrap();
    api.call(
        "verify_data_gap",
        json!({"contract_version":"v1","request_id":"g1","target":{"symbol":"sz.000002","period":"day","start_date":"2000-01-10","end_date":"2000-01-10"}}),
    )
    .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let accepted = api
        .call(
            "backfill_data",
            json!({"request_id":"b1","dataset":"market.daily","contract_version":"v1","selection":{"kind":"default"}}),
        )
        .unwrap();
    let task_id = accepted["task_id"].as_str().unwrap().to_owned();
    api.call("run_worker_once", json!({})).unwrap();
    let done = tasks.read_task(&task_id).unwrap().unwrap();
    assert!(
        done.items[0].outcome.as_ref().unwrap().success,
        "backfill item failed: {:?}",
        done.items[0].outcome
    );

    // The governed path published a new immutable version and advanced the
    // head past the seeded subject; the covering binds the new version.
    let c = rusqlite::Connection::open(tasks.path()).unwrap();
    let new_head: String = c
        .query_row(
            "SELECT version_id FROM effective_heads WHERE symbol='sz.000002' AND period='day'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(new_head, subject_id);
    let merged = market.fixed(&new_head).unwrap();
    assert_eq!(merged.version().records.len(), full.bars.len());
    let restored = merged
        .version()
        .records
        .iter()
        .find(|r| r.date.to_string() == "2000-01-10")
        .expect("restored bar");
    let source_bar = full
        .bars
        .iter()
        .find(|b| b.date.to_string() == "2000-01-10")
        .unwrap();
    assert_eq!(restored.close, source_bar.close);
    assert_eq!(restored.open, source_bar.open);
    market.fixed(&subject_id).unwrap(); // old version retained
    let covered_version: String = c
        .query_row("SELECT version_id FROM gap_coverings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(covered_version, new_head);
    assert_eq!(
        c.query_row("SELECT count(*) FROM data_versions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );

    // Replay stays idempotent; explicit-range backfill remains blocked;
    // update_data still lands on the range provider (NEEDS_VERIFICATION).
    let replay = api
        .call(
            "backfill_data",
            json!({"request_id":"b1","dataset":"market.daily","contract_version":"v1","selection":{"kind":"default"}}),
        )
        .unwrap();
    assert_eq!(replay["reused"], json!(true));
    let explicit = api
        .call(
            "backfill_data",
            json!({"request_id":"bx1","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sz","code":"000002"},"base_version_id":new_head,"start_date":"2000-01-10","end_date":"2000-01-10"}]}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let done = tasks
        .read_task(explicit["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let err = done.items[0]
        .outcome
        .as_ref()
        .unwrap()
        .error
        .clone()
        .unwrap();
    assert!(err.contains("SOURCE_NOT_VERIFIED"), "unexpected: {err}");
    let upd = api
        .call(
            "update_data",
            json!({"dataset":"market.daily","request_id":"u1","contract_version":"v1","items":[{"entity":{"exchange":"sz","code":"000002"},"base_version_id":new_head,"start_date":"2000-01-10","end_date":"2000-01-10"}]}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let done = tasks
        .read_task(upd["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let err = done.items[0]
        .outcome
        .as_ref()
        .unwrap()
        .error
        .clone()
        .unwrap();
    assert!(err.contains("NEEDS_VERIFICATION"), "unexpected: {err}");
    assert_eq!(
        c.query_row("SELECT count(*) FROM data_versions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn materialize_session_shares_decode_and_keeps_symbols_isolated() {
    // Two symbols plus a third symbol's event and a non-category-1 record:
    // the shared decode must produce batches identical to the per-call
    // path, and one symbol's records must never enter another's chain.
    let bars_600004: Vec<u8> = [
        day_record(20250715, [500, 510, 490, 505], 0.0, 10),
        day_record(20250716, [505, 515, 500, 512], 0.0, 20),
        day_record(20250717, [512, 520, 508, 518], 0.0, 30),
    ]
    .concat();
    let bars_600006: Vec<u8> = [
        day_record(20250715, [1128, 1130, 1125, 1129], 0.0, 100),
        day_record(20250716, [1129, 1132, 1128, 1131], 0.0, 110),
        day_record(20250717, [1131, 1135, 1130, 1134], 0.0, 120),
    ]
    .concat();
    let events = gbbq_member(&[
        gbbq_record(1, "600006", 20250716, 1, [0.0, 0.0, 0.0, 5.0]),
        gbbq_record(1, "600004", 20250715, 1, [1.0, 0.0, 0.0, 0.0]),
        // A non-category-1 record for 600004 is excluded uninterpreted and
        // counted only into 600004's evidence.
        gbbq_record(1, "600004", 20250716, 2, [0.0, 0.0, 0.0, 0.0]),
        // A different symbol's category-1 event must be invisible to both.
        gbbq_record(0, "000002", 20250716, 1, [9.0, 9.0, 9.0, 9.0]),
    ]);
    let snap = snapshot(
        &[
            ("sh\\lday\\sh600006.day", bars_600006.as_slice()),
            ("sh\\lday\\sh600004.day", bars_600004.as_slice()),
        ],
        &[("gbbq", events.as_slice())],
        &base_members(),
    );
    let source = make_source(&snap);
    let session = source.materialize_session();
    for symbol in ["sh.600006", "sh.600004"] {
        let single = source.materialize_daily(symbol).unwrap();
        let shared = session.materialize_daily(symbol).unwrap();
        assert_eq!(
            serde_json::to_value(&single).unwrap(),
            serde_json::to_value(&shared).unwrap(),
            "{symbol} diverged between single and shared paths"
        );
    }
    // Symbol isolation: 600006's chain sees exactly its own category-1
    // event; 600004 sees its own event plus one excluded non-category-1
    // record; neither sees the 000002 record.
    let six = session.materialize_daily("sh.600006").unwrap();
    let four = session.materialize_daily("sh.600004").unwrap();
    let reason = |b: &MarketDataBatch| b.adjustment_evidence.reason.to_string();
    assert!(
        reason(&six).contains("(1 events, 0 non-category-1"),
        "{}",
        reason(&six)
    );
    assert!(
        reason(&four).contains("(1 events, 1 non-category-1"),
        "{}",
        reason(&four)
    );
    // And the bars reflect only the symbol's own chain: 600004's
    // post-event bar equals its raw value (5.12 yuan), not a chain
    // polluted by 600006's or 000002's events.
    let bar_716 = four.bars.iter().find(|b| b.date == day(20250716)).unwrap();
    assert_eq!(bar_716.close, dec("5.12"));
}

#[test]
fn materialize_session_matches_single_path_on_retained_snapshot() {
    let Some(snap) = retained_snapshot() else {
        eprintln!("retained-inputs absent; session equivalence test skipped");
        return;
    };
    let source = make_source(&snap);
    let session = source.materialize_session();
    for symbol in ["sz.000002", "sh.600006", "sh.600004"] {
        let single = source
            .materialize_daily(symbol)
            .unwrap_or_else(|e| panic!("{symbol} single path: {e}"));
        let shared = session
            .materialize_daily(symbol)
            .unwrap_or_else(|e| panic!("{symbol} shared path: {e}"));
        assert_eq!(
            serde_json::to_value(&single).unwrap(),
            serde_json::to_value(&shared).unwrap(),
            "{symbol} diverged between single and shared paths"
        );
    }
}

#[test]
fn service_multi_item_collection_uses_shared_session() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(snap) = retained_snapshot() else {
        eprintln!("retained-inputs absent; multi-item e2e skipped");
        return;
    };
    let source = make_source(&snap);
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let manifest_path = db.path().join("manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    let _env = EnvReset::install(&manifest_path, &snap.root);
    let api = DataApi::new(tasks, "run-batch".into(), Access::local()).unwrap();
    // One accepted task with two securities runs through the worker's
    // shared-decode session; both items publish identical batches to the
    // single-symbol path.
    let accepted = api
        .call(
            "start_collection",
            json!({"request_id":"batch-1","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sz","code":"000002"},{"exchange":"sh","code":"600006"}]}}),
        )
        .unwrap();
    assert_eq!(accepted["item_ids"].as_array().unwrap().len(), 2);
    api.call("run_worker_once", json!({})).unwrap();
    let task = market.task(accepted["task_id"].as_str().unwrap()).unwrap();
    assert_eq!(task.items.len(), 2);
    for (item, symbol) in task.items.iter().zip(["sz.000002", "sh.600006"]) {
        assert!(
            item.outcome.as_ref().unwrap().success,
            "{symbol} item failed: {:?}",
            item.outcome
        );
        let head = market
            .effective_head(&Scope {
                symbol: symbol.into(),
                period: Period::Day,
            })
            .unwrap()
            .expect("published head");
        let published = market.fixed(&head).unwrap();
        let expected = source.materialize_daily(symbol).unwrap();
        assert_eq!(
            published.raw()["batch"],
            serde_json::to_value(&expected).unwrap(),
            "{symbol} published batch diverged from the single-symbol path"
        );
    }
}

// ---------- #247: TdxRepublishProvider + republish_data ----------

#[test]
fn republish_provider_enriches_unpromoted_fields() {
    use x_data_mcp::market_update::runner::RangeProvider;
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let (_d, _t, market) = fresh_market();
    let full = source.materialize_daily("sh.600006").unwrap();
    // Pre-#245 base shape: identical bars with the promoted fields still
    // unpromoted — the exact state republish exists to repair.
    let mut legacy = full.clone();
    for b in &mut legacy.bars {
        b.volume = None;
        b.amount = None;
        b.r7_raw = None;
    }
    let raw = subject_raw("base-legacy", &legacy);
    market::published_fixture(&market, &raw);
    let base = market.fixed("base-legacy").unwrap();
    let provider = TdxRepublishProvider {
        source: &source,
        published_at: now(),
    };
    let range = x_data_mcp::market_update::DateRange {
        start: full.bars[0].date,
        end: full.bars.last().unwrap().date,
    };
    let acq = provider.acquire(&base, &range, "cand-r1").unwrap();
    assert_eq!(acq.batch.bars, full.bars);
    // Republish carries no revision_ref: pure enrichment only — prepare
    // independently rejects any revised known value.
    assert!(acq.evidence.revision_ref.is_none());
    let whole = acq.combined.expect("changed republish needs a candidate");
    assert_eq!(whole.record["version_id"], json!("cand-r1"));
    let combined_batch: MarketDataBatch =
        serde_json::from_value(whole.record["batch"].clone()).unwrap();
    assert_eq!(combined_batch.bars, full.bars);
    assert_eq!(combined_batch.coverage.status, CoverageStatus::Complete);
    assert!(
        combined_batch
            .bars
            .iter()
            .all(|b| b.volume.is_some() && b.amount.is_some() && b.r7_raw.is_some())
    );
    // The built record passes the governed stage path.
    market.stage(&whole.record).unwrap();
    assert_eq!(
        market.candidate("cand-r1").unwrap().version().records,
        full.bars
    );

    // A known value the source contradicts is a basis conflict — fail
    // closed, never overwrite (enrichment is None→Some only). A flat
    // shifted bar stays inside low/high bounds while still contradicting
    // the source values.
    let mut conflict = legacy.clone();
    conflict.bars[1].open = dec("999.99");
    conflict.bars[1].high = dec("999.99");
    conflict.bars[1].low = dec("999.99");
    conflict.bars[1].close = dec("999.99");
    let raw = subject_raw("base-conflict", &conflict);
    market
        .commit(&market::prepare(
            &market,
            &raw,
            PublishIntent::Daily {
                expected_head: Some("base-legacy".into()),
            },
        ))
        .unwrap();
    let base = market.fixed("base-conflict").unwrap();
    assert!(matches!(
        provider.acquire(&base, &range, "cand-r2"),
        Err(SourceError::NotVerified(_))
    ));

    // A base that does not bind this snapshot generation fails closed.
    let mut other = legacy.clone();
    for i in other.provenance.snapshot_inputs.as_mut().unwrap() {
        i.member_sha256 = "0".repeat(64);
    }
    let raw = subject_raw("base-other-snapshot", &other);
    market
        .commit(&market::prepare(
            &market,
            &raw,
            PublishIntent::Daily {
                expected_head: Some("base-conflict".into()),
            },
        ))
        .unwrap();
    let base = market.fixed("base-other-snapshot").unwrap();
    assert!(matches!(
        provider.acquire(&base, &range, "cand-r3"),
        Err(SourceError::NotVerified(_))
    ));

    // An empty in-range segment cannot republish anything.
    let base = market.fixed("base-legacy").unwrap();
    let empty = x_data_mcp::market_update::DateRange {
        start: day(20300101),
        end: day(20300101),
    };
    assert!(matches!(
        provider.acquire(&base, &empty, "cand-r4"),
        Err(SourceError::InvalidResponse(_))
    ));
}

#[test]
fn service_republish_data_enriches_published_head() {
    use x_data_mcp::{
        data_supply::{Access, mcp::DataApi},
        mcp::ToolHandler,
    };
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let snap = valid_snapshot();
    let source = make_source(&snap);
    let full = source.materialize_daily("sh.600006").unwrap();
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    let mut legacy = full.clone();
    for b in &mut legacy.bars {
        b.volume = None;
        b.amount = None;
        b.r7_raw = None;
    }
    let subject = subject_raw("subject-legacy", &legacy);
    market::published_fixture(&market, &subject);

    let manifest_path = db.path().join("manifest.json");
    std::fs::write(&manifest_path, &snap.manifest).unwrap();
    let _env = EnvReset::install(&manifest_path, &snap.root);
    let api = DataApi::new(tasks.clone(), "run".into(), Access::local()).unwrap();
    let accepted = api
        .call(
            "republish_data",
            json!({"request_id":"rp-1","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"subject-legacy","start_date":"2025-07-15","end_date":"2025-07-17"}]}),
        )
        .unwrap();
    assert_eq!(
        accepted["contract_version"],
        json!("explicit-republish-task-v1")
    );
    let task_id = accepted["task_id"].as_str().unwrap().to_owned();
    api.call("run_worker_once", json!({})).unwrap();
    let done = tasks.read_task(&task_id).unwrap().unwrap();
    assert!(
        done.items[0].outcome.as_ref().unwrap().success,
        "republish item failed: {:?}",
        done.items[0].outcome
    );
    // Head advanced to the enriched version; the base stays fixed.
    let new_head: String = rusqlite::Connection::open(tasks.path())
        .unwrap()
        .query_row(
            "SELECT version_id FROM effective_heads WHERE symbol='sh.600006' AND period='day'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(new_head, "subject-legacy");
    let merged = market.fixed(&new_head).unwrap();
    assert_eq!(merged.version().records, full.bars);
    market.fixed("subject-legacy").unwrap();
    // range-result evidence rows bind the republished item.
    let c = rusqlite::Connection::open(tasks.path()).unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM market_update_results", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    // Frozen inputs keep the explicit republish shape.
    let snap_task = tasks.read_task(&task_id).unwrap().unwrap();
    let f = &snap_task.items[0].spec.frozen_inputs;
    assert_eq!(f["base_version_id"], json!("subject-legacy"));
    assert!(f["range"]["start"].is_string());

    // Default selection is not a republish surface.
    let sel = api.call(
        "republish_data",
        json!({"request_id":"rp-sel","dataset":"market.daily","contract_version":"v1","selection":{"kind":"default"}}),
    );
    assert!(sel.is_err(), "default selection must not resolve republish");

    // Yearly items never resolve a republish contract.
    let yearly = api.call(
        "republish_data",
        json!({"request_id":"rp-yearly","dataset":"market.yearly","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":new_head,"years":[2025],"publication":{"mode":"historical_fixed_base"}}]}),
    );
    assert!(yearly.is_err(), "market.yearly must not resolve republish");
}
