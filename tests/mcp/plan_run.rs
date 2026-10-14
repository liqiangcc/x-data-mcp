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
/// Counts `UPDATE tasks` statements on the compact writer connection —
/// the regression signature of PLANRUN-COMMIT-COST (the pre-fix path
/// wrote the `frozen_work_json`-sized task row ~2× per item).
static TASK_ROW_WRITES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn task_write_probe(ctx: rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization {
    if let rusqlite::hooks::AuthAction::Update {
        table_name,
        column_name,
    } = ctx.action
        && table_name == "tasks"
        && column_name == "state_revision"
    {
        TASK_ROW_WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    rusqlite::hooks::Authorization::Allow
}
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

/// Seed real published daily heads for `symbols` through the batch
/// collection commit in ~200-item tasks — a single 5.2k-item task makes
/// per-item `decide` scans quadratic — and finish each task so its
/// scopes release before the plan resolution runs.
fn seed_daily_heads(r: &Rig, symbols: &[String]) {
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
    for (batch, chunk) in symbols.chunks(200).enumerate() {
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
    seed_daily_heads(&r, &extra);

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

    let task_id = accepted["task_id"].as_str().unwrap().to_string();
    let item_ids: Vec<String> = r
        .tasks
        .read_task(&task_id)
        .unwrap()
        .unwrap()
        .items
        .iter()
        .map(|i| i.spec.item_id.clone())
        .collect();
    // Bounded reads (PLANRUN-BOUNDED-READS): a >100-item task must be
    // served from head columns plus keyset item pages — response bytes
    // and process HWM must not scale with the item count.
    let hwm0 = vmhwm_kb();
    let view = r
        .api
        .call(
            "get_task",
            json!({"contract_version":"v1","task_id":task_id}),
        )
        .unwrap();
    assert_eq!(view["task_id"], task_id);
    assert_eq!(view["plan_id"], p["plan_id"]);
    assert!(
        serde_json::to_vec(&view).unwrap().len() < 2 * 1024,
        "get_task response must stay small for large tasks"
    );
    let get_task_delta = vmhwm_kb().saturating_sub(hwm0);
    assert!(
        get_task_delta < 10 * 1024,
        "get_task HWM delta {get_task_delta}kB exceeds the 10MB budget"
    );
    let page = r.api.call("list_tasks", json!({"page_size":1000})).unwrap();
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["task_id"] == task_id)
    );
    let list_delta = vmhwm_kb().saturating_sub(hwm0);
    assert!(
        list_delta < 10 * 1024,
        "list_tasks HWM delta {list_delta}kB exceeds the 10MB budget"
    );
    // Keyset pagination over unfinished items: every item appears
    // exactly once, each page stays <50KB, per-item arrays are empty and
    // the plan summary rides along.
    let mut seen = std::collections::BTreeSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..64 {
        let mut args = json!({"task_id":task_id,"page_size":100});
        if let Some(c) = cursor.take() {
            args["cursor"] = json!(c);
        }
        let page = r.api.call("get_task_progress", args).unwrap();
        let bytes = serde_json::to_vec(&page).unwrap().len();
        assert!(bytes < 50 * 1024, "progress page {bytes}B exceeds 50KB");
        assert_eq!(page["total"], symbols.len() as u64);
        assert_eq!(page["plan"]["plan_id"], p["plan_id"]);
        assert_eq!(page["plan"]["generation_id"], p["generation_ref"]);
        assert_eq!(
            page["range_results"],
            json!([]),
            "large-task pages never carry per-item result arrays"
        );
        for item in page["unfinished"].as_array().unwrap() {
            assert!(
                seen.insert(item["item_id"].as_str().unwrap().to_string()),
                "unfinished item repeated across pages"
            );
        }
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(seen.len(), symbols.len(), "every item paged exactly once");
    eprintln!(
        "plan-run reads: get_task +{get_task_delta}kB, list_tasks +{list_delta}kB, {} progress pages",
        52
    );

    // Execution phase (TASK-DEVIN-PLANRUN-CHUNK-EXEC): the accepted task
    // must run to a terminal state with bounded memory — chunked at
    // EXEC_CHUNK=100 so a 5,201-item task peaks like a ≤100-item task
    // (the unchunked prepare_batch peaked ~969MB on the private stack).
    assert_eq!(item_ids.len(), symbols.len());
    let exec_hwm0 = vmhwm_kb();
    TASK_ROW_WRITES.store(0, std::sync::atomic::Ordering::Relaxed);
    r.backend
        .compact_writer_authorizer(Some(task_write_probe))
        .unwrap();
    let t2 = std::time::Instant::now();
    let provider = UnchangedProvider {
        typed_calls: std::cell::Cell::new(0),
    };
    let runner = x_data_mcp::market_update::runner::RangeRunner {
        repository: r.backend.clone(),
        run_id: "run".into(),
    };
    runner
        .run_items(&task_id, &item_ids, &provider)
        .unwrap()
        .expect("compact backend carries the update batch");
    let exec_ms = t2.elapsed().as_millis();
    r.backend.compact_writer_authorizer(None).unwrap();
    let task_row_writes = TASK_ROW_WRITES.load(std::sync::atomic::Ordering::Relaxed);
    // O(chunk) task-row writes: claim + one head persist per 100-item
    // chunk. The pre-fix path ran ~2×items ≈ 10,400 UPDATEs on the
    // multi-MB `frozen_work_json` row (≈490ms/item private-stack).
    let chunks = item_ids.len().div_ceil(100);
    assert!(
        task_row_writes <= 2 * chunks + 8,
        "tasks-row updates {task_row_writes} must scale with chunks, not items"
    );
    assert!(
        exec_ms < 600_000,
        "5,201-item write loop {exec_ms}ms exceeds the 600s hard gate"
    );
    let exec_peak_delta = vmhwm_kb().saturating_sub(exec_hwm0);
    eprintln!(
        "plan-run exec: {} items in {exec_ms}ms; task-row writes {task_row_writes} ({chunks} chunks); exec peak +{exec_peak_delta}kB; rss {}kB",
        item_ids.len(),
        vmrss_kb()
    );
    let task = r.tasks.read_task(&task_id).unwrap().unwrap();
    assert!(
        task.task.status.terminal(),
        "task must reach a terminal state: {:?}",
        task.task.status
    );
    assert!(
        task.items
            .iter()
            .all(|i| i.outcome.as_ref().is_some_and(|o| o.success)),
        "every item must record a successful outcome"
    );
    assert!(
        exec_peak_delta < 200 * 1024,
        "execution peak RSS delta {exec_peak_delta}kB exceeds the 200MB budget"
    );
}

/// Retry of a plan-run task must keep the shared-evidence binding: the
/// item evidence markers (`selection_evidence.evidence_digest` = plan_id)
/// validate against `FrozenWork::validate` only when the rebuilt work
/// carries the same `shared_evidence`. Before the fix `service.retry`
/// rebuilt the work with `shared_evidence: None` and the validate step
/// rejected every marker-stamped task as "item evidence marker
/// mismatches shared evidence" (owner observed on the private stack).
#[test]
fn run_task_retry_keeps_shared_evidence_binding() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    use x_data_mcp::task_control::{Change, ItemOutcome, TaskRepository};
    let r = rig(&[("sh", "600006", day_u32(1))]);
    let p = plan(&r.api, "market.daily");
    let accepted = run(&r.api, "market.daily", "run-retryable", &p);
    let task_id = accepted["task_id"].as_str().unwrap().to_string();
    let original = r.tasks.read_task(&task_id).unwrap().unwrap();
    assert_eq!(original.items.len(), 1);
    let item_id = original.items[0].spec.item_id.clone();
    assert_eq!(
        original.items[0].spec.frozen_inputs["selection_evidence"]["evidence_digest"], p["plan_id"],
        "run item must carry the plan-id evidence marker"
    );
    // Drive the task to a terminal `failed` state under the service run
    // so its scopes release and the item is retry-selectable.
    let mut rev = original.task.state_revision;
    for change in [
        Change::Claim,
        Change::StartItem(item_id.clone()),
        Change::FinishItem(
            item_id.clone(),
            ItemOutcome {
                success: false,
                error: Some("simulated item failure".into()),
                evidence: None,
            },
        ),
        Change::FinishExecution,
    ] {
        rev = r
            .tasks
            .transition(&task_id, rev, "run", change)
            .unwrap()
            .task
            .state_revision;
    }
    assert_eq!(
        r.tasks.read_task(&task_id).unwrap().unwrap().task.status,
        x_data_mcp::task_control::Status::Failed
    );
    let retried = r
        .api
        .call(
            "retry_task",
            json!({"contract_version":"v1","original_task_id":task_id,"new_request_id":"retry-1","item_ids":[item_id]}),
        )
        .unwrap();
    assert_eq!(retried["reused"], false);
    let retry_task = r
        .tasks
        .read_task(retried["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    // The retry task's frozen work carries the original shared evidence —
    // the item markers validate against it instead of mismatching None.
    assert_eq!(
        retry_task.task.work.shared_evidence,
        original.task.work.shared_evidence
    );
    assert_eq!(
        retry_task.items[0].spec.frozen_inputs["selection_evidence"]["evidence_digest"],
        p["plan_id"]
    );
    // Explicit tasks (shared_evidence: None) keep identical behavior —
    // the rig's start_collection seed task is already terminal with every
    // item succeeded, so its single selectable-item retry check lands on
    // the same code path and must not gain evidence.
    let seed = r.tasks.read_task(&r.seed_task).unwrap().unwrap();
    assert!(seed.task.status.terminal());
    assert!(seed.task.work.shared_evidence.is_none());
}

// ---------------------------------------------------------------------------
// Execution-phase chunking (TASK-DEVIN-PLANRUN-CHUNK-EXEC): run_items must
// bound prepare/commit working sets to ~100 items regardless of task size.
// ---------------------------------------------------------------------------
use x_data_mcp::market_storage::MarketRepository as _;
use x_data_mcp::market_update::publication::RangeRepository as _;
use x_data_mcp::task_control::TaskRepository as _;

/// Typed acquisition that reports the base unchanged: enough to drive the
/// real prepare/batch-commit machinery on the compact backend while
/// keeping every receipt a NoChange (version reuse, no new bars).
struct UnchangedProvider {
    typed_calls: std::cell::Cell<usize>,
}
impl x_data_mcp::market_update::runner::RangeProvider for UnchangedProvider {
    fn acquire(
        &self,
        _base: &x_data_mcp::market_storage::compat::CheckedRecord,
        _range: &x_data_mcp::market_update::DateRange,
        _candidate_id: &str,
    ) -> Result<x_data_mcp::market_update::runner::Acquisition, x_data_mcp::market_data::SourceError>
    {
        Err(x_data_mcp::market_data::SourceError::NotVerified(
            "fixture: envelope path unused".into(),
        ))
    }
    fn acquire_typed(
        &self,
        facts: &x_data_mcp::market_update::UpdateBaseFacts,
        _base_window: &[x_data_mcp::market_data::Bar],
        range: &x_data_mcp::market_update::DateRange,
    ) -> Result<
        Option<x_data_mcp::market_update::runner::TypedAcquisition>,
        x_data_mcp::market_data::SourceError,
    > {
        self.typed_calls.set(self.typed_calls.get() + 1);
        Ok(Some(x_data_mcp::market_update::runner::TypedAcquisition {
            decision: x_data_mcp::market_update::runner::TypedDecision::Unchanged,
            evidence: x_data_mcp::market_update::Evidence {
                base_version_id: facts.version_id.clone(),
                base_integrity_sha256: facts.integrity_sha256.clone(),
                candidate_sha256: "fixture:candidate".into(),
                range: range.clone(),
                same_qfq_basis_ref: "fixture:basis".into(),
                revision_ref: None,
                continuity: None,
            },
        }))
    }
}

/// Delegating wrapper over the live backend: every call forwards to the
/// real repository while `commit_update_batch` records each call's item
/// ids and can inject a structural failure or a task cancellation at a
/// chosen call index — the seam the chunk-boundary tests need without
/// changing production code.
#[derive(Clone)]
struct Spy {
    inner: MarketBackend,
    commits: std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>,
    fail_at: std::cell::Cell<usize>,
    cancel_after: std::cell::Cell<usize>,
}
impl Spy {
    fn wrap(inner: &MarketBackend) -> Self {
        Self {
            inner: inner.clone(),
            commits: std::rc::Rc::new(std::cell::RefCell::new(vec![])),
            fail_at: std::cell::Cell::new(0),
            cancel_after: std::cell::Cell::new(0),
        }
    }
    fn commit_sizes(&self) -> Vec<usize> {
        self.commits.borrow().iter().map(Vec::len).collect()
    }
}
impl x_data_mcp::market_storage::MarketRepository for Spy {
    fn stage(&self, raw: &Value) -> Result<(), x_data_mcp::market_storage::Error> {
        self.inner.stage(raw)
    }
    fn candidate(
        &self,
        id: &str,
    ) -> Result<x_data_mcp::market_storage::compat::CheckedRecord, x_data_mcp::market_storage::Error>
    {
        self.inner.candidate(id)
    }
    fn staged_matches(
        &self,
        version_id: &str,
        whole: &Value,
    ) -> Result<bool, x_data_mcp::market_storage::Error> {
        self.inner.staged_matches(version_id, whole)
    }
    fn mark_validated(
        &self,
        record: &x_data_mcp::market_storage::compat::CheckedRecord,
    ) -> Result<(), x_data_mcp::market_storage::Error> {
        self.inner.mark_validated(record)
    }
    fn fixed(
        &self,
        id: &str,
    ) -> Result<x_data_mcp::market_storage::compat::CheckedRecord, x_data_mcp::market_storage::Error>
    {
        self.inner.fixed(id)
    }
    fn fixed_many(
        &self,
        ids: &[String],
    ) -> Result<
        Vec<x_data_mcp::market_storage::compat::CheckedRecord>,
        x_data_mcp::market_storage::Error,
    > {
        self.inner.fixed_many(ids)
    }
    fn fixed_facts(
        &self,
        id: &str,
    ) -> Result<x_data_mcp::market_storage::FixedFacts, x_data_mcp::market_storage::Error> {
        self.inner.fixed_facts(id)
    }
    fn fixed_window(
        &self,
        id: &str,
        first_day: chrono::NaiveDate,
        last_day: chrono::NaiveDate,
    ) -> Result<x_data_mcp::market_storage::compat::CheckedRecord, x_data_mcp::market_storage::Error>
    {
        self.inner.fixed_window(id, first_day, last_day)
    }
    fn fixed_window_years(
        &self,
        id: &str,
        first_day: chrono::NaiveDate,
        last_day: chrono::NaiveDate,
    ) -> Result<std::collections::BTreeSet<i32>, x_data_mcp::market_storage::Error> {
        self.inner.fixed_window_years(id, first_day, last_day)
    }
    fn task(
        &self,
        id: &str,
    ) -> Result<x_data_mcp::task_control::TaskSnapshot, x_data_mcp::market_storage::Error> {
        self.inner.task(id)
    }
    fn task_head(
        &self,
        id: &str,
    ) -> Result<x_data_mcp::task_control::TaskHead, x_data_mcp::market_storage::Error> {
        self.inner.task_head(id)
    }
    fn task_item_binding(
        &self,
        task: &str,
        item: &str,
    ) -> Result<Option<x_data_mcp::task_control::ItemBinding>, x_data_mcp::market_storage::Error>
    {
        self.inner.task_item_binding(task, item)
    }
    fn transition_task_head(
        &self,
        head: &mut x_data_mcp::task_control::TaskHead,
        item: Option<&mut x_data_mcp::task_control::TaskItem>,
        run: &str,
        change: x_data_mcp::task_control::Change,
    ) -> Result<(), x_data_mcp::market_storage::Error> {
        self.inner.transition_task_head(head, item, run, change)
    }
    fn commit(
        &self,
        plan: &x_data_mcp::market_storage::PublishPlan,
    ) -> Result<x_data_mcp::market_storage::Receipt, x_data_mcp::market_storage::Error> {
        self.inner.commit(plan)
    }
    fn receipt(
        &self,
        id: &str,
    ) -> Result<Option<x_data_mcp::market_storage::Receipt>, x_data_mcp::market_storage::Error>
    {
        self.inner.receipt(id)
    }
    fn range(
        &self,
        scope: &x_data_mcp::market_storage::Scope,
        version: Option<&str>,
        range: &x_data_mcp::market_storage::Range,
    ) -> Result<x_data_mcp::market_storage::ReadPage, x_data_mcp::market_storage::Error> {
        self.inner.range(scope, version, range)
    }
    fn versions(
        &self,
        scope: &x_data_mcp::market_storage::Scope,
    ) -> Result<Vec<x_data_mcp::publish::EffectiveVersion>, x_data_mcp::market_storage::Error> {
        self.inner.versions(scope)
    }
}
impl x_data_mcp::market_update::publication::RangeRepository for Spy {
    fn range_item_states(
        &self,
        task_id: &str,
        item_ids: &[String],
    ) -> Result<
        Vec<
            Result<
                x_data_mcp::market_update::publication::RangeItemState,
                x_data_mcp::market_storage::Error,
            >,
        >,
        x_data_mcp::market_storage::Error,
    > {
        self.inner.range_item_states(task_id, item_ids)
    }
    fn range_base_reads(
        &self,
        wants: &[x_data_mcp::market_update::publication::BaseWant],
    ) -> Result<
        Vec<
            Result<
                Option<x_data_mcp::market_update::publication::BaseRead>,
                x_data_mcp::market_storage::Error,
            >,
        >,
        x_data_mcp::market_storage::Error,
    > {
        self.inner.range_base_reads(wants)
    }
    fn update_base_facts(
        &self,
        base_version_id: &str,
    ) -> Result<Option<x_data_mcp::market_update::UpdateBaseFacts>, x_data_mcp::market_storage::Error>
    {
        self.inner.update_base_facts(base_version_id)
    }
    fn base_bars_in_range(
        &self,
        base_version_id: &str,
        start: chrono::NaiveDate,
        end: chrono::NaiveDate,
    ) -> Result<Option<Vec<x_data_mcp::market_data::Bar>>, x_data_mcp::market_storage::Error> {
        self.inner.base_bars_in_range(base_version_id, start, end)
    }
    fn commit_range(
        &self,
        plan: &x_data_mcp::market_update::publication::RangePlan,
    ) -> Result<x_data_mcp::market_storage::Receipt, x_data_mcp::market_storage::Error> {
        self.inner.commit_range(plan)
    }
    fn commit_range_ops_typed(
        &self,
        command: &x_data_mcp::market_storage::Publication,
        scope: &x_data_mcp::market_storage::Scope,
        base_version_id: &str,
        upserts: &[x_data_mcp::market_data::Bar],
        removes: &[chrono::NaiveDate],
        metadata: Option<&x_data_mcp::market_update::TypedVersionMetadata>,
        continuity_evidence: Option<&Value>,
    ) -> Result<Option<x_data_mcp::market_storage::Receipt>, x_data_mcp::market_storage::Error>
    {
        self.inner.commit_range_ops_typed(
            command,
            scope,
            base_version_id,
            upserts,
            removes,
            metadata,
            continuity_evidence,
        )
    }
    fn commit_range_no_change_typed(
        &self,
        command: &x_data_mcp::market_storage::Publication,
        base_version_id: &str,
        range: &x_data_mcp::market_update::DateRange,
        continuity_evidence: Option<&Value>,
    ) -> Result<Option<x_data_mcp::market_storage::Receipt>, x_data_mcp::market_storage::Error>
    {
        self.inner.commit_range_no_change_typed(
            command,
            base_version_id,
            range,
            continuity_evidence,
        )
    }
    fn commit_update_batch(
        &self,
        task_id: &str,
        items: &[x_data_mcp::market_update::UpdateBatchItem],
    ) -> Result<
        Option<Vec<Result<x_data_mcp::market_storage::Receipt, x_data_mcp::market_storage::Error>>>,
        x_data_mcp::market_storage::Error,
    > {
        let index = self.commits.borrow().len() + 1;
        self.commits
            .borrow_mut()
            .push(items.iter().map(|i| i.item_id.clone()).collect());
        if self.fail_at.get() == index {
            return Err(x_data_mcp::market_storage::Error::Invalid(
                "fixture: injected batch failure".into(),
            ));
        }
        let out = self.inner.commit_update_batch(task_id, items);
        if self.cancel_after.get() == index
            && let Ok(head) = self.inner.task_head(task_id)
        {
            self.inner
                .transition_task(
                    task_id,
                    head.state_revision,
                    "run",
                    x_data_mcp::task_control::Change::RequestCancel,
                )
                .unwrap();
        }
        out
    }
    fn range_receipt(
        &self,
        id: &str,
    ) -> Result<Option<x_data_mcp::market_storage::Receipt>, x_data_mcp::market_storage::Error>
    {
        self.inner.range_receipt(id)
    }
    fn item_receipt(
        &self,
        task: &str,
        item: &str,
    ) -> Result<Option<x_data_mcp::market_storage::Receipt>, x_data_mcp::market_storage::Error>
    {
        self.inner.item_receipt(task, item)
    }
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: x_data_mcp::task_control::Change,
    ) -> Result<x_data_mcp::task_control::TaskSnapshot, x_data_mcp::market_storage::Error> {
        self.inner.transition_task(task, revision, run, change)
    }
    fn cover_gap(
        &self,
        covering: &x_data_mcp::market_storage::GapCovering,
    ) -> Result<bool, x_data_mcp::market_storage::Error> {
        self.inner.cover_gap(covering)
    }
    fn gap_coverings(
        &self,
        gap_id: &str,
    ) -> Result<Vec<x_data_mcp::market_storage::GapCovering>, x_data_mcp::market_storage::Error>
    {
        self.inner.gap_coverings(gap_id)
    }
}

/// Build `members` real heads (chunk-seeded) plus a fresh catalog so the
/// plan resolves them all eligible; returns the accepted run task's ids.
fn run_task_with_members(r: &Rig, count: usize) -> (String, Vec<String>) {
    let mut codes = vec!["600006".to_string()];
    let mut symbols = vec!["sh.600006".to_string()];
    for i in 1..count {
        let code = format!("{:06}", 700_000 + i);
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
    seed_daily_heads(r, &symbols[1..]);
    let p = plan(&r.api, "market.daily");
    assert_eq!(p["counts"]["eligible"], symbols.len() as u64);
    let accepted = run(&r.api, "market.daily", "req-chunk", &p);
    let task_id = accepted["task_id"].as_str().unwrap().to_string();
    let item_ids: Vec<String> = r
        .tasks
        .read_task(&task_id)
        .unwrap()
        .unwrap()
        .items
        .iter()
        .map(|i| i.spec.item_id.clone())
        .collect();
    (task_id, item_ids)
}
fn runner(repo: &Spy) -> x_data_mcp::market_update::runner::RangeRunner<Spy> {
    x_data_mcp::market_update::runner::RangeRunner {
        repository: repo.clone(),
        run_id: "run".into(),
    }
}

#[test]
fn run_items_commits_update_batch_in_bounded_chunks() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    let (task_id, item_ids) = run_task_with_members(&r, 250);
    assert_eq!(item_ids.len(), 250);
    let spy = Spy::wrap(&r.backend);
    let provider = UnchangedProvider {
        typed_calls: std::cell::Cell::new(0),
    };
    runner(&spy)
        .run_items(&task_id, &item_ids, &provider)
        .unwrap()
        .expect("compact backend carries the update batch");
    // EXEC_CHUNK = 100 (pub(crate), restated here): 250 items -> [100,100,50].
    assert_eq!(spy.commit_sizes(), vec![100, 100, 50]);
    let task = r.tasks.read_task(&task_id).unwrap().unwrap();
    assert!(task.task.status.terminal(), "task must be terminal");
    assert!(
        task.items
            .iter()
            .all(|i| i.outcome.as_ref().is_some_and(|o| o.success)),
        "every item must record a successful outcome"
    );
    assert_eq!(provider.typed_calls.get(), 250);
}
#[test]
fn run_items_empty_and_single_item_boundaries() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    let spy = Spy::wrap(&r.backend);
    let provider = UnchangedProvider {
        typed_calls: std::cell::Cell::new(0),
    };
    // Zero items: no repository contact at all.
    assert!(matches!(
        runner(&spy).run_items("task-x", &[], &provider),
        Ok(Some(()))
    ));
    assert!(spy.commit_sizes().is_empty());
    // One item: one single-item commit.
    let (task_id, item_ids) = run_task_with_members(&r, 2);
    let one = vec![item_ids[0].clone()];
    runner(&spy)
        .run_items(&task_id, &one, &provider)
        .unwrap()
        .unwrap();
    assert_eq!(spy.commit_sizes(), vec![1]);
}
#[test]
fn run_items_chunk_failure_isolates_and_recovered_items_record() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    let (task_id, item_ids) = run_task_with_members(&r, 150);
    let spy = Spy::wrap(&r.backend);
    spy.fail_at.set(2); // the 50-item second chunk's commit fails structurally
    let provider = UnchangedProvider {
        typed_calls: std::cell::Cell::new(0),
    };
    runner(&spy)
        .run_items(&task_id, &item_ids, &provider)
        .unwrap()
        .unwrap();
    // Chunk 1 committed; chunk 2 rolled back and every one of its items
    // recovered through the per-item path.
    assert_eq!(spy.commit_sizes(), vec![100, 50]);
    let task = r.tasks.read_task(&task_id).unwrap().unwrap();
    assert!(
        task.items
            .iter()
            .all(|i| i.outcome.as_ref().is_some_and(|o| o.success)),
        "failed chunk's items must recover per-item"
    );
    assert!(
        spy.inner
            .item_receipt(&task_id, &item_ids[0])
            .unwrap()
            .is_some(),
        "first chunk's receipts must survive the later chunk's rollback"
    );
}
#[test]
fn run_items_cancel_between_chunks_preserves_committed_receipts() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    let (task_id, item_ids) = run_task_with_members(&r, 150);
    let spy = Spy::wrap(&r.backend);
    spy.cancel_after.set(1); // RequestCancel lands after chunk 1's commit
    let provider = UnchangedProvider {
        typed_calls: std::cell::Cell::new(0),
    };
    runner(&spy)
        .run_items(&task_id, &item_ids, &provider)
        .unwrap()
        .unwrap();
    // Only chunk 1's batch commit ran.
    assert_eq!(spy.commit_sizes(), vec![100]);
    let task = r.tasks.read_task(&task_id).unwrap().unwrap();
    assert_eq!(
        task.task.status,
        x_data_mcp::task_control::Status::Cancelled,
        "unreached items must end through the cancellation finish"
    );
    // The committed chunk's receipts remain; unreached items keep the
    // existing cancellation semantics — never started, outcome absent;
    // the Cancelled head is the finish record.
    assert!(
        spy.inner
            .item_receipt(&task_id, &item_ids[0])
            .unwrap()
            .is_some()
    );
    assert!(
        task.items[..100]
            .iter()
            .all(|i| i.outcome.as_ref().is_some_and(|o| o.success)),
        "committed chunk items keep their success outcomes"
    );
    assert!(
        task.items[100..].iter().all(|i| i.outcome.is_none()),
        "unreached items carry no outcome under cancellation semantics"
    );
}
#[test]
fn run_items_receipt_replay_never_reacquires() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    let (task_id, item_ids) = run_task_with_members(&r, 3);
    let spy = Spy::wrap(&r.backend);
    let provider = UnchangedProvider {
        typed_calls: std::cell::Cell::new(0),
    };
    runner(&spy)
        .run_items(&task_id, &item_ids, &provider)
        .unwrap()
        .unwrap();
    assert_eq!(provider.typed_calls.get(), 3);
    // A killed-mid-task re-entry: items with receipts replay without any
    // further acquisition or batch commit.
    runner(&spy)
        .run_items(&task_id, &item_ids, &provider)
        .unwrap()
        .unwrap();
    assert_eq!(provider.typed_calls.get(), 3);
    assert_eq!(spy.commit_sizes(), vec![3]);
}

