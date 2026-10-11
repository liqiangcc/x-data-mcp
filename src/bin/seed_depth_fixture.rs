//! Development fixture only (TASK-XP-026 S2, ex-XP-025 H): grow the delta
//! chain depth of published daily heads on a PRIVATE COPY of a compact
//! database by committing synthetic appends through the production typed
//! update path — task acceptance (`RangeContract`), `commit_update_batch`
//! (`stage_ops_delta` → `mark_validated_delta` → `commit_ops_update`) and
//! the effective-head CAS all run unchanged.
//!
//! Guards (never a production path):
//! - `--target-is-a-copy` must be passed explicitly;
//! - the resolved database path may not live under `/opt/` or the frozen
//!   `/root/x-perf-g8/svc/db/` store;
//! - the target must already be an initialized compact database;
//! - every written version carries `synthetic: true` in its generation
//!   evidence bundle, a synthetic coverage reason and a synthetic
//!   `basis_ref` in the result's continuity evidence.
//!
//! `--mode rebase-real` (XP-033 S9) builds the same chain depth without
//! synthetic dates: it publishes the real head's `B[..n-depth]` prefix as
//! a new ROOT CHECKPOINT through the production envelope update path
//! (`stage` — no `compact_parent_version_id` → `stage_checkpoint` with a
//! NULL parent — then `Publisher::prepare`/`commit` bound by the
//! effective-head CAS, the same shape as the corporate-action full
//! rewrite `continuity_mode=3`/`Full`), then replays the real-date tail
//! `B[n-depth..]` through the typed append path. Every produced bar date
//! is ≤ today, so yearly derivation can run on deep chains.
//!
//! usage: seed_depth_fixture <database> --entities <ex.code[,ex.code...]>
//!          --depth <1..=256> --target-is-a-copy [--run-id <id>]
//!          [--request-prefix <p>] [--mode append-synthetic|rebase-real]
//!          [--changed-last]
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use x_data_mcp::{
    market_data::{
        AdjustmentEvidence, Bar, CoverageEvidence, MarketDataBatch, Period, UnitCurrencyEvidence,
    },
    market_storage::{MarketRepository, Publication, PublishIntent, Publisher, Scope},
    market_update::{
        TypedVersionMetadata, UpdateBatchItem, UpdateBatchPrepared,
        publication::RangeRepository,
        tasks::{EntityRange, ExplicitRanges, Operation, RangeContract},
    },
    official_market::DailyCollectionRepository,
    publish::EffectiveVersion,
    sqlite_market::backend::MarketBackend,
    sqlite_tasks::SqliteTasks,
    task_control::{
        Change, RequestOutcome, TaskRepository, legacy::scope_id, service::TaskControl,
    },
};

const FROZEN_STORE: &str = "/root/x-perf-g8/svc/db";
/// Mirrors `crate::official_market::SECURITY_EVENTS_ROLE` (pub(crate));
/// the role label is part of the stored generation-evidence shape.
const EVENTS_ROLE: &str = "gbbq-security-events";

fn usage() -> Box<dyn std::error::Error> {
    "usage: seed_depth_fixture <database> --entities <ex.code[,ex.code...]> --depth <1..=256> --target-is-a-copy [--run-id <id>] [--request-prefix <p>] [--mode append-synthetic|rebase-real] [--changed-last]"
        .into()
}

/// How the fixture grows the daily chain.
enum Mode {
    /// Default (S2): append generated bars one day past the current head.
    AppendSynthetic,
    /// XP-033 S9: rebase the real head onto a new root checkpoint, then
    /// replay its real-date tail as single-bar typed appends.
    RebaseReal { changed_last: bool },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn opt(args: &[std::ffi::OsString], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a.as_os_str() == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.to_str().map(String::from))
}

/// Guard: explicit copy confirmation plus path rejections. The lexical
/// and the canonicalized forms are both checked so a nonexistent path
/// under a guarded prefix still fails and a symlink into it resolves
/// first.
fn guarded_db_path(db: &str, copy_confirmed: bool) -> Result<PathBuf, String> {
    if !copy_confirmed {
        return Err(
            "refusing without --target-is-a-copy: the database must be a private copy".into(),
        );
    }
    let raw = Path::new(db);
    let lexical = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(raw)
    };
    for candidate in [
        lexical.clone(),
        std::fs::canonicalize(&lexical).unwrap_or(lexical),
    ] {
        if candidate.starts_with("/opt") || candidate.starts_with(FROZEN_STORE) {
            return Err(format!("refusing guarded path {}", candidate.display()));
        }
    }
    let resolved = std::fs::canonicalize(Path::new(db))
        .map_err(|e| format!("database copy not found: {db} ({e})"))?;
    if !resolved.is_file() {
        return Err(format!("database copy is not a file: {db}"));
    }
    Ok(resolved)
}

fn storage(e: impl ToString) -> x_data_mcp::market_storage::Error {
    x_data_mcp::market_storage::Error::Storage(e.to_string())
}

