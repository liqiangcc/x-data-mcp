//! Server-internal daily-run items (issue-383 §9.2, M1 PR-7): the trading
//! calendar and the nine benchmark indices are maintained by the same
//! `run_market_update` task as the per-entity daily items.
//!
//! Layer position (separation points 1/2/5/7):
//!
//! - the item list is built from the domain registrations
//!   ([`crate::index_registry::INDEX_SERIES`],
//!   [`crate::trading_calendar::INPUTS`]) — this module never names a source
//!   file and adds no per-index branch;
//! - the adapter ([`crate::official_market::MaterializeSession`]) hands over
//!   neutral facts (`IndexBatch`, `CalendarBatch`); the storage layer commits
//!   them (`commit_index`, `commit_calendar`); this module only sequences the
//!   two, so the derived data stays recomputable from the layer below;
//! - the run is the *orchestration*: it appends the internal items to the
//!   already-accepted task and executes them after the entity items. No MCP
//!   tool, dataset or wire field is added.
//!
//! Failure semantics (§9.2): an internal item that fails records
//! `success=false` on its own row and the run ends `partially_failed`; a
//! committed entity item is never rolled back because the calendar failed.
//! An *uncertain* outcome (storage/commit result unknown) parks the task in
//! `needs_verification` exactly like the per-entity runner — a derived item
//! must not invent a failure for a commit that may have landed.

use chrono::Utc;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::{
    data_supply::generation::SOURCE_GENERATION_KEY,
    exchange_source::RunCache,
    index_registry,
    market_storage::{Error, MarketRepository},
    official_market::MaterializeSession,
    security_master::assembly::{self, PendingSource},
    sqlite_market::{
        backend::MarketBackend, calendar::CalendarWrite, index::IndexWrite,
        risk_warnings::RiskWarningWrite, rules::TradingRulesWrite, security::SecurityMasterWrite,
        security_names::SecurityNameWrite, suspensions::SuspensionWrite,
    },
    task_control::{self, Change, ItemOutcome, ItemSpec, Stage, Status, TaskItem},
    trading_calendar::TradingCalendar,
};

/// `task_items.progress_unit` of a derived dataset item. The unit is "one
/// dataset", not "one security/period".
pub const PROGRESS_UNIT: &str = "derived_dataset";

/// The internal items a daily plan-run task maintains, in execution order:
/// the nine benchmark indices first, the calendar next (its inputs are index
/// members, §9.2), then the whole-market dataset items (issue-391 §12.1 plus
/// the M3 name history, PR-12), the M3 industry snapshot (M3 PR-6, `#446`
/// §10.1) and finally the M3 delisted-series check (M3 PR-5, `#446` §4.5),
/// which must observe the state the entity items of this run left behind —
/// hence last. `derived:security_master` runs before `derived:suspensions`
/// because the gap derivation reads the main table this run assembled; the
/// name history runs before `derived:risk_warnings` so the shared SZSE table is
/// downloaded once and the ST item re-uses the frozen page.
///
/// `delisted` carries the registrations resolved before the entity items ran.
/// `None` means the backend keeps no M3 lifecycle/series tables (legacy schema
/// 18), where the check has nothing to read and is therefore not part of the
/// item set instead of being a permanent no-op.
pub fn item_specs(
    generation: &Value,
    delisted: Option<&crate::data_supply::delisted_series::Resolution>,
) -> Vec<ItemSpec> {
    let extra = usize::from(delisted.is_some());
    let mut items = Vec::with_capacity(
        index_registry::INDEX_SERIES.len() + 1 + task_control::derived::DATASETS.len() + 1 + extra,
    );
    for registration in &index_registry::INDEX_SERIES {
        let item_id = task_control::derived::index(registration.entity);
        items.push(ItemSpec {
            target: task_control::derived::target(&item_id),
            progress_unit: PROGRESS_UNIT.into(),
            frozen_inputs: json!({SOURCE_GENERATION_KEY: generation.clone()}),
            scope_keys: BTreeSet::from([task_control::derived::index_scope(registration.entity)]),
            item_id,
        });
    }
    let calendar = task_control::derived::CALENDAR.to_string();
    items.push(ItemSpec {
        target: task_control::derived::target(&calendar),
        progress_unit: PROGRESS_UNIT.into(),
        frozen_inputs: json!({SOURCE_GENERATION_KEY: generation.clone()}),
        scope_keys: BTreeSet::from([task_control::derived::calendar_scope()]),
        item_id: calendar,
    });
    for dataset in task_control::derived::DATASETS {
        items.push(ItemSpec {
            target: task_control::derived::target(dataset),
            progress_unit: PROGRESS_UNIT.into(),
            frozen_inputs: json!({SOURCE_GENERATION_KEY: generation.clone()}),
            scope_keys: BTreeSet::from([task_control::derived::dataset_scope(dataset)]),
            item_id: dataset.to_string(),
        });
    }
    // The M3 industry snapshot is its own item: it writes the M3 index tables
    // (§5.1), not an M2 dataset version, and its failure must not block the
    // sessions that already ran (§10.1 "内部 item 之间无事务耦合").
    let industry = task_control::derived::INDUSTRY_SNAPSHOT;
    items.push(ItemSpec {
        target: task_control::derived::target(industry),
        progress_unit: PROGRESS_UNIT.into(),
        frozen_inputs: json!({SOURCE_GENERATION_KEY: generation.clone()}),
        scope_keys: BTreeSet::from([task_control::derived::industry_scope()]),
        item_id: industry.to_string(),
    });
    // The M3 delisted-series check runs last: it compares the registrations
    // frozen before the entity items with the series facts they left behind,
    // so it must observe the finished state (§4.5 item 2).
    if let Some(resolution) = delisted {
        items.push(crate::data_supply::delisted_series::item_spec(
            generation,
            &resolution.baseline,
            &resolution.skipped,
        ));
    }
    items
}