/// Chunked batch execution must produce the same durable facts as the
/// authoritative per-item path: same per-item outcomes and receipt
/// version bindings for identical inputs (two rigs seeded identically —
/// seeded version ids are deterministic, so receipt version_ids compare
/// across rigs).
#[test]
fn run_items_chunked_execution_matches_per_item_outcomes() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    const N: usize = 120;
    let rig_a = rig(&[("sh", "600006", day_u32(1))]);
    let (task_a, ids_a) = run_task_with_members(&rig_a, N);
    let rig_b = rig(&[("sh", "600006", day_u32(1))]);
    let (task_b, ids_b) = run_task_with_members(&rig_b, N);
    assert_eq!(
        ids_a, ids_b,
        "identical seeds must produce identical item ids"
    );
    let spy = Spy::wrap(&rig_a.backend);
    let provider = UnchangedProvider {
        typed_calls: std::cell::Cell::new(0),
    };
    runner(&spy)
        .run_items(&task_a, &ids_a, &provider)
        .unwrap()
        .unwrap();
    // Reference: the per-item path on the same inputs (commit_update_batch
    // untouched — spy B records nothing).
    let spy_b = Spy::wrap(&rig_b.backend);
    let runner_b = runner(&spy_b);
    let mut per_item: Vec<Option<String>> = Vec::new();
    for item_id in &ids_b {
        match runner_b.run_item(&task_b, item_id, &provider).unwrap() {
            x_data_mcp::market_update::runner::ItemExecution::Published(receipt) => {
                per_item.push(Some(receipt.version_id));
            }
            other => panic!("per-item execution must publish: {other:?}"),
        }
    }
    assert!(spy_b.commit_sizes().is_empty());
    // Durable equivalence: every item's receipt version_id and success
    // outcome matches between the chunked batch and the per-item path.
    let task = rig_a.tasks.read_task(&task_a).unwrap().unwrap();
    let task_b_items = rig_b.tasks.read_task(&task_b).unwrap().unwrap();
    for ((item_a, item_b), receipt_version) in task
        .items
        .iter()
        .zip(task_b_items.items.iter())
        .zip(per_item.iter())
    {
        assert_eq!(item_a.spec.item_id, item_b.spec.item_id);
        // outcome.success/kind must match; result_id embeds a task-bound
        // digest so it legitimately differs across rigs.
        assert_eq!(
            item_a.outcome.as_ref().map(|o| o.success),
            item_b.outcome.as_ref().map(|o| o.success)
        );
        assert_eq!(
            item_a
                .outcome
                .as_ref()
                .and_then(|o| o.evidence.as_ref())
                .and_then(|e| e.get("result_kind")),
            item_b
                .outcome
                .as_ref()
                .and_then(|o| o.evidence.as_ref())
                .and_then(|e| e.get("result_kind"))
        );
        assert!(item_a.outcome.as_ref().is_some_and(|o| o.success));
        let receipt_a = rig_a
            .backend
            .item_receipt(&task_a, &item_a.spec.item_id)
            .unwrap()
            .expect("chunked item must hold a receipt");
        assert_eq!(&receipt_a.version_id, receipt_version.as_ref().unwrap());
    }
    // Task-level parity: both terminal-succeeded, same counters.
    assert_eq!(task.task.status, task_b_items.task.status);
    let head_a = rig_a.backend.task_head(&task_a).unwrap();
    let head_b = rig_b.backend.task_head(&task_b).unwrap();
    assert_eq!(head_a.counts, head_b.counts);
}

