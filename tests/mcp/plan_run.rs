//! `plan_market_update` / `run_market_update` (generic-data-v7): the
//! digest-bound stateless plan workflow on the compact backend — digest
//! determinism, stale/in-flight refusal, request replay and end-to-end
//! execution through the existing worker.
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{io::Write, path::Path};
use x_data_mcp::{
    catalog::{
        CatalogSnapshot, CompletenessEvidence, Exchange, IndustryInfo, SecurityRecord, SecurityType,
    },
    data_supply::{Access, mcp::DataApi},
    mcp::ToolHandler,
    sqlite_catalog::SqliteCatalog,
    sqlite_market::backend::MarketBackend,
    sqlite_tasks::SqliteTasks,
};

fn sha(bytes: &[u8]) -> String {
    x_data_mcp::day_file::sha256(bytes)
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
    x_data_mcp::gbbq::encrypt_record(&clear)
}
fn gbbq_member(records: &[[u8; 29]]) -> Vec<u8> {
    let mut bytes = (records.len() as u32).to_le_bytes().to_vec();
    for r in records {
        bytes.extend_from_slice(r);
    }
    bytes
}
fn now() -> DateTime<Utc> {
    Utc::now()
}
/// `YYYYMMDD` for `days` before the Shanghai today.
fn day_u32(days: i64) -> u32 {
    (x_data_mcp::calendar::shanghai_date(Utc::now()) - chrono::Duration::days(days))
        .format("%Y%m%d")
        .to_string()
        .parse()
        .unwrap()
}
fn day_member(_code: &str, last_day: u32) -> Vec<u8> {
    [
        day_record(last_day - 2, [1128, 1130, 1125, 1129], 0.0, 100),
        day_record(last_day - 1, [1129, 1132, 1128, 1131], 0.0, 110),
        day_record(last_day, [1131, 1135, 1130, 1134], 0.0, 120),
    ]
    .concat()
}

/// Minimal inspected snapshot tree (three role-bound archives). `dir` holds
/// the generated zips and the returned manifest.
struct Snapshot {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    manifest: Vec<u8>,
}
fn snapshot(day_members: &[(&str, &[u8])]) -> Snapshot {
    let dir = tempfile::tempdir().unwrap();
    let gbbq = gbbq_member(&[gbbq_record(1, "600006", 20240101, 1)]);
    write_zip(
        &dir.path().join("base.zip"),
        &[("base.dbf", b"unused-by-daily-source".as_slice())],
    );
    write_zip(&dir.path().join("hsjday.zip"), day_members);
    write_zip(&dir.path().join("gbbq.zip"), &[("gbbq", gbbq.as_slice())]);
    let roles = [
        (
            "base_industry",
            "base.zip",
            "http://www.tdx.com.cn/products/data/data/dbf/base.zip",
        ),
        (
            "day",
            "hsjday.zip",
            "https://data.tdx.com.cn/vipdoc/hsjday.zip",
        ),
        (
            "gbbq",
            "gbbq.zip",
            "http://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
        ),
    ];
    let mut files = Vec::new();
    for (role, name, url) in roles {
        let bytes = std::fs::read(dir.path().join(name)).unwrap();
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

/// Env mutation is process-global; serialize all env-installed source tests.
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
}
impl Drop for EnvReset {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("X_DATA_MCP_OFFICIAL_MANIFEST");
            std::env::remove_var("X_DATA_MCP_OFFICIAL_DIR");
        }
    }
}