/// Everything one internal-item run needs, frozen once per worker turn.
///
/// A struct rather than a long positional argument list (the PR-9
/// `WholeMarket` shape): the source-store `policy` was the argument that made
/// `run` exceed clippy's seven-argument limit, and a struct means the next
/// run-wide fact cannot silently shift a positional argument. The item list
/// stays separate because it is the loop's subject, not run-wide context.
pub struct RunContext<'a, 's> {
    pub market: &'a MarketBackend,
    pub session: Option<&'a MaterializeSession<'s>>,
    pub generation_id: &'a str,
    pub generation_manifest_sha256: &'a str,
    pub task_id: &'a str,
    pub run_id: &'a str,
    pub policy: crate::source_store::Policy,
}

/// Execute every pending internal item of `items`, then close the execution.
///
/// The caller appended these items to the running task before the entity
/// items started (see `worker::run_once`), so the task cannot reach a
/// terminal status ahead of them. Items already finished (a resumed run) are
/// skipped; a terminal or cancelled task stops the loop.
pub fn run(ctx: &RunContext, items: &[ItemSpec]) -> Result<(), Error> {
    // The remaining run-wide facts travel to `execute` through `ctx`.
    let RunContext {
        market,
        session,
        task_id,
        run_id,
        ..
    } = *ctx;
    // `security_facts` reads and parses `base.dbf`; the main table and the
    // suspension binding both need the same member facts, so one worker turn
    // parses them at most once (issue-391 §12.1).
    let source_cache: RefCell<Option<DatasetSource>> = RefCell::new(None);
    // One download cache per run (§10.1): `derived:risk_warnings` and
    // `derived:security_names` both read the SZSE short-name table, so the
    // second item re-uses the frozen page instead of fetching it again.
    let run_cache: RefCell<Option<Arc<RunCache>>> = RefCell::new(None);
    for item in items {
        let Some(mut binding) = market.task_item_binding(task_id, &item.item_id)? else {
            continue;
        };
        if !matches!(binding.head.status, Status::Pending | Status::Running) {
            break;
        }
        if binding.item.stage == Stage::Finished {
            continue;
        }
        if binding.head.status == Status::Pending {
            let mut head = binding.head.clone();
            market.transition_task_head(&mut head, None, run_id, Change::Claim)?;
            binding = market
                .task_item_binding(task_id, &item.item_id)?
                .ok_or_else(|| Error::Integrity("derived item vanished after claim".into()))?;
        }
        let mut head = binding.head.clone();
        let mut row = binding.item.clone();
        market.transition_task_head(
            &mut head,
            Some(&mut row),
            run_id,
            Change::StartItem(item.item_id.clone()),
        )?;
        let revision = head.state_revision;
        let outcome = if item.item_id == task_control::derived::DELISTED_SERIES_CHECK {
            // The registrations were frozen into the item on this task's first
            // turn; a resumed turn must compare against the same numbers
            // (`#446` §4.5), not against a state that already absorbed the
            // commits this run made before it stopped. The stored spec is
            // authoritative whenever it carries a baseline.
            let frozen = &binding.item.spec;
            let spec = if frozen
                .frozen_inputs
                .get(crate::data_supply::delisted_series::BASELINE_KEY)
                .is_some()
            {
                frozen
            } else {
                item
            };
            check_delisted_series(market, spec)
        } else {
            match session {
                Some(_) => execute(ctx, item, revision, &source_cache, &run_cache)
                    .map_err(ItemFailure::Plain),
                None => Err(ItemFailure::Plain(Error::Invalid(format!(
                    "{} needs a verified official source; none is installed for this run",
                    item.item_id
                )))),
            }
        };
        match outcome {
            Ok(evidence) => {
                // The index commit finishes its own item (the series commit
                // path owns the item/task bookkeeping); the calendar commit
                // only writes the derived dataset, so its item is finished
                // here.
                if task_control::derived::index_entity(&item.item_id).is_none() {
                    finish(
                        market,
                        task_id,
                        run_id,
                        &item.item_id,
                        true,
                        None,
                        Some(evidence),
                    )?;
                }
            }
            Err(ItemFailure::Plain(error)) if uncertain(&error) => {
                let mut head = market.task_head(task_id)?;
                let Some(mut binding) = market.task_item_binding(task_id, &item.item_id)? else {
                    return Err(Error::Integrity("derived item vanished".into()));
                };
                if binding.item.stage == Stage::Running {
                    market.transition_task_head(
                        &mut head,
                        Some(&mut binding.item),
                        run_id,
                        Change::RequireVerification {
                            item_id: item.item_id.clone(),
                            reason: format!(
                                "derived item result not confirmed after acquisition/commit: {error}"
                            ),
                        },
                    )?;
                }
                return Ok(());
            }
            Err(ItemFailure::Plain(error)) => {
                finish(
                    market,
                    task_id,
                    run_id,
                    &item.item_id,
                    false,
                    Some(error.to_string()),
                    None,
                )?;
            }
            // A check that *finds* something is a finding, not an unknown
            // commit: the item keeps the evidence of what it saw and the run
            // ends `partially_failed` like any other failed internal item.
            Err(ItemFailure::Evidence(error, evidence)) => {
                finish(
                    market,
                    task_id,
                    run_id,
                    &item.item_id,
                    false,
                    Some(error.to_string()),
                    Some(evidence),
                )?;
            }
        }
    }
    close(market, task_id, run_id)
}

