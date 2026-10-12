//! Schema-21 write path (S2): one event + one series-state mutation per
//! item inside a single write transaction (SPEC-LATEST-ONLY §3, RULINGS
//! A2/A4). The event row is the replay anchor (`result_id` UNIQUE) and the
//! durable result record; `UNIQUE(task_id, item_id)` keeps one result per
//! task item; `market_series.last_event_seq` is the CAS field compared on
//! every state-mutating commit.
use chrono::{DateTime, Datelike, Utc};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::Digest as _;

use super::compact_repo::period_code;
use super::{evidence_store, integrity, series, storage};
use crate::compact_market::{self, CompactBar, Digest, EvidenceKind, SourceBinding};
use crate::market_data::{Bar, CoverageStatus, Period};
use crate::market_storage::{Error, Receipt};
use crate::task_control;

const OUTCOME_LIMIT: usize = 192;
/// Durable `algorithm_version` labels stored on event rows (the same
/// labels the v19 result rows carried).
pub(crate) const UPDATE_ALGO: &str = "local-range-update-v1";
pub(crate) const COLLECTION_ALGO: &str = "local-collection-v1";

fn invalid(message: impl std::fmt::Display) -> Error {
    Error::Invalid(message.to_string())
}

fn conflict_or_storage(error: rusqlite::Error) -> Error {
    match error {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                ..
            },
            _,
        ) => Error::Conflict(error.to_string()),
        _ => storage(error),
    }
}