/// ≤100-item tasks keep the v6 progress shape byte-identical: the full
/// per-item arrays ride every page and the bounded `plan` object is
/// absent entirely.
#[test]
fn small_task_progress_keeps_full_shape() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    let (task_id, item_ids) = run_task_with_members(&r, 2);
    let page = r
        .api
        .call(
            "get_task_progress",
            json!({"task_id":task_id,"page_size":100}),
        )
        .unwrap();
    assert_eq!(page["total"], 2);
    assert!(
        page.get("plan").is_none(),
        "small tasks must not carry the bounded plan object"
    );
    assert_eq!(
        page["frozen_daily_ranges"].as_array().unwrap().len(),
        item_ids.len(),
        "small tasks keep the per-item frozen ranges"
    );
    assert_eq!(page["unfinished"].as_array().unwrap().len(), 2);
    let view = r
        .api
        .call(
            "get_task",
            json!({"contract_version":"v1","task_id":task_id}),
        )
        .unwrap();
    assert_eq!(view["task_id"], task_id);
}

// ---------------------------------------------------------------------------
// Recovery (TASK-DEVIN-PLANRUN-RECOVERY): a live plan-run task accepts
// cancellation at the committed revision, and a task left non-terminal by
// a vanished run auto-closes as `interrupted` at the next reconcile.
// ---------------------------------------------------------------------------