/// The run-level download cache (`§10.1`), created on first use and shared by
/// every item of this run.
fn run_cache(cell: &RefCell<Option<Arc<RunCache>>>) -> Arc<RunCache> {
    let mut slot = cell.borrow_mut();
    slot.get_or_insert_with(|| Arc::new(RunCache::new()))
        .clone()
}

/// Why an internal item ended without an outcome. A check item's failure *is* a
/// finding, so it travels with the evidence of what was found; every other
/// failure has only a message.
enum ItemFailure {
    Plain(Error),
    Evidence(Error, Value),
}

/// Execute `derived:delisted_series_check` (M3 PR-5, `#446` §4.5). The baseline
/// comes from the item's frozen inputs — resolved before the entity items ran —
/// and is compared with the series facts they left behind. The item writes no
/// business table: a mismatch fails *this item* with the offending entities and
/// the run ends `partially_failed`; nothing is rebuilt, rewritten or deleted.
fn check_delisted_series(market: &MarketBackend, item: &ItemSpec) -> Result<Value, ItemFailure> {
    use crate::data_supply::delisted_series;
    let started = std::time::Instant::now();
    let baseline = item
        .frozen_inputs
        .get(delisted_series::BASELINE_KEY)
        .ok_or_else(|| {
            ItemFailure::Plain(Error::Integrity(
                "delisted series check item carries no baseline".into(),
            ))
        })
        .and_then(|value| delisted_series::decode_baseline(value).map_err(ItemFailure::Plain))?;
    let report = market
        .check_delisted_series(&baseline)
        .map_err(ItemFailure::Plain)?
        .ok_or_else(|| {
            ItemFailure::Plain(Error::Invalid(
                "delisted series check requires the compact market schema".into(),
            ))
        })?;
    let failed = report.failed();
    let evidence = json!({
        "dataset": item.item_id,
        "as_of": baseline.as_of,
        "checked": report.registered,
        "unchanged": report.unchanged,
        "tail_extended": report.tail_extended,
        "failures": report.failures,
        // Why a terminal entity was not registered (ambiguous issuer, no
        // series yet): evidence, so a smaller `checked` is explainable.
        "skipped": item
            .frozen_inputs
            .get(delisted_series::SKIPPED_KEY)
            .cloned()
            .unwrap_or(Value::Null),
        "elapsed_ms": elapsed_ms(started),
    });
    if failed {
        return Err(ItemFailure::Evidence(
            Error::Invalid(report.failure_message()),
            evidence,
        ));
    }
    Ok(evidence)
}

