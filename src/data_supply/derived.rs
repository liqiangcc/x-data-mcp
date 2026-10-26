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
use std::collections::BTreeSet;

use crate::{
    data_supply::generation::SOURCE_GENERATION_KEY,
    index_registry,
    market_storage::{Error, MarketRepository},
    official_market::MaterializeSession,
    sqlite_market::{backend::MarketBackend, calendar::CalendarWrite, index::IndexWrite},
    task_control::{self, Change, ItemOutcome, ItemSpec, Stage, Status, TaskItem},
    trading_calendar::TradingCalendar,
};

/// `task_items.progress_unit` of a derived dataset item. The unit is "one
/// dataset", not "one security/period".
pub const PROGRESS_UNIT: &str = "derived_dataset";

/// The internal items a daily plan-run task maintains, in execution order:
/// the nine benchmark indices first, the calendar last (its inputs are index
/// members, §9.2).
pub fn item_specs(generation: &Value) -> Vec<ItemSpec> {
    let mut items = Vec::with_capacity(index_registry::INDEX_SERIES.len() + 1);
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
    items
}

/// Execute every pending internal item of `items`, then close the execution.
///
/// The caller appended these items to the running task before the entity
/// items started (see `worker::run_once`), so the task cannot reach a
/// terminal status ahead of them. Items already finished (a resumed run) are
/// skipped; a terminal or cancelled task stops the loop.
pub fn run(
    market: &MarketBackend,
    session: Option<&MaterializeSession<'_>>,
    generation_id: &str,
    generation_manifest_sha256: &str,
    task_id: &str,
    run_id: &str,
    items: &[ItemSpec],
) -> Result<(), Error> {
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
        let outcome = match session {
            Some(session) => execute(
                market,
                session,
                generation_id,
                generation_manifest_sha256,
                task_id,
                run_id,
                item,
                revision,
            ),
            None => Err(Error::Invalid(format!(
                "{} needs a verified official source; none is installed for this run",
                item.item_id
            ))),
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
            Err(error) if uncertain(&error) => {
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
            Err(error) => {
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
        }
    }
    close(market, task_id, run_id)
}

/// One internal item's acquisition + commit, returning the evidence recorded
/// on its outcome row.
#[allow(clippy::too_many_arguments)]
fn execute(
    market: &MarketBackend,
    session: &MaterializeSession<'_>,
    generation_id: &str,
    generation_manifest_sha256: &str,
    task_id: &str,
    run_id: &str,
    item: &ItemSpec,
    expected_task_revision: u64,
) -> Result<Value, Error> {
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
                "outside_bars": coverage.outside_bars,
                "ratio": coverage.ratio_text(),
            },
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

#[cfg(test)]
mod tests;