/// Append-at-target fixture provider: each typed acquire commits one bar
/// at the item's range end, advancing the entity head to the plan's
/// target day — a committed item re-plans as `already_current`.
struct AppendProvider;
impl x_data_mcp::market_update::runner::RangeProvider for AppendProvider {
    fn acquire(
        &self,
        _base: &x_data_mcp::market_storage::compat::CheckedRecord,
        _range: &x_data_mcp::market_update::DateRange,
        _candidate_id: &str,
    ) -> Result<x_data_mcp::market_update::runner::Acquisition, x_data_mcp::market_data::SourceError>
    {
        Err(x_data_mcp::market_data::SourceError::NotVerified(
            "fixture: envelope path unused".into(),
        ))
    }
    fn acquire_typed(
        &self,
        facts: &x_data_mcp::market_update::UpdateBaseFacts,
        _base_window: &[x_data_mcp::market_data::Bar],
        range: &x_data_mcp::market_update::DateRange,
    ) -> Result<
        Option<x_data_mcp::market_update::runner::TypedAcquisition>,
        x_data_mcp::market_data::SourceError,
    > {
        let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        let bar = x_data_mcp::compact_market::CompactBar {
            trade_day: range.end.signed_duration_since(epoch).num_days(),
            open_cents: 1000,
            high_cents: 1010,
            low_cents: 990,
            close_cents: 1005,
            volume: Some(100),
            amount: Some("1000".into()),
            r7_raw: Some(7),
        }
        .to_bar(x_data_mcp::market_data::Period::Day)
        .unwrap();
        Ok(Some(x_data_mcp::market_update::runner::TypedAcquisition {
            decision: x_data_mcp::market_update::runner::TypedDecision::Append {
                upserts: vec![bar],
                metadata: None,
            },
            evidence: x_data_mcp::market_update::Evidence {
                base_version_id: facts.version_id.clone(),
                base_integrity_sha256: facts.integrity_sha256.clone(),
                candidate_sha256: "fixture:candidate".into(),
                range: range.clone(),
                same_qfq_basis_ref: "fixture:basis".into(),
                revision_ref: None,
                continuity: None,
            },
        }))
    }
}