/// One internal item's acquisition + commit, returning the evidence recorded
/// on its outcome row.
fn execute(
    ctx: &RunContext,
    item: &ItemSpec,
    expected_task_revision: u64,
    source_cache: &RefCell<Option<DatasetSource>>,
    run_cache_cell: &RefCell<Option<Arc<RunCache>>>,
) -> Result<Value, Error> {
    let RunContext {
        market,
        session: Some(session),
        generation_id,
        generation_manifest_sha256,
        task_id,
        run_id,
        policy,
    } = *ctx
    else {
        return Err(Error::Integrity(
            "a derived item needs the run's verified source session".into(),
        ));
    };
    if let Some(entity) = task_control::derived::index_entity(&item.item_id) {
        let index = session
            .index_batch(entity)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let generation_evidence = json!({
            "manifest_sha256": generation_manifest_sha256,
            "snapshot_inputs": index.batch.provenance.snapshot_inputs,
        });
        let receipt = market.commit_index(&IndexWrite {
            index: &index,
            result_id: &result_id(task_id, &item.item_id),
            task_id,
            item_id: &item.item_id,
            run_id,
            expected_task_revision,
            generation_id,
            generation_manifest_sha256,
            generation_evidence: &generation_evidence,
            published_at: Utc::now(),
        })?;
        return Ok(json!({
            "dataset": item.item_id,
            "version_id": receipt.version_id,
        }));
    }
    if item.item_id == task_control::derived::CALENDAR {
        let batch = session
            .calendar_dates()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let calendar = TradingCalendar::union(&batch).map_err(Error::Invalid)?;
        // Measured against the stored 不复权 series on a read connection, so
        // the full scan never holds the write lock (§5.2).
        let coverage = market.measure_calendar_coverage(&calendar)?;
        let generation_evidence = json!({
            "manifest_sha256": generation_manifest_sha256,
            "calendar_inputs": batch
                .inputs
                .iter()
                .map(|input| json!({
                    "entity": input.entity,
                    "member": input.member,
                    "day_member_sha256": input.day_member_sha256,
                    "day_count": input.days.len(),
                }))
                .collect::<Vec<_>>(),
        });
        let registration = market.commit_calendar(&CalendarWrite {
            batch: &batch,
            coverage: &coverage,
            task_id,
            item_id: &item.item_id,
            generation_id,
            generation_manifest_sha256,
            generation_evidence: &generation_evidence,
        })?;
        return Ok(json!({
            "dataset": item.item_id,
            "version_id": registration.version_id,
            "day_count": registration.record_count,
            "coverage": {
                "total_bars": coverage.total_bars,
                "total_bars_scope": "inside [first_day,last_day]",
                "outside_bars": coverage.outside_bars,
                "ratio": coverage.ratio_text(),
                // #446 tail: bars after the calendar's last day are unknown
                // (§3.8), reported here so a lagging index input is visible
                // on the item's own evidence instead of surfacing as an
                // out-of-calendar ratio that fails the run.
                "stale_bars": coverage.stale_bars,
                "stale_days": coverage
                    .stale_days
                    .iter()
                    .map(|(day, bars)| json!({"day": day.to_string(), "bars": bars}))
                    .collect::<Vec<_>>(),
                "stock_last_day": coverage.stock_last_day.map(|day| day.to_string()),
                "calendar_last_day": coverage.calendar_last_day.map(|day| day.to_string()),
            },
        }));
    }
    if item.item_id == task_control::derived::SECURITY_MASTER {
        let started = std::time::Instant::now();
        let snapshot = session
            .security_facts()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let source = DatasetSource::from_snapshot(&snapshot);
        *source_cache.borrow_mut() = Some(source.clone());
        // Bar spans are evidence only: they propose candidate code
        // migrations, never dedup truth (§4.1).
        let spans = market.raw_bar_spans()?;
        let mut candidates = assembly::detect_candidate_migrations(&spans);
        // A-1/A-2 (issue-446 §7, D-45/D-46): the exchange branch is an admitted
        // snapshot when the process has a source store configured. Without one,
        // the item keeps `pending_source` under `Policy::Optional` (stdio,
        // tests, non-ops) and fails with `source_store_unconfigured` under
        // `Policy::Required` (the ops entry of `data_service` on compact), so
        // the production form cannot silently write a zero-exchange master
        // (`silent-path #446` §14.5). A configured source that cannot be
        // fetched fails this item; the previously committed master version
        // stays readable (§6.3) either way.
        let (exchange, exchange_evidence) =
            match crate::official_security::a1a2::run_source(market, policy).map_err(|error| {
                Error::Invalid(format!(
                    "exchange listing/delisting source: {}: {error}",
                    error.code()
                ))
            })? {
                crate::official_security::a1a2::RunSource::Pending(pending) => {
                    (assembly::ExchangeSource::Pending(pending), Value::Null)
                }
                crate::official_security::a1a2::RunSource::Admitted(admitted) => {
                    let evidence = admitted.evidence();
                    // The candidate migration of D-46 is a recorded lead, never
                    // dedup truth (§7.1).
                    candidates.extend(crate::official_security::a1a2::candidate_migrations());
                    (assembly::exchange_source(Some(admitted.snapshot)), evidence)
                }
            };
        let assembled = assembly::assemble_security_master(
            &snapshot.facts.lifecycle,
            &snapshot.facts.arrangements,
            exchange,
            &candidates,
        )
        .map_err(|e| Error::Invalid(e.to_string()))?;
        // The item's write set is 主表 + 上市/退市 + 迁移 (§12.1). The name
        // history is its own M3 item (`derived:security_names`, #446 PR-12),
        // so this item neither writes a name row nor reports a name
        // `pending_source`: no "current name" is invented here. The exchange
        // pending entry, when the branch is not admitted, is already in
        // `assembled.pending_sources`.
        let pending = assembled.pending_sources.clone();
        // The generation binding digest is shared by every whole-market item of
        // one generation (`derived:suspensions` and friends recompute it and
        // refuse a master assembled from another generation), so the A-1/A-2
        // evidence goes on the *item* outcome only, never into the generation
        // evidence.
        let generation_evidence = source.generation_evidence(generation_manifest_sha256);
        let registration = market.commit_security_master(&SecurityMasterWrite {
            assembled: &assembled,
            task_id,
            item_id: &item.item_id,
            generation_id,
            generation_manifest_sha256,
            member_sha256: &source.base_member_sha256,
            security_event_sha256: &source.security_event_sha256,
            generation_evidence: &generation_evidence,
        })?;
        return Ok(json!({
            "dataset": item.item_id,
            "version_id": registration.version_id,
            "row_count": registration.record_count,
            "migration_count": assembled.migrations.len(),
            // Official `base.dbf` ships byte-identical repeat records; the
            // parser collapses them and names the record indices here, so a
            // later audit can tell a dedupe from a missing row (XDM-70, #391).
            "duplicates_collapsed": collapsed_duplicates_json(&snapshot.facts.duplicates_collapsed),
            "exchange_source": exchange_evidence,
            "pending_sources": pending_json(&pending),
            "elapsed_ms": elapsed_ms(started),
        }));
    }
    if item.item_id == task_control::derived::SECURITY_NAMES {
        let started = std::time::Instant::now();
        let source = dataset_source(session, source_cache)?;
        // M3 PR-12 (D-61): the SZSE `SSGSGMXX`/`tab2` table is the only dated
        // name source; SH/BJ are `NOT_AVAILABLE` and write no row. The adapter
        // shares PR-10's frozen entry and parse, so both items read one frozen
        // table per run. Without the source store the item stays
        // `pending_source` (0 rows) under `Optional` and fails with
        // `source_store_unconfigured` under `Required`; a configured store that
        // cannot be fetched fails this item either way and leaves the previous
        // version readable.
        match crate::official_security::security_names::run_source(
            market,
            run_cache(run_cache_cell),
            policy,
        )
        .map_err(|error| {
            Error::Invalid(format!(
                "security name-history source: {}: {error}",
                error.code()
            ))
        })? {
            crate::official_security::security_names::RunSource::Pending(pending) => {
                let pending = vec![pending];
                return Ok(json!({
                    "dataset": item.item_id,
                    "version_id": Value::Null,
                    "row_count": 0,
                    "pending_sources": pending_json(&pending),
                    "elapsed_ms": elapsed_ms(started),
                }));
            }
            crate::official_security::security_names::RunSource::Admitted(admitted) => {
                let evidence = admitted.evidence();
                let generation_evidence = source.generation_evidence(generation_manifest_sha256);
                let registration = market.commit_security_names(&SecurityNameWrite {
                    set: &admitted.set,
                    task_id,
                    item_id: &item.item_id,
                    generation_id,
                    generation_manifest_sha256,
                    member_sha256: &source.base_member_sha256,
                    security_event_sha256: &source.security_event_sha256,
                    generation_evidence: &generation_evidence,
                })?;
                let pending = admitted.set.pending_sources.clone();
                return Ok(json!({
                    "dataset": item.item_id,
                    "version_id": registration.map(|row| row.version_id),
                    "row_count": admitted.set.rows.len(),
                    "coverage_windows": admitted.set.coverage.len(),
                    "not_observed_windows": admitted.set.not_observed_windows(),
                    "source": evidence,
                    "pending_sources": pending_json(&pending),
                    "elapsed_ms": elapsed_ms(started),
                }));
            }
        }
    }
    if item.item_id == task_control::derived::RISK_WARNINGS {
        let started = std::time::Instant::now();
        let source = dataset_source(session, source_cache)?;
        match crate::official_security::risk_warnings::run_source(
            market,
            run_cache(run_cache_cell),
            policy,
        )
        .map_err(|error| {
            Error::Invalid(format!(
                "historical ST/risk-warning source: {}: {error}",
                error.code()
            ))
        })? {
            crate::official_security::risk_warnings::RunSource::Pending(pending) => {
                let pending = vec![pending];
                return Ok(json!({
                    "dataset": item.item_id,
                    "version_id": Value::Null,
                    "row_count": 0,
                    "pending_sources": pending_json(&pending),
                    "elapsed_ms": elapsed_ms(started),
                }));
            }
            crate::official_security::risk_warnings::RunSource::Admitted(admitted) => {
                let evidence = admitted.evidence();
                let generation_evidence = source.generation_evidence(generation_manifest_sha256);
                let registration = market.commit_risk_warnings(&RiskWarningWrite {
                    set: &admitted.set,
                    task_id,
                    item_id: &item.item_id,
                    generation_id,
                    generation_manifest_sha256,
                    member_sha256: &source.base_member_sha256,
                    security_event_sha256: &source.security_event_sha256,
                    generation_evidence: &generation_evidence,
                })?;
                let pending = admitted.set.pending_sources.clone();
                return Ok(json!({
                    "dataset": item.item_id,
                    "version_id": registration.map(|row| row.version_id),
                    "row_count": admitted.set.rows.len(),
                    "coverage_windows": admitted.set.coverage.len(),
                    "not_observed_windows": admitted.set.not_observed_windows(),
                    "source": evidence,
                    "pending_sources": pending_json(&pending),
                    "elapsed_ms": elapsed_ms(started),
                }));
            }
        }
    }
    if item.item_id == task_control::derived::SUSPENSIONS {
        let started = std::time::Instant::now();
        let source = dataset_source(session, source_cache)?;
        let inputs = market.suspension_inputs()?;
        // A-3 (M3 PR-11, D-49/D-60): the SSE structured suspension event table
        // is an admitted source; the SZSE side has no `confirmed` source, so an
        // SZ gap stays `unresolved_gap` and SZ tradability stays `unknown`.
        // Without the source store the branch stays `pending_source` (gap-only,
        // the pre-PR-11 behaviour) under `Optional` and fails with
        // `source_store_unconfigured` under `Required`; a configured source
        // that cannot be fetched fails this item so no `confirmed` row is
        // written (§6.3/§10.1).
        let (seam, exchange_evidence) = match crate::official_security::a3::run_source(
            market,
            &inputs.intervals,
            Utc::now().date_naive(),
            policy,
        )
        .map_err(|error| {
            Error::Invalid(format!(
                "suspension/resumption source: {}: {error}",
                error.code()
            ))
        })? {
            crate::official_security::a3::RunSource::Pending(pending) => {
                let evidence = json!(pending_json(&[pending]));
                (
                    crate::security_master::suspensions::suspension_source(None),
                    evidence,
                )
            }
            crate::official_security::a3::RunSource::Admitted(admitted) => {
                let evidence = admitted.evidence();
                (
                    crate::security_master::suspensions::suspension_source(Some(
                        admitted.snapshot.clone(),
                    )),
                    evidence,
                )
            }
        };
        let set = crate::security_master::suspensions::assemble_suspensions(
            &seam,
            &inputs.intervals,
            &inputs.calendar,
            &inputs.bar_days,
        )
        .map_err(|e| Error::Invalid(e.to_string()))?;
        let generation_evidence = source.generation_evidence(generation_manifest_sha256);
        // Registered as evidence so a later replay can tell a changed
        // master/calendar/bar-day state from changed stored rows (XDM-138).
        let inputs_digest = crate::security_master::suspensions::inputs_digest(
            &inputs.master.version_id,
            &inputs.calendar,
            &inputs.bar_days,
        );
        let registration = market.commit_suspensions(&SuspensionWrite {
            set: &set,
            master: &inputs.master,
            task_id,
            item_id: &item.item_id,
            generation_id,
            generation_manifest_sha256,
            member_sha256: &source.base_member_sha256,
            security_event_sha256: &source.security_event_sha256,
            generation_evidence: &generation_evidence,
            replay_inputs_digest: Some(&inputs_digest),
        })?;
        return Ok(json!({
            "dataset": item.item_id,
            "version_id": registration.map(|row| row.version_id),
            "row_count": set.rows.len(),
            "gap_rows": set.gap_rows,
            "confirmed_rows": set.confirmed_rows,
            "missing_days": set.missing_days(),
            // XDM-134: confirmed intervals that wrote no row stay countable and
            // sampled; neither shape is an item failure and neither produces a
            // suspension row. A deferred interval still explains the covered
            // part to the gap derivation (its known suspension days are not
            // re-derived as gaps).
            "deferred_beyond_calendar": set.deferred_beyond_calendar,
            "deferred_samples": skipped_samples_json(&set.deferred_samples),
            "skipped_no_trading_day": set.skipped_no_trading_day,
            "skipped_samples": skipped_samples_json(&set.skipped_samples),
            "pending_sources": pending_json(&set.pending_sources),
            "exchange_source": exchange_evidence,
            "elapsed_ms": elapsed_ms(started),
        }));
    }
    if item.item_id == task_control::derived::TRADING_RULES {
        let started = std::time::Instant::now();
        let source = crate::trading_rules::rule_source()
            .map_err(|error| Error::Integrity(error.to_string()))?;
        let book = match source {
            crate::trading_rules::RuleSource::Admitted(book) => book,
            crate::trading_rules::RuleSource::Pending(pending) => {
                // A-7 is admitted (D-44/D-59), so this branch is unreachable
                // through `rule_source`; keep it so an adapter that later has
                // no frozen text reports `pending_source` instead of writing
                // an empty rule set.
                return Ok(json!({
                    "dataset": item.item_id,
                    "version_id": Value::Null,
                    "row_count": 0,
                    "pending_sources": pending_json(&[pending]),
                    "elapsed_ms": elapsed_ms(started),
                }));
            }
        };
        // The book is not derived from the generation's members, but the
        // registration's `source_binding_digest` must name a real binding, so
        // the commit records the generation the item first wrote it under.
        let source = dataset_source(session, source_cache)?;
        let generation_evidence = source.generation_evidence(generation_manifest_sha256);
        let commit = market.commit_trading_rules(&TradingRulesWrite {
            book: &book,
            task_id,
            item_id: &item.item_id,
            generation_id,
            generation_manifest_sha256,
            member_sha256: &source.base_member_sha256,
            security_event_sha256: &source.security_event_sha256,
            generation_evidence: &generation_evidence,
        })?;
        let registration = commit.registration.ok_or_else(|| {
            Error::Integrity("the admitted rule book wrote no registration".into())
        })?;
        return Ok(json!({
            "dataset": item.item_id,
            "version_id": registration.version_id,
            "row_count": registration.record_count,
            "price_limit_rule_count": book.price_limits.len(),
            "min_tick_rule_count": book.min_ticks.len(),
            // A-7 is admitted, so the item has no blocked source branch left.
            "pending_sources": [],
            // Unchanged content keeps the stored version instead of minting a
            // new one; the flag is the item's `published`/`no_change` answer.
            "no_change": commit.no_change,
            "elapsed_ms": elapsed_ms(started),
        }));
    }
    if item.item_id == task_control::derived::INDUSTRY_SNAPSHOT {
        let started = std::time::Instant::now();
        let Some(facts) = session
            .industry_facts()
            .map_err(|e| Error::Invalid(e.to_string()))?
        else {
            // This generation ships no `tdxhy.cfg`: there is no industry fact
            // to observe, so the item is a successful no-op. A *present* file
            // that cannot be read or loses its dictionary still fails (§10.1),
            // because then the source did claim to carry the fact.
            return Ok(json!({
                "dataset": item.item_id,
                "no_change": true,
                "reason": "member_absent",
                "elapsed_ms": elapsed_ms(started),
            }));
        };
        // §9.4.1 write order: the source store holds the bytes (blob + fsync +
        // manifest) before any index row is written. A parsing failure happens
        // before registration, like the archive importer, so an unreadable
        // member never enters the store a later replay would have to trust.
        let store = crate::industry::source_store_for_database(market.tasks().path());
        let snapshot = crate::industry::register_generation_members(
            &store,
            &crate::industry::GenerationMembers {
                manifest_sha256: &facts.manifest_sha256,
                tdxhy_member: &facts.tdxhy_member,
                tdxhy_bytes: &facts.tdxhy_bytes,
                tdxhy_publisher_last_modified: facts.tdxhy_publisher_last_modified.as_deref(),
                tdxhy_observed_at: &facts.tdxhy_observed_at,
                incon_member: &facts.incon_member,
                incon_bytes: &facts.incon_bytes,
                incon_publisher_last_modified: facts.incon_publisher_last_modified.as_deref(),
                incon_observed_at: &facts.incon_observed_at,
                base_zip_sha256: &facts.base_zip_sha256,
            },
        )
        .map_err(|e| Error::Integrity(e.to_string()))?;
        let commit = market.commit_industry_snapshot(
            &crate::sqlite_market::industry::IndustrySnapshotWrite {
                facts: &snapshot.facts,
                previous_member_sha256: snapshot.previous_member_sha256.as_deref(),
            },
        )?;
        let record = &snapshot.facts.record;
        // M3 PR-8: register the industry facts this item produced. Both
        // evidence digests are read back from the store before the rows are
        // written (§8.2: irreproducible facts carry verified evidence).
        if let Some(dictionary_sha256) = record.dictionary_sha256.as_deref() {
            let facts = crate::generation_gate::industry_fact_rows(
                &store,
                &facts.manifest_sha256,
                &record.store_sha256,
                dictionary_sha256,
            )
            .map_err(|error| Error::Integrity(error.to_string()))?;
            market.record_generation_facts(&facts)?;
        }
        return Ok(json!({
            "dataset": item.item_id,
            // The snapshot content identity: the item's version, since the M3
            // tables carry no `derived_dataset_versions` registration (§5.1).
            "version_id": record.member_sha256,
            "snapshot_sha256": record.member_sha256,
            "store_sha256": record.store_sha256,
            "source_kind": record.source_kind.as_str(),
            "evidence_grade": record.evidence_grade,
            // A content re-observed inside one run (`A → A`) continues the open
            // interval instead of opening a second one (§5.3); `A → B → A`
            // does not, so the flag is the observation link, not a lookup.
            "continues_run": snapshot.previous_member_sha256.as_deref()
                == Some(record.member_sha256.as_str()),
            "effective_from_basis": record
                .boundary()
                .ok()
                .flatten()
                .map(|(_, basis)| basis.as_str().to_string()),
            "row_count": record.row_count,
            "scope_member_count": record.scope_member_count,
            "dictionary_sha256": record.dictionary_sha256,
            "snapshot_rows": commit.snapshot_rows,
            "assignment_rows": commit.assignment_rows,
            "superseded_same_day": commit.superseded,
            // Unchanged content keeps the stored snapshot and intervals instead
            // of re-stamping them (§5.1).
            "no_change": commit.no_change,
            "elapsed_ms": elapsed_ms(started),
        }));
    }
    Err(Error::Integrity(format!(
        "{} is not a known derived dataset item",
        item.item_id
    )))
}

