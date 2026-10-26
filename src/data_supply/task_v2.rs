//! Task surface v2 projections (v8 PR-5, #387 §7.2 rows 9/14, §14 PR-5).
//!
//! Pure shape functions: they take the v1 observation types and return the v2
//! wire projection. No storage, no SQL, no MCP. The observer calls them from
//! both the materialized (≤100 items) and the bounded (keyset) progress paths,
//! so the two paths cannot drift apart.
//!
//! v2 changes exactly four things:
//!
//! 1. `item_id` is structured (`{dataset, entity, period}`) everywhere the
//!    response carries one; a server-internal derived item keeps its whole id
//!    in `dataset` and has no entity/period (issue-383 §9.2 items);
//! 2. `failed_by_reason` is a top-level field of every task response (v1 only
//!    had it under `plan`, and only for bounded large tasks);
//! 3. `get_task` carries the counters plus a bounded sample of failed items;
//! 4. the task's own contract version is `task_contract_version`, and the wire
//!    version is `wire_contract_version` (row 14).
use super::wire::*;
use crate::market_storage::Error;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Failed items grouped by `reason_code`. One alias so the task surface, the
/// progress read scope and the repositories all speak the same type.
pub type FailedByReason = BTreeMap<String, usize>;

/// Longest `error` a v2 task response carries. A failed item's full error can
/// be tens of kilobytes (a revision error lists every revised day of the
/// history); the class is in `reason_code`, the full text stays in the task
/// record.
pub const BOUNDED_ERROR_CHARS: usize = 240;
/// Failed items a `get_task` v2 diagnosis samples before truncating. The
/// remainder is reachable through `get_task_progress`' `unfinished` pages.
pub const ERROR_SAMPLE: usize = 20;

/// Every stable failure class `reason_code` can return. A `list_tasks` v2
/// `reason_code` filter must be one of these, so a typo is rejected instead of
/// matching nothing.
pub const REASON_CODES: &[&str] = &[
    "REVISION_UNVERIFIED",
    "PUBLISH_PRECONDITION",
    "NO_BARS_IN_RANGE",
    "OTHER",
];

/// Stable class of a failed item's error text. The mapping is part of the
/// contract (`generic-data-v7.md` §5) and stays in this one place.
pub fn reason_code(error: &str) -> &'static str {
    if error.contains("RevisionUnverified") {
        "REVISION_UNVERIFIED"
    } else if error.contains("PUBLISH_PRECONDITION") {
        "PUBLISH_PRECONDITION"
    } else if error.contains("no bars inside the accepted range") {
        "NO_BARS_IN_RANGE"
    } else {
        "OTHER"
    }
}

/// `error` capped at [`BOUNDED_ERROR_CHARS`] with the original length kept in
/// the marker, so the class stays readable without the full text.
pub fn bounded_error(error: &str) -> String {
    if error.chars().count() > BOUNDED_ERROR_CHARS {
        let head: String = error.chars().take(BOUNDED_ERROR_CHARS).collect();
        format!("{head}… ({} chars total)", error.chars().count())
    } else {
        error.to_owned()
    }
}

/// Structured identity of one stored `item_id` string. The stored form of a
/// security item is `["market","sh.600000","day"]`; any other text (the
/// `derived:*` items, or a shape this version does not know) becomes the
/// dataset name verbatim, so no entity is ever invented.
pub fn item_id(text: &str) -> ItemId {
    match serde_json::from_str::<[String; 3]>(text) {
        Ok([dataset, entity, period]) => ItemId {
            dataset,
            entity: Some(entity),
            period: Some(period),
        },
        Err(_) => ItemId {
            dataset: text.to_owned(),
            entity: None,
            period: None,
        },
    }
}

/// Class and bounded text of one failed item. Idempotent: an item that already
/// carries a class was bounded at read time (large-task keyset pages), so its
/// error is kept instead of truncating the truncation marker again.
fn failure(item: &ProgressItem) -> (String, Option<String>) {
    let text = item.error.as_deref().unwrap_or("");
    let (code, error) = match &item.reason_code {
        Some(code) => (code.clone(), item.error.clone()),
        None => (reason_code(text).to_owned(), Some(bounded_error(text))),
    };
    (code, error.filter(|error| !error.is_empty()))
}

/// One failed item in a bounded diagnosis sample.
pub fn item_error(item: &ProgressItem) -> ItemError {
    let (reason_code, error) = failure(item);
    ItemError {
        item_id: item_id(&item.item_id),
        reason_code,
        error,
    }
}

/// Failed items grouped by class. Counts all failed items of a task; the
/// caller passes the full item set (small tasks) or the repository's
/// narrow count query (bounded tasks).
pub fn failed_by_reason(items: &[ProgressItem]) -> FailedByReason {
    let mut counts = BTreeMap::new();
    for item in items.iter().filter(|item| item.success == Some(false)) {
        let (code, _) = failure(item);
        *counts.entry(code).or_insert(0) += 1;
    }
    counts
}

/// v2 task summary: the v1 fields with the task's own contract version renamed
/// so it cannot be confused with the wire contract version (row 14).
pub fn task_summary(task: TaskSummary) -> Value {
    let mut value = serde_json::to_value(task).expect("a task summary always serializes");
    if let Some(map) = value.as_object_mut()
        && let Some(version) = map.remove("contract_version")
    {
        map.insert("task_contract_version".into(), version);
    }
    value
}

