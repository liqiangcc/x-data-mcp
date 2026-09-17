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
    official_market::{DailyCollectionRepository, TdxDailyRunner, TdxGapVerifier, TdxMarketSource},
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
    assert!(
        batch
            .bars
            .iter()
            .all(|b| b.volume.is_none() && b.amount.is_none())
    );
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