fn catalog_fixture(codes: &[&str], exchanges: &[&str]) -> CatalogSnapshot {
    let now = Utc::now();
    let source = "fixture:catalog".to_string();
    CatalogSnapshot {
        source: source.clone(),
        acquired_at: now,
        completeness: CompletenessEvidence::Complete {
            expected_count: codes.len(),
        },
        records: codes
            .iter()
            .zip(exchanges)
            .map(|(code, ex)| SecurityRecord {
                symbol: code.to_string(),
                name: "样例".into(),
                exchange: match *ex {
                    "sh" => Exchange::Shanghai,
                    _ => Exchange::Shenzhen,
                },
                security_type: SecurityType::AShare,
                source_security_id: format!("{ex}{code}"),
                industry: IndustryInfo::Missing {
                    reason: "fixture unknown".into(),
                },
                source: source.clone(),
                acquired_at: now,
            })
            .collect(),
    }
}
/// Compact-backend service root: catalog head + collected daily heads.
struct Rig {
    _db: tempfile::TempDir,
    api: DataApi,
    snapshot: Snapshot,
    tasks: SqliteTasks,
    backend: MarketBackend,
    seed_task: String,
}
/// Seed `codes` (sh.600006-style ("sh","600006") tuples) into a fresh
/// compact database: catalog fixture head, then `start_collection` +
/// `run_worker_once` so each collected entity has a real published head and
/// an execution-start marker.
fn rig(members: &[(&str, &str, u32)]) -> Rig {
    let day_members: Vec<(String, Vec<u8>)> = members
        .iter()
        .map(|(ex, code, last)| {
            (
                format!("{ex}\\lday\\{ex}{code}.day"),
                day_member(code, *last),
            )
        })
        .collect();
    let refs: Vec<(&str, &[u8])> = day_members
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let snap = snapshot(&refs);
    let source =
        x_data_mcp::official_market::TdxMarketSource::inspect(&snap.root, &snap.manifest, now())
            .unwrap();
    let db = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(db.path().join("m.sqlite"));
    tasks.initialize_compact().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(tasks.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let backend = MarketBackend::open(&tasks).unwrap();
    SqliteCatalog::new(tasks.clone())
        .publish_fixture(
            "catalog-1",
            catalog_fixture(
                &members.iter().map(|m| m.1).collect::<Vec<_>>(),
                &members.iter().map(|m| m.0).collect::<Vec<_>>(),
            ),
            None,
        )
        .unwrap();
    let api = DataApi::new_with_backend(
        tasks.clone(),
        backend.clone(),
        "run".into(),
        Access::local(),
        None,
    )
    .unwrap()
    .with_market_source(source.clone());
    let securities: Vec<Value> = members
        .iter()
        .map(|(ex, code, _)| json!({"exchange":ex,"code":code}))
        .collect();
    let accepted = api
        .call(
            "start_collection",
            json!({"request_id":"seed","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":securities}}),
        )
        .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    Rig {
        api,
        snapshot: snap,
        tasks,
        backend,
        seed_task: accepted["task_id"].as_str().unwrap().to_string(),
        _db: db,
    }
}
fn plan(api: &DataApi, dataset: &str) -> Value {
    api.call(
        "plan_market_update",
        json!({"dataset":dataset,"contract_version":"v1"}),
    )
    .unwrap()
}
fn run(api: &DataApi, dataset: &str, request_id: &str, view: &Value) -> Value {
    api.call(
        "run_market_update",
        json!({"dataset":dataset,"contract_version":"v1","request_id":request_id,"target_day":view["target_day"],"plan_id":view["plan_id"]}),
    )
    .unwrap()
}

