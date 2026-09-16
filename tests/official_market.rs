//! `TdxMarketSource` / `TdxDailyRunner` — official-file snapshot source for
//! market.daily collection. Synthetic fixtures cover fail-closed behavior;
//! the retained gen-2 snapshot (when present) covers the end-to-end oracle.
use chrono::{DateTime, Datelike, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{io::Write, path::Path, str::FromStr};
use x_data_mcp::{
    data_supply::{collect::CollectionContract, wire::Dataset},
    gbbq,
    market_data::{Adjustment, CoverageStatus, Period, SourceError},
    market_storage::{MarketRepository, Range, Scope},
    market_update::runner::ItemExecution,
    official_market::{DailyCollectionRepository, TdxDailyRunner, TdxMarketSource},
    sqlite_market::SqliteMarket,
    sqlite_supply::SqliteSupply,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Contract, RequestOutcome, Stage, Status, legacy::LegacyCollection, service::TaskControl,
    },
};

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

    // Service-level yearly stays gated: the worker's OfficialYearlyVerifier
    // is unimplemented, so an update_data/market.yearly task fails closed.
    let gated = api
        .call(
            "update_data",
            json!({"dataset":"market.yearly","request_id":"yr-gated","contract_version":"v1","items":[{"entity":{"exchange":"sz","code":"000002"},"base_daily_version_id":head,"years":[2000],"publication":{"mode":"current_head","expected_yearly_head":null}}]}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    let gated_task = market.task(gated["task_id"].as_str().unwrap()).unwrap();
    let gated_error = gated_task.items[0]
        .outcome
        .as_ref()
        .unwrap()
        .error
        .as_deref()
        .unwrap();
    assert!(
        gated_error.contains("SOURCE_NOT_VERIFIED")
            && gated_error.contains("LOCAL_PIPELINE_NOT_IMPLEMENTED"),
        "expected the yearly verifier gate, got {gated_error:?}"
    );

    // The derivation chain itself is exercised through the same runner the
    // worker uses, with the test-injected verifier that existing
    // yearly_publication tests rely on (the trait exists for injection; the
    // official verifier above is untouched).
    use std::collections::BTreeSet;
    use x_data_mcp::{
        market_data::CoverageEvidence,
        market_storage::compat::CheckedRecord,
        yearly_publication::{
            DerivationEvidence,
            runner::{YearlyExecution, YearlyRunner, YearlyVerifier},
        },
    };
    struct FixtureVerifier;
    impl YearlyVerifier for FixtureVerifier {
        fn verify(
            &self,
            base: &CheckedRecord,
            years: &BTreeSet<i32>,
            at: DateTime<Utc>,
        ) -> Result<DerivationEvidence, SourceError> {
            Ok(DerivationEvidence {
                base_integrity_sha256: base.raw()["integrity_sha256"].as_str().unwrap().into(),
                years: years.clone(),
                as_of: at
                    .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
                    .date_naive(),
                coverage: CoverageEvidence {
                    status: CoverageStatus::Complete,
                    reason: "FIXTURE: derived from snapshot-published daily; not listing history"
                        .into(),
                },
            })
        }
    }
    let derived = api
        .call(
            "update_data",
            json!({"dataset":"market.yearly","request_id":"yr-1","contract_version":"v1","items":[{"entity":{"exchange":"sz","code":"000002"},"base_daily_version_id":head,"years":[2000],"publication":{"mode":"current_head","expected_yearly_head":null}}]}),
        )
        .unwrap();
    let task_id = derived["task_id"].as_str().unwrap();
    let item_id = market.task(task_id).unwrap().items[0].spec.item_id.clone();
    // derived_at determines the verification as-of; the retained .day member
    // ends 2026-09-14, so pick a later instant or the aggregation's
    // bar-date<=as_of rule fails closed as designed.
    let runner = YearlyRunner {
        repository: market.clone(),
        run_id: "run-env".into(),
        derived_at: "2026-09-15T00:00:00Z".parse().unwrap(),
    };
    let YearlyExecution::Published(receipt) = runner
        .run_item(task_id, &item_id, &FixtureVerifier)
        .unwrap()
    else {
        let task = market.task(task_id).unwrap();
        panic!(
            "expected yearly publication; outcome: {:?}",
            task.items[0].outcome
        )
    };
    let record = market.fixed(&receipt.version_id).unwrap();
    let records = record.raw()["local_yearly_derivation_v1"]["annual"]["records"]
        .as_array()
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        chrono::NaiveDate::parse_from_str(records[0]["bar"]["date"].as_str().unwrap(), "%Y-%m-%d")
            .unwrap()
            .year(),
        2000
    );
    assert_eq!(
        record.raw()["local_yearly_derivation_v1"]["annual"]["records"][0]["lineage"]["base_daily_version_id"],
        json!(head)
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
