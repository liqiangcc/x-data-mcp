//! #259 S5B retained-generation capacity harness. `#[ignore]`d — run
//! manually with:
//!
//! ```text
//! X_DATA_MCP_UNSAFE_NONLOCAL_FS=1 \
//! X_CAPACITY_DIR=/home/box/capacity-s5b \
//! X_CAPACITY_PHASES=initial,measure,nochange,append,soak,verify \
//! X_CAPACITY_SOAK_DAYS=250 \
//! cargo test --release --locked s5b_retained_capacity -- --ignored --nocapture
//! ```
//!
//! The DB lives on overlayfs (Box has no local disk); the lease opt-out is
//! Box-only acceptance — production wiring is unaffected. Phases resume via
//! `progress.json` and the database's own published-version set.
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::NaiveDate;
use serde_json::{Value, json};

use super::*;
use crate::market_data::{Bar, EvidenceValue, UnitCurrencyEvidence};
use crate::market_storage::compat;
use crate::official_market::TdxMarketSource;
use crate::task_control::{CommandKey, ItemSpec};

const TASK_ITEMS: usize = 100; // production-shaped task bound (#259 gate)
const RETAINED: &str = "/home/box/retained-inputs";
const RUN: &str = "capacity-run";

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| {
                l.split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse::<u64>().ok())
            })
        })
        .unwrap_or(0)
        * 1024
}

fn wal_bytes(db: &Path) -> u64 {
    let wal = db.with_extension("db-wal");
    std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0)
}

fn db_bytes(db: &Path) -> u64 {
    std::fs::metadata(db).map(|m| m.len()).unwrap_or(0)
}

