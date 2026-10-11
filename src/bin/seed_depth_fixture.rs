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
//! usage: seed_depth_fixture <database> --entities <ex.code[,ex.code...]>
//!          --depth <1..=256> --target-is-a-copy [--yearly-depth <0..=256>]
//!          [--run-id <id>] [--request-prefix <p>]
//!
//! `--yearly-depth K` (TASK-XP-029 S8, default 0 = unchanged): after the
//! daily chain is seeded, each entity additionally publishes K yearly
//! delta-chain nodes through the production yearly path — `YearlyContract`
//! acceptance, `YearlyRunner::run_item` (windowed base read, memoized
//! `yearly::aggregate`, `derive_yearly`, `prepare_yearly_result_prefetched`,
//! `commit_yearly` → `stage_tx` yearly delta append). Yearly node j derives
//! from the j-th of the last K seeded daily versions; its lineage `as_of`
//! is that base's `last_day`, so `bar.date > as_of` can never reject the
//! derivation. The fixture's own `YearlyVerifier` only re-aggregates the
//! fixed base and stamps a synthetic coverage reason — the same proof the
//! production verifiers bind, marked as fixture output.
use chrono::Datelike;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use x_data_mcp::{
    market_data::{
        AdjustmentEvidence, Bar, CoverageEvidence, Period, SourceError, UnitCurrencyEvidence,
    },
    market_storage::{MarketRepository, PublishIntent, Scope, compat::CheckedRecord},
    market_update::{
        TypedVersionMetadata, UpdateBatchItem, UpdateBatchPrepared,
        publication::RangeRepository,
        tasks::{EntityRange, ExplicitRanges, Operation, RangeContract},
    },
    official_market::DailyCollectionRepository,
    sqlite_market::backend::MarketBackend,
    sqlite_tasks::SqliteTasks,
    task_control::{Contract, RequestOutcome, legacy::scope_id, service::TaskControl},
    yearly::AnnualSeries,
    yearly_publication::{
        DerivationEvidence,
        runner::{YearlyRunner, YearlyVerifier},
        tasks::{ExplicitYears, YearlyContract, YearlyItem},
    },
};

const FROZEN_STORE: &str = "/root/x-perf-g8/svc/db";
/// Mirrors `crate::official_market::SECURITY_EVENTS_ROLE` (pub(crate));
/// the role label is part of the stored generation-evidence shape.
const EVENTS_ROLE: &str = "gbbq-security-events";

