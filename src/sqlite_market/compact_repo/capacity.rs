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

fn input_dir() -> PathBuf {
    PathBuf::from(std::env::var("X_CAPACITY_INPUT_DIR").unwrap_or_else(|_| RETAINED.into()))
}

fn input_path(dir: &Path, canonical: &str, legacy: &str) -> PathBuf {
    let canonical = dir.join(canonical);
    if canonical.exists() {
        canonical
    } else {
        dir.join(legacy)
    }
}

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
    let zip = input_path(&input_dir(), "hsjday.zip", "official-hsjday.zip");
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
    if let Ok(path) = std::env::var("X_CAPACITY_SYMBOLS_FILE") {
        let wanted: BTreeSet<String> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        out.retain(|s| wanted.contains(s));
    }
    if limit > 0 {
        out.truncate(limit as usize);
    }
    out
}

fn snapshot_root(dir: &Path) -> (PathBuf, Vec<u8>) {
    let root = dir.join("snapshot");
    std::fs::create_dir_all(&root).unwrap();
    let input = input_dir();
    let inputs = [
        (
            input_path(&input, "base.zip", "official-base.zip"),
            "base.zip",
            "base_industry",
            "http://www.tdx.com.cn/products/data/data/dbf/base.zip",
        ),
        (
            input_path(&input, "hsjday.zip", "official-hsjday.zip"),
            "hsjday.zip",
            "day",
            "https://data.tdx.com.cn/vipdoc/hsjday.zip",
        ),
        (
            input_path(&input, "gbbq.zip", "official-gbbq.zip"),
            "gbbq.zip",
            "gbbq",
            "http://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
        ),
    ];
    let mut files = Vec::new();
    for (src, name, role, url) in inputs {
        let path = root.join(name);
        if !path.exists() {
            std::fs::hard_link(&src, &path).unwrap();
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

/// Typed collection candidate mirroring `envelope`'s inputs exactly —
/// same dedupe, unit override, generation binding — without the JSON
/// round-trip (#259 S5F).
fn collection_candidate(
    batch: &MarketDataBatch,
    version_id: &str,
    publication_id: &str,
    task_id: &str,
    item_id: &str,
    generation_id: &str,
    manifest_sha256: &str,
) -> crate::official_market::CollectionCandidate {
    let mut batch = batch.clone();
    batch.unit_currency = units();
    let (records, _) = crate::publish::validate_and_deduplicate(&batch).unwrap();
    let inputs = batch.provenance.snapshot_inputs.clone().unwrap_or_default();
    let member = |role: &str| {
        inputs
            .iter()
            .find(|i| i.role == role)
            .map(|i| i.member_sha256.clone())
            .unwrap_or_else(|| "00".repeat(32))
    };
    crate::official_market::CollectionCandidate {
        task_id: task_id.into(),
        item_id: item_id.into(),
        publication_id: publication_id.into(),
        version_id: version_id.into(),
        run_id: RUN.into(),
        symbol: batch.symbol.clone(),
        source: batch.provenance.source.clone(),
        bars: records,
        coverage: batch.coverage.clone(),
        unit_currency: batch.unit_currency.clone(),
        adjustment_evidence: batch.adjustment_evidence.clone(),
        published_at: Utc::now(),
        generation_id: generation_id.into(),
        generation_manifest_sha256: manifest_sha256.into(),
        day_member_sha256: member("day"),
        security_event_sha256: member("gbbq-security-events"),
        generation_evidence: json!({
            "manifest_sha256": manifest_sha256,
            "snapshot_inputs": inputs,
        }),
    }
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
        .unwrap_or_else(|| panic!("item {item_id} not bound to task {}", head.task_id));
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
    let mut materialize_ns = 0u128;
    let mut accept_ns = 0u128;
    let mut candidate_ns = 0u128;
    let mut commit_ns = 0u128;
    let mut inter_chunk_gap_ns = 0u128;
    let mut prior_chunk_end: Option<Instant> = None;
    let mut wal_peak = wal_bytes(db);
    let mut chunks = Vec::new();
    for (chunk_index, batch) in symbols.chunks(chunk).enumerate() {
        let chunk_started = Instant::now();
        let inter_chunk_gap_ms = prior_chunk_end
            .map(|end| chunk_started.saturating_duration_since(end).as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        inter_chunk_gap_ns += (inter_chunk_gap_ms * 1e6) as u128;
        // Materialize first: a source/validation failure is an item-level
        // refusal in production shape — record it, never abort the run.
        let mut materialized: Vec<(String, MarketDataBatch)> = Vec::new();
        let mut chunk_materialize_ns = 0u128;
        for symbol in batch {
            if done.contains(symbol) {
                continue;
            }
            let t = Instant::now();
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
            let elapsed = t.elapsed().as_nanos();
            materialize_ns += elapsed;
            chunk_materialize_ns += elapsed;
        }
        let mut chunk_accept_ns = 0u128;
        let mut chunk_candidate_ns = 0u128;
        let mut chunk_commit_ns = 0u128;
        if !materialized.is_empty() {
            let items: Vec<(String, String)> = materialized
                .iter()
                .map(|(s, _)| {
                    (
                        format!("item-{}", s),
                        task_control::legacy::scope_id(s, "day"),
                    )
                })
                .collect();
            let t = Instant::now();
            let task_id = accept_phase_task(repo, &format!("req-initial-{item_cursor}"), &items);
            chunk_accept_ns = t.elapsed().as_nanos();
            accept_ns += chunk_accept_ns;
            item_cursor += 1;
            // S5F production shape: one write transaction commits the whole
            // task — typed stage + publish + typed result + item finish.
            let t = Instant::now();
            let candidates: Vec<_> = materialized
                .iter()
                .zip(&items)
                .map(|((symbol, batch_data), (item_id, _))| {
                    bars_total += batch_data.bars.len() as u64;
                    collection_candidate(
                        batch_data,
                        &format!("v1-{symbol}"),
                        &format!("pub-initial-{symbol}"),
                        &task_id,
                        item_id,
                        generation_id,
                        manifest_sha256,
                    )
                })
                .collect();
            chunk_candidate_ns = t.elapsed().as_nanos();
            candidate_ns += chunk_candidate_ns;
            let t = Instant::now();
            let results = repo.commit_collection_batch(&task_id, &candidates).unwrap();
            chunk_commit_ns = t.elapsed().as_nanos();
            commit_ns += chunk_commit_ns;
            for (candidate, result) in candidates.iter().zip(results) {
                match result {
                    Ok(_) => committed += 1,
                    Err(e) => failed.push(format!("{}: {e}", candidate.symbol)),
                }
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
        wal_peak = wal_peak.max(wal_bytes(db));
        chunks.push(json!({
            "index": chunk_index,
            "symbols": batch.len(),
            "materialized": materialized.len(),
            "wall_ms": chunk_started.elapsed().as_secs_f64() * 1000.0,
            "materialize_ms": chunk_materialize_ns as f64 / 1e6,
            "accept_ms": chunk_accept_ns as f64 / 1e6,
            "candidate_ms": chunk_candidate_ns as f64 / 1e6,
            "commit_ms": chunk_commit_ns as f64 / 1e6,
            "inter_chunk_gap_ms": inter_chunk_gap_ms,
        }));
        prior_chunk_end = Some(Instant::now());
    }
    let metrics = repo.checkpoint_idle().unwrap();
    report.set(
        "initial",
        json!({
            "symbols_committed": committed,
            "symbols_failed": failed,
            "bars_total": bars_total,
            "wall_s": started.elapsed().as_secs_f64(),
            "materialize_ms": materialize_ns as f64 / 1e6,
            "prepare_ms": candidate_ns as f64 / 1e6,
            "accept_ms": accept_ns as f64 / 1e6,
            "commit_ms": commit_ns as f64 / 1e6,
            "inter_chunk_gap_ms": inter_chunk_gap_ns as f64 / 1e6,
            "chunks": chunks,
            "rss_peak_bytes": rss_peak,
            "db_bytes_before_checkpoint": db_bytes(db),
            "wal_before_checkpoint": wal_bytes(db),
            "wal_peak_bytes": wal_peak,
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
        "evidence_parts",
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

/// Accept an `update_data`/explicit-contract task without pre-claiming:
/// the batch commit claims inside its transaction (#259 S5G-6).
fn accept_update_phase_task(
    repo: &SqliteCompactMarket,
    request: &str,
    items: &[(
        String,
        String,
        String,
        String,
        crate::market_update::DateRange,
    )],
) -> String {
    let scopes: BTreeSet<String> = items
        .iter()
        .map(|(_, scope, _, _, _)| scope.clone())
        .collect();
    let work = FrozenWork {
        items: items
            .iter()
            .map(|(item_id, scope, base_id, base_digest, range)| ItemSpec {
                item_id: item_id.clone(),
                target: json!({
                    "symbol": item_id.strip_prefix("item-").unwrap_or_default(),
                    "period": "day",
                }),
                progress_unit: "security_period".into(),
                frozen_inputs: json!({
                    "base_version_id": base_id,
                    "base_integrity_sha256": base_digest,
                    "range": {"start": range.start, "end": range.end},
                }),
                scope_keys: BTreeSet::from([scope.clone()]),
            })
            .collect(),
        scopes,
        shared_evidence: None,
    };
    let command = Command {
        key: CommandKey {
            operation: "update_data".into(),
            dataset: "market.daily".into(),
            contract_version: crate::market_update::tasks::CONTRACT_VERSION.into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept(request, &command, &work, RUN, None).unwrap();
    match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    }
}

/// Timing-only provider wrapper for the capacity/profile harness. The real
/// provider still performs all acquisition work; this only splits provider
/// wall time from repository/task preparation time.
struct TimedRangeProvider<'a> {
    inner: &'a dyn crate::market_update::runner::RangeProvider,
    acquire_ns: std::cell::Cell<u128>,
    typed_acquire_ns: std::cell::Cell<u128>,
}

impl crate::market_update::runner::RangeProvider for TimedRangeProvider<'_> {
    fn acquire(
        &self,
        base: &crate::market_storage::compat::CheckedRecord,
        range: &crate::market_update::DateRange,
        candidate_id: &str,
    ) -> Result<crate::market_update::runner::Acquisition, crate::market_data::SourceError> {
        let started = Instant::now();
        let acquired = self.inner.acquire(base, range, candidate_id);
        self.acquire_ns
            .set(self.acquire_ns.get() + started.elapsed().as_nanos());
        acquired
    }

    fn acquire_typed(
        &self,
        facts: &crate::market_update::UpdateBaseFacts,
        base_window: &[Bar],
        range: &crate::market_update::DateRange,
    ) -> Result<
        Option<crate::market_update::runner::TypedAcquisition>,
        crate::market_data::SourceError,
    > {
        let started = Instant::now();
        let acquired = self.inner.acquire_typed(facts, base_window, range);
        self.typed_acquire_ns
            .set(self.typed_acquire_ns.get() + started.elapsed().as_nanos());
        acquired
    }
}

/// S5G-7 typed daily update through the production runner pieces:
/// `update_candidate` (binding checks + typed acquire — member read,
/// sha256, continuity probe, windowed base read) timed separately from
/// `commit_update_batch` (one write transaction per task). `unchanged`
/// runs the real `TdxRangeProvider`; `append` wraps it in
/// `FabricatedAppendProvider` so the stage+commit delta path is
/// exercised at scale against the unchanged snapshot.
fn phase_typed(
    repo: &SqliteCompactMarket,
    session: &crate::official_market::MaterializeSession<'_>,
    symbols: &[String],
    db: &Path,
    mode: &str,
) -> Value {
    let db_before = db_bytes(db);
    let started = Instant::now();
    let mut prepare_ns = 0u128;
    let mut spec_ns = 0u128;
    let mut typed_acquire_ns = 0u128;
    let mut commit_ns = 0u128;
    let mut accept_ns = 0u128;
    let mut inter_chunk_gap_ns = 0u128;
    let mut committed = 0usize;
    let mut append_items = 0usize;
    let mut nochange_items = 0usize;
    let mut metadata_items = 0usize;
    let mut fallback_ran = 0usize;
    let mut fallback_published = 0usize;
    let mut fallback_not_published = 0usize;
    let mut fallback_errors = 0usize;
    let mut fallback_ns = 0u128;
    let mut fallback_typed_acquire_ns = 0u128;
    let mut fallback_acquire_ns = 0u128;
    let mut failed_items = 0usize;
    let mut fallback_reasons: Vec<String> = Vec::new();
    let mut tasks = 0usize;
    let mut wal_peak = wal_bytes(db);
    let mut chunks = Vec::new();
    let mut prior_chunk_end: Option<Instant> = None;
    let published = published_entities(repo);
    let active: Vec<String> = symbols
        .iter()
        .filter(|s| published.contains(*s))
        .cloned()
        .collect();
    let runner = crate::market_update::runner::RangeRunner {
        repository: repo.clone(),
        run_id: RUN.into(),
    };
    let real = crate::official_market::TdxRangeProvider {
        session,
        generation_id: session.source().manifest_sha256().to_string(),
        published_at: Utc::now(),
    };
    let fabricated = crate::official_market::FabricatedAppendProvider {
        inner: crate::official_market::TdxRangeProvider {
            session,
            generation_id: session.source().manifest_sha256().to_string(),
            published_at: Utc::now(),
        },
        acquire_ns: std::cell::Cell::new(0),
    };
    let provider_inner: &dyn crate::market_update::runner::RangeProvider =
        if mode == "append" { &fabricated } else { &real };
    let timed_provider = TimedRangeProvider {
        inner: provider_inner,
        acquire_ns: std::cell::Cell::new(0),
        typed_acquire_ns: std::cell::Cell::new(0),
    };
    let provider: &dyn crate::market_update::runner::RangeProvider = &timed_provider;
    // Request ids are idempotent: a rerun of this phase after a previous
    // (possibly all-fallback) execution must not replay the old tasks.
    let run_tag = std::env::var("X_CAPACITY_RUN_TAG").unwrap_or_else(|_| "1".into());
    for (chunk_index, batch) in active.chunks(TASK_ITEMS).enumerate() {
        let chunk_started = Instant::now();
        let inter_chunk_gap_ms = prior_chunk_end
            .map(|end| chunk_started.saturating_duration_since(end).as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        inter_chunk_gap_ns += (inter_chunk_gap_ms * 1e6) as u128;
        let mut specs = Vec::with_capacity(batch.len());
        let spec_started = Instant::now();
        for symbol in batch {
            let head_id = head_version_id(repo, symbol).unwrap();
            let (last_day, base_digest): (i64, String) = repo
                .read(|c| {
                    c.query_row(
                        "SELECT v.last_day, lower(hex(v.integrity_digest)) FROM effective_heads h \
                         JOIN data_versions v ON v.version_key=h.version_key \
                         WHERE h.entity=?1 AND h.period=1 AND h.adjustment=1",
                        params![symbol],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .map_err(storage)
                })
                .unwrap();
            let last = compact_market::epoch_date(last_day).unwrap();
            specs.push((
                format!("item-{symbol}"),
                task_control::legacy::scope_id(symbol, "day"),
                head_id,
                base_digest,
                crate::market_update::DateRange {
                    start: last - chrono::Duration::days(30),
                    end: last + chrono::Duration::days(30),
                },
            ));
        }
        let chunk_spec_ns = spec_started.elapsed().as_nanos();
        spec_ns += chunk_spec_ns;
        let t = Instant::now();
        let task_id = accept_update_phase_task(
            repo,
            &format!("req-typed-{mode}-r{run_tag}-{chunk_index}"),
            &specs,
        );
        let chunk_accept_ns = t.elapsed().as_nanos();
        accept_ns += chunk_accept_ns;
        let item_ids: Vec<String> = specs.iter().map(|s| s.0.clone()).collect();
        let mut chunk_prepare_ns = 0u128;
        let mut chunk_typed_acquire_ns = 0u128;
        let mut prepared = Vec::with_capacity(item_ids.len());
        let mut fallbacks = Vec::new();
        for item_id in &item_ids {
            let provider_before = timed_provider.typed_acquire_ns.get();
            let t = Instant::now();
            let candidate = runner.update_candidate(&task_id, item_id, provider);
            let elapsed = t.elapsed().as_nanos();
            let provider_elapsed = timed_provider
                .typed_acquire_ns
                .get()
                .saturating_sub(provider_before);
            typed_acquire_ns += provider_elapsed;
            chunk_typed_acquire_ns += provider_elapsed;
            let candidate_ns = elapsed.saturating_sub(provider_elapsed);
            prepare_ns += candidate_ns;
            chunk_prepare_ns += candidate_ns;
            match candidate {
                Ok(Some((item, _))) => {
                    match &item.prepared {
                        crate::market_update::UpdateBatchPrepared::NoChange { .. } => {
                            nochange_items += 1;
                        }
                        crate::market_update::UpdateBatchPrepared::Append { metadata, .. } => {
                            append_items += 1;
                            metadata_items += usize::from(metadata.is_some());
                        }
                    }
                    prepared.push(item);
                }
                Ok(None) => {
                    if fallback_reasons.len() < 8 {
                        fallback_reasons.push(format!("{item_id}: typed-contract declined"));
                    }
                    fallbacks.push(item_id.clone());
                }
                Err(error) => {
                    if fallback_reasons.len() < 8 {
                        fallback_reasons.push(format!("{item_id}: {error}"));
                    }
                    fallbacks.push(item_id.clone());
                }
            }
        }
        let mut chunk_commit_ns = 0u128;
        if !prepared.is_empty() {
            let t = Instant::now();
            let results = repo.commit_update_batch(&task_id, &prepared).unwrap();
            chunk_commit_ns = t.elapsed().as_nanos();
            commit_ns += chunk_commit_ns;
            for (item, result) in prepared.iter().zip(results) {
                match result {
                    Ok(_) => committed += 1,
                    Err(_) => {
                        failed_items += 1;
                        fallbacks.push(item.item_id.clone());
                    }
                }
            }
        }
        let chunk_fallback_items = fallbacks.len();
        let mut chunk_fallback_ns = 0u128;
        let mut chunk_fallback_typed_acquire_ns = 0u128;
        let mut chunk_fallback_acquire_ns = 0u128;
        for item_id in fallbacks {
            let typed_before = timed_provider.typed_acquire_ns.get();
            let acquire_before = timed_provider.acquire_ns.get();
            let t = Instant::now();
            match runner.run_item(&task_id, &item_id, provider) {
                Ok(crate::market_update::runner::ItemExecution::Published(_)) => {
                    fallback_published += 1;
                }
                Ok(crate::market_update::runner::ItemExecution::NotPublished(_)) => {
                    fallback_not_published += 1;
                }
                Err(error) => {
                    fallback_errors += 1;
                    if fallback_reasons.len() < 8 {
                        fallback_reasons.push(format!("{item_id}: fallback failed: {error}"));
                    }
                }
            }
            let elapsed = t.elapsed().as_nanos();
            let typed_delta = timed_provider
                .typed_acquire_ns
                .get()
                .saturating_sub(typed_before);
            let acquire_delta = timed_provider
                .acquire_ns
                .get()
                .saturating_sub(acquire_before);
            fallback_ns += elapsed;
            fallback_typed_acquire_ns += typed_delta;
            fallback_acquire_ns += acquire_delta;
            chunk_fallback_ns += elapsed;
            chunk_fallback_typed_acquire_ns += typed_delta;
            chunk_fallback_acquire_ns += acquire_delta;
            fallback_ran += 1;
        }
        tasks += 1;
        wal_peak = wal_peak.max(wal_bytes(db));
        chunks.push(json!({
            "index": chunk_index,
            "items": batch.len(),
            "wall_ms": chunk_started.elapsed().as_secs_f64() * 1000.0,
            "spec_ms": chunk_spec_ns as f64 / 1e6,
            "accept_ms": chunk_accept_ns as f64 / 1e6,
            "prepare_ms": chunk_prepare_ns as f64 / 1e6,
            "acquire_ms": chunk_typed_acquire_ns as f64 / 1e6,
            "commit_ms": chunk_commit_ns as f64 / 1e6,
            "fallback_items": chunk_fallback_items,
            "fallback_ms": chunk_fallback_ns as f64 / 1e6,
            "fallback_typed_acquire_ms": chunk_fallback_typed_acquire_ns as f64 / 1e6,
            "fallback_envelope_acquire_ms": chunk_fallback_acquire_ns as f64 / 1e6,
            "inter_chunk_gap_ms": inter_chunk_gap_ms,
        }));
        prior_chunk_end = Some(Instant::now());
    }
    let metrics = repo.checkpoint_idle().unwrap();
    json!({
        "mode": mode,
        "items_committed": committed,
        "items_failed_in_batch": failed_items,
        "typed_nochange_items": nochange_items,
        "typed_append_items": append_items,
        "typed_metadata_items": metadata_items,
        "fallback_items": fallback_ran,
        "fallback_published_items": fallback_published,
        "fallback_not_published_items": fallback_not_published,
        "fallback_error_items": fallback_errors,
        "fallback_ms": fallback_ns as f64 / 1e6,
        "fallback_typed_acquire_ms": fallback_typed_acquire_ns as f64 / 1e6,
        "fallback_envelope_acquire_ms": fallback_acquire_ns as f64 / 1e6,
        "fallback_reasons": fallback_reasons,
        "symbols_active": active.len(),
        "tasks": tasks,
        "wall_s": started.elapsed().as_secs_f64(),
        "spec_ms": spec_ns as f64 / 1e6,
        "accept_ms": accept_ns as f64 / 1e6,
        "prepare_ms": prepare_ns as f64 / 1e6,
        "acquire_ms": typed_acquire_ns as f64 / 1e6,
        "provider_acquire_ms": fabricated.acquire_ns.get() as f64 / 1e6,
        "commit_ms": commit_ns as f64 / 1e6,
        "inter_chunk_gap_ms": inter_chunk_gap_ns as f64 / 1e6,
        "chunks": chunks,
        "per_item_ms": started.elapsed().as_secs_f64() * 1000.0 / active.len().max(1) as f64,
        "db_growth_bytes": db_bytes(db).saturating_sub(db_before),
        "wal_peak_bytes": wal_peak,
        "wal_after_checkpoint": wal_bytes(db),
        "checkpoint": format!("{metrics:?}"),
        "peak_rss_bytes": rss_bytes(),
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

/// Ops-first adapter day: uses the incremental contract
/// (`stage_ops_delta` + `validate_staged` + `commit_ops_update`) — the
/// proposed production update shape. Task/item/binding semantics are
/// identical to `phase_append`.
fn phase_append_ops(
    repo: &SqliteCompactMarket,
    symbols: &[String],
    db: &Path,
    day_index: u64,
    item_limit: u64,
) -> Value {
    let db_before = db_bytes(db);
    let started = Instant::now();
    let mut items_done = 0usize;
    let mut wal_peak = wal_bytes(db);
    let published = published_entities(repo);
    let all_active: Vec<String> = symbols
        .iter()
        .filter(|s| published.contains(*s))
        .cloned()
        .collect();
    let limit = if item_limit > 0 {
        (item_limit as usize).min(all_active.len())
    } else {
        all_active.len()
    };
    let active: Vec<String> = if limit == all_active.len() {
        all_active.clone()
    } else {
        let offset = ((day_index - 1) as usize * limit) % all_active.len();
        all_active
            .iter()
            .cycle()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    };
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
        let task_id =
            accept_phase_task(repo, &format!("req-ops-{day_index}-{chunk_index}"), &items);
        let mut head = repo.task_head(&task_id).unwrap();
        let task_items: Vec<String> = repo
            .task(&task_id)
            .unwrap()
            .items
            .iter()
            .map(|i| i.spec.item_id.clone())
            .collect();
        for item_id in &task_items {
            let symbol = item_id.strip_prefix("item-").unwrap();
            start_head_item(repo, &mut head, item_id);
            let done: bool = repo
                .read(|c| {
                    c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM market_update_results \
                         WHERE task_id=?1 AND item_id=?2)",
                        params![task_id, item_id],
                        |r| r.get(0),
                    )
                    .map_err(storage)
                })
                .unwrap();
            if !done {
                let head_id = head_version_id(repo, symbol).unwrap();
                let version_id = format!("v{day_index}-{symbol}-ops");
                let last_day: i64 = repo
                    .read(|c| {
                        c.query_row(
                            "SELECT v.last_day FROM effective_heads h \
                             JOIN data_versions v ON v.version_key=h.version_key \
                             WHERE h.entity=?1 AND h.period=1 AND h.adjustment=1",
                            [symbol],
                            |r| r.get(0),
                        )
                        .map_err(storage)
                    })
                    .unwrap();
                let day = next_trading_day(compact_market::epoch_date(last_day).unwrap());
                let bar = Bar {
                    date: day,
                    open: rust_decimal::Decimal::new(10000, 2),
                    high: rust_decimal::Decimal::new(10025, 2),
                    low: rust_decimal::Decimal::new(9975, 2),
                    close: rust_decimal::Decimal::new(10010, 2),
                    volume: Some(rust_decimal::Decimal::new(1_500_000, 0)),
                    amount: Some(rust_decimal::Decimal::new(150_225_000, 0)),
                    r7_raw: None,
                };
                repo.stage_ops_delta(symbol, &head_id, &version_id, &[bar], &[])
                    .unwrap();
                repo.validate_staged(&version_id).unwrap();
                repo.commit_ops_update(
                    &format!("upd-ops-{day_index}-{symbol}"),
                    &task_id,
                    item_id,
                    &head_id,
                    &version_id,
                    head.state_revision,
                    RUN,
                    &task_control::legacy::scope_id(symbol, "day"),
                    None,
                )
                .unwrap();
                head.state_revision += 1;
                head.counts.running -= 1;
                head.counts.finished += 1;
                head.counts.succeeded += 1;
            }
            items_done += 1;
            wal_peak = wal_peak.max(wal_bytes(db));
        }
    }
    let metrics = repo.checkpoint_idle().unwrap();
    json!({
        "day_index": day_index,
        "mode": "ops",
        "items": items_done,
        "symbols_active": active.len(),
        "wall_s": started.elapsed().as_secs_f64(),
        "db_growth_bytes": db_bytes(db).saturating_sub(db_before),
        "wal_peak_bytes": wal_peak,
        "wal_after_checkpoint": wal_bytes(db),
        "checkpoint": format!("{metrics:?}"),
    })
}

/// Primitive soak day: identical physical artifacts to the adapter path
/// (same segment/delta/version/result/task rows, same binding and head
/// semantics) but skips the harness-side `fixed()`/envelope CPU that does
/// not change stored bytes. Used for the long growth-ratio run where
/// wall-clock is reported but not the metric under test.
fn soak_item_primitive(
    repo: &SqliteCompactMarket,
    symbol: &str,
    task_id: &str,
    item_id: &str,
    day_index: u64,
    expected_revision: u64,
) -> Result<bool, Error> {
    repo.write(|c| {
        let result_id = format!("upd-soak-{day_index}-{symbol}");
        let version_id = format!("v{day_index}-{symbol}-soak");
        let committed: bool = c
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM result_registry WHERE result_id=?1) \
                 OR EXISTS(SELECT 1 FROM market_update_results WHERE task_id=?2 AND item_id=?3) \
                 OR EXISTS(SELECT 1 FROM publication_results WHERE task_id=?2 AND item_id=?3)",
                params![result_id, task_id, item_id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if committed {
            return Ok(false);
        }
        let (parent_key, parent_id, record_count, last_day, sbd, evd, cov): (
            i64,
            String,
            i64,
            i64,
            Vec<u8>,
            Vec<u8>,
            i64,
        ) = c
            .query_row(
                "SELECT h.version_key, v.version_id, v.record_count, v.last_day, \
                 v.source_binding_digest, v.evidence_digest, v.coverage_status \
                 FROM effective_heads h JOIN data_versions v ON v.version_key=h.version_key \
                 WHERE h.entity=?1 AND h.period=1 AND h.adjustment=1",
                [symbol],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .map_err(storage)?;
        let source_binding_digest: Digest = sbd.try_into().map_err(|_| integrity("bad binding"))?;
        let evidence_arr: Digest = evd.try_into().map_err(|_| integrity("bad evidence"))?;
        let day = next_trading_day(compact_market::epoch_date(last_day).map_err(integrity)?);
        let trade_day = compact_market::epoch_day(day);
        let salt = (day_index as i64) * 61 + i64::from(symbol.as_bytes()[symbol.len() - 1]);
        let open = 1_000 + (salt % 900).abs() * 10;
        let bar = CompactBar {
            trade_day,
            open_cents: open,
            high_cents: open + 25,
            low_cents: open - 25,
            close_cents: open + 10,
            volume: Some(1_000_000 + (salt % 9_000_000).abs()),
            amount: Some((100_000_000 + (salt % 50_000_000).abs()).to_string()),
            r7_raw: Some(0),
        };
        let segment = compact_v19::insert_segment(c, symbol, Period::Day, &[bar])?;
        let ops = vec![StoredDelta {
            range_start_day: trade_day,
            range_end_day: trade_day,
            kind: StoredDeltaKind::Upsert(StoredClip {
                segment_key: segment.key,
                clip: crate::compact_market::SegmentClip {
                    segment_digest: segment.digest,
                    clip_start_day: trade_day,
                    clip_end_day: trade_day,
                    row_count: 1,
                },
            }),
        }];
        let coverage_status = match cov {
            1 => crate::market_data::CoverageStatus::Complete,
            2 => crate::market_data::CoverageStatus::Incomplete,
            _ => crate::market_data::CoverageStatus::Unknown,
        };
        let draft = compact_versions::VersionDraft {
            version_id: version_id.clone(),
            entity: symbol.into(),
            period: Period::Day,
            source_binding_digest,
            coverage_status,
            evidence_digest: evidence_arr,
            created_at: Utc::now().to_rfc3339(),
            yearly_lineage: None,
        };
        // Resume-safe: a killed day may leave a staged/validated version
        // behind; reuse it rather than restaging.
        let key: i64 = c
            .query_row(
                "SELECT version_key FROM data_versions WHERE version_id=?1",
                [&version_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?
            .map(Ok)
            .unwrap_or_else(|| compact_versions::stage_delta(c, &draft, parent_key, &ops))?;
        let state: i64 = c
            .query_row(
                "SELECT state FROM data_versions WHERE version_key=?1",
                [key],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if state == 1 {
            compact_versions::mark_validated(c, key)?;
        }
        let prefix_bytes = record_count as u64 * 32;
        let evidence = compact_versions::put_evidence(
            c,
            EvidenceKind::Continuity,
            &json!({
                "mode": "append",
                "base_version_id": parent_id,
                "source_binding_digest": hex_bytes(&source_binding_digest),
                "prefix_bytes": prefix_bytes,
            }),
        )?;
        compact_results::commit_market_update(
            c,
            &compact_results::MarketUpdateCommit {
                result_id,
                task_id: task_id.into(),
                item_id: item_id.into(),
                base_version_key: parent_key,
                result_version_key: key,
                range_start_day: trade_day,
                range_end_day: trade_day,
                algorithm_version: UPDATE_ALGORITHM.into(),
                source_binding_digest,
                evidence_digest: evidence,
                expected_task_revision: expected_revision,
                published_at: Some(Utc::now()),
                head: compact_results::HeadExpectation::Version(parent_key),
                required_heads: vec![compact_results::RequiredHead {
                    entity: symbol.into(),
                    period: Period::Day,
                    expected: Some(parent_key),
                }],
                binding: compact_results::BindingCheck {
                    run_id: RUN.into(),
                    entity: symbol.into(),
                    period: Period::Day,
                    occupancy_key: task_control::legacy::scope_id(symbol, "day"),
                },
                change: compact_results::MarketUpdateChange::Append { prefix_bytes, ops },
            },
        )?;
        Ok(true)
    })
}

#[allow(clippy::too_many_arguments)]
fn phase_soak_primitive(
    repo: &SqliteCompactMarket,
    symbols: &[String],
    db: &Path,
    day_index: u64,
    item_limit: u64,
) -> Value {
    let db_before = db_bytes(db);
    let started = Instant::now();
    let mut items_done = 0usize;
    let mut wal_peak = wal_bytes(db);
    let published = published_entities(repo);
    let all_active: Vec<String> = symbols
        .iter()
        .filter(|s| published.contains(*s))
        .cloned()
        .collect();
    // Rotate the per-day item window across the whole published set so
    // chain depth and per-item cost stay representative instead of
    // repeatedly hitting the deepest-history symbols.
    let limit = if item_limit > 0 {
        (item_limit as usize).min(all_active.len())
    } else {
        all_active.len()
    };
    let active: Vec<String> = if limit == all_active.len() {
        all_active.clone()
    } else {
        let offset = ((day_index - 1) as usize * limit) % all_active.len();
        all_active
            .iter()
            .cycle()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    };
    // Finish leftover non-terminal tasks from killed runs first: their
    // scope occupancy blocks new accepts (production-faithful drain).
    let stale: Vec<String> = repo
        .read(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT task_id FROM tasks WHERE status IN \
                     ('pending','running','cancel_requested') ORDER BY created_at",
                )
                .map_err(storage)?;
            let rows = stmt
                .query_map([], |r| r.get(0))
                .map_err(storage)?
                .collect::<Result<Vec<String>, _>>()
                .map_err(storage)?;
            Ok(rows)
        })
        .unwrap();
    for task_id in &stale {
        let mut head = repo.task_head(task_id).unwrap();
        if head.status == task_control::Status::Pending {
            repo.transition_task_head(
                &mut head,
                Option::<&mut task_control::TaskItem>::None,
                RUN,
                Change::Claim,
            )
            .unwrap();
        }
        let task_items: Vec<String> = repo
            .task(task_id)
            .unwrap()
            .items
            .iter()
            .map(|i| i.spec.item_id.clone())
            .collect();
        for item_id in &task_items {
            let symbol = item_id.strip_prefix("item-").unwrap();
            start_head_item(repo, &mut head, item_id);
            if soak_item_primitive(
                repo,
                symbol,
                task_id,
                item_id,
                day_index,
                head.state_revision,
            )
            .unwrap()
            {
                head.state_revision += 1;
                head.counts.running -= 1;
                head.counts.finished += 1;
                head.counts.succeeded += 1;
            }
            wal_peak = wal_peak.max(wal_bytes(db));
        }
    }
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
            &format!("req-soakp-{day_index}-{chunk_index}"),
            &items,
        );
        let mut head = repo.task_head(&task_id).unwrap();
        // Iterate the task's own item set: on resume the stored task may
        // carry a different window than the locally recomputed one.
        let task_items: Vec<String> = repo
            .task(&task_id)
            .unwrap()
            .items
            .iter()
            .map(|i| i.spec.item_id.clone())
            .collect();
        for item_id in &task_items {
            let item_id: &str = item_id;
            let symbol = item_id.strip_prefix("item-").unwrap();
            start_head_item(repo, &mut head, item_id);
            if soak_item_primitive(
                repo,
                symbol,
                &task_id,
                item_id,
                day_index,
                head.state_revision,
            )
            .unwrap()
            {
                head.state_revision += 1;
                head.counts.running -= 1;
                head.counts.finished += 1;
                head.counts.succeeded += 1;
            }
            items_done += 1;
            wal_peak = wal_peak.max(wal_bytes(db));
        }
    }
    let metrics = repo.checkpoint_idle().unwrap();
    let growth = db_bytes(db).saturating_sub(db_before);
    json!({
        "day_index": day_index,
        "mode": "primitive",
        "items": items_done,
        "symbols_active": active.len(),
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
            .initialize_compact()
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
            "input_dir": input_dir().display().to_string(),
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
        let soak_mode = std::env::var("X_CAPACITY_SOAK_MODE").unwrap_or_else(|_| "adapter".into());
        let soak_items = env_u64("X_CAPACITY_SOAK_ITEMS", 0);
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
            let out = if soak_mode == "ops" {
                phase_append_ops(&repo, &symbols, &db, day + 1, soak_items)
            } else if soak_mode == "primitive" {
                phase_soak_primitive(&repo, &symbols, &db, day + 1, soak_items)
            } else {
                phase_append(
                    &repo,
                    &symbols,
                    &db,
                    day + 1,
                    generation_id,
                    &manifest_sha256,
                    "soak",
                )
            };
            eprintln!(
                "[soak] day {day}/{soak_days} growth={} wall={}s",
                out["db_growth_bytes"], out["wall_s"]
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
    if all("typed") || all("typed_unchanged") || all("typed_append") {
        // S5G-7: unchanged first (no-change commits), then append —
        // the fabricated tails move the heads one fabricated day. Individual
        // phase names allow a retained generation-A DB to be measured against
        // a later generation-B snapshot without rerunning the fabricated day.
        if (all("typed") || all("typed_unchanged")) && !report.done("typed_unchanged") {
            let out = phase_typed(&repo, &session, &symbols, &db, "unchanged");
            report.set("typed_unchanged", out);
            report.complete("typed_unchanged", &dir);
        }
        if (all("typed") || all("typed_append")) && !report.done("typed_append") {
            let out = phase_typed(&repo, &session, &symbols, &db, "append");
            report.set("typed_append", out);
            report.complete("typed_append", &dir);
        }
        if all("typed") {
            report.complete("typed", &dir);
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