fn symbols(limit: u64) -> Vec<String> {
    let zip = Path::new(RETAINED).join("official-hsjday.zip");
    let file = std::fs::File::open(&zip).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let name = archive.by_index(i).unwrap().name().replace('\\', "/");
        if let Some(member) = name
            .strip_suffix(".day")
            .and_then(|rest| rest.rsplit('/').next())
        {
            for market in ["sh", "sz"] {
                if let Some(code) = member.strip_prefix(market)
                    && code.len() == 6
                    && code.bytes().all(|b| b.is_ascii_digit())
                {
                    out.push(format!("{market}.{code}"));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    if limit > 0 {
        out.truncate(limit as usize);
    }
    out
}

fn snapshot_root(dir: &Path) -> (PathBuf, Vec<u8>) {
    let root = dir.join("snapshot");
    std::fs::create_dir_all(&root).unwrap();
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
    let mut files = Vec::new();
    for (src, name, role, url) in inputs {
        let path = root.join(name);
        if !path.exists() {
            std::fs::hard_link(Path::new(RETAINED).join(src), &path).unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        files.push(json!({
            "role": role,
            "relative_path": name,
            "source_url": url,
            "acquisition_started_at": "2026-09-15T01:00:00Z",
            "acquisition_completed_at": "2026-09-15T02:00:00Z",
            "last_modified": null,
            "etag": null,
            "bytes": bytes.len(),
            "sha256": crate::hash::sha256(&bytes),
        }));
    }
    let manifest = serde_json::to_vec(&json!({
        "schema": "x-data-mcp.official-file-input.v1",
        "files": files
    }))
    .unwrap();
    (root, manifest)
}

fn make_source(dir: &Path) -> TdxMarketSource {
    let (root, manifest) = snapshot_root(dir);
    TdxMarketSource::inspect(&root, &manifest, Utc::now()).unwrap()
}

fn units() -> UnitCurrencyEvidence {
    UnitCurrencyEvidence {
        price_currency: EvidenceValue::confirmed("CNY", "bound"),
        price_unit: EvidenceValue::confirmed("yuan", "bound"),
        volume_unit: EvidenceValue::confirmed("share", "bound"),
        amount_currency: EvidenceValue::confirmed("CNY", "bound"),
        amount_unit: EvidenceValue::confirmed("yuan", "bound"),
    }
}

fn envelope(
    batch: &MarketDataBatch,
    version_id: &str,
    parent_id: Option<&str>,
    generation_id: &str,
    manifest_sha256: &str,
) -> Value {
    let mut batch = batch.clone();
    batch.unit_currency = units();
    let (records, validation) = crate::publish::validate_and_deduplicate(&batch).unwrap();
    let version = EffectiveVersion {
        version_id: version_id.into(),
        symbol: batch.symbol.clone(),
        period: batch.period,
        adjustment: batch.adjustment,
        source: batch.provenance.source.clone(),
        records,
        published_at: Utc::now(),
        validation,
        unit_currency: batch.unit_currency.clone(),
        adjustment_evidence: batch.adjustment_evidence.clone(),
        yearly_derivation: None,
    };
    let mut raw = serde_json::to_value(&version).unwrap();
    raw["batch"] = serde_json::to_value(&batch).unwrap();
    let inputs = batch.provenance.snapshot_inputs.clone().unwrap_or_default();
    let member = |role: &str| {
        inputs
            .iter()
            .find(|i| i.role == role)
            .map(|i| i.member_sha256.clone())
            .unwrap_or_else(|| "00".repeat(32))
    };
    raw["compact_source"] = json!({
        "generation_id": generation_id,
        "generation_manifest_sha256": manifest_sha256,
        "day_member_sha256": member("day"),
        "security_event_sha256": member("gbbq-security-events"),
        "generation_evidence": {
            "manifest_sha256": manifest_sha256,
            "snapshot_inputs": inputs,
        }
    });
    if let Some(parent) = parent_id {
        raw["compact_parent_version_id"] = json!(parent);
    }
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    raw
}

fn published_entities(repo: &SqliteCompactMarket) -> HashSet<String> {
    repo.read(|c| {
        let mut stmt = c
            .prepare("SELECT DISTINCT entity FROM data_versions WHERE state=3 AND period=1")
            .map_err(storage)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        Ok(rows.into_iter().collect())
    })
    .unwrap()
}

fn head_version_id(repo: &SqliteCompactMarket, symbol: &str) -> Option<String> {
    repo.read(|c| {
        c.query_row(
            "SELECT v.version_id FROM effective_heads h JOIN data_versions v ON v.version_key=h.version_key WHERE h.entity=?1 AND h.period=1 AND h.adjustment=1",
            [symbol],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)
    })
    .unwrap()
}

fn accept_phase_task(
    repo: &SqliteCompactMarket,
    request: &str,
    items: &[(String, String)],
) -> String {
    let scopes: BTreeSet<String> = items.iter().map(|(_, scope)| scope.clone()).collect();
    let work = FrozenWork {
        items: items
            .iter()
            .map(|(item_id, scope)| ItemSpec {
                item_id: item_id.clone(),
                target: json!({"symbol": item_id.strip_prefix("item-").map(|s| s.to_string()).unwrap_or_default(), "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            })
            .collect(),
        scopes,
        shared_evidence: None,
    };
    let command = Command {
        key: CommandKey {
            operation: "collect_data".into(),
            dataset: "market.daily".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept(request, &command, &work, RUN, None).unwrap();
    let snapshot = match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot,
        _ => panic!("expected task outcome"),
    };
    let task_id = snapshot.task.task_id.clone();
    // Replayed accepts return the existing task; claim only when it is
    // still pending so phase reruns are idempotent.
    if snapshot.task.status == task_control::Status::Pending {
        repo.transition(&task_id, snapshot.task.state_revision, RUN, Change::Claim)
            .unwrap();
    }
    task_id
}

/// Ensures the item is running before commit. Replayed commits remain
/// idempotent on finished items, so callers may still invoke commit.
fn start_head_item(repo: &SqliteCompactMarket, head: &mut TaskHead, item_id: &str) {
    let binding = repo
        .task_item_binding(&head.task_id, item_id)
        .unwrap()
        .unwrap();
    let mut item = binding.item;
    match item.stage {
        task_control::Stage::Pending => {
            repo.transition_task_head(
                head,
                Some(&mut item),
                RUN,
                Change::StartItem(item_id.into()),
            )
            .unwrap_or_else(|e| {
                panic!(
                    "start {item_id} failed: {e}; head={:?}",
                    (head.status, head.state_revision, head.counts)
                )
            });
        }
        task_control::Stage::Running | task_control::Stage::Finished => {}
    }
}

struct Report {
    facts: Value,
}

impl Report {
    fn load(dir: &Path) -> Self {
        let path = dir.join("result.json");
        let facts = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| json!({}));
        Self { facts }
    }
    fn set(&mut self, key: &str, value: Value) {
        self.facts[key] = value;
    }
    fn done(&self, phase: &str) -> bool {
        self.facts
            .get("phases_completed")
            .and_then(Value::as_array)
            .is_some_and(|p| p.iter().any(|v| v.as_str() == Some(phase)))
    }
    fn complete(&mut self, phase: &str, dir: &Path) {
        let mut phases: Vec<Value> = self
            .facts
            .get("phases_completed")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !phases.iter().any(|v| v.as_str() == Some(phase)) {
            phases.push(json!(phase));
        }
        self.facts["phases_completed"] = json!(phases);
        self.save(dir);
    }
    fn save(&self, dir: &Path) {
        std::fs::write(
            dir.join("result.json"),
            serde_json::to_string_pretty(&self.facts).unwrap(),
        )
        .unwrap();
    }
}

#[allow(clippy::too_many_arguments)]
fn phase_initial(
    repo: &SqliteCompactMarket,
    session: &crate::official_market::MaterializeSession<'_>,
    generation_id: &str,
    manifest_sha256: &str,
    symbols: &[String],
    report: &mut Report,
    dir: &Path,
    db: &Path,
) {
    let done = published_entities(repo);
    let mut committed = done.len();
    let mut failed: Vec<String> = Vec::new();
    let mut bars_total: u64 = 0;
    let started = Instant::now();
    let mut rss_peak = rss_bytes();
    let chunk = TASK_ITEMS;
    let mut item_cursor = 0usize;
    for batch in symbols.chunks(chunk) {
        // Materialize first: a source/validation failure is an item-level
        // refusal in production shape — record it, never abort the run.
        let mut materialized: Vec<(String, MarketDataBatch)> = Vec::new();
        for symbol in batch {
            if done.contains(symbol) {
                continue;
            }
            match session
                .materialize_daily(symbol)
                .map_err(|e| e.to_string())
                .and_then(|b| {
                    crate::publish::validate_and_deduplicate(&b)
                        .map(|_| b)
                        .map_err(|e| e.to_string())
                }) {
                Ok(b) => materialized.push((symbol.clone(), b)),
                Err(e) => failed.push(format!("{symbol}: {e}")),
            }
        }
        if materialized.is_empty() {
            continue;
        }
        let items: Vec<(String, String)> = materialized
            .iter()
            .map(|(s, _)| {
                (
                    format!("item-{}", s),
                    task_control::legacy::scope_id(s, "day"),
                )
            })
            .collect();
        let task_id = accept_phase_task(repo, &format!("req-initial-{item_cursor}"), &items);
        item_cursor += 1;
        let mut head = repo.task_head(&task_id).unwrap();
        for ((symbol, batch_data), (item_id, _)) in materialized.iter().zip(&items) {
            start_head_item(repo, &mut head, item_id);
            bars_total += batch_data.bars.len() as u64;
            let version_id = format!("v1-{symbol}");
            let raw = envelope(
                batch_data,
                &version_id,
                None,
                generation_id,
                manifest_sha256,
            );
            repo.stage(&raw).unwrap();
            let candidate = repo.candidate(&version_id).unwrap();
            repo.mark_validated(&candidate).unwrap();
            let plan = PublishPlan {
                command: Publication {
                    publication_id: format!("pub-initial-{symbol}"),
                    version_id: version_id.clone(),
                    task_id: task_id.clone(),
                    item_id: item_id.clone(),
                    run_id: RUN.into(),
                    expected_revision: head.state_revision,
                    intent: PublishIntent::Daily {
                        expected_head: None,
                    },
                },
                record: candidate.clone(),
                required_heads: vec![(Scope::of(&candidate.version), None)],
                promote: None,
                base_digest: None,
            };
            repo.commit(&plan).unwrap();
            head.state_revision += 1;
            head.counts.running -= 1;
            head.counts.finished += 1;
            head.counts.succeeded += 1;
            committed += 1;
            rss_peak = rss_peak.max(rss_bytes());
            if committed.is_multiple_of(500) {
                eprintln!(
                    "[initial] {committed}/{} bars={bars_total} elapsed={:?} rss={}MiB",
                    symbols.len(),
                    started.elapsed(),
                    rss_peak / (1024 * 1024)
                );
                report.set("initial_progress", json!({"committed": committed, "bars": bars_total, "elapsed_s": started.elapsed().as_secs(), "rss_peak": rss_peak, "db_bytes": db_bytes(db), "wal_bytes": wal_bytes(db)}));
                report.save(dir);
            }
        }
    }
    let metrics = repo.checkpoint_idle().unwrap();
    report.set(
        "initial",
        json!({
            "symbols_committed": committed,
            "symbols_failed": failed,
            "bars_total": bars_total,
            "wall_s": started.elapsed().as_secs_f64(),
            "rss_peak_bytes": rss_peak,
            "db_bytes_before_checkpoint": db_bytes(db),
            "wal_before_checkpoint": wal_bytes(db),
            "checkpoint": format!("{metrics:?}"),
            "db_bytes_after_checkpoint": db_bytes(db),
            "wal_after_checkpoint": wal_bytes(db),
        }),
    );
}

fn count_table(repo: &SqliteCompactMarket, table: &str) -> i64 {
    repo.read(|c| {
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .map_err(storage)
    })
    .unwrap()
}

fn dbstat(repo: &SqliteCompactMarket) -> Value {
    repo.read(|c| {
        let mut stmt = c
            .prepare(
                "SELECT name, sum(pgsize), sum(unused) FROM dbstat GROUP BY name ORDER BY 2 DESC",
            )
            .map_err(storage)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        Ok(json!(
            rows.iter()
                .map(|(n, p, u)| json!({"name": n, "pgsize": p, "unused": u}))
                .collect::<Vec<_>>()
        ))
    })
    .unwrap_or_else(|_| json!("dbstat unavailable"))
}

fn phase_measure(repo: &SqliteCompactMarket, report: &mut Report, db: &Path) {
    let tables = [
        "segment_bars",
        "bar_segments",
        "version_checkpoint_clips",
        "version_delta_clips",
        "data_versions",
        "effective_heads",
        "evidence_bundles",
        "source_bindings",
        "market_update_results",
        "publication_results",
        "result_registry",
        "tasks",
        "task_items",
        "requests",
        "task_publications",
    ];
    let counts: BTreeMap<String, i64> = tables
        .iter()
        .map(|t| (t.to_string(), count_table(repo, t)))
        .collect();
    report.set(
        "measure",
        json!({
            "db_bytes": db_bytes(db),
            "wal_bytes": wal_bytes(db),
            "counts": counts,
            "dbstat": dbstat(repo),
        }),
    );
}

fn phase_nochange(repo: &SqliteCompactMarket, symbols: &[String], db: &Path) -> Value {
    let db_before = db_bytes(db);
    let wal_before = wal_bytes(db);
    let started = Instant::now();
    let mut items_done = 0usize;
    let published = published_entities(repo);
    let active: Vec<String> = symbols
        .iter()
        .filter(|s| published.contains(*s))
        .cloned()
        .collect();
    for (chunk_index, batch) in active.chunks(TASK_ITEMS).enumerate() {
        let items: Vec<(String, String)> = batch
            .iter()
            .map(|s| {
                (
                    format!("item-{s}"),
                    task_control::legacy::scope_id(s, "day"),
                )
            })
            .collect();
        let task_id = accept_phase_task(repo, &format!("req-nochange-{chunk_index}"), &items);
        let mut head = repo.task_head(&task_id).unwrap();
        for (item_id, _) in &items {
            let symbol = item_id.strip_prefix("item-").unwrap();
            start_head_item(repo, &mut head, item_id);
            let head_id = head_version_id(repo, symbol).unwrap();
            let base = repo.fixed(&head_id).unwrap();
            let plan = RangePlan {
                publication: PublishPlan {
                    command: Publication {
                        publication_id: format!("upd-nochange-{symbol}"),
                        version_id: head_id.clone(),
                        task_id: task_id.clone(),
                        item_id: item_id.clone(),
                        run_id: RUN.into(),
                        expected_revision: head.state_revision,
                        intent: PublishIntent::Daily {
                            expected_head: Some(head_id.clone()),
                        },
                    },
                    record: base.clone(),
                    required_heads: vec![(Scope::of(&base.version), Some(head_id.clone()))],
                    promote: None,
                    base_digest: None,
                },
                base: base.clone(),
                no_change: true,
                audit: json!({
                    "algorithm_version": "local-range-update-v1",
                    "range": {"start": "2026-09-15", "end": "2026-09-15"},
                    "evidence": {"same_qfq_basis_ref": "soak-basis", "revision_ref": "rev-none"}
                }),
                audit_payloads: Vec::new(),
            };
            repo.commit_range(&plan).unwrap();
            head.state_revision += 1;
            head.counts.running -= 1;
            head.counts.finished += 1;
            head.counts.succeeded += 1;
            items_done += 1;
        }
    }
    let metrics = repo.checkpoint_idle().unwrap();
    let growth = db_bytes(db).saturating_sub(db_before);
    json!({
        "items": items_done,
        "wall_s": started.elapsed().as_secs_f64(),
        "db_growth_bytes": growth,
        "bytes_per_item": if items_done > 0 { growth as f64 / items_done as f64 } else { 0.0 },
        "wal_before": wal_before,
        "wal_after_checkpoint": wal_bytes(db),
        "checkpoint": format!("{metrics:?}"),
    })
}

fn next_trading_day(day: NaiveDate) -> NaiveDate {
    use chrono::Datelike;
    let mut d = day + chrono::Duration::days(1);
    while matches!(d.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
        d += chrono::Duration::days(1);
    }
    d
}

fn fabricate_bar(base: &Bar, day: NaiveDate, salt: i64) -> Bar {
    let mut bar = base.clone();
    bar.date = day;
    bar.close += rust_decimal::Decimal::new(salt % 7, 2);
    bar.open += rust_decimal::Decimal::new(salt % 5, 2);
    bar.high = bar.high.max(bar.open).max(bar.close) + rust_decimal::Decimal::new(1, 2);
    bar.low = bar.low.min(bar.open).min(bar.close) - rust_decimal::Decimal::new(1, 2);
    bar
}

#[allow(clippy::too_many_arguments)]
fn phase_append(
    repo: &SqliteCompactMarket,
    symbols: &[String],
    db: &Path,
    day_index: u64,
    generation_id: &str,
    manifest_sha256: &str,
    key: &str,
) -> Value {
    let db_before = db_bytes(db);
    let started = Instant::now();
    let mut items_done = 0usize;
    let mut bars_written = 0usize;
    let mut wal_peak = wal_bytes(db);
    let published = published_entities(repo);
    let active: Vec<String> = symbols
        .iter()
        .filter(|s| published.contains(*s))
        .cloned()
        .collect();
    for (chunk_index, batch) in active.chunks(TASK_ITEMS).enumerate() {
        let items: Vec<(String, String)> = batch
            .iter()
            .map(|s| {
                (
                    format!("item-{s}"),
                    task_control::legacy::scope_id(s, "day"),
                )
            })
            .collect();
        let task_id = accept_phase_task(
            repo,
            &format!("req-{key}-{day_index}-{chunk_index}"),
            &items,
        );
        let mut head = repo.task_head(&task_id).unwrap();
        for (item_id, _) in &items {
            let symbol = item_id.strip_prefix("item-").unwrap();
            start_head_item(repo, &mut head, item_id);
            let head_id = head_version_id(repo, symbol).unwrap();
            let base = repo.fixed(&head_id).unwrap();
            let last = base.version.records.last().unwrap().clone();
            let new_day = next_trading_day(last.date);
            let new_bar = fabricate_bar(&last, new_day, items_done as i64);
            let mut batch = base.batch.clone();
            batch.bars.push(new_bar.clone());
            batch.provenance.acquired_at = Utc::now();
            let version_id = format!("v{day_index}-{symbol}-{key}");
            let raw = envelope(
                &batch,
                &version_id,
                Some(&head_id),
                generation_id,
                manifest_sha256,
            );
            repo.stage(&raw).unwrap();
            let candidate = repo.candidate(&version_id).unwrap();
            repo.mark_validated(&candidate).unwrap();
            let plan = PublishPlan {
                command: Publication {
                    publication_id: format!("upd-{key}-{day_index}-{symbol}"),
                    version_id: version_id.clone(),
                    task_id: task_id.clone(),
                    item_id: item_id.clone(),
                    run_id: RUN.into(),
                    expected_revision: head.state_revision,
                    intent: PublishIntent::Daily {
                        expected_head: Some(head_id.clone()),
                    },
                },
                record: candidate.clone(),
                required_heads: vec![(Scope::of(&candidate.version), Some(head_id.clone()))],
                promote: None,
                base_digest: None,
            };
            repo.commit(&plan).unwrap();
            head.state_revision += 1;
            head.counts.running -= 1;
            head.counts.finished += 1;
            head.counts.succeeded += 1;
            items_done += 1;
            bars_written += 1;
            wal_peak = wal_peak.max(wal_bytes(db));
        }
    }
    let metrics = repo.checkpoint_idle().unwrap();
    let growth = db_bytes(db).saturating_sub(db_before);
    json!({
        "day_index": day_index,
        "items": items_done,
        "symbols_active": active.len(),
        "bars_written": bars_written,
        "wall_s": started.elapsed().as_secs_f64(),
        "db_growth_bytes": growth,
        "wal_peak_bytes": wal_peak,
        "wal_after_checkpoint": wal_bytes(db),
        "checkpoint": format!("{metrics:?}"),
    })
}

fn phase_verify(repo: &SqliteCompactMarket, symbols: &[String]) -> Value {
    let published = published_entities(repo);
    let active: Vec<String> = symbols
        .iter()
        .filter(|s| published.contains(*s))
        .cloned()
        .collect();
    let mut verified = 0;
    for symbol in active.iter().step_by((active.len() / 50).max(1)).take(50) {
        let head_id = head_version_id(repo, symbol).unwrap();
        let fixed = repo.fixed(&head_id).unwrap();
        assert!(!fixed.version.records.is_empty());
        verified += 1;
    }
    let integrity: String = repo
        .read(|c| {
            c.query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .map_err(storage)
        })
        .unwrap();
    json!({"sampled_fixed_reads": verified, "integrity_check": integrity})
}

#[test]
#[ignore = "#259 S5B retained-generation capacity soak; run manually"]
fn s5b_retained_capacity() {
    assert!(
        std::env::var_os("X_DATA_MCP_UNSAFE_NONLOCAL_FS").is_some(),
        "Box-only acceptance: set X_DATA_MCP_UNSAFE_NONLOCAL_FS=1 (overlayfs)"
    );
    let dir = PathBuf::from(
        std::env::var("X_CAPACITY_DIR").unwrap_or_else(|_| "/home/box/capacity-s5b".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("market.db");
    let symbol_limit = env_u64("X_CAPACITY_SYMBOLS", 0);
    let soak_days = env_u64("X_CAPACITY_SOAK_DAYS", 250);
    let phases: HashSet<String> = std::env::var("X_CAPACITY_PHASES")
        .unwrap_or_else(|_| "initial,measure,nochange,append,soak,verify".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let all = |p: &str| phases.contains("all") || phases.contains(p);

    if !db.exists() {
        crate::sqlite_tasks::SqliteTasks::new(&db)
            .initialize19()
            .unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    let repo = SqliteCompactMarket::open(&db).unwrap();

    let symbols = symbols(symbol_limit);
    let source = make_source(&dir);
    let session = source.materialize_session();
    let generation_id = "gen-2";
    let manifest_sha256 = source.manifest_sha256().to_owned();

    let mut report = Report::load(&dir);
    report.set(
        "environment",
        json!({
            "symbols": symbols.len(),
            "soak_days": soak_days,
            "sqlite": rusqlite::version(),
            "profile": if cfg!(debug_assertions) {"debug"} else {"release"},
            "git": std::env::var("GIT_SHA").unwrap_or_else(|_| "working-tree".into()),
        }),
    );
    report.save(&dir);

    if all("initial") && !report.done("initial") {
        phase_initial(
            &repo,
            &session,
            generation_id,
            &manifest_sha256,
            &symbols,
            &mut report,
            &dir,
            &db,
        );
        report.complete("initial", &dir);
    }
    if all("measure") && !report.done("measure") {
        phase_measure(&repo, &mut report, &db);
        report.complete("measure", &dir);
    }
    if all("nochange") && !report.done("nochange") {
        let out = phase_nochange(&repo, &symbols, &db);
        report.set("nochange", out);
        report.complete("nochange", &dir);
    }
    if all("append") && !report.done("append") {
        let out = phase_append(
            &repo,
            &symbols,
            &db,
            1,
            generation_id,
            &manifest_sha256,
            "append",
        );
        report.set("append_day1", out);
        report.complete("append", &dir);
    }
    if all("soak") {
        let done = report
            .facts
            .get("soak_days_done")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let mut days: Vec<Value> = report
            .facts
            .get("soak_days")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for day in (done + 1)..=soak_days {
            let out = phase_append(
                &repo,
                &symbols,
                &db,
                day + 1,
                generation_id,
                &manifest_sha256,
                "soak",
            );
            days.push(out);
            report.set("soak_days_done", json!(day));
            report.set("soak_days", json!(days.clone()));
            report.save(&dir);
        }
        if done + days.len() as u64 >= soak_days {
            report.complete("soak", &dir);
        }
    }
    if all("verify") && !report.done("verify") {
        drop(repo);
        let repo = SqliteCompactMarket::open(&db).unwrap();
        let out = phase_verify(&repo, &symbols);
        report.set("verify", out);
        report.complete("verify", &dir);
    }
    eprintln!(
        "[s5b] done; report at {}",
        dir.join("result.json").display()
    );
}