/// A running plan-run task advances its revision per committed chunk, so
/// an `expected_revision` read earlier always loses the race. Cancellation
/// is a monotone intent: the request now applies at the committed
/// revision, and only terminal tasks keep the strict precondition.
#[test]
fn running_planrun_task_accepts_cancel_at_a_stale_revision() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    let (task_id, item_ids) = run_task_with_members(&r, 150);
    let provider = UnchangedProvider {
        typed_calls: std::cell::Cell::new(0),
    };
    let runner = x_data_mcp::market_update::runner::RangeRunner {
        repository: r.backend.clone(),
        run_id: "run".into(),
    };
    // Commit the first chunk: the task is live at a later revision.
    runner
        .run_items(&task_id, &item_ids[..100], &provider)
        .unwrap()
        .expect("compact backend carries the update batch");
    let live = r.tasks.read_task(&task_id).unwrap().unwrap();
    assert_eq!(live.task.status, x_data_mcp::task_control::Status::Running);
    assert!(live.task.state_revision > 0);
    // The acceptance-time revision is stale, yet the request lands on the
    // state the write actually reads.
    let reply = r
        .api
        .call(
            "cancel_task",
            json!({"contract_version":"v1","task_id":task_id,"expected_revision":0}),
        )
        .unwrap();
    assert_eq!(reply["status"], "cancel_requested");
    // The worker drains through the existing cancellation finish:
    // committed chunk outcomes survive, unreached items carry none.
    runner
        .run_items(&task_id, &item_ids[100..], &provider)
        .unwrap()
        .unwrap();
    let task = r.tasks.read_task(&task_id).unwrap().unwrap();
    assert_eq!(
        task.task.status,
        x_data_mcp::task_control::Status::Cancelled
    );
    assert!(
        task.items[..100]
            .iter()
            .all(|i| i.outcome.as_ref().is_some_and(|o| o.success))
    );
    assert!(task.items[100..].iter().all(|i| i.outcome.is_none()));
    // A terminal task still enforces the revision precondition.
    assert_eq!(
        r.api
            .call(
                "cancel_task",
                json!({"contract_version":"v1","task_id":task_id,"expected_revision":0}),
            )
            .unwrap_err()
            .code,
        "precondition_conflict"
    );
}