#[test]
fn daily_plan_is_deterministic_and_runs_end_to_end() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1)), ("sz", "000002", day_u32(1))]);
    let p1 = plan(&r.api, "market.daily");
    assert_eq!(p1["counts"]["eligible"], 2);
    assert_eq!(p1["counts"]["absent"], 0);
    assert_eq!(p1["counts"]["already_current"], 0);
    assert_eq!(p1["plan_id"].as_str().unwrap().len(), 64);
    assert_eq!(p1["generation_ref"], Value::Null);
    // Deterministic: the same state resolves the identical digest.
    assert_eq!(plan(&r.api, "market.daily")["plan_id"], p1["plan_id"]);

    let accepted = run(&r.api, "market.daily", "run-1", &p1);
    assert_eq!(accepted["reused"], false);
    assert_eq!(accepted["item_count"], 2);
    assert_eq!(accepted["plan_id"], p1["plan_id"]);
    assert_eq!(accepted["contract_version"], "market-plan-run-task-v1");
    let task_id = accepted["task_id"].clone();

    // A second run while the first task is non-terminal refuses.
    assert_eq!(
        r.api
            .call(
                "run_market_update",
                json!({"dataset":"market.daily","contract_version":"v1","request_id":"run-2","target_day":p1["target_day"],"plan_id":p1["plan_id"]}),
            )
            .unwrap_err()
            .code,
        "plan_in_progress"
    );

    // Progress exposes plan_id/counts before execution starts.
    let progress = r
        .api
        .call("get_task_progress", json!({"task_id":task_id}))
        .unwrap();
    assert_eq!(progress["task"]["plan_id"], p1["plan_id"]);
    assert_eq!(progress["task"]["plan_counts"]["eligible"], 2);

    r.api.call("run_worker_once", json!({})).unwrap();
    let done = r
        .api
        .call(
            "get_task",
            json!({"contract_version":"v1","task_id":task_id}),
        )
        .unwrap();
    assert_eq!(done["status"], "succeeded", "{done}");
    let progress = r
        .api
        .call("get_task_progress", json!({"task_id":task_id}))
        .unwrap();
    assert_eq!(progress["finished"], 2);

    // Request replay returns the original acceptance — staleness does not
    // override the request binding.
    let replay = run(&r.api, "market.daily", "run-1", &p1);
    assert_eq!(replay["reused"], true);
    assert_eq!(replay["task_id"], task_id);

    // Same state, same digest — no_change commits do not move heads, so the
    // plan is unchanged after a clean no-op run.
    let p2 = plan(&r.api, "market.daily");
    assert_eq!(p2["plan_id"], p1["plan_id"]);
    assert_eq!(p2["counts"]["eligible"], 2);

    // A foreign digest is a stale refusal, never a partial run.
    assert_eq!(
        r.api
            .call(
                "run_market_update",
                json!({"dataset":"market.daily","contract_version":"v1","request_id":"run-3","target_day":p1["target_day"],"plan_id":"0000000000000000000000000000000000000000000000000000000000000001"}),
            )
            .unwrap_err()
            .code,
        "plan_stale"
    );
    // Malformed plan ids fail before any state read.
    assert_eq!(
        r.api
            .call(
                "run_market_update",
                json!({"dataset":"market.daily","contract_version":"v1","request_id":"run-4","target_day":p1["target_day"],"plan_id":"bogus"}),
            )
            .unwrap_err()
            .code,
        "invalid_plan"
    );
}

#[test]
fn daily_plan_shifts_to_already_current_when_heads_reach_target() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    // Target before the base last_day: the entity is already current.
    let past =
        (chrono::NaiveDate::parse_from_str(&day_u32(1).to_string(), "%Y%m%d").unwrap()).to_string();
    let view = r
        .api
        .call(
            "plan_market_update",
            json!({"dataset":"market.daily","contract_version":"v1","end_date":past}),
        )
        .unwrap();
    assert_eq!(view["counts"]["already_current"], 1);
    assert_eq!(view["counts"]["eligible"], 0);
    assert_ne!(view["plan_id"], plan(&r.api, "market.daily")["plan_id"]);
    // An empty plan refuses at run.
    assert_eq!(
        r.api
            .call(
                "run_market_update",
                json!({"dataset":"market.daily","contract_version":"v1","request_id":"run-empty","target_day":view["target_day"],"plan_id":view["plan_id"]}),
            )
            .unwrap_err()
            .code,
        "invalid_plan"
    );
}

#[test]
fn yearly_plan_derives_and_daily_inflight_blocks_yearly_run() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1)), ("sz", "000002", day_u32(1))]);
    // Yearly needs the env-installed source for derivation verification.
    let manifest_path = r._db.path().join("snapshot-manifest.json");
    std::fs::write(&manifest_path, &r.snapshot.manifest).unwrap();
    let _env = EnvReset::install(&manifest_path, &r.snapshot.root);

    let py = plan(&r.api, "market.yearly");
    assert_eq!(py["counts"]["eligible"], 2, "{py}");

    // Daily plan + run accepted but not yet executed: yearly run refuses.
    let pd = plan(&r.api, "market.daily");
    let _daily_run = run(&r.api, "market.daily", "run-d", &pd);
    assert_eq!(
        r.api
            .call(
                "run_market_update",
                json!({"dataset":"market.yearly","contract_version":"v1","request_id":"run-y","target_day":py["target_day"],"plan_id":py["plan_id"]}),
            )
            .unwrap_err()
            .code,
        "daily_update_inflight"
    );
    r.api.call("run_worker_once", json!({})).unwrap();

    let accepted = run(&r.api, "market.yearly", "run-y", &py);
    assert_eq!(accepted["item_count"], 2);
    r.api.call("run_worker_once", json!({})).unwrap();
    let done = r
        .api
        .call(
            "get_task",
            json!({"contract_version":"v1","task_id":accepted["task_id"]}),
        )
        .unwrap();
    assert_eq!(done["status"], "succeeded", "{done}");
    // After derivation the yearly heads are fresh: the plan re-classifies.
    let py2 = plan(&r.api, "market.yearly");
    assert_eq!(py2["counts"]["already_current"], 2, "{py2}");
    assert_eq!(py2["counts"]["eligible"], 0);
    assert_ne!(py2["plan_id"], py["plan_id"]);
}

