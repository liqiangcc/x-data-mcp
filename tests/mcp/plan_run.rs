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
    api.call(
        "start_collection",
        json!({"request_id":"seed","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":securities}}),
    )
    .unwrap();
    api.call("run_worker_once", json!({})).unwrap();
    Rig {
        api,
        snapshot: snap,
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