/// Stable result identity of one internal item (replay anchor of its event).
fn result_id(task_id: &str, item_id: &str) -> String {
    let digest = crate::hash::sha256(
        serde_json::to_string(&json!([task_id, item_id]))
            .unwrap_or_default()
            .as_bytes(),
    );
    format!("derived-result-{digest}")
}

/// Record an internal item's outcome. The index path finishes its own item
/// inside the commit, so this is used for the calendar item and for failures;
/// a row that is no longer `running` (already finished, or the task moved on)
/// is left untouched instead of inventing a second outcome.
fn finish(
    market: &MarketBackend,
    task_id: &str,
    run_id: &str,
    item_id: &str,
    success: bool,
    error: Option<String>,
    evidence: Option<Value>,
) -> Result<(), Error> {
    let mut head = market.task_head(task_id)?;
    if !matches!(head.status, Status::Running | Status::CancelRequested) {
        return Ok(());
    }
    let Some(mut binding) = market.task_item_binding(task_id, item_id)? else {
        return Ok(());
    };
    if binding.item.stage != Stage::Running {
        return Ok(());
    }
    let item: &mut TaskItem = &mut binding.item;
    market.transition_task_head(
        &mut head,
        Some(item),
        run_id,
        Change::FinishItem(
            item_id.into(),
            ItemOutcome {
                success,
                error,
                evidence,
            },
        ),
    )
}