/// Per-entity durable content of one database — bars, head rows and the
/// event projection minus task-bound columns (task_id/result_id/seq and
/// clock fields legitimately differ across runs).
type SqlRows = Vec<Vec<String>>;
fn market_projection(path: &Path) -> (SqlRows, SqlRows, SqlRows) {
    let c = rusqlite::Connection::open(path).unwrap();
    let dump = |sql: &str| {
        c.prepare(sql)
            .unwrap()
            .query_map([], |r| {
                let mut row = Vec::with_capacity(r.as_ref().column_count());
                for i in 0..r.as_ref().column_count() {
                    row.push(format!(
                        "{:?}",
                        r.get::<_, rusqlite::types::Value>(i).unwrap()
                    ));
                }
                Ok(row)
            })
            .unwrap()
            .collect::<Result<Vec<Vec<String>>, _>>()
            .unwrap()
    };
    let bars = dump(
        "SELECT s.entity,b.trade_day,b.open_cents,b.high_cents,b.low_cents,b.close_cents,b.volume,b.amount,b.r7_raw \
         FROM series_bars b JOIN market_series s USING(series_key) ORDER BY s.entity,b.trade_day",
    );
    let series = dump(
        "SELECT entity,period,adjustment,version_id,hex(content_digest),digest_format,record_count,first_day,last_day,coverage_status,hex(source_binding_digest),hex(evidence_digest) \
         FROM market_series ORDER BY entity,period,adjustment",
    );
    let events = dump(
        "SELECT entity,period,kind,outcome,metadata_changed,semantics_changed,continuity_mode,prefix_bytes,hex(old_content_digest),hex(new_content_digest),range_start_day,range_end_day,hex(window_digest),hex(base_content_digest),algorithm_version,item_id \
         FROM market_events ORDER BY entity,period,seq",
    );
    (bars, series, events)
}
fn sql_count(c: &rusqlite::Connection, sql: &str, id: &str) -> i64 {
    c.query_row(sql, [id], |r| r.get(0)).unwrap()
}

/// A plan-run task whose accepting run died mid-execution is an orphan no
/// tool could previously close. The startup reconcile now closes it
/// `interrupted` in one transaction: occupancy releases, the killed
/// running item returns to pending (no invented outcome), and every row —
/// scopes, request, execution-start markers — is preserved. Re-planning
/// then re-selects exactly the unfinished set, and the resumed run
/// converges to the same durable content as an uninterrupted run.
#[test]
fn planrun_orphan_closes_as_interrupted_and_resumes_to_the_same_state() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    use x_data_mcp::task_control::{Change, Stage, Status};
    const N: usize = 150;
    // Identically seeded control rig: same task runs uninterrupted.
    let rig_b = rig(&[("sh", "600006", day_u32(1))]);
    let (task_b, ids_b) = run_task_with_members(&rig_b, N);
    let runner_b = x_data_mcp::market_update::runner::RangeRunner {
        repository: rig_b.backend.clone(),
        run_id: "run".into(),
    };
    runner_b
        .run_items(&task_b, &ids_b, &AppendProvider)
        .unwrap()
        .expect("compact backend carries the update batch");
    assert_eq!(
        rig_b.tasks.read_task(&task_b).unwrap().unwrap().task.status,
        Status::Succeeded
    );

    let rig_a = rig(&[("sh", "600006", day_u32(1))]);
    let (task_a, ids_a) = run_task_with_members(&rig_a, N);
    assert_eq!(ids_a, ids_b, "identical seeds must produce identical items");
    let runner_a = x_data_mcp::market_update::runner::RangeRunner {
        repository: rig_a.backend.clone(),
        run_id: "run".into(),
    };
    // Chunk 1 commits (100 durable item outcomes); the kill lands with
    // item 101 still at the running stage — a start marker but no outcome.
    runner_a
        .run_items(&task_a, &ids_a[..100], &AppendProvider)
        .unwrap()
        .unwrap();
    let rev = rig_a.backend.task_head(&task_a).unwrap().state_revision;
    rig_a
        .tasks
        .transition(&task_a, rev, "run", Change::StartItem(ids_a[100].clone()))
        .unwrap();

    // Service restart under a new run id: the startup reconcile closes
    // the orphan in the same transaction.
    assert_eq!(rig_a.tasks.reconcile_plan_run("run-b").unwrap(), 1);
    let closed = rig_a.tasks.read_task(&task_a).unwrap().unwrap();
    assert_eq!(closed.task.status, Status::Interrupted);
    let interruption = &closed.task.result.as_ref().unwrap()["interruption"];
    assert_eq!(interruption["reason"], "service_restarted");
    assert_eq!(interruption["successful_items"], 100);
    assert_eq!(interruption["unfinished_items"], N - 100);
    assert!(closed.claims.is_empty(), "interrupt releases all occupancy");
    assert!(closed.items.iter().all(|i| i.stage != Stage::Running));
    assert_eq!(
        closed
            .items
            .iter()
            .filter(|i| i.outcome.as_ref().is_some_and(|o| o.success))
            .count(),
        100
    );
    // Nothing is deleted: scopes, the request binding and execution-start
    // markers all survive the close.
    let c = rusqlite::Connection::open(rig_a.tasks.path()).unwrap();
    assert_eq!(
        sql_count(
            &c,
            "SELECT count(*) FROM task_scopes WHERE task_id=?1",
            &task_a
        ),
        N as i64
    );
    assert_eq!(
        sql_count(
            &c,
            "SELECT count(*) FROM task_item_execution_starts WHERE task_id=?1",
            &task_a
        ),
        101
    );
    assert_eq!(
        sql_count(
            &c,
            "SELECT count(*) FROM requests WHERE task_id=?1",
            &task_a
        ),
        1
    );
    // The closed registry passes the full sweep and the accept gates —
    // `accept.gate.orphan` no longer refuses this database at startup.
    assert!(rig_a.tasks.registry_sweep().unwrap().is_empty());
    // Reconcile is idempotent.
    assert_eq!(rig_a.tasks.reconcile_plan_run("run-b").unwrap(), 0);
    assert_eq!(rig_a.tasks.read_task(&task_a).unwrap().unwrap(), closed);
    drop(c);

    // The restarted service reads the task as terminal, `verify` reports
    // it not applicable (never needs_verification), and re-planning is
    // unblocked by `plan_in_progress`/`daily_update_inflight`.
    let api_b = DataApi::new_with_backend(
        rig_a.tasks.clone(),
        rig_a.backend.clone(),
        "run-b".into(),
        Access::local(),
        None,
    )
    .unwrap();
    assert_eq!(
        api_b
            .call(
                "get_task",
                json!({"contract_version":"v1","task_id":task_a}),
            )
            .unwrap()["status"],
        "interrupted"
    );
    assert_eq!(
        api_b
            .call(
                "verify_stopped_writes",
                json!({"contract_version":"v1","request_id":"verify-orphan","target_task_id":task_a}),
            )
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let p2 = plan(&api_b, "market.daily");
    assert_eq!(p2["counts"]["eligible"], (N - 100) as u64, "{p2}");
    let resumed = run(&api_b, "market.daily", "resume-1", &p2);
    assert_eq!(resumed["item_count"], (N - 100) as u64);
    let task_a2 = resumed["task_id"].as_str().unwrap().to_string();
    let ids_a2: Vec<String> = rig_a
        .tasks
        .read_task(&task_a2)
        .unwrap()
        .unwrap()
        .items
        .iter()
        .map(|i| i.spec.item_id.clone())
        .collect();
    let runner_a2 = x_data_mcp::market_update::runner::RangeRunner {
        repository: rig_a.backend.clone(),
        run_id: "run-b".into(),
    };
    runner_a2
        .run_items(&task_a2, &ids_a2, &AppendProvider)
        .unwrap()
        .unwrap();
    let done = rig_a.tasks.read_task(&task_a2).unwrap().unwrap();
    assert_eq!(done.task.status, Status::Succeeded);

    // Durable equivalence: every item's receipt version binding and the
    // whole market projection match the uninterrupted control rig.
    let mut receipts_a = std::collections::BTreeMap::new();
    for item in &ids_a[..100] {
        receipts_a.insert(
            item.clone(),
            rig_a
                .backend
                .item_receipt(&task_a, item)
                .unwrap()
                .expect("committed item keeps its receipt")
                .version_id,
        );
    }
    for item in &ids_a2 {
        receipts_a.insert(
            item.clone(),
            rig_a
                .backend
                .item_receipt(&task_a2, item)
                .unwrap()
                .expect("resumed item has a receipt")
                .version_id,
        );
    }
    let receipts_b: std::collections::BTreeMap<_, _> = ids_b
        .iter()
        .map(|item| {
            (
                item.clone(),
                rig_b
                    .backend
                    .item_receipt(&task_b, item)
                    .unwrap()
                    .unwrap()
                    .version_id,
            )
        })
        .collect();
    assert_eq!(receipts_a, receipts_b);
    assert_eq!(
        market_projection(rig_a.tasks.path()),
        market_projection(rig_b.tasks.path()),
        "interrupted+resumed library must equal the uninterrupted run"
    );
}