/// v2 `get_task`: v1 summary + counters + bounded failure diagnosis.
pub fn task_view(
    task: TaskSummary,
    counts: TaskCounts,
    failed_by_reason: FailedByReason,
    sample: &[ProgressItem],
) -> Value {
    let mut value = task_summary(task);
    let map = value.as_object_mut().expect("a task summary is an object");
    map.insert(
        "wire_contract_version".into(),
        json!(ReadContractVersion::V2),
    );
    for (key, number) in [
        ("total", counts.total),
        ("pending", counts.pending),
        ("running", counts.running),
        ("finished", counts.finished),
        ("succeeded", counts.succeeded),
        ("failed", counts.failed),
    ] {
        map.insert(key.into(), json!(number));
    }
    if !failed_by_reason.is_empty() {
        map.insert("failed_by_reason".into(), json!(failed_by_reason));
        map.insert(
            "errors".into(),
            json!(sample.iter().map(item_error).collect::<Vec<_>>()),
        );
    }
    value
}

/// v2 progress item: identity inside `item_id`, failed items always classified
/// and bounded.
pub fn progress_item_v2(item: &ProgressItem) -> ProgressItemV2 {
    let (error, reason_code) = if item.success == Some(false) {
        let (code, error) = failure(item);
        (error, Some(code))
    } else {
        (item.error.clone().filter(|error| !error.is_empty()), None)
    };
    ProgressItemV2 {
        item_id: item_id(&item.item_id),
        progress_unit: item.progress_unit.clone(),
        stage: item.stage.clone(),
        success: item.success,
        error,
        reason_code,
    }
}

fn frozen_range_v2(range: FrozenDailyRange) -> FrozenDailyRangeV2 {
    FrozenDailyRangeV2 {
        item_id: item_id(&range.item_id),
        base_version_id: range.base_version_id,
        base_integrity_sha256: range.base_integrity_sha256,
        start_date: range.start_date,
        end_date: range.end_date,
    }
}

fn range_result_v2(result: RangeExecutionResult) -> RangeExecutionResultV2 {
    RangeExecutionResultV2 {
        item_id: item_id(&result.item_id),
        result_id: result.result_id,
        base_version: result.base_version,
        result_version: result.result_version,
        no_change: result.no_change,
        acquired_at: result.acquired_at,
    }
}

fn frozen_yearly_v2(input: FrozenYearlyInput) -> FrozenYearlyInputV2 {
    FrozenYearlyInputV2 {
        item_id: item_id(&input.item_id),
        base_daily_version_id: input.base_daily_version_id,
        base_integrity_sha256: input.base_integrity_sha256,
        years: input.years,
        publication: input.publication,
        comparison: input.comparison,
    }
}

fn yearly_result_v2(result: YearlyExecutionResult) -> YearlyExecutionResultV2 {
    YearlyExecutionResultV2 {
        item_id: item_id(&result.item_id),
        result_id: result.result_id,
        base_daily_version: result.base_daily_version,
        result_version: result.result_version,
        no_change: result.no_change,
        derived_at: result.derived_at,
    }
}

/// v2 `get_task_progress`: the v1 page with structured item identities, the
/// wire version, and one top-level `failed_by_reason`. The plan keeps its
/// identity fields; its own `failed_by_reason` is not repeated (v1 shape under
/// `plan` stays what it was for v1 callers).
pub fn progress_view(page: ProgressPage, failed_by_reason: FailedByReason) -> Result<Value, Error> {
    let ProgressPage {
        snapshot_id,
        snapshot_time,
        task,
        total,
        finished,
        succeeded,
        failed,
        pending,
        running,
        execution_percent,
        unfinished,
        next_cursor,
        page_complete,
        plan,
        publication_versions,
        frozen_daily_ranges,
        range_results,
        frozen_yearly_inputs,
        yearly_results,
        stopped_write_check,
        publication_check,
    } = page;
    let mut value = json!({
        "wire_contract_version": ReadContractVersion::V2,
        "snapshot_id": snapshot_id,
        "snapshot_time": snapshot_time,
        "task": task_summary(task),
        "total": total,
        "finished": finished,
        "succeeded": succeeded,
        "failed": failed,
        "pending": pending,
        "running": running,
        "execution_percent": execution_percent,
        "unfinished": unfinished.iter().map(progress_item_v2).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
        "page_complete": page_complete,
        "publication_versions": publication_versions,
        "frozen_daily_ranges": frozen_daily_ranges
            .into_iter()
            .map(frozen_range_v2)
            .collect::<Vec<_>>(),
        "range_results": range_results
            .into_iter()
            .map(range_result_v2)
            .collect::<Vec<_>>(),
        "frozen_yearly_inputs": frozen_yearly_inputs
            .into_iter()
            .map(frozen_yearly_v2)
            .collect::<Vec<_>>(),
        "yearly_results": yearly_results
            .into_iter()
            .map(yearly_result_v2)
            .collect::<Vec<_>>(),
        "stopped_write_check": stopped_write_check,
        "publication_check": publication_check,
    });
    let map = value
        .as_object_mut()
        .ok_or_else(|| Error::Integrity("progress projection is not an object".into()))?;
    if let Some(plan) = plan {
        map.insert(
            "plan".into(),
            json!({
                "plan_id": plan.plan_id,
                "generation_id": plan.generation_id,
                "target_day": plan.target_day,
            }),
        );
    }
    if !failed_by_reason.is_empty() {
        map.insert("failed_by_reason".into(), json!(failed_by_reason));
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