/// Close the execution once every item has an outcome. `FinishExecution`
/// computes `succeeded`/`partially_failed`/`failed` from the counters, so a
/// failed internal item lands as `partially_failed` without touching the
/// entity outcomes (§9.2). A terminal or cancelled task is left alone.
fn close(market: &MarketBackend, task_id: &str, run_id: &str) -> Result<(), Error> {
    let mut head = market.task_head(task_id)?;
    if !matches!(head.status, Status::Running | Status::CancelRequested) {
        return Ok(());
    }
    market.transition_task_head(&mut head, None, run_id, Change::FinishExecution)
}

/// Same classification as the per-entity runner: a storage/outcome-unknown
/// error may have committed, so it is never recorded as a plain failure.
fn uncertain(error: &Error) -> bool {
    matches!(
        error,
        Error::OutcomeUnknown(_)
            | Error::Storage(_)
            | Error::Task(
                task_control::Error::StorageUnavailable(_) | task_control::Error::OutcomeUnknown(_)
            )
    )
}

/// The member facts shared by every M2 dataset of one worker turn: which
/// `base.dbf` member and which gbbq event set the generation froze
/// (issue-391 §12.1). The main table and the suspensions must bind the *same*
/// inputs; caching the values keeps the two items from re-reading the archive
/// while still failing closed when the member is unreadable.
#[derive(Debug, Clone)]
struct DatasetSource {
    base_member_sha256: String,
    security_event_sha256: String,
    base_member: String,
    gbbq_member: String,
    gbbq_member_sha256: String,
}