/// Per-entity seeding state: the current published head plus the source
/// bindings a synthetic generation must diverge from / carry forward.
struct EntityState {
    symbol: String,
    head_version_id: String,
    last_day: chrono::NaiveDate,
    record_count: u32,
    integrity_sha256: String,
    source: String,
    unit_currency: UnitCurrencyEvidence,
    adjustment_evidence: AdjustmentEvidence,
    security_event_sha256: String,
    depth: i64,
    /// `--mode rebase-real` only: the pre-rebase head's identity, its
    /// full slim envelope (`compact_source`/`batch` are carried into the
    /// new root checkpoint), its verified bar series, and the real-date
    /// tail still waiting to be re-appended.
    original_head_version_id: String,
    original_raw: Value,
    original_bars: Vec<Bar>,
    pending_tail: VecDeque<Bar>,
}

fn entity_state(backend: &MarketBackend, symbol: &str) -> Result<EntityState, String> {
    let scope = Scope {
        symbol: symbol.into(),
        period: Period::Day,
    };
    let head = backend
        .effective_head(&scope)
        .map_err(|e| format!("effective head read failed for {symbol}: {e}"))?
        .ok_or_else(|| format!("{symbol} has no published daily head to append onto"))?;
    let facts = RangeRepository::update_base_facts(backend, &head)
        .map_err(|e| format!("base facts read failed for {symbol}: {e}"))?
        .ok_or_else(|| format!("{symbol}: backend cannot serve typed base facts"))?;
    let depth = backend
        .read(|c| {
            c.query_row(
                "SELECT delta_depth FROM data_versions WHERE version_id=?1",
                [&head],
                |r| r.get::<_, i64>(0),
            )
            .map_err(storage)
        })
        .map_err(|e| format!("delta depth read failed for {symbol}: {e}"))?;
    // The adjustment evidence is not part of typed base facts; read it
    // once per entity through the verified materialization path.
    let version = backend
        .fixed(&head)
        .map_err(|e| format!("head materialization failed for {symbol}: {e}"))?;
    Ok(EntityState {
        symbol: symbol.into(),
        head_version_id: head,
        last_day: facts.last_day,
        record_count: facts.record_count,
        integrity_sha256: facts.integrity_sha256,
        source: facts.source,
        unit_currency: facts.unit_currency,
        adjustment_evidence: version.version().adjustment_evidence.clone(),
        security_event_sha256: facts.security_event_sha256,
        depth,
        original_head_version_id: String::new(),
        original_raw: Value::Null,
        original_bars: Vec::new(),
        pending_tail: VecDeque::new(),
    })
}