/// A-share-scale plan timing (TASK-DEVIN-IMPL-PLAN-RUN-1: 5k synthetic
/// entities): phantom catalog members carry execution-start markers seeded
/// straight into `task_item_execution_starts` (the selection layer only
/// reads the marker identity + task command key); 20 real members exercise
/// the head-reading eligible path. Measured numbers land in test output;
/// the assertions stay loose so slow CI hosts do not flake.
#[test]
fn plan_and_run_recompute_scale_to_full_catalog() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // 20 real members, heads pinned one day behind today.
    let real: Vec<(&str, String, u32)> = (10..30)
        .map(|i| ("sh", format!("600{i:03}"), day_u32(1)))
        .collect();
    let real_refs: Vec<(&str, &str, u32)> = real.iter().map(|m| (m.0, m.1.as_str(), m.2)).collect();
    let r = rig(&real_refs);

    // 4,980 phantom members: catalog records + task_items + markers on the
    // already-terminal seed task, inserted in one transaction.
    let mut phantom: Vec<String> = Vec::new();
    for i in 10_000..14_980 {
        phantom.push(format!("sz.00{i}"));
    }
    // catalog-2 carries every member: the real sh.6000xx keep their heads,
    // the phantom sz.00xxxx are started-but-absent.
    let mut codes: Vec<&str> = real.iter().map(|m| m.1.as_str()).collect();
    codes.extend(phantom.iter().map(|s| s.strip_prefix("sz.").unwrap()));
    let mut exchanges = vec!["sh"; real.len()];
    exchanges.extend(std::iter::repeat_n("sz", phantom.len()));
    SqliteCatalog::new(r.tasks.clone())
        .publish_fixture(
            "catalog-2",
            catalog_fixture(&codes, &exchanges),
            Some("catalog-1"),
        )
        .unwrap();
    let mut c = rusqlite::Connection::open(r.tasks.path()).unwrap();
    let tx = c.transaction().unwrap();
    for sym in &phantom {
        let item = serde_json::to_string(&json!(["market", sym, "day"])).unwrap();
        tx.execute(
            "INSERT INTO task_items(task_id,item_id,target_json,progress_unit,frozen_inputs_json,scope_keys_json,stage) \
             VALUES(?1,?2,?3,'entity','{}','[]','pending')",
            rusqlite::params![r.seed_task, item, format!("{{\"symbol\":\"{sym}\",\"period\":\"day\"}}")],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO task_item_execution_starts VALUES(?1,?2,'perf-seed',1,'2026-01-01T00:00:00Z')",
            rusqlite::params![r.seed_task, item],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(c);

    let t0 = std::time::Instant::now();
    let p = plan(&r.api, "market.daily");
    let plan_ms = t0.elapsed().as_millis();
    assert_eq!(p["counts"]["eligible"], 20, "{p}");
    assert_eq!(p["counts"]["absent"], phantom.len(), "{p}");

    let t1 = std::time::Instant::now();
    let accepted = run(&r.api, "market.daily", "run-perf", &p);
    let run_ms = t1.elapsed().as_millis();
    assert_eq!(accepted["item_count"], 20);
    eprintln!(
        "5k-member plan resolve {plan_ms}ms, run recompute+accept {run_ms}ms (eligible 20 / absent {})",
        phantom.len()
    );
    assert!(plan_ms < 30_000, "plan resolve took {plan_ms}ms");
    assert!(run_ms < 60_000, "run recompute+accept took {run_ms}ms");
}

fn status_kb(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}
fn vmrss_kb() -> u64 {
    status_kb("VmRSS:")
}
fn vmhwm_kb() -> u64 {
    status_kb("VmHWM:")
}

/// ~5.2k entities each carrying a REAL published head (no phantom markers):
/// the batch collection commit — the same mechanism the series-commit suite
/// seeds with — writes a real checkpoint version + head per member, then
/// `plan_market_update` and `run_market_update` acceptance are measured for
/// wall time and process VmHWM/VmRSS deltas. This is the scale at which the
/// bulk acceptance path peaked ~950MB (TASK-DEVIN-PLANRUN-MEMORY).
#[test]
fn planrun_scales_to_market_wide_membership() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    const EXTRA: usize = 5_200;
    let mut codes = vec!["600006".to_string()];
    let mut symbols = vec!["sh.600006".to_string()];
    let mut extra = Vec::with_capacity(EXTRA);
    for i in 0..EXTRA {
        let code = format!("{:06}", 700_000 + i);
        extra.push(format!("sh.{code}"));
        symbols.push(format!("sh.{code}"));
        codes.push(code);
    }
    let catalog = SqliteCatalog::new(r.tasks.clone());
    catalog
        .publish_fixture(
            "catalog-2",
            catalog_fixture(
                &codes.iter().map(String::as_str).collect::<Vec<_>>(),
                &vec!["sh"; codes.len()],
            ),
            Some("catalog-1"),
        )
        .unwrap();
    use x_data_mcp::official_market::DailyCollectionRepository;
    use x_data_mcp::task_control::{
        Command, CommandKey, FrozenWork, RequestOutcome, TaskRepository,
    };
    // Reuse the rig's live backend handle — `MarketBackend::open` cannot
    // take a second lease on the same database file.
    let backend = r.backend.clone();
    use x_data_mcp::market_data::{
        AdjustmentEvidence, CoverageEvidence, EvidenceValue, Period, UnitCurrencyEvidence,
    };
    let bar = |day| {
        x_data_mcp::compact_market::CompactBar {
            trade_day: day,
            open_cents: 1000,
            high_cents: 1010,
            low_cents: 990,
            close_cents: 1005,
            volume: Some(100),
            amount: Some("1000".into()),
            r7_raw: Some(7),
        }
        .to_bar(Period::Day)
        .unwrap()
    };
    let units = || UnitCurrencyEvidence {
        price_currency: EvidenceValue::confirmed("CNY", "bound"),
        price_unit: EvidenceValue::confirmed("share", "bound"),
        volume_unit: EvidenceValue::confirmed("share", "bound"),
        amount_currency: EvidenceValue::confirmed("CNY", "bound"),
        amount_unit: EvidenceValue::confirmed("yuan", "bound"),
    };
    // Seed real heads in ~200-item tasks — a single 5.2k-item task makes
    // per-item `decide` scans quadratic — and finish each task so its
    // scopes release before the plan resolution runs.
    for (batch, chunk) in extra.chunks(200).enumerate() {
        let mut scopes = std::collections::BTreeSet::new();
        let mut specs = Vec::with_capacity(chunk.len());
        for sym in chunk {
            let scope = x_data_mcp::task_control::legacy::scope_id(sym, "day");
            scopes.insert(scope.clone());
            specs.push(x_data_mcp::task_control::ItemSpec {
                item_id: scope.clone(),
                scope_keys: [scope].into(),
                target: json!({"symbol": sym, "period": "day"}),
                progress_unit: "security_period".into(),
                frozen_inputs: json!({}),
            });
        }
        let seed = r
            .tasks
            .accept(
                &format!("req-seed-{batch}"),
                &Command {
                    key: CommandKey {
                        operation: "start_collection".into(),
                        dataset: "market.daily".into(),
                        // family() in default_selection only maps
                        // ("market.daily","v1") to the typed family —
                        // other contract versions leave the execution-
                        // start markers out of the daily universe.
                        contract_version: "v1".into(),
                    },
                    intent: json!({}),
                },
                &FrozenWork {
                    items: specs,
                    scopes,
                    shared_evidence: None,
                },
                "run-seed",
                None,
            )
            .unwrap();
        let RequestOutcome::Task(snap) = seed.record.outcome else {
            panic!("seed acceptance must bind a task")
        };
        let seed_task = snap.task.task_id;
        let mut candidates = Vec::with_capacity(chunk.len());
        for (i, sym) in chunk.iter().enumerate() {
            candidates.push(x_data_mcp::official_market::CollectionCandidate {
                task_id: seed_task.clone(),
                item_id: x_data_mcp::task_control::legacy::scope_id(sym, "day"),
                publication_id: format!("scale-col-{batch}-{i}"),
                version_id: format!("scale-v-{batch}-{i}"),
                run_id: "run-seed".into(),
                symbol: sym.clone(),
                source: "official".into(),
                // Head day 1 — same lag as the rig seed so every member is
                // eligible for the pending-day plan (day-2 heads classify
                // `already_current` and would fall out of `items`).
                bars: vec![bar(1)],
                coverage: CoverageEvidence::complete("seed"),
                unit_currency: units(),
                adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
                published_at: Utc::now(),
                generation_id: "gen-1".into(),
                generation_manifest_sha256: "0b".repeat(32),
                day_member_sha256: "0c".repeat(32),
                security_event_sha256: "0e".repeat(32),
                generation_evidence: json!({
                    "generation_id": "gen-1",
                    "snapshot_inputs": [{
                        "role": "daily",
                        "member": format!("{sym}.day"),
                        "archive_sha256": "12".repeat(32),
                        "member_sha256": "cd".repeat(32),
                    }],
                }),
            });
        }
        for res in backend
            .commit_collection_batch(&seed_task, &candidates)
            .unwrap()
            .unwrap()
        {
            assert!(res.is_ok(), "seed collection commit failed: {res:?}");
        }
        // `finish_item_and_task` drives the last item's task to a terminal
        // status and releases its scope occupancy inside the same tx — no
        // explicit FinishExecution (decide rejects it on terminal tasks).
        let task = r.tasks.read_task(&seed_task).unwrap().unwrap();
        assert!(
            task.task.status.terminal(),
            "seed task must be terminal after its batch: {:?}",
            task.task.status
        );
    }

    // 5,201 eligible members (head day 1 << pending day) + 0 absent.
    let rss0 = vmrss_kb();
    let hwm0 = vmhwm_kb();
    let t0 = std::time::Instant::now();
    let p = plan(&r.api, "market.daily");
    let plan_ms = t0.elapsed().as_millis();
    let hwm_after_plan = vmhwm_kb();
    let plan_hwm_delta = hwm_after_plan.saturating_sub(hwm0);
    let t1 = std::time::Instant::now();
    let accepted = run(&r.api, "market.daily", "req-scale", &p);
    let run_ms = t1.elapsed().as_millis();
    let run_peak_delta = vmhwm_kb().saturating_sub(hwm_after_plan);
    eprintln!(
        "plan-run scale: {} members; plan {plan_ms}ms peak +{plan_hwm_delta}kB; run-accept {run_ms}ms peak +{run_peak_delta}kB; rss {rss0}→{}kB",
        symbols.len(),
        vmrss_kb()
    );
    assert_eq!(p["counts"]["eligible"], symbols.len() as u64);
    assert_eq!(accepted["reused"], false);
    assert_eq!(accepted["item_count"], symbols.len() as u64);
    assert_eq!(accepted["plan_id"], p["plan_id"]);
    // Memory: acceptance must stay far below the production 1.9GB host —
    // the pre-refactor path peaked ~950MB here; the target is <200MB per
    // call (GOAL recorded in TASK-DEVIN-PLANRUN-MEMORY §2).
    let peak = plan_hwm_delta.max(run_peak_delta);
    assert!(
        peak < 200 * 1024,
        "peak RSS delta {peak}kB exceeds the 200MB plan-run budget"
    );
    assert!(plan_ms < 5_000, "plan took {plan_ms}ms > 5s");
    assert!(run_ms < 10_000, "run acceptance took {run_ms}ms > 10s");
}