impl DatasetSource {
    fn from_snapshot(facts: &crate::official_market::SecuritySnapshotFacts) -> Self {
        Self {
            base_member_sha256: facts.base_member_sha256.clone(),
            security_event_sha256: facts.security_event_sha256.clone(),
            base_member: facts.base_member.clone(),
            gbbq_member: facts.gbbq_member.clone(),
            gbbq_member_sha256: facts.gbbq_member_sha256.clone(),
        }
    }

    /// The generation evidence the source binding stores: member names *and*
    /// digests, so a later audit can tell which files the dataset read.
    fn generation_evidence(&self, manifest_sha256: &str) -> Value {
        json!({
            "manifest_sha256": manifest_sha256,
            "base_member": &self.base_member,
            "base_member_sha256": &self.base_member_sha256,
            "gbbq_member": &self.gbbq_member,
            "gbbq_member_sha256": &self.gbbq_member_sha256,
            "security_event_sha256": &self.security_event_sha256,
        })
    }
}

/// The turn's [`DatasetSource`], parsed on first use. A dataset whose item
/// runs before the main table (never the order this module emits) pays the
/// parse itself instead of reading another generation's facts.
fn dataset_source(
    session: &MaterializeSession<'_>,
    cache: &RefCell<Option<DatasetSource>>,
) -> Result<DatasetSource, Error> {
    if let Some(source) = cache.borrow().as_ref() {
        return Ok(source.clone());
    }
    let snapshot = session
        .security_facts()
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let source = DatasetSource::from_snapshot(&snapshot);
    *cache.borrow_mut() = Some(source.clone());
    Ok(source)
}