/// A plan-run orphan and an explicit `update_data` orphan die with the
/// same run. The startup reconcile (`reconcile_plan_run`, the entry
/// `data_service` calls) closes only the plan-run task — the explicit
/// task's stored snapshot, revision and occupancy stay byte-for-byte
/// identical; its needs_verification marking remains a visible-state
/// decision, never a startup write. The generic `reconcile` keeps its
/// old semantics for callers that opt in.
#[test]
fn startup_reconcile_closes_only_planrun_orphans_and_leaves_explicit_tasks_intact() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    use x_data_mcp::task_control::{
        Command, CommandKey, FrozenWork, ItemSpec, RequestOutcome, Stage, Status,
    };
    let r = rig(&[("sh", "600006", day_u32(1)), ("sz", "000002", day_u32(1))]);
    let p = plan(&r.api, "market.daily");
    let accepted = run(&r.api, "market.daily", "run-orphan", &p);
    let task_id = accepted["task_id"].as_str().unwrap().to_string();
    // An explicit update_data task (CONTRACT_VERSION) accepted by the
    // same dying run — a scope disjoint from the planned members.
    let scopes: std::collections::BTreeSet<String> =
        std::iter::once("market:xx.999999:day".to_string()).collect();
    let RequestOutcome::Task(other) = r
        .tasks
        .accept(
            "explicit-orphan",
            &Command {
                key: CommandKey {
                    operation: "update_data".into(),
                    dataset: "market.daily".into(),
                    contract_version: x_data_mcp::market_update::tasks::CONTRACT_VERSION.into(),
                },
                intent: json!({}),
            },
            &FrozenWork {
                items: vec![ItemSpec {
                    item_id: "market:xx.999999:day".into(),
                    target: json!({"entity":"xx.999999"}),
                    progress_unit: "entity".into(),
                    frozen_inputs: json!({}),
                    scope_keys: scopes.clone(),
                }],
                scopes,
                shared_evidence: None,
            },
            "run",
            None,
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!("explicit acceptance must bind a task")
    };
    let explicit_before = r.tasks.read_task(&other.task.task_id).unwrap().unwrap();
    // Restart: the startup reconcile closes only the plan-run orphan.
    assert_eq!(r.tasks.reconcile_plan_run("run-b").unwrap(), 1);
    let closed = r.tasks.read_task(&task_id).unwrap().unwrap();
    assert_eq!(closed.task.status, Status::Interrupted);
    let interruption = &closed.task.result.as_ref().unwrap()["interruption"];
    assert_eq!(interruption["successful_items"], 0);
    assert_eq!(interruption["unfinished_items"], 2);
    assert!(closed.claims.is_empty());
    assert!(
        closed
            .items
            .iter()
            .all(|i| i.stage == Stage::Pending && i.outcome.is_none())
    );
    // The explicit orphan's persisted state is byte-for-byte untouched —
    // same status, revision, items and claims — while its *visible*
    // status under the new run still reports needs_verification.
    assert_eq!(
        r.tasks.read_task(&other.task.task_id).unwrap().unwrap(),
        explicit_before
    );
    assert_eq!(
        explicit_before.visible_status("run-b"),
        Status::NeedsVerification
    );
    assert_eq!(explicit_before.claims.len(), 1);
    let c = rusqlite::Connection::open(r.tasks.path()).unwrap();
    assert_eq!(
        sql_count(
            &c,
            "SELECT count(*) FROM scope_occupancy WHERE task_id=?1",
            &other.task.task_id
        ),
        1
    );
    // Idempotent for the scoped entry; the generic reconcile still
    // persists the needs_verification marking when a caller opts in.
    assert_eq!(r.tasks.reconcile_plan_run("run-b").unwrap(), 0);
    assert_eq!(r.tasks.read_task(&task_id).unwrap().unwrap(), closed);
    assert_eq!(r.tasks.reconcile("run-b").unwrap(), 1);
    assert_eq!(
        r.tasks
            .read_task(&other.task.task_id)
            .unwrap()
            .unwrap()
            .task
            .status,
        Status::NeedsVerification
    );
    assert_eq!(r.tasks.reconcile("run-b").unwrap(), 0);
    // The restarted service's registry and plan paths are unblocked.
    assert!(r.tasks.registry_sweep().unwrap().is_empty());
    let api_b = DataApi::new_with_backend(
        r.tasks.clone(),
        r.backend.clone(),
        "run-b".into(),
        Access::local(),
        None,
    )
    .unwrap();
    assert_eq!(plan(&api_b, "market.daily")["counts"]["eligible"], 2);
}

/// PLANRUN-COMMIT-COST scale-independence: the same 100-item chunk inside
/// a ≤100-item task performs the same O(1) task-row writes as inside a
/// 5,201-item task — the per-chunk `UPDATE tasks` count must not grow
/// with the task's item count (≈2 per chunk + claim, vs ~2×items before).
#[test]
fn commit_task_row_writes_do_not_scale_with_task_size() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    let (task_id, item_ids) = run_task_with_members(&r, 100);
    assert_eq!(item_ids.len(), 100);
    TASK_ROW_WRITES.store(0, std::sync::atomic::Ordering::Relaxed);
    r.backend
        .compact_writer_authorizer(Some(task_write_probe))
        .unwrap();
    let provider = UnchangedProvider {
        typed_calls: std::cell::Cell::new(0),
    };
    let runner = x_data_mcp::market_update::runner::RangeRunner {
        repository: r.backend.clone(),
        run_id: "run".into(),
    };
    runner
        .run_items(&task_id, &item_ids, &provider)
        .unwrap()
        .expect("compact backend carries the update batch");
    r.backend.compact_writer_authorizer(None).unwrap();
    let writes = TASK_ROW_WRITES.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        writes <= 4,
        "100-item task: {writes} task-row writes must be O(chunks), not O(items)"
    );
    let task = r.tasks.read_task(&task_id).unwrap().unwrap();
    assert!(task.task.status.terminal(), "{:?}", task.task.status);
    assert_eq!(
        task.items.iter().filter(|i| i.outcome.is_some()).count(),
        100
    );
}