/// What the commit does to the series' bar content.
#[derive(Debug)]
pub(crate) enum SeriesChange<'a> {
    /// No series row may exist; inserts the row and every bar.
    FirstPublish { bars: &'a [CompactBar] },
    /// Pure tail extension: every upsert must sort strictly after the
    /// stored `last_day`; the fold continues from the stored digest.
    Append { upserts: &'a [CompactBar] },
    /// Non-prefix rewrite (`continuity_mode=3`, `revision_ref`-style):
    /// the stored bars are deleted and replaced wholesale.
    Replace { bars: &'a [CompactBar] },
    /// A5: metadata-only revision — content digest unchanged, the
    /// supplied binding/evidence/coverage fields update the state row.
    Metadata,
    /// Observed range identical to the stored content — event + task
    /// bookkeeping only; the state row's `last_event_seq` still advances
    /// so the event log stays closed (A2).
    NoChange,
    /// Yearly derivation commit (S3): `upserts` carries the candidate's
    /// affected-year bars; the merge/fold happens inside the commit.
    /// `daily_base_digest` is the daily `content_digest` the derivation
    /// was verified against — the commit re-checks it against the current
    /// daily head inside the same transaction and fails `Stale`.
    Yearly {
        upserts: &'a [CompactBar],
        daily_base_digest: Digest,
        /// `local-natural-year-v1` provenance string stored on the lineage
        /// row and the event.
        algorithm_version: &'a str,
        /// Derivation as-of day (+8 calendar day) for the lineage row.
        as_of_day: i64,
    },
}

/// One item commit on the latest-only store.
pub(crate) struct Commit<'a> {
    pub result_id: &'a str,
    pub task_id: &'a str,
    pub item_id: &'a str,
    pub run_id: &'a str,
    pub occupancy_key: &'a str,
    pub expected_task_revision: u64,
    pub entity: &'a str,
    pub period: Period,
    /// CAS on the current head's `version_id`: `None` requires the series
    /// row be absent (first publish); `Some(id)` must equal the stored id.
    /// `NoChange` still uses `Some(base)` so a concurrent replacement is
    /// detected instead of logging a no-change against a moved head.
    pub base_version_id: Option<&'a str>,
    pub change: SeriesChange<'a>,
    pub range_start_day: i64,
    pub range_end_day: i64,
    pub algorithm_version: &'a str,
    /// Digest of the fold over the observed window (append upserts /
    /// no-change range / committed range); `None` where no window was
    /// folded.
    pub window_digest: Option<Digest>,
    /// Metadata fields for the new state row — `None` inherits stored
    /// values. `Metadata`/`Append`-with-metadata commits must supply them.
    pub source_binding_digest: Option<Digest>,
    pub evidence_digest: Option<Digest>,
    pub coverage_status: Option<CoverageStatus>,
    /// A5 flag: any of the metadata fields moved relative to the stored row.
    pub metadata_changed: bool,
    /// Event-row evidence digest (continuity/audit record) — must exist in
    /// `evidence_bundles`.
    pub event_evidence_digest: Digest,
    /// Provenance timestamp for `first_publish` series rows.
    pub published_at: Option<DateTime<Utc>>,
}

/// Canonical result identity digest stored on the event row — replay and
/// audit compare this, never the caller's declared candidate label.
#[allow(clippy::too_many_arguments)]
pub(crate) fn event_result_digest(
    result_id: &str,
    task_id: &str,
    item_id: &str,
    kind: &str,
    entity: &str,
    period: Period,
    new_version_id: &str,
    task_revision: i64,
) -> Result<Digest, Error> {
    let value = json!({
        "result_id": result_id,
        "task_id": task_id,
        "item_id": item_id,
        "kind": kind,
        "entity": entity,
        "period": period_code(period),
        "result_version_id": new_version_id,
        "task_revision": task_revision,
    });
    let canonical = serde_json_canonicalizer::to_string(&value)
        .map_err(|_| integrity("event result identity is not canonicalizable"))?;
    Ok(sha2::Sha256::digest(canonical.as_bytes()).into())
}

fn result_outcome_json(result_id: &str, result_kind: &str) -> Result<String, Error> {
    let value = serde_json::json!({
        "error": Value::Null,
        "evidence": {"result_id": result_id, "result_kind": result_kind},
        "success": true,
    });
    let canonical = serde_json_canonicalizer::to_string(&value)
        .map_err(|_| integrity("outcome is not canonicalizable"))?;
    if canonical.len() > OUTCOME_LIMIT {
        return Err(integrity("outcome_json exceeds byte limit"));
    }
    Ok(canonical)
}

fn coverage_code(status: CoverageStatus) -> i64 {
    match status {
        CoverageStatus::Complete => 1,
        CoverageStatus::Incomplete => 2,
        CoverageStatus::Unknown => 3,
    }
}

/// Replay anchor: a committed event under this `result_id` returns its
/// receipt; a mismatched identity fails closed.
fn replay_receipt(c: &Connection, w: &Commit) -> Result<Option<Receipt>, Error> {
    let Some(event) = series::event_by_result(c, w.result_id)? else {
        return Ok(None);
    };
    if event.task_id != w.task_id || event.item_id != w.item_id {
        return Err(Error::Conflict("result identity conflict".into()));
    }
    Ok(Some(Receipt {
        publication_id: w.result_id.into(),
        version_id: series::version_id(&event.new_content_digest),
        task_id: w.task_id.into(),
        task_revision: event.task_revision as u64,
    }))
}

/// One task item may produce exactly one market event
/// (`UNIQUE(task_id,item_id)` is the backstop; this keeps the error text).
fn require_item_unused(c: &Connection, w: &Commit) -> Result<(), Error> {
    let exists: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM market_events WHERE task_id=?1 AND item_id=?2)",
            params![w.task_id, w.item_id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if exists {
        return Err(Error::Conflict("task item already has a result".into()));
    }
    Ok(())
}

/// The task/item binding gate, unchanged from the v19 results path: the
/// task must be running under this run, the item running with matching
/// target and occupancy keys.
fn check_binding(c: &Connection, w: &Commit) -> Result<(), Error> {
    let binding = crate::sqlite_tasks::read_item_binding(c, w.task_id, w.item_id)
        .map_err(Error::Task)?
        .ok_or(Error::Task(task_control::Error::TaskNotFound))?;
    if binding.head.visible_status(w.run_id) != task_control::Status::Running
        || binding.head.owner_run_id.as_deref() != Some(w.run_id)
    {
        return Err(Error::Conflict("task owner/state".into()));
    }
    let expected_target = json!({
        "symbol": w.entity,
        "period": match w.period {
            Period::Day => "day",
            Period::Year => "year",
        },
    });
    if binding.item.stage != task_control::Stage::Running
        || binding.item.spec.item_id != w.item_id
        || binding.item.spec.target != expected_target
        || !binding.item.spec.scope_keys.contains(w.occupancy_key)
        || !binding.scopes_bound
    {
        return Err(Error::Conflict("task target/occupancy".into()));
    }
    Ok(())
}

/// Marks the item finished and advances the task counters — the same task
/// bookkeeping the v19 results path ran (`task_revision` CAS is inside
/// the tasks UPDATE).
fn finish_item_and_task(c: &Connection, w: &Commit, task_revision: i64) -> Result<(), Error> {
    let outcome_json = result_outcome_json(w.result_id, "market_update")?;
    let expected_i64 = i64::try_from(w.expected_task_revision)
        .map_err(|_| integrity("task revision out of range"))?;
    let changed = c
        .execute(
            "UPDATE task_items SET stage='finished',outcome_json=?1 WHERE task_id=?2 AND item_id=?3 AND stage='running' AND outcome_json IS NULL",
            params![outcome_json, w.task_id, w.item_id],
        )
        .map_err(storage)?;
    if changed != 1 {
        return Err(Error::Conflict("task item is not running".into()));
    }
    let changed = c
        .execute(
            "UPDATE tasks SET state_revision=?1,items_running=items_running-1,items_finished=items_finished+1,items_succeeded=items_succeeded+1,status=CASE WHEN items_finished+1=item_count THEN CASE WHEN items_succeeded+1=item_count THEN 'succeeded' ELSE 'partially_failed' END ELSE status END WHERE task_id=?2 AND status='running' AND state_revision=?3 AND items_running>0",
            params![task_revision, w.task_id, expected_i64],
        )
        .map_err(storage)?;
    if changed != 1 {
        return Err(Error::Conflict(
            "task state does not match expectation".into(),
        ));
    }
    let (terminal, finished, succeeded): (bool, i64, i64) = c
        .query_row(
            "SELECT status IN ('succeeded','partially_failed','failed','cancelled','interrupted'),items_finished,items_succeeded FROM tasks WHERE task_id=?1",
            [w.task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(storage)?;
    if terminal {
        let result = serde_json::json!({
            "finished_items": finished,
            "successful_items": succeeded,
        });
        let changed = c
            .execute(
                "UPDATE tasks SET result_json=?1 WHERE task_id=?2 AND result_json IS NULL",
                params![result.to_string(), w.task_id],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(integrity("terminal task already carries a result"));
        }
        c.execute("DELETE FROM scope_occupancy WHERE task_id=?1", [w.task_id])
            .map_err(storage)?;
    }
    Ok(())
}

fn check_bars(bars: &[CompactBar]) -> Result<(), Error> {
    if bars.is_empty() {
        return Err(integrity("commit carries no bars"));
    }
    if bars.windows(2).any(|p| p[0].trade_day >= p[1].trade_day) {
        return Err(integrity("commit bars are not strictly increasing"));
    }
    Ok(())
}

/// The schema-21 commit: replay → binding/CAS gates → event → series
/// state mutation → bars → task bookkeeping — all inside the caller's
/// `BEGIN IMMEDIATE` transaction (A4: no autocommit inside).
pub(crate) fn commit_series_tx(c: &Connection, w: &Commit) -> Result<Receipt, Error> {
    compact_market::epoch_date(w.range_start_day).map_err(integrity)?;
    compact_market::epoch_date(w.range_end_day).map_err(integrity)?;
    if w.range_start_day > w.range_end_day {
        return Err(integrity("commit range inverted"));
    }
    if let Some(receipt) = replay_receipt(c, w)? {
        return Ok(receipt);
    }
    require_item_unused(c, w)?;
    check_binding(c, w)?;

    let head = series::read_series(c, w.entity, w.period)?;
    match (w.base_version_id, &head) {
        (None, Some(_)) => return Err(Error::Conflict("effective head already exists".into())),
        (None, None) => {}
        (Some(_), None) => {
            return Err(Error::NotFound);
        }
        (Some(expected), Some(row)) if row.version_id != expected => {
            return Err(Error::Stale(format!(
                "effective head changed: expected {expected}, found {}",
                row.version_id
            )));
        }
        (Some(_), Some(_)) => {}
    }

    // Yearly commits own their kind/merge logic; the shared gates above
    // (replay, item binding, owner/state, head CAS) still apply.
    if matches!(w.change, SeriesChange::Yearly { .. }) && w.period != Period::Year {
        return Err(integrity("SeriesChange::Yearly requires period=year"));
    }
    if let SeriesChange::Yearly {
        upserts,
        daily_base_digest,
        algorithm_version,
        as_of_day,
    } = &w.change
    {
        return commit_yearly_inner(
            c,
            w,
            head.as_ref(),
            upserts,
            daily_base_digest,
            algorithm_version,
            *as_of_day,
        );
    }
    if w.period == Period::Year {
        return Err(integrity("yearly series require SeriesChange::Yearly"));
    }

    // Resolve the kind/effect and the new state fields.
    let (kind, outcome, continuity, prefix_bytes, old_digest, new_digest): (
        &'static str,
        i64,
        Option<i64>,
        Option<i64>,
        Option<Digest>,
        Digest,
    ) = match (&w.change, &head) {
        (SeriesChange::FirstPublish { bars }, None) => {
            check_bars(bars)?;
            (
                "first_publish",
                2,
                None,
                None,
                None,
                series::fold_bars(w.entity, w.period, bars)?,
            )
        }
        (SeriesChange::FirstPublish { .. }, Some(_)) => {
            return Err(Error::Conflict(
                "first publish on an existing series".into(),
            ));
        }
        (SeriesChange::Append { upserts }, Some(row)) => {
            if upserts.is_empty() {
                return Err(integrity("append requires upserts"));
            }
            if upserts.iter().any(|b| b.trade_day <= row.last_day) {
                return Err(integrity("append upserts are not tail-only"));
            }
            if upserts.windows(2).any(|p| p[0].trade_day >= p[1].trade_day) {
                return Err(integrity("commit bars are not strictly increasing"));
            }
            (
                "append",
                2,
                Some(2),
                Some(row.record_count * 32),
                Some(row.content_digest),
                series::fold_more(row.content_digest, upserts)?,
            )
        }
        (SeriesChange::Append { .. }, None) => {
            return Err(Error::NotFound);
        }
        (SeriesChange::Replace { bars }, Some(row)) => {
            check_bars(bars)?;
            (
                "replace",
                2,
                Some(3),
                None,
                Some(row.content_digest),
                series::fold_bars(w.entity, w.period, bars)?,
            )
        }
        (SeriesChange::Replace { .. }, None) => {
            return Err(Error::NotFound);
        }
        (SeriesChange::Metadata, Some(row)) => (
            "metadata",
            2,
            None,
            None,
            Some(row.content_digest),
            row.content_digest,
        ),
        (SeriesChange::Metadata, None) => return Err(Error::NotFound),
        (SeriesChange::NoChange, Some(row)) => (
            "no_change",
            1,
            Some(1),
            Some(row.record_count * 32),
            Some(row.content_digest),
            row.content_digest,
        ),
        (SeriesChange::NoChange, None) => return Err(Error::NotFound),
        // Yearly dispatches to commit_yearly_inner before this match.
        (SeriesChange::Yearly { .. }, _) => unreachable!("yearly dispatched earlier"),
    };
    let metadata_changed = w.metadata_changed
        || w.source_binding_digest.is_some()
        || w.evidence_digest.is_some()
        || w.coverage_status.is_some();
    // A5: `metadata` kind must actually change metadata; content-changing
    // kinds may carry metadata_changed=1 alongside.
    if matches!(w.change, SeriesChange::Metadata) && !metadata_changed {
        return Err(integrity("metadata commit carries no metadata change"));
    }

    // C3: a commit carrying new version evidence whose semantic fields
    // ({source, unit_currency, adjustment_evidence}) differ from the
    // head's is flagged on the event — yearly rows derived before it are
    // classified Invalid rather than merely Behind.
    let semantics_changed: i64 = if !matches!(
        w.change,
        SeriesChange::Append { .. } | SeriesChange::Metadata | SeriesChange::Replace { .. }
    ) {
        0
    } else {
        match (head.as_ref(), w.evidence_digest) {
            (Some(row), Some(digest)) if digest != row.evidence_digest => {
                (!same_version_semantics(c, &row.evidence_digest, &digest)?) as i64
            }
            _ => 0,
        }
    };

    let new_version_id = series::version_id(&new_digest);
    let task_revision = w
        .expected_task_revision
        .checked_add(1)
        .ok_or_else(|| integrity("task revision overflow"))?;
    let task_revision_i64 =
        i64::try_from(task_revision).map_err(|_| integrity("task revision out of range"))?;
    let result_digest = event_result_digest(
        w.result_id,
        w.task_id,
        w.item_id,
        kind,
        w.entity,
        w.period,
        &new_version_id,
        task_revision_i64,
    )?;

    // Evidence/binding referents must exist (the FKs enforce it; the kind
    // check keeps generation binding semantics).
    if let Some(digest) = w.source_binding_digest {
        evidence_store::verified_source_binding(c, &digest)?;
    }
    let (evidence_kind, _) = evidence_store::read_evidence(c, &w.event_evidence_digest)?;
    if !matches!(
        evidence_kind,
        EvidenceKind::Continuity | EvidenceKind::Version | EvidenceKind::Yearly
    ) {
        return Err(integrity("event evidence kind mismatch"));
    }

    let seq = series::next_event_seq(c)?;
    let now = Utc::now().to_rfc3339();
    let source_binding = w
        .source_binding_digest
        .or(head.as_ref().map(|h| h.source_binding_digest))
        .ok_or_else(|| integrity("commit lacks a source binding"))?;
    let evidence_digest = w
        .evidence_digest
        .or(head.as_ref().map(|h| h.evidence_digest))
        .ok_or_else(|| integrity("commit lacks version evidence"))?;
    let coverage_status = w
        .coverage_status
        .or(head.as_ref().map(|h| h.coverage_status))
        .unwrap_or(CoverageStatus::Unknown);

    series::insert_event(
        c,
        seq,
        &series::NewEvent {
            entity: w.entity.into(),
            period: w.period,
            kind,
            outcome,
            metadata_changed,
            continuity_mode: continuity,
            prefix_bytes,
            old_content_digest: old_digest,
            new_content_digest: new_digest,
            range_start_day: Some(w.range_start_day),
            range_end_day: Some(w.range_end_day),
            window_digest: w.window_digest,
            base_content_digest: None,
            algorithm_version: Some(w.algorithm_version.into()),
            source_binding_digest: Some(source_binding),
            evidence_digest: w.event_evidence_digest,
            task_id: w.task_id.into(),
            item_id: w.item_id.into(),
            task_revision: task_revision_i64,
            result_id: w.result_id.into(),
            result_digest,
            created_at: now.clone(),
            semantics_changed,
        },
    )?;

    // State-row mutation with the A2 CAS on last_event_seq.
    match &w.change {
        SeriesChange::FirstPublish { bars } => {
            let created = w
                .published_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| now.clone());
            let key = series::insert_series(
                c,
                &series::NewSeries {
                    entity: w.entity.into(),
                    period: w.period,
                    content_digest: new_digest,
                    record_count: bars.len() as i64,
                    first_day: bars[0].trade_day,
                    last_day: bars[bars.len() - 1].trade_day,
                    coverage_status,
                    source_binding_digest: source_binding,
                    evidence_digest,
                    last_event_seq: seq,
                    created_at: created.clone(),
                    updated_at: created,
                },
            )?;
            series::insert_bars(c, key, bars)?;
        }
        SeriesChange::Append { upserts } => {
            let row = head.as_ref().unwrap();
            let changed = c
                .execute(
                    "UPDATE market_series SET content_digest=?1,version_id=?2,record_count=record_count+?3,last_day=?4,coverage_status=?5,source_binding_digest=?6,evidence_digest=?7,updated_at=?8,last_event_seq=?9 WHERE series_key=?10 AND last_event_seq=?11",
                    params![
                        new_digest.as_slice(),
                        new_version_id,
                        upserts.len() as i64,
                        upserts[upserts.len() - 1].trade_day,
                        coverage_code(coverage_status),
                        source_binding.as_slice(),
                        evidence_digest.as_slice(),
                        now,
                        seq,
                        row.series_key,
                        row.last_event_seq,
                    ],
                )
                .map_err(storage)?;
            if changed != 1 {
                return Err(Error::Conflict("effective head changed".into()));
            }
            series::insert_bars(c, row.series_key, upserts)?;
        }
        SeriesChange::Replace { bars } => {
            let row = head.as_ref().unwrap();
            let changed = c
                .execute(
                    "UPDATE market_series SET content_digest=?1,version_id=?2,record_count=?3,first_day=?4,last_day=?5,coverage_status=?6,source_binding_digest=?7,evidence_digest=?8,updated_at=?9,last_event_seq=?10 WHERE series_key=?11 AND last_event_seq=?12",
                    params![
                        new_digest.as_slice(),
                        new_version_id,
                        bars.len() as i64,
                        bars[0].trade_day,
                        bars[bars.len() - 1].trade_day,
                        coverage_code(coverage_status),
                        source_binding.as_slice(),
                        evidence_digest.as_slice(),
                        now,
                        seq,
                        row.series_key,
                        row.last_event_seq,
                    ],
                )
                .map_err(storage)?;
            if changed != 1 {
                return Err(Error::Conflict("effective head changed".into()));
            }
            c.execute(
                "DELETE FROM series_bars WHERE series_key=?1",
                params![row.series_key],
            )
            .map_err(storage)?;
            series::insert_bars(c, row.series_key, bars)?;
        }
        SeriesChange::Metadata | SeriesChange::NoChange => {
            let row = head.as_ref().unwrap();
            // Metadata reflects new durable state -> bump updated_at;
            // NoChange only logs an event -> leave it alone.
            let touched =
                matches!(w.change, SeriesChange::Metadata).then(|| Utc::now().to_rfc3339());
            let changed = c
                .execute(
                    "UPDATE market_series SET coverage_status=?1,source_binding_digest=?2,evidence_digest=?3,last_event_seq=?4,updated_at=COALESCE(?5,updated_at) WHERE series_key=?6 AND last_event_seq=?7",
                    params![
                        coverage_code(coverage_status),
                        source_binding.as_slice(),
                        evidence_digest.as_slice(),
                        seq,
                        touched,
                        row.series_key,
                        row.last_event_seq,
                    ],
                )
                .map_err(storage)?;
            if changed != 1 {
                return Err(Error::Conflict("effective head changed".into()));
            }
        }
        // Yearly dispatches to commit_yearly_inner before this match.
        SeriesChange::Yearly { .. } => unreachable!("yearly dispatched earlier"),
    }

    finish_item_and_task(c, w, task_revision_i64)?;
    Ok(Receipt {
        publication_id: w.result_id.into(),
        version_id: new_version_id,
        task_id: w.task_id.into(),
        task_revision,
    })
}

/// Bars → `CompactBar` for the commit callers that carry `Bar`.
pub(crate) fn to_compact_bars(bars: &[Bar], period: Period) -> Result<Vec<CompactBar>, Error> {
    bars.iter()
        .map(|b| CompactBar::from_bar(b, period))
        .collect::<Result<Vec<_>, _>>()
        .map_err(integrity)
}

/// Fold over an observed window — the `window_digest` carried by
/// no_change/append event rows.
pub(crate) fn window_fold(bars: &[CompactBar]) -> Result<Digest, Error> {
    let mut state: Digest = sha2::Sha256::digest(b"x-data-series-window-v1\0").into();
    for bar in bars {
        state = series::fold_bar(state, bar);
    }
    Ok(state)
}

pub(crate) fn hex_digest(text: &str) -> Result<Digest, Error> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("digest hex must be 32 bytes"));
    }
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(invalid)?;
    }
    Ok(digest)
}