/// Byte-identical `base.dbf` repeats collapsed by the parser, as the main
/// table's item evidence reports them (`{market, code, records}`).
fn collapsed_duplicates_json(
    duplicates: &[crate::official_security::CollapsedDuplicate],
) -> Vec<Value> {
    duplicates
        .iter()
        .map(|duplicate| {
            json!({
                "market": duplicate.market,
                "code": duplicate.code,
                "records": duplicate.records,
            })
        })
        .collect()
}

/// `pending_source` as the item evidence reports it (the same object the
/// dataset evidence stores, so a run and its stored version agree).
fn pending_json(pending: &[PendingSource]) -> Vec<Value> {
    pending
        .iter()
        .map(|source| {
            json!({
                "dataset": source.dataset,
                "source": source.source,
                "reason": source.reason,
            })
        })
        .collect()
}

/// Confirmed intervals that wrote no row because the calendar cannot place them
/// (XDM-134), as the item evidence reports them (`entity`, `start_day`,
/// `end_day`, `source_record_id`). The counters stay complete next to the
/// truncated sample list.
fn skipped_samples_json(
    samples: &[crate::security_master::suspensions::SkippedInterval],
) -> Vec<Value> {
    samples
        .iter()
        .map(|sample| {
            json!({
                "entity": sample.entity,
                "start_day": sample.start_day.format("%Y-%m-%d").to_string(),
                "end_day": sample.end_day.format("%Y-%m-%d").to_string(),
                "source_record_id": sample.source_record_id,
            })
        })
        .collect()
}

/// Whole milliseconds an internal item spent acquiring and committing. It is
/// evidence for the SLO measurement (§12.1 "耗时记录"), never a task timeout.
fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