/// Startup reconcile on a market-scale pending orphan stays bounded: the
/// snapshot path materializes the whole 5,201-item task once — the close
/// writes the task row and item stage resets in a single transaction.
/// Asserts the wall-clock (<10s) and VmHWM-rise (<300MB) budgets on the
/// CI host so the restart cost is measured, not assumed.
#[test]
fn planrun_orphan_reconcile_at_market_scale_stays_bounded() {
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
    seed_daily_heads(&r, &extra);
    let p = plan(&r.api, "market.daily");
    let accepted = run(&r.api, "market.daily", "req-scale", &p);
    assert_eq!(accepted["item_count"], symbols.len() as u64);
    let task_id = accepted["task_id"].as_str().unwrap().to_string();

    // Simulated restart: the task is a pending orphan under a new run.
    let hwm0 = vmhwm_kb();
    let started = std::time::Instant::now();
    let closed_n = r.tasks.reconcile_plan_run("run-b").unwrap();
    let wall = started.elapsed();
    let hwm_delta_kb = vmhwm_kb().saturating_sub(hwm0);
    eprintln!(
        "orphan reconcile scale: {} items, {wall:?}, VmHWM +{hwm_delta_kb}kB",
        symbols.len()
    );
    assert_eq!(closed_n, 1);
    assert!(
        wall < std::time::Duration::from_secs(10),
        "reconcile took {wall:?} > 10s"
    );
    assert!(
        hwm_delta_kb < 300 * 1024,
        "reconcile VmHWM delta {hwm_delta_kb}kB exceeds 300MB"
    );
    let closed = r.tasks.read_task(&task_id).unwrap().unwrap();
    assert_eq!(
        closed.task.status,
        x_data_mcp::task_control::Status::Interrupted
    );
    assert!(closed.claims.is_empty());
    assert_eq!(closed.items.len(), symbols.len());
    assert!(
        closed
            .items
            .iter()
            .all(|i| i.stage == x_data_mcp::task_control::Stage::Pending && i.outcome.is_none())
    );
}

// ---------------------------------------------------------------------------
// Failure routing (TASK-DEVIN-PLANRUN-FROZEN-FIX): a large task reports why
// items failed in bounded form, so a caller can route one failure class
// (REVISION_UNVERIFIED -> explicit update_data forward window) without
// paging every failed item or receiving tens of kilobytes of error text.
// ---------------------------------------------------------------------------

/// Typed acquisition that fails for chosen symbols with a very long
/// "revision" message (the real run's `RevisionUnverified` error listed
/// every revised day of the history) and reports everything else unchanged.
struct FailingProvider {
    fail: std::collections::BTreeSet<String>,
    ok: UnchangedProvider,
}
impl x_data_mcp::market_update::runner::RangeProvider for FailingProvider {
    fn acquire(
        &self,
        base: &x_data_mcp::market_storage::compat::CheckedRecord,
        range: &x_data_mcp::market_update::DateRange,
        candidate_id: &str,
    ) -> Result<x_data_mcp::market_update::runner::Acquisition, x_data_mcp::market_data::SourceError>
    {
        self.ok.acquire(base, range, candidate_id)
    }
    fn acquire_typed(
        &self,
        facts: &x_data_mcp::market_update::UpdateBaseFacts,
        base_window: &[x_data_mcp::market_data::Bar],
        range: &x_data_mcp::market_update::DateRange,
    ) -> Result<
        Option<x_data_mcp::market_update::runner::TypedAcquisition>,
        x_data_mcp::market_data::SourceError,
    > {
        if self.fail.contains(&facts.symbol) {
            return Err(x_data_mcp::market_data::SourceError::NotVerified(format!(
                "RevisionUnverified([{}])",
                "2003-03-25, ".repeat(6_000)
            )));
        }
        self.ok.acquire_typed(facts, base_window, range)
    }
}

#[test]
fn large_task_progress_routes_failures_by_bounded_reason() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    let (task_id, item_ids) = run_task_with_members(&r, 150);
    let spy = Spy::wrap(&r.backend);
    let fail: std::collections::BTreeSet<String> = [10, 20, 30, 40, 50]
        .iter()
        .map(|i| format!("sh.{:06}", 700_000 + i))
        .collect();
    let provider = FailingProvider {
        fail: fail.clone(),
        ok: UnchangedProvider {
            typed_calls: std::cell::Cell::new(0),
        },
    };
    runner(&spy)
        .run_items(&task_id, &item_ids, &provider)
        .unwrap()
        .unwrap();

    let page = r
        .api
        .call(
            "get_task_progress",
            json!({"task_id":task_id,"page_size":100}),
        )
        .unwrap();
    assert_eq!(page["total"], 150, "{page}");
    assert_eq!(page["failed"], fail.len() as u64, "{page}");
    // plan-level class counts add up to the failed items
    let by_reason = page["plan"]["failed_by_reason"].as_object().unwrap();
    let counted: u64 = by_reason.values().map(|v| v.as_u64().unwrap()).sum();
    assert_eq!(counted, fail.len() as u64, "{by_reason:?}");
    // every unfinished entry is a failed item with a class and a bounded error
    let unfinished = page["unfinished"].as_array().unwrap();
    assert_eq!(unfinished.len(), fail.len(), "{page}");
    for item in unfinished {
        assert_eq!(item["success"], false);
        assert!(item["reason_code"].is_string(), "{item}");
        assert!(
            item["error"].as_str().unwrap().chars().count() < 300,
            "large-task errors must be truncated: {item}"
        );
    }
    let whole = serde_json::to_vec(&page).unwrap().len();
    assert!(
        whole < 50 * 1024,
        "page of failures must stay small: {whole}"
    );
}

// ---------------------------------------------------------------------------
// Default target day (TASK-CB-4): without `end_date` the plan follows the
// active generation's latest trade day and reports where the day came from.
// ---------------------------------------------------------------------------

#[test]
fn plan_reports_where_the_target_day_came_from() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = rig(&[("sh", "600006", day_u32(1))]);
    // The rig binds no generation, so the default is the previous behavior
    // (today) and the response says so.
    let defaulted = plan(&r.api, "market.daily");
    assert_eq!(
        defaulted["target_day_source"], "today_fallback",
        "{defaulted}"
    );
    // An explicit end_date is honored and reported as such.
    let explicit = r
        .api
        .call(
            "plan_market_update",
            json!({"dataset":"market.daily","contract_version":"v1","end_date":"2026-10-01"}),
        )
        .unwrap();
    assert_eq!(explicit["target_day"], "2026-10-01", "{explicit}");
    assert_eq!(explicit["target_day_source"], "explicit", "{explicit}");
    // The source is informational: it is not part of the canonical plan, so
    // the same state + target day yields the same plan_id either way.
    let again = r
        .api
        .call(
            "plan_market_update",
            json!({"dataset":"market.daily","contract_version":"v1","end_date":defaulted["target_day"]}),
        )
        .unwrap();
    assert_eq!(again["plan_id"], defaulted["plan_id"]);
    assert_eq!(again["target_day_source"], "explicit");
}