/// Refresh the head-only fields a committed append changed.
fn refresh_head(backend: &MarketBackend, e: &mut EntityState) -> Result<(), String> {
    let facts = RangeRepository::update_base_facts(backend, &e.head_version_id)
        .map_err(|e| format!("base facts re-read failed: {e}"))?
        .ok_or_else(|| format!("{}: committed head lacks typed facts", e.symbol))?;
    e.last_day = facts.last_day;
    e.record_count = facts.record_count;
    e.integrity_sha256 = facts.integrity_sha256;
    e.depth = backend
        .read(|c| {
            c.query_row(
                "SELECT delta_depth FROM data_versions WHERE version_id=?1",
                [&e.head_version_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(storage)
        })
        .map_err(|e| format!("delta depth re-read failed: {e}"))?;
    Ok(())
}

/// The bar the next append step commits for `e`: a generated bar one
/// day past the head (append-synthetic), or the next real-date tail bar
/// (rebase-real). Pure peek — `append_item` pops it.
fn next_day(e: &EntityState, mode: &Mode) -> chrono::NaiveDate {
    match mode {
        Mode::AppendSynthetic => e.last_day + chrono::Duration::days(1),
        Mode::RebaseReal { .. } => e
            .pending_tail
            .front()
            .map(|bar| bar.date)
            .unwrap_or_else(|| e.last_day + chrono::Duration::days(1)),
    }
}

/// `--mode rebase-real` read phase: materialize the published head's
/// verified bar series `B` (n bars, requires n > depth + 1), keep its
/// slim envelope for the checkpoint's source binding, and split off the
/// real-date tail that the typed appends will replay. `--changed-last`
/// bumps the last tail bar's `close` by one cent so the final head
/// differs from the old head's content.
fn prepare_rebase(
    backend: &MarketBackend,
    e: &mut EntityState,
    depth: i64,
    changed_last: bool,
) -> Result<(), String> {
    let old = backend
        .fixed(&e.head_version_id)
        .map_err(|err| format!("rebase base materialization failed for {}: {err}", e.symbol))?;
    let bars = old.version().records.clone();
    let n = bars.len();
    if n as i64 <= depth + 1 {
        return Err(format!(
            "{}: real head has {n} bars; --depth {depth} requires n > depth+1",
            e.symbol
        ));
    }
    e.original_head_version_id = e.head_version_id.clone();
    e.original_raw = old.raw().clone();
    e.original_bars = bars.clone();
    let mut tail: VecDeque<Bar> = bars[n - depth as usize..].iter().cloned().collect();
    if changed_last {
        let last = tail.back_mut().expect("non-empty rebase tail");
        last.close += rust_decimal::Decimal::new(1, 2);
        if last.high < last.close {
            last.high = last.close;
        }
    }
    e.pending_tail = tail;
    Ok(())
}

/// `--mode rebase-real` write phase: publish `B[..n-depth]` as a new
/// ROOT CHECKPOINT through the production envelope update path —
/// `stage` (no `compact_parent_version_id` → `stage_checkpoint` with a
/// NULL parent) then `Publisher::prepare`/`commit` under
/// `PublishIntent::Daily{expected_head: old}` so the effective-head CAS
/// and the `continuity_mode=3`/`Full` update-result row record exactly
/// the corporate-action full-rewrite shape. The staged envelope reuses
/// the old head's `compact_source` binding (same generation / pinned
/// member digests) plus a `synthetic: true` marker and a synthetic
/// coverage reason.
fn rebase_entity(
    backend: &MarketBackend,
    ctl: &TaskControl<SqliteTasks>,
    e: &mut EntityState,
    run_id: &str,
    request_prefix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let symbol = e.symbol.clone();
    let old_head = e.head_version_id.clone();
    let prefix: Vec<Bar> = e.original_bars[..e.original_bars.len() - e.pending_tail.len()].to_vec();
    let intent = ExplicitRanges {
        items: vec![EntityRange {
            symbol: symbol.clone(),
            base_version_id: old_head.clone(),
            start_date: prefix.first().unwrap().date,
            end_date: prefix.last().unwrap().date,
        }],
    };
    let payload = serde_json::to_value(&intent)?;
    let request_id = format!(
        "{request_prefix}-rebase-{}",
        &x_data_mcp::hash::sha256(payload.to_string().as_bytes())[..16]
    );
    let accepted = ctl.accept(&request_id, Operation::Update.key(), payload)?;
    let RequestOutcome::Task(snapshot) = accepted.record.outcome else {
        return Err("rebase acceptance carried no task".into());
    };
    let task_id = snapshot.task.task_id;
    let item_id = scope_id(&symbol, "day");
    let tasks = backend.tasks();
    let snapshot = tasks.transition(
        &task_id,
        snapshot.task.state_revision,
        run_id,
        Change::Claim,
    )?;
    let snapshot = tasks.transition(
        &task_id,
        snapshot.task.state_revision,
        run_id,
        Change::StartItem(item_id.clone()),
    )?;
    // Rebuild the staged envelope: the old head's batch/provenance with
    // the prefix bars and a synthetic coverage reason, the old head's
    // source binding plus the synthetic marker, a fresh legacy digest.
    let mut batch: MarketDataBatch = serde_json::from_value(e.original_raw["batch"].clone())?;
    batch.bars = prefix;
    batch.coverage = CoverageEvidence::complete(format!(
        "synthetic rebase-real rewrite on {symbol}; rebased prefix of {old_head}, not source data"
    ));
    let (records, validation) = x_data_mcp::publish::validate_and_deduplicate(&batch)?;
    let old_version: EffectiveVersion = serde_json::from_value(e.original_raw.clone())?;
    let version_id = format!(
        "seed-rebase-v-{symbol}-r{}",
        &x_data_mcp::hash::sha256(old_head.as_bytes())[..12]
    );
    let mut raw = serde_json::to_value(EffectiveVersion {
        version_id: version_id.clone(),
        symbol: symbol.clone(),
        period: Period::Day,
        adjustment: old_version.adjustment,
        source: old_version.source.clone(),
        records,
        published_at: chrono::Utc::now(),
        validation,
        unit_currency: batch.unit_currency.clone(),
        adjustment_evidence: batch.adjustment_evidence.clone(),
        yearly_derivation: None,
    })?;
    raw["batch"] = serde_json::to_value(&batch)?;
    raw["compact_source"] = e.original_raw["compact_source"].clone();
    raw["compact_source"]["generation_evidence"]["synthetic"] = json!(true);
    raw["compact_source"]["generation_evidence"]["fixture"] = json!("seed_depth_fixture");
    raw["integrity_sha256"] = json!(x_data_mcp::market_storage::compat::legacy_digest(&raw)?);
    backend.stage(&raw)?;
    let plan = (Publisher {
        repository: backend.clone(),
    })
    .prepare(Publication {
        publication_id: format!("seed-rebase-r-{task_id}-{item_id}"),
        version_id,
        task_id: task_id.clone(),
        item_id,
        run_id: run_id.into(),
        expected_revision: snapshot.task.state_revision,
        intent: PublishIntent::Daily {
            expected_head: Some(old_head),
        },
    })?;
    let receipt = backend.commit(&plan)?;
    e.head_version_id = receipt.version_id;
    refresh_head(backend, e).map_err(Into::into)?;
    Ok(())
}

/// One append step for one entity: a single bar committed as a typed
/// metadata append so the new version's generation evidence carries
/// `synthetic: true`. The bar is generated (append-synthetic) or popped
/// from the entity's real-date tail (rebase-real).
fn append_item(e: &mut EntityState, task_id: &str, run_id: &str, mode: &Mode) -> UpdateBatchItem {
    let (bar, id_prefix) = match mode {
        Mode::AppendSynthetic => (
            Bar {
                date: e.last_day + chrono::Duration::days(1),
                open: rust_decimal::Decimal::new(10000, 2),
                high: rust_decimal::Decimal::new(10010, 2),
                low: rust_decimal::Decimal::new(9990, 2),
                close: rust_decimal::Decimal::new(10000, 2),
                volume: Some(rust_decimal::Decimal::new(100, 0)),
                amount: Some(rust_decimal::Decimal::new(1000, 0)),
                r7_raw: Some(7),
            },
            "seed-depth",
        ),
        Mode::RebaseReal { .. } => (
            e.pending_tail
                .pop_front()
                .expect("rebase-real pending tail drained"),
            "seed-rebase",
        ),
    };
    let next = bar.date;
    let seq = e.depth + 1;
    let member_sha = x_data_mcp::hash::sha256(
        format!("{id_prefix}-member:{}:{next}:{seq}", e.symbol).as_bytes(),
    );
    let manifest_sha = x_data_mcp::hash::sha256(
        format!("{id_prefix}-manifest:{}:{next}:{seq}", e.symbol).as_bytes(),
    );
    let result_version_id = format!("{id_prefix}-v-{}-{seq}", e.symbol);
    let item_id = scope_id(&e.symbol, "day");
    UpdateBatchItem {
        task_id: task_id.into(),
        item_id: item_id.clone(),
        publication_id: format!("{id_prefix}-r-{task_id}-{item_id}"),
        run_id: run_id.into(),
        symbol: e.symbol.clone(),
        occupancy_key: item_id,
        prepared: UpdateBatchPrepared::Append {
            base_version_id: e.head_version_id.clone(),
            result_version_id,
            upserts: vec![bar],
            metadata: Some(Box::new(TypedVersionMetadata {
                generation_id: format!("{id_prefix}-gen-{}-{seq}", e.symbol),
                generation_manifest_sha256: manifest_sha.clone(),
                day_member_sha256: member_sha.clone(),
                security_event_sha256: e.security_event_sha256.clone(),
                generation_evidence: json!({
                    "synthetic": true,
                    "fixture": "seed_depth_fixture",
                    "manifest_sha256": &manifest_sha,
                    "snapshot_inputs": [
                        {
                            "role": "day",
                            "member": format!("{}.day", e.symbol),
                            "archive_sha256": &manifest_sha,
                            "member_sha256": &member_sha,
                        },
                        {
                            "role": EVENTS_ROLE,
                            "member": format!("{}.{}", e.symbol, EVENTS_ROLE),
                            "archive_sha256": &manifest_sha,
                            "member_sha256": &e.security_event_sha256,
                        },
                    ],
                }),
                source: e.source.clone(),
                coverage: CoverageEvidence::complete(format!(
                    "synthetic depth-fixture append on {}; fixture-produced version, not source data",
                    e.symbol
                )),
                unit_currency: e.unit_currency.clone(),
                adjustment_evidence: e.adjustment_evidence.clone(),
            })),
            continuity: json!({
                "base_version_id": &e.head_version_id,
                "base_integrity_sha256": &e.integrity_sha256,
                "candidate_sha256": &member_sha,
                "range": {"start": next, "end": next},
                "same_qfq_basis_ref": "synthetic:true;seed_depth_fixture",
                "continuity": {
                    "mode": "append",
                    "day_member_sha256": &member_sha,
                    "day_prefix_bytes": u64::from(e.record_count) * 32,
                    "gbbq_events_sha256": &e.security_event_sha256,
                },
            }),
        },
    }
}

/// Seed `symbols` to `target_depth` through the production update commit
/// path. One task per step batches up to 100 entity items (the contract
/// budget); each item is one typed append on that entity's current head,
/// so `delta_depth` climbs by one per step. `--mode rebase-real` first
/// rebases every entity onto a new root checkpoint (see `rebase_entity`)
/// and the appends then replay its real-date tail.
fn seed_entities(
    backend: &MarketBackend,
    run_id: &str,
    symbols: &[String],
    target_depth: i64,
    request_prefix: &str,
    mode: &Mode,
) -> Result<Value, Box<dyn std::error::Error>> {
    if !(1..=256).contains(&target_depth) {
        return Err("--depth must be within 1..=256 (the delta-depth bound)".into());
    }
    let ctl = TaskControl::new(
        backend.tasks(),
        run_id.to_string(),
        vec![Box::new(RangeContract {
            operation: Operation::Update,
            repository: backend.clone(),
            // The accepted-range future guard protects production writes;
            // a synthetic fixture legitimately extends days past today on
            // its private copy. rebase-real replays real dates only, so
            // it keeps the production guard.
            today: match mode {
                Mode::AppendSynthetic => || chrono::NaiveDate::MAX,
                Mode::RebaseReal { .. } => || chrono::Utc::now().date_naive(),
            },
        })],
    )?;
    let mut entities = symbols
        .iter()
        .map(|s| entity_state(backend, s))
        .collect::<Result<Vec<_>, _>>()?;
    if let Mode::RebaseReal { changed_last } = mode {
        for e in &mut entities {
            prepare_rebase(backend, e, target_depth, *changed_last)?;
        }
        for e in &mut entities {
            rebase_entity(backend, &ctl, e, run_id, request_prefix)?;
        }
    }
    loop {
        let mut pending: Vec<&mut EntityState> = entities
            .iter_mut()
            .filter(|e| e.depth < target_depth)
            .collect();
        if pending.is_empty() {
            break;
        }
        for chunk in pending.chunks_mut(100) {
            let intent = ExplicitRanges {
                items: chunk
                    .iter()
                    .map(|e| EntityRange {
                        symbol: e.symbol.clone(),
                        base_version_id: e.head_version_id.clone(),
                        start_date: next_day(e, mode),
                        end_date: next_day(e, mode),
                    })
                    .collect(),
            };
            let payload = serde_json::to_value(&intent)?;
            let request_id = format!(
                "{request_prefix}-{}",
                &x_data_mcp::hash::sha256(payload.to_string().as_bytes())[..16]
            );
            let accepted = ctl.accept(&request_id, Operation::Update.key(), payload)?;
            let RequestOutcome::Task(snapshot) = accepted.record.outcome else {
                return Err("update_data acceptance carried no task".into());
            };
            let task_id = snapshot.task.task_id;
            let items: Vec<UpdateBatchItem> = chunk
                .iter_mut()
                .map(|e| append_item(e, &task_id, run_id, mode))
                .collect();
            let results = RangeRepository::commit_update_batch(backend, &task_id, &items)?
                .ok_or_else(|| {
                    "backend lacks the typed update path (compact schema required)".to_string()
                })?;
            if results.len() != items.len() {
                return Err("update batch result count does not match items".into());
            }
            for (e, result) in chunk.iter_mut().zip(results) {
                let receipt = result.map_err(|e| e.to_string())?;
                e.head_version_id = receipt.version_id;
                refresh_head(backend, e)?;
            }
        }
    }
    let mut reports = Vec::new();
    for e in &entities {
        let stored: (i64, i64) = backend.read(|c| {
            c.query_row(
                "SELECT delta_depth,manifest_mode FROM data_versions WHERE version_id=?1",
                [&e.head_version_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(storage)
        })?;
        // `fixed` runs the full verified chain materialization — the
        // integrity readback the fixture promises.
        let readback = backend.fixed(&e.head_version_id)?;
        let mut report = json!({
            "symbol": e.symbol,
            "head_version_id": e.head_version_id,
            "delta_depth": stored.0,
            "manifest_mode": stored.1,
            "verified": true,
        });
        if let Mode::RebaseReal { changed_last } = mode {
            report["original_head_version_id"] = json!(e.original_head_version_id);
            report["bars_equal_old_head"] = json!(readback.version().records == e.original_bars);
            report["changed_last"] = json!(changed_last);
        }
        reports.push(report);
    }
    Ok(json!({
        "entities": reports,
        "target_depth": target_depth,
        "synthetic": true,
        "mode": match mode {
            Mode::AppendSynthetic => "append-synthetic",
            Mode::RebaseReal { .. } => "rebase-real",
        },
    }))
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        return Err(usage());
    }
    let db = args[0].to_str().ok_or_else(usage)?.to_string();
    let copy_confirmed = args.iter().any(|a| a.as_os_str() == "--target-is-a-copy");
    let path = guarded_db_path(&db, copy_confirmed)?;
    let symbols: Vec<String> = opt(&args, "--entities")
        .ok_or_else(usage)?
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if symbols.is_empty() {
        return Err("--entities requires at least one symbol".into());
    }
    let depth: i64 = opt(&args, "--depth")
        .ok_or_else(usage)?
        .parse()
        .map_err(|_| "--depth must be an integer")?;
    let run_id = opt(&args, "--run-id")
        .unwrap_or_else(|| format!("seed-depth-{}", chrono::Utc::now().timestamp_millis()));
    let prefix = opt(&args, "--request-prefix").unwrap_or_else(|| "seed-depth".into());
    let changed_last = args.iter().any(|a| a.as_os_str() == "--changed-last");
    let mode = match opt(&args, "--mode").as_deref() {
        None | Some("append-synthetic") => {
            if changed_last {
                return Err("--changed-last requires --mode rebase-real".into());
            }
            Mode::AppendSynthetic
        }
        Some("rebase-real") => Mode::RebaseReal { changed_last },
        Some(other) => return Err(format!("unknown --mode {other}").into()),
    };
    let backend = MarketBackend::open(&SqliteTasks::new(&path))?;
    if !matches!(backend, MarketBackend::Compact(_)) {
        return Err("target database is not a compact-schema copy".into());
    }
    let report = seed_entities(&backend, &run_id, &symbols, depth, &prefix, &mode)?;
    println!("{report}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use x_data_mcp::{
        hash::sha256,
        market_data::{Adjustment, MarketDataBatch, Provenance},
        market_storage::{Publication, PublishIntent, Publisher},
        publish::EffectiveVersion,
        task_control::{Change, Command, CommandKey, FrozenWork, ItemSpec, TaskRepository},
    };

    const RUN: &str = "seed-depth-test";

    #[test]
    fn guards_reject_unconfirmed_and_guarded_paths() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("copy.db");
        std::fs::write(&db, b"").unwrap();
        assert!(guarded_db_path(db.to_str().unwrap(), false).is_err());
        for guarded in [
            "/opt/x-data-mcp-preview/state/preview.sqlite",
            "/opt/anything/market.db",
            "/root/x-perf-g8/svc/db/market.db",
        ] {
            assert!(
                guarded_db_path(guarded, true).is_err(),
                "guarded path accepted: {guarded}"
            );
        }
        assert!(guarded_db_path(db.to_str().unwrap(), true).is_ok());
    }

    /// Publish one checkpoint head for `symbol` through the same
    /// stage→validate→commit path the production collection commit uses.
    fn publish_head(backend: &MarketBackend, symbol: &str, day: chrono::NaiveDate) {
        publish_head_bars(
            backend,
            symbol,
            vec![Bar {
                date: day,
                open: rust_decimal::Decimal::new(10000, 2),
                high: rust_decimal::Decimal::new(10010, 2),
                low: rust_decimal::Decimal::new(9990, 2),
                close: rust_decimal::Decimal::new(10000, 2),
                volume: Some(rust_decimal::Decimal::new(100, 0)),
                amount: Some(rust_decimal::Decimal::new(1000, 0)),
                r7_raw: Some(7),
            }],
        );
    }

    /// `publish_head` with an explicit bar series.
    fn publish_head_bars(backend: &MarketBackend, symbol: &str, bars: Vec<Bar>) {
        let tasks = backend.tasks();
        let scope = scope_id(symbol, "day");
        let work = FrozenWork {
            items: vec![ItemSpec {
                item_id: "item-1".into(),
                target: json!({"symbol": symbol, "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            }],
            scopes: BTreeSet::from([scope]),
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
        let accepted = tasks
            .accept(&format!("req-base-{symbol}"), &command, &work, RUN, None)
            .unwrap();
        let RequestOutcome::Task(task) = accepted.record.outcome else {
            panic!("expected task outcome")
        };
        let task_id = task.task.task_id;
        let snap = tasks.transition(&task_id, 0, RUN, Change::Claim).unwrap();
        let snap = tasks
            .transition(
                &task_id,
                snap.task.state_revision,
                RUN,
                Change::StartItem("item-1".into()),
            )
            .unwrap();
        let version_id = format!("base-v-{symbol}");
        let member_sha = sha256(format!("base-member-{symbol}").as_bytes());
        let events_sha = sha256(format!("base-events-{symbol}").as_bytes());
        let manifest_sha = sha256(format!("base-manifest-{symbol}").as_bytes());
        let batch = MarketDataBatch {
            symbol: symbol.into(),
            period: Period::Day,
            adjustment: Adjustment::Forward,
            bars,
            provenance: Provenance {
                source: "fixture:seed-depth".into(),
                source_period: "day".into(),
                source_adjustment: "qfq".into(),
                symbol: symbol.into(),
                acquired_at: chrono::Utc::now(),
                adapter_version: "seed-depth-test".into(),
                yearly_derivation: None,
                snapshot_inputs: None,
            },
            coverage: CoverageEvidence::complete("seed-depth test base"),
            unit_currency: UnitCurrencyEvidence::confirmed(
                "CNY", "share", "share", "CNY", "yuan", "bound",
            ),
            adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        };
        let (records, validation) = x_data_mcp::publish::validate_and_deduplicate(&batch).unwrap();
        let mut raw = serde_json::to_value(EffectiveVersion {
            version_id: version_id.clone(),
            symbol: symbol.into(),
            period: Period::Day,
            adjustment: Adjustment::Forward,
            source: "fixture:seed-depth".into(),
            records,
            published_at: chrono::Utc::now(),
            validation,
            unit_currency: batch.unit_currency.clone(),
            adjustment_evidence: batch.adjustment_evidence.clone(),
            yearly_derivation: None,
        })
        .unwrap();
        raw["batch"] = serde_json::to_value(&batch).unwrap();
        raw["compact_source"] = json!({
            "generation_id": "seed-depth-base-gen",
            "generation_manifest_sha256": manifest_sha,
            "day_member_sha256": member_sha,
            "security_event_sha256": events_sha,
            "generation_evidence": {
                "manifest_sha256": manifest_sha,
                "snapshot_inputs": [{
                    "role": "day",
                    "member": format!("{symbol}.day"),
                    "archive_sha256": manifest_sha,
                    "member_sha256": member_sha,
                }]
            }
        });
        raw["integrity_sha256"] =
            json!(x_data_mcp::market_storage::compat::legacy_digest(&raw).unwrap());
        backend.stage(&raw).unwrap();
        let plan = (Publisher {
            repository: backend.clone(),
        })
        .prepare(Publication {
            publication_id: format!("base-pub-{symbol}"),
            version_id,
            task_id,
            item_id: "item-1".into(),
            run_id: RUN.into(),
            expected_revision: snap.task.state_revision,
            intent: PublishIntent::Daily {
                expected_head: None,
            },
        })
        .unwrap();
        backend.commit(&plan).unwrap();
    }

    #[test]
    fn seeded_chain_depth_verifies() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/dev/shm")
            .unwrap();
        let db = dir.path().join("market.db");
        SqliteTasks::new(&db).initialize_compact().unwrap();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
        let backend = MarketBackend::open(&SqliteTasks::new(&db)).unwrap();
        publish_head(
            &backend,
            "sh.600006",
            chrono::NaiveDate::from_ymd_opt(2026, 9, 29).unwrap(),
        );
        let report = seed_entities(
            &backend,
            RUN,
            &["sh.600006".into()],
            4,
            "seed-depth",
            &Mode::AppendSynthetic,
        )
        .unwrap();
        let entity = &report["entities"][0];
        assert_eq!(entity["delta_depth"], json!(4));
        assert_eq!(entity["manifest_mode"], json!(2));
        // Readback through SQL plus the fully verified materialization.
        let (depth, mode): (i64, i64) = backend
            .read(|c| {
                c.query_row(
                    "SELECT delta_depth,manifest_mode FROM data_versions WHERE entity='sh.600006' ORDER BY version_key DESC LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(storage)
            })
            .unwrap();
        assert_eq!((depth, mode), (4, 2));
        // The synthetic marker is bound to every seeded version through
        // its generation evidence bundle.
        let marked: i64 = backend
            .read(|c| {
                c.query_row(
                    "SELECT count(*) FROM data_versions v \
                     JOIN source_bindings sb ON v.source_binding_digest=sb.source_binding_digest \
                     JOIN evidence_bundles e ON sb.evidence_digest=e.evidence_digest \
                     WHERE v.entity='sh.600006' AND e.canonical_json LIKE '%\"synthetic\":true%'",
                    [],
                    |r| r.get(0),
                )
                .map_err(storage)
            })
            .unwrap();
        assert_eq!(marked, 4);
    }

    /// Temp compact DB with an `n`-bar real-date head (all days ≤
    /// today), returned with the bars as committed.
    fn rebase_fixture_db(n: i64) -> (tempfile::TempDir, MarketBackend) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/dev/shm")
            .unwrap();
        let db = dir.path().join("market.db");
        SqliteTasks::new(&db).initialize_compact().unwrap();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
        let backend = MarketBackend::open(&SqliteTasks::new(&db)).unwrap();
        let start = chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let bars: Vec<Bar> = (0..n)
            .map(|i| Bar {
                date: start + chrono::Duration::days(i),
                open: rust_decimal::Decimal::new(10000 + i, 2),
                high: rust_decimal::Decimal::new(10010 + i, 2),
                low: rust_decimal::Decimal::new(9990 + i, 2),
                close: rust_decimal::Decimal::new(10000 + i, 2),
                volume: Some(rust_decimal::Decimal::new(100, 0)),
                amount: Some(rust_decimal::Decimal::new(1000, 0)),
                r7_raw: Some(7),
            })
            .collect();
        publish_head_bars(&backend, "sh.600006", bars);
        (dir, backend)
    }

    fn day_head(backend: &MarketBackend) -> String {
        backend
            .effective_head(&Scope {
                symbol: "sh.600006".into(),
                period: Period::Day,
            })
            .unwrap()
            .unwrap()
    }

    #[test]
    fn rebase_real_keeps_real_dates_and_verifies() {
        let (_dir, backend) = rebase_fixture_db(12);
        let old_head = day_head(&backend);
        let old_records = backend.fixed(&old_head).unwrap().version().records.clone();
        let report = seed_entities(
            &backend,
            RUN,
            &["sh.600006".into()],
            8,
            "seed-rebase",
            &Mode::RebaseReal {
                changed_last: false,
            },
        )
        .unwrap();
        let entity = &report["entities"][0];
        assert_eq!(entity["delta_depth"], json!(8));
        assert_eq!(entity["manifest_mode"], json!(2));
        assert_eq!(entity["bars_equal_old_head"], json!(true));
        assert_eq!(entity["changed_last"], json!(false));
        assert_eq!(entity["original_head_version_id"], json!(old_head));
        let head = entity["head_version_id"].as_str().unwrap();
        // The rebased root R is a checkpoint; the head is 8 deltas above
        // it and every new version carries the synthetic marker.
        let (depth, mode, root_depth, root_parent): (i64, i64, i64, Option<i64>) = backend
            .read(|c| {
                let head_row = c
                    .query_row(
                        "SELECT delta_depth,manifest_mode FROM data_versions WHERE version_id=?1",
                        [head],
                        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                    )
                    .map_err(storage)?;
                let root_row = c
                    .query_row(
                        "SELECT delta_depth,parent_version_key FROM data_versions \
                         WHERE entity='sh.600006' AND parent_version_key IS NULL \
                         ORDER BY version_key DESC LIMIT 1",
                        [],
                        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)),
                    )
                    .map_err(storage)?;
                Ok((head_row.0, head_row.1, root_row.0, root_row.1))
            })
            .unwrap();
        assert_eq!((depth, mode, root_depth, root_parent), (8, 2, 0, None));
        // Verified readback equals the old head's bars byte-for-byte.
        let readback = backend.fixed(head).unwrap();
        assert_eq!(readback.version().records, old_records);
        // R + 8 appends — nine new versions, all synthetically marked.
        let marked: i64 = backend
            .read(|c| {
                c.query_row(
                    "SELECT count(*) FROM data_versions v \
                     JOIN source_bindings sb ON v.source_binding_digest=sb.source_binding_digest \
                     JOIN evidence_bundles e ON sb.evidence_digest=e.evidence_digest \
                     WHERE v.entity='sh.600006' AND e.canonical_json LIKE '%\"synthetic\":true%'",
                    [],
                    |r| r.get(0),
                )
                .map_err(storage)
            })
            .unwrap();
        assert_eq!(marked, 9);
    }

    #[test]
    fn rebase_real_changed_last_differs_only_in_last_close() {
        let (_dir, backend) = rebase_fixture_db(12);
        let old_head = day_head(&backend);
        let old_records = backend.fixed(&old_head).unwrap().version().records.clone();
        let report = seed_entities(
            &backend,
            RUN,
            &["sh.600006".into()],
            8,
            "seed-rebase",
            &Mode::RebaseReal { changed_last: true },
        )
        .unwrap();
        let entity = &report["entities"][0];
        assert_eq!(entity["delta_depth"], json!(8));
        assert_eq!(entity["bars_equal_old_head"], json!(false));
        let head = entity["head_version_id"].as_str().unwrap();
        let records = backend.fixed(head).unwrap().version().records.clone();
        let n = old_records.len();
        assert_eq!(records.len(), n);
        assert_eq!(records[..n - 1], old_records[..n - 1]);
        let (last, old_last) = (&records[n - 1], &old_records[n - 1]);
        assert_eq!(
            last.close,
            old_last.close + rust_decimal::Decimal::new(1, 2)
        );
        assert_eq!(last.date, old_last.date);
        assert_eq!(last.open, old_last.open);
        assert_eq!(last.low, old_last.low);
        assert_eq!(last.volume, old_last.volume);
        assert_eq!(last.amount, old_last.amount);
        assert_eq!(last.r7_raw, old_last.r7_raw);
    }

    #[test]
    fn rebase_real_rejects_shallow_head() {
        let (_dir, backend) = rebase_fixture_db(5);
        let err = seed_entities(
            &backend,
            RUN,
            &["sh.600006".into()],
            4,
            "seed-rebase",
            &Mode::RebaseReal {
                changed_last: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("n > depth+1"), "{err}");
    }

    #[test]
    fn rebase_real_fails_closed_on_segment_tamper() {
        let (_dir, backend) = rebase_fixture_db(12);
        let report = seed_entities(
            &backend,
            RUN,
            &["sh.600006".into()],
            8,
            "seed-rebase",
            &Mode::RebaseReal {
                changed_last: false,
            },
        )
        .unwrap();
        let head = report["entities"][0]["head_version_id"]
            .as_str()
            .unwrap()
            .to_string();
        // Corrupt one bar row inside the new root checkpoint's first
        // segment; the verified readback must fail closed.
        backend
            .read(|c| {
                c.execute_batch(
                    "UPDATE segment_bars SET close_cents=close_cents+1 \
                     WHERE segment_key=(\
                         SELECT segment_key FROM version_checkpoint_clips \
                         WHERE version_key=(\
                             SELECT version_key FROM data_versions \
                             WHERE entity='sh.600006' AND parent_version_key IS NULL \
                             ORDER BY version_key DESC LIMIT 1) \
                         ORDER BY seq LIMIT 1) \
                     AND trade_day=(\
                         SELECT min(trade_day) FROM segment_bars t \
                         WHERE t.segment_key=segment_bars.segment_key)",
                )
                .map_err(storage)
            })
            .unwrap();
        assert!(backend.fixed(&head).is_err());
    }
}