fn usage() -> Box<dyn std::error::Error> {
    "usage: seed_depth_fixture <database> --entities <ex.code[,ex.code...]> --depth <1..=256> --target-is-a-copy [--yearly-depth <0..=256>] [--run-id <id>] [--request-prefix <p>]"
        .into()
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
    /// (version_id, appended bar date) of each seeded daily node, in order.
    versions: Vec<(String, chrono::NaiveDate)>,
    /// Current published yearly head, when the entity already has one.
    yearly_head: Option<String>,
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
        versions: Vec::new(),
        yearly_head: None,
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

/// One synthetic append step for one entity: a single generated bar one
/// day past the base's last day, committed as a typed metadata append so
/// the new version's generation evidence carries `synthetic: true`.
fn append_item(e: &EntityState, task_id: &str, run_id: &str) -> UpdateBatchItem {
    let next = e.last_day + chrono::Duration::days(1);
    let seq = e.depth + 1;
    let member_sha =
        x_data_mcp::hash::sha256(format!("seed-depth-member:{}:{next}:{seq}", e.symbol).as_bytes());
    let manifest_sha = x_data_mcp::hash::sha256(
        format!("seed-depth-manifest:{}:{next}:{seq}", e.symbol).as_bytes(),
    );
    let result_version_id = format!("seed-depth-v-{}-{seq}", e.symbol);
    let bar = Bar {
        date: next,
        open: rust_decimal::Decimal::new(10000, 2),
        high: rust_decimal::Decimal::new(10010, 2),
        low: rust_decimal::Decimal::new(9990, 2),
        close: rust_decimal::Decimal::new(10000, 2),
        volume: Some(rust_decimal::Decimal::new(100, 0)),
        amount: Some(rust_decimal::Decimal::new(1000, 0)),
        r7_raw: Some(7),
    };
    let item_id = scope_id(&e.symbol, "day");
    UpdateBatchItem {
        task_id: task_id.into(),
        item_id: item_id.clone(),
        publication_id: format!("seed-depth-r-{task_id}-{item_id}"),
        run_id: run_id.into(),
        symbol: e.symbol.clone(),
        occupancy_key: item_id,
        prepared: UpdateBatchPrepared::Append {
            base_version_id: e.head_version_id.clone(),
            result_version_id,
            upserts: vec![bar],
            metadata: Some(Box::new(TypedVersionMetadata {
                generation_id: format!("seed-depth-gen-{}-{seq}", e.symbol),
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
                    "synthetic depth-fixture append on {}; generated bars only, not source data",
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
/// budget); each item is one synthetic typed append on that entity's
/// current head, so `delta_depth` climbs by one per step.
fn seed_entities(
    backend: &MarketBackend,
    run_id: &str,
    symbols: &[String],
    target_depth: i64,
    request_prefix: &str,
) -> Result<(Vec<EntityState>, Value), Box<dyn std::error::Error>> {
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
            // its private copy.
            today: || chrono::NaiveDate::MAX,
        })],
    )?;
    let mut entities = symbols
        .iter()
        .map(|s| entity_state(backend, s))
        .collect::<Result<Vec<_>, _>>()?;
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
                        start_date: e.last_day + chrono::Duration::days(1),
                        end_date: e.last_day + chrono::Duration::days(1),
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
                .iter()
                .map(|e| append_item(e, &task_id, run_id))
                .collect();
            let results = RangeRepository::commit_update_batch(backend, &task_id, &items)?
                .ok_or_else(|| {
                    "backend lacks the typed update path (compact schema required)".to_string()
                })?;
            if results.len() != items.len() {
                return Err("update batch result count does not match items".into());
            }
            for (e, result) in chunk.iter_mut().zip(results) {
                let appended_day = e.last_day + chrono::Duration::days(1);
                let receipt = result.map_err(|e| e.to_string())?;
                e.versions.push((receipt.version_id.clone(), appended_day));
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
        backend.fixed(&e.head_version_id)?;
        reports.push(json!({
            "symbol": e.symbol,
            "head_version_id": e.head_version_id,
            "delta_depth": stored.0,
            "manifest_mode": stored.1,
            "verified": true,
        }));
    }
    Ok((
        entities,
        json!({"entities": reports, "target_depth": target_depth, "synthetic": true}),
    ))
}

/// Fixture verifier for the production yearly runner: same shape as the
/// bound-generation verifier (fail closed on missing base digest) but the
/// coverage reason marks the output as synthetic fixture data.
struct FixtureYearlyVerifier;

impl YearlyVerifier for FixtureYearlyVerifier {
    fn verify(
        &self,
        base: &CheckedRecord,
        years: &BTreeSet<i32>,
        annual: &AnnualSeries,
        derived_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<DerivationEvidence, SourceError> {
        let as_of = derived_at
            .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .date_naive();
        if annual.base_daily_version_id != base.version().version_id
            || annual.as_of != as_of
            || annual.records.len() != years.len()
        {
            return Err(SourceError::NotVerified(
                "aggregated annual series does not cover the requested years".into(),
            ));
        }
        Ok(DerivationEvidence {
            base_integrity_sha256: base.raw()["integrity_sha256"]
                .as_str()
                .ok_or_else(|| {
                    SourceError::NotVerified("fixed base carries no integrity digest".into())
                })?
                .into(),
            years: years.clone(),
            as_of,
            coverage: CoverageEvidence::complete(
                "synthetic:true;seed_depth_fixture;selected years verified by re-aggregating the fixture daily base; generated fixture data only",
            ),
        })
    }
}

/// Read the yearly delta depth of a version row.
fn version_depth(backend: &MarketBackend, version_id: &str) -> Result<i64, String> {
    backend
        .read(|c| {
            c.query_row(
                "SELECT delta_depth FROM data_versions WHERE version_id=?1",
                [version_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(storage)
        })
        .map_err(|e| format!("yearly delta depth read failed: {e}"))
}

/// After the daily chain is seeded, publish `yearly_depth` yearly nodes
/// per entity through the production yearly runner. Node j derives from
/// the j-th of the last `yearly_depth` seeded daily versions; the prior
/// yearly head becomes the comparison/parent, so `delta_depth` climbs by
/// one per node exactly as production publishes do.
fn seed_yearly(
    backend: &MarketBackend,
    run_id: &str,
    entities: &mut [EntityState],
    yearly_depth: i64,
    request_prefix: &str,
    mut report: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    if !(0..=256).contains(&yearly_depth) {
        return Err("--yearly-depth must be within 0..=256".into());
    }
    if yearly_depth == 0 {
        return Ok(report);
    }
    for e in entities.iter() {
        if (e.versions.len() as i64) < yearly_depth {
            return Err(format!(
                "{}: yearly depth {yearly_depth} exceeds the {} seeded daily nodes",
                e.symbol,
                e.versions.len()
            )
            .into());
        }
    }
    let contract = YearlyContract {
        operation: Operation::Update,
        repository: backend.clone(),
        // Same rationale as the daily contract override: the fixture
        // legitimately seeds past years on its private copy.
        today: || chrono::NaiveDate::MAX,
    };
    let key = contract.key();
    let ctl = TaskControl::new(
        backend.tasks(),
        run_id.to_string(),
        vec![Box::new(contract)],
    )?;
    let runner = YearlyRunner {
        repository: backend.clone(),
        run_id: run_id.to_string(),
        derived_at: chrono::Utc::now(),
    };
    // Per-entity progress: seeded yearly node count and current head.
    for e in entities.iter_mut() {
        let scope = Scope {
            symbol: e.symbol.clone(),
            period: Period::Year,
        };
        e.yearly_head = backend
            .effective_head(&scope)
            .map_err(|e2| format!("yearly effective head read failed: {e2}"))?;
    }
    let mut done: Vec<i64> = vec![0; entities.len()];
    loop {
        let pending: Vec<usize> = (0..entities.len())
            .filter(|&i| done[i] < yearly_depth)
            .collect();
        if pending.is_empty() {
            break;
        }
        for chunk in pending.chunks(100) {
            let items: Vec<YearlyItem> = chunk
                .iter()
                .map(|&i| {
                    let e = &entities[i];
                    let idx = e.versions.len() - yearly_depth as usize + done[i] as usize;
                    let (base_id, base_day) = &e.versions[idx];
                    YearlyItem {
                        symbol: e.symbol.clone(),
                        base_daily_version_id: base_id.clone(),
                        years: BTreeSet::from([base_day.year()]),
                        intent: PublishIntent::CurrentHead {
                            expected_daily_head: base_id.clone(),
                            expected_yearly_head: e.yearly_head.clone(),
                        },
                        comparison_version_id: e.yearly_head.clone(),
                    }
                })
                .collect();
            let payload = serde_json::to_value(ExplicitYears {
                items: items.clone(),
            })?;
            let request_id = format!(
                "{request_prefix}-yearly-{}",
                &x_data_mcp::hash::sha256(payload.to_string().as_bytes())[..16]
            );
            let accepted = ctl.accept(&request_id, key.clone(), payload)?;
            let RequestOutcome::Task(snapshot) = accepted.record.outcome else {
                return Err("yearly acceptance carried no task".into());
            };
            let task_id = snapshot.task.task_id;
            for (i, item) in chunk.iter().zip(items.iter()) {
                let e = &mut entities[*i];
                let item_id = scope_id(&item.symbol, "year");
                match runner.run_item(&task_id, &item_id, &FixtureYearlyVerifier)? {
                    x_data_mcp::yearly_publication::runner::YearlyExecution::Published(receipt) => {
                        e.yearly_head = Some(receipt.version_id);
                        done[*i] += 1;
                    }
                    x_data_mcp::yearly_publication::runner::YearlyExecution::NotPublished(
                        snapshot,
                    ) => {
                        return Err(format!(
                            "{}: yearly item not published: {snapshot:?}",
                            e.symbol
                        )
                        .into());
                    }
                }
            }
        }
    }
    let mut yearly_reports = Vec::new();
    for e in entities.iter() {
        let head = e
            .yearly_head
            .as_ref()
            .ok_or_else(|| format!("{}: yearly seeding produced no head", e.symbol))?;
        let depth = version_depth(backend, head)?;
        // Same verified readback as the daily chain: full materialization
        // replays every yearly node and its lineage re-aggregation.
        backend.fixed(head)?;
        yearly_reports.push(json!({
            "symbol": e.symbol,
            "yearly_head_version_id": head,
            "yearly_delta_depth": depth,
            "verified": true,
        }));
    }
    report["yearly"] = json!({
        "entities": yearly_reports,
        "target_depth": yearly_depth,
        "synthetic": true,
    });
    Ok(report)
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
    let backend = MarketBackend::open(&SqliteTasks::new(&path))?;
    if !matches!(backend, MarketBackend::Compact(_)) {
        return Err("target database is not a compact-schema copy".into());
    }
    let yearly_depth: i64 = opt(&args, "--yearly-depth")
        .unwrap_or_else(|| "0".into())
        .parse()
        .map_err(|_| "--yearly-depth must be an integer")?;
    let (mut entities, mut report) = seed_entities(&backend, &run_id, &symbols, depth, &prefix)?;
    if yearly_depth > 0 {
        report = seed_yearly(
            &backend,
            &run_id,
            &mut entities,
            yearly_depth,
            &prefix,
            report,
        )?;
    }
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
            bars: vec![Bar {
                date: day,
                open: rust_decimal::Decimal::new(10000, 2),
                high: rust_decimal::Decimal::new(10010, 2),
                low: rust_decimal::Decimal::new(9990, 2),
                close: rust_decimal::Decimal::new(10000, 2),
                volume: Some(rust_decimal::Decimal::new(100, 0)),
                amount: Some(rust_decimal::Decimal::new(1000, 0)),
                r7_raw: Some(7),
            }],
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
        let (_entities, report) =
            seed_entities(&backend, RUN, &["sh.600006".into()], 4, "seed-depth").unwrap();
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

    /// depth 3 + yearly-depth 3: the yearly head sits at delta depth 3,
    /// `fixed` replays the whole lineage-verified chain, tampering an
    /// ancestor yearly node's projection fails closed, and the yearly
    /// coverage evidence carries the synthetic marker.
    #[test]
    fn seeded_yearly_chain_verifies() {
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
        let (mut entities, _) =
            seed_entities(&backend, RUN, &["sh.600006".into()], 3, "seed-depth").unwrap();
        let report = seed_yearly(&backend, RUN, &mut entities, 3, "seed-depth", json!({})).unwrap();
        let yearly = &report["yearly"]["entities"][0];
        assert_eq!(yearly["yearly_delta_depth"], json!(3));
        let head = yearly["yearly_head_version_id"].as_str().unwrap();
        backend.fixed(head).unwrap();
        // Every seeded yearly node binds the synthetic coverage marker in
        // its version evidence.
        let marked: i64 = backend
            .read(|c| {
                c.query_row(
                    "SELECT count(*) FROM data_versions v \
                     JOIN evidence_bundles e ON v.evidence_digest=e.evidence_digest \
                     WHERE v.entity='sh.600006' AND v.period=2 \
                     AND e.canonical_json LIKE '%synthetic:true%'",
                    [],
                    |r| r.get(0),
                )
                .map_err(storage)
            })
            .unwrap();
        assert_eq!(marked, 3);
        // Tamper the earliest yearly projection bar; the verified chain
        // read must fail closed.
        let raw = rusqlite::Connection::open(&db).unwrap();
        raw.execute(
            "UPDATE segment_bars SET close_cents=close_cents+1 WHERE rowid=(\
             SELECT sb.rowid FROM segment_bars sb \
             JOIN bar_segments bs ON sb.segment_key=bs.segment_key \
             WHERE bs.entity='sh.600006' AND bs.period=2 \
             ORDER BY sb.trade_day LIMIT 1)",
            [],
        )
        .unwrap();
        drop(raw);
        assert!(backend.fixed(head).is_err());
    }

    /// --yearly-depth 0 keeps the pre-S8 behaviour: no yearly nodes.
    #[test]
    fn no_yearly_depth_keeps_existing_behaviour() {
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
        let (mut entities, report) =
            seed_entities(&backend, RUN, &["sh.600006".into()], 2, "seed-depth").unwrap();
        let report = seed_yearly(&backend, RUN, &mut entities, 0, "seed-depth", report).unwrap();
        assert!(report["yearly"].is_null() || report.get("yearly").is_none());
        let yearly_rows: i64 = backend
            .read(|c| {
                c.query_row(
                    "SELECT count(*) FROM data_versions WHERE period=2",
                    [],
                    |r| r.get(0),
                )
                .map_err(storage)
            })
            .unwrap();
        assert_eq!(yearly_rows, 0);
    }
}