/// Version evidence the collection/typed paths carry verbatim.
pub(crate) fn put_generation_binding(
    c: &Connection,
    generation_id: &str,
    manifest_sha: &str,
    member_sha: &str,
    event_sha: &str,
    generation_evidence: &Value,
) -> Result<Digest, Error> {
    let evidence = evidence_store::put_evidence(c, EvidenceKind::Generation, generation_evidence)?;
    evidence_store::put_source_binding(
        c,
        &SourceBinding {
            generation_id: generation_id.into(),
            generation_manifest_sha256: hex_digest(manifest_sha)?,
            day_member_sha256: hex_digest(member_sha)?,
            security_event_sha256: hex_digest(event_sha)?,
            evidence_digest: evidence,
        },
    )
}

/// Event evidence for a continuity/audit record (typed commits carry the
/// probe's proof verbatim; envelope commits carry the audit projection).
pub(crate) fn put_continuity_evidence(c: &Connection, value: &Value) -> Result<Digest, Error> {
    evidence_store::put_evidence(c, EvidenceKind::Continuity, value)
}

/// C3 semantic comparison: the three fields yearly aggregation depends
/// on. Both digests must name version evidence — anything else is a
/// binding bug, not a semantic difference.
fn same_version_semantics(c: &Connection, a: &Digest, b: &Digest) -> Result<bool, Error> {
    let (kind_a, json_a) = evidence_store::read_evidence(c, a)?;
    let (kind_b, json_b) = evidence_store::read_evidence(c, b)?;
    if kind_a != EvidenceKind::Version || kind_b != EvidenceKind::Version {
        return Err(integrity("semantics comparison requires version evidence"));
    }
    let va: Value = serde_json::from_str(&json_a).map_err(integrity)?;
    let vb: Value = serde_json::from_str(&json_b).map_err(integrity)?;
    Ok(va["source"] == vb["source"]
        && va["unit_currency"] == vb["unit_currency"]
        && va["adjustment_evidence"] == vb["adjustment_evidence"])
}

fn year_of_day(trade_day: i64) -> Result<i32, Error> {
    Ok(compact_market::epoch_date(trade_day)
        .map_err(integrity)?
        .year())
}

/// The schema-21 yearly commit (S3 §5): runs inside `commit_series_tx`
/// after the shared gates. Re-checks the derivation's daily base against
/// the *current* daily head inside the transaction, merges the affected
/// years into the stored yearly rows, logs the event, advances lineage —
/// `no_change` still moves the lineage base so the row stays Fresh.
fn commit_yearly_inner(
    c: &Connection,
    w: &Commit<'_>,
    head: Option<&series::SeriesRow>,
    upserts: &[CompactBar],
    daily_base_digest: &Digest,
    algorithm_version: &str,
    as_of_day: i64,
) -> Result<Receipt, Error> {
    let daily = series::read_series(c, w.entity, Period::Day)?
        .ok_or_else(|| integrity("yearly commit without daily series"))?;
    if &daily.content_digest != daily_base_digest {
        return Err(Error::Stale(format!(
            "daily base changed since derivation: expected {}, found {}",
            series::version_id(daily_base_digest),
            daily.version_id
        )));
    }
    if upserts.is_empty() {
        return Err(integrity("yearly commit carries no bars"));
    }
    // Merge the candidate's affected-year bars over the stored rows.
    let mut merged = match head {
        Some(row) => series::all_bars(c, row.series_key)?,
        None => Vec::new(),
    };
    let affected: std::collections::BTreeSet<i32> = upserts
        .iter()
        .map(|b| year_of_day(b.trade_day))
        .collect::<Result<_, _>>()?;
    merged.retain(|b| {
        year_of_day(b.trade_day)
            .map(|y| !affected.contains(&y))
            .unwrap_or(false)
    });
    merged.extend_from_slice(upserts);
    merged.sort_by_key(|b| b.trade_day);
    check_bars(&merged)?;
    let new_digest = series::fold_bars(w.entity, w.period, &merged)?;

    let task_revision = w
        .expected_task_revision
        .checked_add(1)
        .ok_or_else(|| integrity("task revision overflow"))?;
    let task_revision_i64 =
        i64::try_from(task_revision).map_err(|_| integrity("task revision out of range"))?;
    let seq = series::next_event_seq(c)?;
    let now = Utc::now().to_rfc3339();
    let (kind, outcome, continuity, prefix_bytes) = match head {
        None => ("first_publish", 2, None, None),
        Some(row) if row.content_digest == new_digest => {
            ("no_change", 1, Some(1), Some(merged.len() as i64 * 32))
        }
        Some(_) => ("yearly_rederive", 2, Some(3), None),
    };
    let new_version_id = series::version_id(&new_digest);
    let result_digest = event_result_digest(
        w.result_id,
        w.task_id,
        w.item_id,
        kind,
        w.entity,
        w.period,
        &new_version_id,
        task_revision_i64,
    )?;
    let source_binding = daily.source_binding_digest;
    let evidence_digest = w
        .evidence_digest
        .or(head.as_ref().map(|h| h.evidence_digest))
        .ok_or_else(|| integrity("commit lacks version evidence"))?;
    let coverage_status = w
        .coverage_status
        .or(head.as_ref().map(|h| h.coverage_status))
        .unwrap_or(CoverageStatus::Unknown);
    let (evidence_kind, _) = evidence_store::read_evidence(c, &w.event_evidence_digest)?;
    if evidence_kind != EvidenceKind::Yearly {
        return Err(integrity("yearly event evidence kind mismatch"));
    }
    series::insert_event(
        c,
        seq,
        &series::NewEvent {
            entity: w.entity.into(),
            period: w.period,
            kind,
            outcome,
            metadata_changed: false,
            continuity_mode: continuity,
            prefix_bytes,
            old_content_digest: head.as_ref().map(|h| h.content_digest),
            new_content_digest: new_digest,
            range_start_day: Some(merged[0].trade_day),
            range_end_day: Some(merged[merged.len() - 1].trade_day),
            window_digest: None,
            base_content_digest: Some(*daily_base_digest),
            algorithm_version: Some(algorithm_version.into()),
            source_binding_digest: Some(source_binding),
            evidence_digest: w.event_evidence_digest,
            task_id: w.task_id.into(),
            item_id: w.item_id.into(),
            task_revision: task_revision_i64,
            result_id: w.result_id.into(),
            result_digest,
            created_at: now.clone(),
            semantics_changed: 0,
        },
    )?;

    // State-row mutation with the A2 CAS on last_event_seq.
    let series_key = match head {
        None => {
            let created = w
                .published_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| now.clone());
            let key = series::insert_series(
                c,
                &series::NewSeries {
                    entity: w.entity.into(),
                    period: w.period,
                    content_digest: new_digest,
                    record_count: merged.len() as i64,
                    first_day: merged[0].trade_day,
                    last_day: merged[merged.len() - 1].trade_day,
                    coverage_status,
                    source_binding_digest: source_binding,
                    evidence_digest,
                    last_event_seq: seq,
                    created_at: created.clone(),
                    updated_at: created,
                },
            )?;
            series::insert_bars(c, key, &merged)?;
            key
        }
        Some(row) => {
            let changed = if kind == "no_change" {
                c.execute(
                    "UPDATE market_series SET coverage_status=?1,source_binding_digest=?2,evidence_digest=?3,last_event_seq=?4 WHERE series_key=?5 AND last_event_seq=?6",
                    params![
                        coverage_code(coverage_status),
                        source_binding.as_slice(),
                        evidence_digest.as_slice(),
                        seq,
                        row.series_key,
                        row.last_event_seq,
                    ],
                )
                .map_err(storage)?
            } else {
                c.execute(
                    "UPDATE market_series SET content_digest=?1,version_id=?2,record_count=?3,first_day=?4,last_day=?5,coverage_status=?6,source_binding_digest=?7,evidence_digest=?8,updated_at=?9,last_event_seq=?10 WHERE series_key=?11 AND last_event_seq=?12",
                    params![
                        new_digest.as_slice(),
                        new_version_id,
                        merged.len() as i64,
                        merged[0].trade_day,
                        merged[merged.len() - 1].trade_day,
                        coverage_code(coverage_status),
                        source_binding.as_slice(),
                        evidence_digest.as_slice(),
                        now,
                        seq,
                        row.series_key,
                        row.last_event_seq,
                    ],
                )
                .map_err(storage)?
            };
            if changed != 1 {
                return Err(Error::Conflict("effective head changed".into()));
            }
            if kind == "yearly_rederive" {
                // Affected years: drop that year's stored rows, insert the
                // candidate's. Unaffected years are untouched (§8.1-8).
                for year in &affected {
                    let (first, last) = match (
                        chrono::NaiveDate::from_ymd_opt(*year, 1, 1),
                        chrono::NaiveDate::from_ymd_opt(*year, 12, 31),
                    ) {
                        (Some(first), Some(last)) => (
                            compact_market::epoch_day(first),
                            compact_market::epoch_day(last),
                        ),
                        _ => return Err(integrity("affected year out of range")),
                    };
                    c.execute(
                        "DELETE FROM series_bars WHERE series_key=?1 AND trade_day>=?2 AND trade_day<=?3",
                        params![row.series_key, first, last],
                    )
                    .map_err(storage)?;
                }
                series::insert_bars(c, row.series_key, upserts)?;
            }
            row.series_key
        }
    };
    // Lineage always advances to the commit-time daily base — otherwise a
    // no_change commit would leave the row permanently Behind.
    series::put_lineage(
        c,
        series_key,
        &series::LineageWrite {
            base_daily_digest: *daily_base_digest,
            base_event_seq: daily.last_event_seq,
            algorithm_version,
            first_trade_day: merged[0].trade_day,
            last_trade_day: merged[merged.len() - 1].trade_day,
            as_of_day,
        },
    )?;
    finish_item_and_task(c, w, task_revision_i64)?;
    Ok(Receipt {
        publication_id: w.result_id.into(),
        version_id: new_version_id,
        task_id: w.task_id.into(),
        task_revision,
    })
}

#[cfg(test)]
mod tests;
