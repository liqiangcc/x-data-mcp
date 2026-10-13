//! Schema-21 repository adapter (S2): the version-chain machinery is
//! retired. `SqliteCompactMarket` keeps one `WriteCoordinator`; every
//! repository method resolves against `market_series`/`series_bars`/
//! `market_events` through the `series`/`series_commit` modules. Legacy
//! `CheckedRecord` envelopes are synthesized in memory at read boundaries.
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::compact_runtime::{CheckpointMetrics, SchemaIdentity, WriteCoordinator};
use super::{evidence_store, integrity, series, series_commit, storage};
use crate::compact_market::{self, CompactBar, Digest, EvidenceKind, VersionEvidence};
use crate::local_runtime::DatabaseLease;
use crate::market_data::{Bar, CoverageEvidence, CoverageStatus, Period};
use crate::market_storage::compat::{self, CheckedRecord};
use crate::market_storage::{
    Error, MarketRepository, Publication, PublishPlan, Range, ReadPage, Receipt, Scope,
};
use crate::market_update::publication::{RangePlan, RangeRepository};
use crate::official_market::CollectionCandidate;
use crate::publish::EffectiveVersion;
use crate::sqlite_tasks::SqliteTasks;
use crate::task_control::{
    self, Acceptance, Change, Command, FrozenWork, RequestRecord, RetryGuard, TaskHead, TaskItem,
    TaskRepository, TaskSnapshot,
};

const BUSY: Duration = Duration::from_secs(5);

/// Latest-only series/event store (schema 21). Same handle shape the v19
/// backend had: one writer behind the coordinator, task store sharing the
/// database file.
#[derive(Clone)]
pub struct SqliteCompactMarket {
    coordinator: Arc<WriteCoordinator>,
    tasks: SqliteTasks,
}

/// Deterministic schema identity (recomputes the migrations digest without
/// opening the database).
pub(crate) fn schema_compact_identity() -> SchemaIdentity {
    const DOMAIN: &[u8] = b"x-data-schema-identity-v1\0";
    const VERSION: u32 = crate::sqlite_tasks::COMPACT_SCHEMA_VERSION;
    let mut payload = DOMAIN.to_vec();
    payload.extend_from_slice(&VERSION.to_be_bytes());
    payload.extend_from_slice(&VERSION.to_be_bytes());
    for (index, sql) in crate::sqlite_tasks::MIGRATIONS
        .iter()
        .chain(crate::sqlite_tasks::COMPACT_MIGRATIONS.iter())
        .enumerate()
    {
        payload.extend_from_slice(&(index as u32 + 1).to_be_bytes());
        let checksum = crate::hash::sha256(sql.as_bytes());
        payload.extend_from_slice(&(checksum.len() as u32).to_be_bytes());
        payload.extend_from_slice(checksum.as_bytes());
    }
    SchemaIdentity {
        user_version: VERSION,
        digest: Sha256::digest(&payload).into(),
    }
}

impl SqliteCompactMarket {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let lease = DatabaseLease::acquire(path)
            .map_err(|e| Error::Base(format!("database lease: {e}")))?;
        Self::open_shared(path, Arc::new(lease))
    }

    /// Opens on an already-held database lease — the service process owns
    /// the flock, so the backend must share it rather than acquire a
    /// second one.
    pub fn open_shared(path: &Path, lease: Arc<DatabaseLease>) -> Result<Self, Error> {
        let coordinator = Arc::new(WriteCoordinator::open_existing(
            path,
            lease,
            schema_compact_identity(),
            BUSY,
        )?);
        Ok(Self {
            tasks: SqliteTasks::new(path).with_coordinator(coordinator.clone()),
            coordinator,
        })
    }

    /// Task-store handle sharing the same database file (used by catalog
    /// executors that bind `SqliteCatalog` to the tasks database).
    pub(crate) fn tasks(&self) -> SqliteTasks {
        self.tasks.clone()
    }

    pub(crate) fn coordinator(&self) -> &Arc<WriteCoordinator> {
        &self.coordinator
    }

    pub(crate) fn checkpoint_idle(&self) -> Result<CheckpointMetrics, Error> {
        self.coordinator.checkpoint_idle()
    }

    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.coordinator.read(f)
    }
    pub(crate) fn write<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.coordinator.write(f)
    }
    pub(crate) fn tx<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.coordinator.transaction(f)
    }
}

pub(crate) fn period_code(period: Period) -> i64 {
    match period {
        Period::Day => 1,
        Period::Year => 2,
    }
}

pub(crate) fn hex_bytes(digest: &Digest) -> String {
    compact_market::digest_hex(digest)
}

fn invalid(message: impl std::fmt::Display) -> Error {
    Error::Invalid(message.to_string())
}

// ---------------------------------------------------------------------------
// Staged candidate scratch (staged_records — see 021 migration comment)
// ---------------------------------------------------------------------------

/// Staged-row TTL: a candidate exists only between `stage()` and its
/// item's commit — seconds apart in every pipeline. A row untouched for
/// longer belongs to an item run that crashed or was cancelled mid-flight
/// and will never be consumed; the stage-time sweep reclaims it.
const STAGED_TTL: chrono::Duration = chrono::Duration::hours(24);

/// Delete staged rows whose last stage attempt precedes `cutoff`. Runs
/// inside `stage()`'s transaction — the `staged_records_created_at` index
/// keeps it O(expired) — so orphan growth is bounded to candidates staged
/// within the TTL window.
pub(crate) fn gc_staged(c: &Connection, cutoff: chrono::DateTime<Utc>) -> Result<usize, Error> {
    c.execute(
        "DELETE FROM staged_records WHERE created_at < ?1",
        params![cutoff.to_rfc3339()],
    )
    .map_err(storage)
}

fn stage_tx(c: &Connection, raw: &Value) -> Result<(), Error> {
    gc_staged(c, Utc::now() - STAGED_TTL)?;
    let record = compat::check(raw)?;
    let period = record.version().period;
    let entity = record.version().symbol.clone();
    let evidence_digest = evidence_store::put_evidence(
        c,
        EvidenceKind::Version,
        &json!(VersionEvidence {
            source: record.version().source.clone(),
            coverage_reason: record.batch.coverage.reason.clone(),
            unit_currency: record.batch.unit_currency.clone(),
            adjustment_evidence: record.batch.adjustment_evidence.clone(),
        }),
    )?;
    match period {
        Period::Day => {
            if record.version().yearly_derivation.is_some() {
                return Err(invalid("daily record carries yearly derivation"));
            }
            let source: CompactSource = serde_json::from_value(
                raw.get("compact_source")
                    .cloned()
                    .ok_or_else(|| invalid("compact_source required for compact staging"))?,
            )
            .map_err(invalid)?;
            series_commit::put_generation_binding(
                c,
                &source.generation_id,
                &source.generation_manifest_sha256,
                &source.day_member_sha256,
                &source.security_event_sha256,
                &source.generation_evidence,
            )?;
        }
        Period::Year => {
            let derivation = record
                .version()
                .yearly_derivation
                .clone()
                .ok_or_else(|| invalid("yearly record lacks derivation"))?;
            let base = series::require_series(c, &derivation.base_daily_version_id)?;
            if base.period != Period::Day || base.entity != entity {
                return Err(invalid("yearly base is not a published daily version"));
            }
        }
    }
    let _ = evidence_digest;
    let version_id = record.version().version_id.clone();
    c.execute(
        "INSERT INTO staged_records(version_id,raw,created_at) VALUES(?1,?2,?3)
         ON CONFLICT(version_id) DO UPDATE SET raw=excluded.raw,created_at=excluded.created_at",
        params![version_id, raw.to_string(), Utc::now().to_rfc3339()],
    )
    .map_err(storage)?;
    Ok(())
}

fn read_staged(c: &Connection, version_id: &str) -> Result<CheckedRecord, Error> {
    let raw: Option<String> = c
        .query_row(
            "SELECT raw FROM staged_records WHERE version_id=?1",
            params![version_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    let raw = raw.ok_or(Error::NotFound)?;
    compat::check(&serde_json::from_str(&raw).map_err(integrity)?)
}

/// Same typed-facts equality `staged_matches`/`commit` bind on.
fn same_candidate(a: &CheckedRecord, b: &CheckedRecord) -> bool {
    let (v, bv) = (a.version(), &a.batch);
    let (rv, rb) = (b.version(), &b.batch);
    v.version_id == rv.version_id
        && v.symbol == rv.symbol
        && v.period == rv.period
        && v.adjustment == rv.adjustment
        && v.source == rv.source
        && v.records == rv.records
        && v.published_at == rv.published_at
        && v.validation == rv.validation
        && v.unit_currency == rv.unit_currency
        && v.adjustment_evidence == rv.adjustment_evidence
        && v.yearly_derivation == rv.yearly_derivation
        && bv.symbol == rb.symbol
        && bv.period == rb.period
        && bv.adjustment == rb.adjustment
        && bv.bars == rb.bars
        && bv.coverage == rb.coverage
        && bv.unit_currency == rb.unit_currency
        && bv.adjustment_evidence == rb.adjustment_evidence
        && bv.provenance.source == rb.provenance.source
        && bv.provenance.symbol == rb.provenance.symbol
        && bv.provenance.source_period == rb.provenance.source_period
        && bv.provenance.source_adjustment == rb.provenance.source_adjustment
        && bv.provenance.snapshot_inputs == rb.provenance.snapshot_inputs
        && bv.provenance.yearly_derivation == rb.provenance.yearly_derivation
}

#[derive(Debug, Deserialize)]
struct CompactSource {
    generation_id: String,
    generation_manifest_sha256: String,
    day_member_sha256: String,
    security_event_sha256: String,
    generation_evidence: Value,
}

impl MarketRepository for SqliteCompactMarket {
    fn stage(&self, raw: &Value) -> Result<(), Error> {
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            stage_tx(&tx, raw)?;
            tx.commit()
                .map_err(|error| Error::OutcomeUnknown(error.to_string()))
        })
    }
    fn candidate(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.read(|c| read_staged(c, id))
    }
    fn staged_matches(&self, version_id: &str, whole: &Value) -> Result<bool, Error> {
        let candidate = self.candidate(version_id)?;
        let record = compat::check(whole)?;
        Ok(same_candidate(&candidate, &record))
    }
    fn mark_validated(&self, record: &CheckedRecord) -> Result<(), Error> {
        // No staged-state flip on v21: the candidate must exist in
        // staged_records and match the sealed record — commit re-checks.
        self.read(|c| {
            let stored = read_staged(c, &record.version().version_id)?;
            if !same_candidate(&stored, record) {
                return Err(Error::Conflict("validated record differs".into()));
            }
            Ok(())
        })
    }
    fn fixed(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.read(|c| series::materialize_checked(c, id, true))
    }
    fn fixed_comparison(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.read(|c| series::materialize_checked(c, id, true))
    }
    fn range(&self, scope: &Scope, id: Option<&str>, range: &Range) -> Result<ReadPage, Error> {
        range.validate()?;
        self.read(|c| {
            let id = match id {
                Some(id) => id.to_owned(),
                None => head_version_id(c, &scope.symbol, scope.period)?.ok_or(Error::NotFound)?,
            };
            let series_row = series::require_series(c, &id)?;
            if series_row.entity != scope.symbol || series_row.period != scope.period {
                return Err(Error::NotFound);
            }
            let start = compact_market::epoch_day(range.start);
            let end = compact_market::epoch_day(range.end);
            let mut bars = series::bars_in_range(c, series_row.series_key, start, end)?
                .iter()
                .map(|b| b.to_bar(scope.period))
                .collect::<Result<Vec<_>, _>>()
                .map_err(integrity)?;
            if let Some(after) = range.after {
                let after_day = compact_market::epoch_day(after);
                bars.retain(|b| compact_market::epoch_day(b.date) > after_day);
            }
            let next_after = if bars.len() > range.limit {
                bars.truncate(range.limit);
                bars.last().map(|b| b.date)
            } else {
                None
            };
            Ok(ReadPage {
                integrity_sha256: series_row.version_id.clone(),
                version: series::materialize_checked(c, &id, false)?
                    .version()
                    .clone(),
                rows: bars,
                next_after,
            })
        })
    }
    fn versions(&self, scope: &Scope) -> Result<Vec<EffectiveVersion>, Error> {
        self.read(
            |c| match series::read_series(c, &scope.symbol, scope.period)? {
                Some(row) => {
                    let record = series::checked_record(c, &row, false)?;
                    Ok(vec![record.version().clone()])
                }
                None => Ok(vec![]),
            },
        )
    }
    fn fixed_many(&self, ids: &[String]) -> Result<Vec<CheckedRecord>, Error> {
        self.read(|c| {
            ids.iter()
                .map(|id| series::materialize_checked(c, id, true))
                .collect()
        })
    }
    fn fixed_facts(&self, id: &str) -> Result<crate::market_storage::FixedFacts, Error> {
        self.read(|c| {
            let row = series::require_series(c, id)?;
            Ok(crate::market_storage::FixedFacts {
                version_id: row.version_id.clone(),
                symbol: row.entity.clone(),
                period: row.period,
                integrity_sha256: row.version_id.clone(),
                first_day: compact_market::epoch_date(row.first_day).map_err(integrity)?,
                last_day: compact_market::epoch_date(row.last_day).map_err(integrity)?,
            })
        })
    }
    fn fixed_window(
        &self,
        id: &str,
        first_day: NaiveDate,
        last_day: NaiveDate,
    ) -> Result<CheckedRecord, Error> {
        self.read(|c| {
            let row = series::require_series(c, id)?;
            let mut record = series::checked_record(c, &row, false)?;
            let bars = series::bars_in_range(
                c,
                row.series_key,
                compact_market::epoch_day(first_day),
                compact_market::epoch_day(last_day),
            )?
            .iter()
            .map(|b| b.to_bar(row.period))
            .collect::<Result<Vec<_>, _>>()
            .map_err(integrity)?;
            record.version.records = bars.clone();
            record.batch.bars = bars;
            Ok(record)
        })
    }
    fn fixed_window_years(
        &self,
        id: &str,
        first_day: NaiveDate,
        last_day: NaiveDate,
    ) -> Result<BTreeSet<i32>, Error> {
        use chrono::Datelike;
        self.read(|c| {
            let row = series::require_series(c, id)?;
            Ok(series::days_in_range(
                c,
                row.series_key,
                compact_market::epoch_day(first_day),
                compact_market::epoch_day(last_day),
            )?
            .iter()
            .filter_map(|d| compact_market::epoch_date(*d).ok())
            .map(|d| d.year())
            .collect())
        })
    }
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error> {
        self.read(|c| {
            crate::sqlite_tasks::read_bound_task(c, id)
                .map_err(Error::Task)?
                .ok_or(Error::Task(task_control::Error::TaskNotFound))
        })
    }
    fn task_head(&self, id: &str) -> Result<TaskHead, Error> {
        self.read(|c| {
            crate::sqlite_tasks::read_task_head(c, id)?
                .ok_or(Error::Task(task_control::Error::TaskNotFound))
        })
    }
    fn task_item_binding(
        &self,
        task: &str,
        item: &str,
    ) -> Result<Option<crate::task_control::ItemBinding>, Error> {
        self.read(|c| crate::sqlite_tasks::read_item_binding(c, task, item).map_err(Error::Task))
    }
    fn transition_task_head(
        &self,
        head: &mut TaskHead,
        item: Option<&mut TaskItem>,
        run: &str,
        change: Change,
    ) -> Result<(), Error> {
        self.write(|c| {
            crate::sqlite_tasks::transition_head_tx(c, head, item, run, change, Utc::now())
                .map_err(Error::Task)
        })
    }
    fn commit(&self, plan: &PublishPlan) -> Result<Receipt, Error> {
        commit_publish_plan(self, plan)
    }
    fn receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| receipt_for(c, id))
    }
}

// ---------------------------------------------------------------------------
// Commit plumbing shared by every entry point
// ---------------------------------------------------------------------------

/// Receipt reconstruction from the event row (`result_id` is the
/// publication id; `new_content_digest` hex is the committed version id).
fn receipt_for(c: &Connection, result_id: &str) -> Result<Option<Receipt>, Error> {
    Ok(series::event_by_result(c, result_id)?.map(|e| Receipt {
        publication_id: result_id.into(),
        version_id: series::version_id(&e.new_content_digest),
        task_id: e.task_id.clone(),
        task_revision: e.task_revision as u64,
    }))
}

fn head_version_id(c: &Connection, entity: &str, period: Period) -> Result<Option<String>, Error> {
    Ok(series::read_series(c, entity, period)?.map(|s| s.version_id))
}

/// The continuity evidence stored on the event (v19 `ops_continuity_json`
/// shape verbatim).
fn ops_continuity_json(
    mode: &str,
    base_version_id: &str,
    source_binding_digest: &Digest,
    prefix_bytes: Option<u64>,
    evidence: Option<&Value>,
) -> Value {
    let mut continuity = json!({
        "mode": mode,
        "base_version_id": base_version_id,
        "source_binding_digest": hex_bytes(source_binding_digest),
    });
    if let Some(evidence) = evidence {
        if let Some(proof) = evidence.get("continuity") {
            continuity["proof"] = proof.clone();
        }
        if let Some(basis) = evidence.get("same_qfq_basis_ref") {
            continuity["basis_ref"] = basis.clone();
        }
        if let Some(revision) = evidence.get("revision_ref") {
            continuity["revision_ref"] = revision.clone();
        }
    }
    if let Some(prefix_bytes) = prefix_bytes {
        continuity["prefix_bytes"] = json!(prefix_bytes);
    }
    continuity
}

/// Resolve the metadata fields a typed append may carry (#320): the new
/// source binding + version evidence, checked against the stored base —
/// complete coverage only, no unit/adjustment semantic drift, matching
/// snapshot-input digests.
#[allow(clippy::too_many_arguments)]
/// New (binding, version-evidence, coverage) when a typed metadata carry
/// accompanies the commit.
type MetadataCarry = (Option<Digest>, Option<Digest>, Option<CoverageStatus>);

fn metadata_fields(
    c: &Connection,
    base: &series::SeriesRow,
    metadata: Option<&crate::market_update::TypedVersionMetadata>,
) -> Result<MetadataCarry, Error> {
    let Some(metadata) = metadata else {
        return Ok((None, None, None));
    };
    if metadata.coverage.status != CoverageStatus::Complete
        || base.coverage_status != CoverageStatus::Complete
        || metadata.coverage.reason.trim().is_empty()
    {
        return Err(integrity("typed metadata coverage is not complete"));
    }
    let base_binding = evidence_store::read_source_binding(c, &base.source_binding_digest)?;
    let (kind, canonical) = evidence_store::read_evidence(c, &base.evidence_digest)?;
    if kind != EvidenceKind::Version {
        return Err(integrity("base version evidence kind mismatch"));
    }
    let base_evidence: VersionEvidence = serde_json::from_str(&canonical).map_err(integrity)?;
    if metadata.source != base_evidence.source
        || metadata.adjustment_evidence != base_evidence.adjustment_evidence
    {
        return Err(integrity("typed metadata drifts source or adjustment"));
    }
    for (a, b) in [
        (
            &metadata.unit_currency.price_currency,
            &base_evidence.unit_currency.price_currency,
        ),
        (
            &metadata.unit_currency.price_unit,
            &base_evidence.unit_currency.price_unit,
        ),
        (
            &metadata.unit_currency.volume_unit,
            &base_evidence.unit_currency.volume_unit,
        ),
        (
            &metadata.unit_currency.amount_currency,
            &base_evidence.unit_currency.amount_currency,
        ),
        (
            &metadata.unit_currency.amount_unit,
            &base_evidence.unit_currency.amount_unit,
        ),
    ] {
        if a.is_confirmed() && b.is_confirmed() && a.value != b.value {
            return Err(integrity("typed metadata drifts units"));
        }
    }
    let binding_digest = series_commit::put_generation_binding(
        c,
        &metadata.generation_id,
        &metadata.generation_manifest_sha256,
        &metadata.day_member_sha256,
        &metadata.security_event_sha256,
        &metadata.generation_evidence,
    )?;
    let _ = &base_binding;
    let evidence_digest = evidence_store::put_evidence(
        c,
        EvidenceKind::Version,
        &json!(VersionEvidence {
            source: metadata.source.clone(),
            coverage_reason: metadata.coverage.reason.clone(),
            unit_currency: metadata.unit_currency.clone(),
            adjustment_evidence: metadata.adjustment_evidence.clone(),
        }),
    )?;
    Ok((
        Some(binding_digest),
        Some(evidence_digest),
        Some(metadata.coverage.status),
    ))
}

/// `commit_range_ops`/`commit_range_ops_typed` share one body: classify
/// the diff into append vs replace, then one `commit_series_tx`.
#[allow(clippy::too_many_arguments)]
fn commit_ops(
    c: &Connection,
    command: &Publication,
    scope: &Scope,
    base_version_id: &str,
    upserts: &[Bar],
    removes: &[NaiveDate],
    expected: Option<&[Bar]>,
    metadata: Option<&crate::market_update::TypedVersionMetadata>,
    continuity_evidence: Option<&Value>,
) -> Result<Receipt, Error> {
    // Replay wins over the head CAS: an already-committed result id must
    // replay its receipt even after the head moved on. Identity mismatch
    // (same result id, different task/item) stays a hard Conflict.
    if let Some(event) = series::event_by_result(c, &command.publication_id)? {
        if event.task_id != command.task_id || event.item_id != command.item_id {
            return Err(Error::Conflict("result identity conflict".into()));
        }
        series_commit::consume_staged(c, Some(command.version_id.as_str()))?;
        return Ok(Receipt {
            publication_id: command.publication_id.clone(),
            version_id: series::version_id(&event.new_content_digest),
            task_id: command.task_id.clone(),
            task_revision: event.task_revision as u64,
        });
    }
    let head = series::read_series(c, &scope.symbol, scope.period)?.ok_or(Error::NotFound)?;
    if head.version_id != base_version_id {
        return Err(Error::Stale(format!(
            "effective head changed: expected {base_version_id}, found {}",
            head.version_id
        )));
    }
    let (new_binding, new_evidence, new_coverage) = metadata_fields(c, &head, metadata)?;
    let compact_upserts = series_commit::to_compact_bars(upserts, scope.period)?;
    let tail_only = removes.is_empty()
        && !compact_upserts.is_empty()
        && compact_upserts.iter().all(|b| b.trade_day > head.last_day);
    let full_bars;
    let (change, mode, prefix_bytes, window_bars) = if tail_only {
        (
            series_commit::SeriesChange::Append {
                upserts: compact_upserts.as_slice(),
            },
            "append",
            Some(head.record_count as u64 * 32),
            compact_upserts.as_slice(),
        )
    } else {
        let Some(expected) = expected else {
            // Typed ops without a merged candidate must never rewrite
            // content (the runner only routes proven appends here).
            return Err(Error::Conflict("ops diff is not a tail append".into()));
        };
        full_bars = series_commit::to_compact_bars(expected, scope.period)?;
        (
            series_commit::SeriesChange::Replace {
                bars: full_bars.as_slice(),
            },
            "full",
            None,
            full_bars.as_slice(),
        )
    };
    let window = series_commit::window_fold(window_bars)?;
    let evidence = series_commit::put_continuity_evidence(
        c,
        &ops_continuity_json(
            mode,
            base_version_id,
            &head.source_binding_digest,
            prefix_bytes,
            continuity_evidence,
        ),
    )?;
    let metadata_changed = new_binding.is_some() || new_evidence.is_some();
    series_commit::commit_series_tx(
        c,
        &series_commit::Commit {
            result_id: &command.publication_id,
            task_id: &command.task_id,
            item_id: &command.item_id,
            run_id: &command.run_id,
            occupancy_key: &scope.occupancy_key(),
            expected_task_revision: command.expected_revision,
            entity: &scope.symbol,
            period: scope.period,
            base_version_id: Some(base_version_id),
            change,
            range_start_day: upserts
                .iter()
                .map(|b| compact_market::epoch_day(b.date))
                .chain(removes.iter().map(|d| compact_market::epoch_day(*d)))
                .min()
                .unwrap_or(head.last_day),
            range_end_day: upserts
                .iter()
                .map(|b| compact_market::epoch_day(b.date))
                .chain(removes.iter().map(|d| compact_market::epoch_day(*d)))
                .max()
                .unwrap_or(head.last_day),
            algorithm_version: series_commit::UPDATE_ALGO,
            window_digest: Some(window),
            source_binding_digest: new_binding,
            evidence_digest: new_evidence,
            coverage_status: new_coverage,
            metadata_changed,
            event_evidence_digest: evidence,
            published_at: None,
            staged_version_id: Some(command.version_id.as_str()),
        },
    )
}

/// `commit`/`commit_range` envelope classification: compare the sealed
/// record's bars against the stored series — prefix + longer ⇒ append,
/// equal ⇒ no_change/metadata, otherwise replace.
fn classify_envelope<'a>(
    c: &Connection,
    entity: &str,
    period: Period,
    head: Option<&'a series::SeriesRow>,
    new_bars: &'a [CompactBar],
) -> Result<series_commit::SeriesChange<'a>, Error> {
    match head {
        None => Ok(series_commit::SeriesChange::FirstPublish { bars: new_bars }),
        Some(row) => {
            if row.entity != entity || row.period != period {
                return Err(invalid("commit scope mismatch"));
            }
            let stored = series::all_bars(c, row.series_key)?;
            let new_digest = series::fold_bars(entity, period, new_bars)?;
            if new_digest == row.content_digest {
                return Ok(series_commit::SeriesChange::NoChange);
            }
            if stored.len() < new_bars.len() && stored[..] == new_bars[..stored.len()] {
                return Ok(series_commit::SeriesChange::Append {
                    upserts: &new_bars[stored.len()..],
                });
            }
            Ok(series_commit::SeriesChange::Replace { bars: new_bars })
        }
    }
}

/// Shared body of `MarketRepository::commit` and `RangeRepository::
/// commit_range`: bind the sealed plan to the staged candidate, classify,
/// commit one event.
fn commit_plan(
    c: &Connection,
    plan: &PublishPlan,
    event_evidence: Option<Digest>,
    no_change: bool,
    range: Option<crate::market_update::DateRange>,
) -> Result<Receipt, Error> {
    let command = &plan.command;
    let record = &plan.record;
    let v = record.version();
    let scope = Scope::of(v);
    if v.period == Period::Year {
        return Err(invalid("yearly commits go through commit_yearly"));
    }
    // Replay first: the result-id anchor outranks every commit-time gate —
    // a repeated command must return the original receipt, not conflict on
    // the already-advanced head. A result id bound to a different
    // task/item is an identity conflict, not a replay.
    if let Some(event) = series::event_by_result(c, &command.publication_id)? {
        if event.task_id != command.task_id || event.item_id != command.item_id {
            return Err(Error::Conflict("result identity conflict".into()));
        }
        series_commit::consume_staged(c, Some(command.version_id.as_str()))?;
        return receipt_for(c, &command.publication_id)?
            .ok_or_else(|| integrity("receipt event lacks result payload"));
    }
    // The sealed plan's record must equal the staged candidate.
    let staged = read_staged(c, &command.version_id)?;
    if !same_candidate(&staged, record) {
        return Err(integrity("validated candidate changed since plan"));
    }
    for (required_scope, expected) in &plan.required_heads {
        if *required_scope != scope {
            continue;
        }
        let head = head_version_id(c, &required_scope.symbol, required_scope.period)?;
        if head != *expected {
            return Err(Error::Conflict("effective head changed".into()));
        }
    }
    let new_bars: Vec<CompactBar> = v
        .records
        .iter()
        .map(|b| CompactBar::from_bar(b, v.period))
        .collect::<Result<Vec<_>, _>>()
        .map_err(integrity)?;
    let head = series::read_series(c, &scope.symbol, scope.period)?;
    // Version evidence + binding re-derived at commit time (put_* is
    // idempotent on identical canonical content).
    let evidence_digest = evidence_store::put_evidence(
        c,
        EvidenceKind::Version,
        &json!(VersionEvidence {
            source: v.source.clone(),
            coverage_reason: record.batch.coverage.reason.clone(),
            unit_currency: record.batch.unit_currency.clone(),
            adjustment_evidence: record.batch.adjustment_evidence.clone(),
        }),
    )?;
    // The binding always derives from the CANDIDATE's compact_source —
    // cross-generation commits (append or rewrite) must rebind to the
    // new generation, never silently inherit the head's.
    let source: CompactSource = serde_json::from_value(
        record
            .raw()
            .get("compact_source")
            .cloned()
            .ok_or_else(|| invalid("compact_source required"))?,
    )
    .map_err(invalid)?;
    let binding_digest = series_commit::put_generation_binding(
        c,
        &source.generation_id,
        &source.generation_manifest_sha256,
        &source.day_member_sha256,
        &source.security_event_sha256,
        &source.generation_evidence,
    )?;
    let metadata_changed = head.as_ref().is_some_and(|h| {
        h.source_binding_digest != binding_digest
            || h.evidence_digest != evidence_digest
            || h.coverage_status != record.batch.coverage.status
    });
    let (base_version_id, change, mode, prefix_bytes) = if no_change {
        (
            head.as_ref().map(|h| h.version_id.as_str()),
            series_commit::SeriesChange::NoChange,
            "unchanged",
            head.as_ref().map(|h| h.record_count as u64 * 32),
        )
    } else {
        let mut change =
            classify_envelope(c, &scope.symbol, scope.period, head.as_ref(), &new_bars)?;
        if matches!(change, series_commit::SeriesChange::NoChange) && metadata_changed {
            // Content identical, metadata moved — the A5 metadata kind.
            change = series_commit::SeriesChange::Metadata;
        }
        let mode = match &change {
            series_commit::SeriesChange::FirstPublish { .. } => "first",
            series_commit::SeriesChange::Append { .. } => "append",
            series_commit::SeriesChange::Replace { .. } => "full",
            series_commit::SeriesChange::Metadata | series_commit::SeriesChange::NoChange => {
                "unchanged"
            }
            series_commit::SeriesChange::Yearly { .. } => {
                unreachable!("yearly commits go through commit_yearly")
            }
        };
        let prefix = if matches!(change, series_commit::SeriesChange::Append { .. }) {
            head.as_ref().map(|h| h.record_count as u64 * 32)
        } else {
            None
        };
        (
            head.as_ref().map(|h| h.version_id.as_str()),
            change,
            mode,
            prefix,
        )
    };
    let (range_start, range_end) = match range {
        Some(r) => (
            compact_market::epoch_day(r.start),
            compact_market::epoch_day(r.end),
        ),
        None => (
            new_bars.first().map(|b| b.trade_day).unwrap_or(0),
            new_bars.last().map(|b| b.trade_day).unwrap_or(0),
        ),
    };
    let event_evidence = match event_evidence {
        Some(digest) => digest,
        None => series_commit::put_continuity_evidence(
            c,
            &ops_continuity_json(
                mode,
                head.as_ref().map(|h| h.version_id.as_str()).unwrap_or(""),
                &binding_digest,
                prefix_bytes,
                None,
            ),
        )?,
    };
    let receipt = series_commit::commit_series_tx(
        c,
        &series_commit::Commit {
            result_id: &command.publication_id,
            task_id: &command.task_id,
            item_id: &command.item_id,
            run_id: &command.run_id,
            occupancy_key: &scope.occupancy_key(),
            expected_task_revision: command.expected_revision,
            entity: &scope.symbol,
            period: scope.period,
            base_version_id,
            change,
            range_start_day: range_start,
            range_end_day: range_end,
            algorithm_version: series_commit::UPDATE_ALGO,
            window_digest: None,
            source_binding_digest: if no_change {
                None
            } else {
                Some(binding_digest)
            },
            evidence_digest: if no_change {
                None
            } else {
                Some(evidence_digest)
            },
            coverage_status: if no_change {
                None
            } else {
                Some(record.batch.coverage.status)
            },
            metadata_changed: !no_change && metadata_changed,
            event_evidence_digest: event_evidence,
            published_at: Some(v.published_at),
            staged_version_id: Some(command.version_id.as_str()),
        },
    )?;
    Ok(receipt)
}

/// Envelope-path no-change: the base record is the committed head — nothing
/// is staged for a no-change result.
fn commit_plan_no_change(
    c: &Connection,
    plan: &RangePlan,
    event_evidence: Digest,
    range: Option<crate::market_update::DateRange>,
) -> Result<Receipt, Error> {
    let command = &plan.publication.command;
    if let Some(receipt) = receipt_for(c, &command.publication_id)? {
        series_commit::consume_staged(c, Some(command.version_id.as_str()))?;
        return Ok(receipt);
    }
    let base_id = &plan.base.version().version_id;
    let scope = Scope::of(plan.base.version());
    let (range_start, range_end) = range
        .map(|r| {
            (
                compact_market::epoch_day(r.start),
                compact_market::epoch_day(r.end),
            )
        })
        .ok_or_else(|| invalid("no-change audit lacks a range"))?;
    let head = series::read_series(c, &scope.symbol, scope.period)?.ok_or(Error::NotFound)?;
    let window_bars = series::bars_in_range(c, head.series_key, range_start, range_end)?;
    series_commit::commit_series_tx(
        c,
        &series_commit::Commit {
            result_id: &command.publication_id,
            task_id: &command.task_id,
            item_id: &command.item_id,
            run_id: &command.run_id,
            occupancy_key: &scope.occupancy_key(),
            expected_task_revision: command.expected_revision,
            entity: &scope.symbol,
            period: scope.period,
            base_version_id: Some(base_id),
            change: series_commit::SeriesChange::NoChange,
            range_start_day: range_start,
            range_end_day: range_end,
            algorithm_version: series_commit::UPDATE_ALGO,
            window_digest: Some(series_commit::window_fold(&window_bars)?),
            source_binding_digest: None,
            evidence_digest: None,
            coverage_status: None,
            metadata_changed: false,
            event_evidence_digest: event_evidence,
            published_at: None,
            // The envelope path stages the candidate before classification
            // — a no_change commit must consume that row too.
            staged_version_id: Some(command.version_id.as_str()),
        },
    )
}

fn commit_publish_plan(repo: &SqliteCompactMarket, plan: &PublishPlan) -> Result<Receipt, Error> {
    repo.write(|c| {
        let tx = c
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        let receipt = commit_plan(&tx, plan, None, false, None)?;
        tx.commit()
            .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
        Ok(receipt)
    })
}

// ---------------------------------------------------------------------------
// RangeRepository
// ---------------------------------------------------------------------------

impl RangeRepository for SqliteCompactMarket {
    fn range_item_states(
        &self,
        task_id: &str,
        item_ids: &[String],
    ) -> Result<Vec<Result<crate::market_update::publication::RangeItemState, Error>>, Error> {
        self.read(|c| {
            Ok(item_ids
                .iter()
                .map(|item_id| {
                    Ok(crate::market_update::publication::RangeItemState {
                        binding: crate::sqlite_tasks::read_item_binding(c, task_id, item_id)
                            .map_err(Error::Task)?,
                        has_receipt: series::event_by_task_item(c, task_id, item_id)?.is_some(),
                    })
                })
                .collect())
        })
    }
    fn range_base_reads(
        &self,
        wants: &[crate::market_update::publication::BaseWant],
    ) -> Result<Vec<Result<Option<crate::market_update::publication::BaseRead>, Error>>, Error>
    {
        self.read(|c| {
            Ok(wants
                .iter()
                .map(|want| {
                    let Some(facts) = update_base_facts_on(c, &want.base_version_id)? else {
                        return Ok(None);
                    };
                    let Some(bars) =
                        base_bars_in_range_on(c, &want.base_version_id, want.start, want.end)?
                    else {
                        return Ok(None);
                    };
                    Ok(Some(crate::market_update::publication::BaseRead {
                        facts,
                        bars,
                    }))
                })
                .collect())
        })
    }
    fn commit_range(&self, plan: &RangePlan) -> Result<Receipt, Error> {
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            // The audit object IS the event's continuity evidence (Q12).
            let evidence = series_commit::put_continuity_evidence(&tx, &plan.audit)?;
            let range = plan.audit.get("range").and_then(|r| {
                Some(crate::market_update::DateRange {
                    start: r.get("start")?.as_str()?.parse().ok()?,
                    end: r.get("end")?.as_str()?.parse().ok()?,
                })
            });
            let receipt = if plan.no_change {
                commit_plan_no_change(&tx, plan, evidence, range)?
            } else {
                commit_plan(&tx, &plan.publication, Some(evidence), false, range)?
            };
            tx.commit()
                .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
            Ok(receipt)
        })
    }
    fn commit_range_ops(
        &self,
        command: &Publication,
        scope: &Scope,
        base_version_id: &str,
        upserts: &[Bar],
        removes: &[NaiveDate],
        expected: &[Bar],
    ) -> Result<Option<Receipt>, Error> {
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let receipt = commit_ops(
                &tx,
                command,
                scope,
                base_version_id,
                upserts,
                removes,
                Some(expected),
                None,
                None,
            )?;
            tx.commit()
                .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
            Ok(Some(receipt))
        })
    }
    fn update_base_facts(
        &self,
        base_version_id: &str,
    ) -> Result<Option<crate::market_update::UpdateBaseFacts>, Error> {
        self.read(|c| update_base_facts_on(c, base_version_id))
    }
    fn base_bars_in_range(
        &self,
        base_version_id: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Option<Vec<Bar>>, Error> {
        self.read(|c| base_bars_in_range_on(c, base_version_id, start, end))
    }
    fn commit_range_ops_typed(
        &self,
        command: &Publication,
        scope: &Scope,
        base_version_id: &str,
        upserts: &[Bar],
        removes: &[NaiveDate],
        metadata: Option<&crate::market_update::TypedVersionMetadata>,
        continuity_evidence: Option<&Value>,
    ) -> Result<Option<Receipt>, Error> {
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let receipt = commit_ops(
                &tx,
                command,
                scope,
                base_version_id,
                upserts,
                removes,
                None,
                metadata,
                continuity_evidence,
            )?;
            tx.commit()
                .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
            Ok(Some(receipt))
        })
    }
    fn commit_range_no_change_typed(
        &self,
        command: &Publication,
        base_version_id: &str,
        range: &crate::market_update::DateRange,
        continuity_evidence: Option<&Value>,
    ) -> Result<Option<Receipt>, Error> {
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let head =
                series::read_series_by_version_id(&tx, base_version_id)?.ok_or(Error::NotFound)?;
            let scope = Scope {
                symbol: head.entity.clone(),
                period: head.period,
            };
            let window_bars = series::bars_in_range(
                &tx,
                head.series_key,
                compact_market::epoch_day(range.start),
                compact_market::epoch_day(range.end),
            )?;
            let evidence = series_commit::put_continuity_evidence(
                &tx,
                &ops_continuity_json(
                    "unchanged",
                    base_version_id,
                    &head.source_binding_digest,
                    Some(head.record_count as u64 * 32),
                    continuity_evidence,
                ),
            )?;
            let receipt = series_commit::commit_series_tx(
                &tx,
                &series_commit::Commit {
                    result_id: &command.publication_id,
                    task_id: &command.task_id,
                    item_id: &command.item_id,
                    run_id: &command.run_id,
                    occupancy_key: &scope.occupancy_key(),
                    expected_task_revision: command.expected_revision,
                    entity: &scope.symbol,
                    period: scope.period,
                    base_version_id: Some(base_version_id),
                    change: series_commit::SeriesChange::NoChange,
                    range_start_day: compact_market::epoch_day(range.start),
                    range_end_day: compact_market::epoch_day(range.end),
                    algorithm_version: series_commit::UPDATE_ALGO,
                    window_digest: Some(series_commit::window_fold(&window_bars)?),
                    source_binding_digest: None,
                    evidence_digest: None,
                    coverage_status: None,
                    metadata_changed: false,
                    event_evidence_digest: evidence,
                    published_at: None,
                    staged_version_id: None,
                },
            )?;
            tx.commit()
                .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
            Ok(Some(receipt))
        })
    }
    fn commit_update_batch(
        &self,
        task_id: &str,
        items: &[crate::market_update::UpdateBatchItem],
    ) -> Result<Option<Vec<Result<Receipt, Error>>>, Error> {
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let mut snapshot = crate::sqlite_tasks::read_bound_task(&tx, task_id)
                .map_err(Error::Task)?
                .ok_or(Error::Task(task_control::Error::TaskNotFound))?;
            if snapshot.task.status == crate::task_control::Status::Pending {
                let run_id = items
                    .first()
                    .map(|item| item.run_id.clone())
                    .ok_or_else(|| invalid("update batch carries no items"))?;
                crate::sqlite_tasks::transition_light_tx(
                    &tx,
                    &mut snapshot,
                    &run_id,
                    Change::Claim,
                )
                .map_err(Error::Task)?;
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(update_batch_item(&tx, &mut snapshot, item)?);
            }
            tx.commit().map_err(storage)?;
            Ok(Some(out))
        })
    }
    fn range_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| receipt_for(c, id))
    }
    fn item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| {
            Ok(series::event_by_task_item(c, task, item)?.map(|e| Receipt {
                publication_id: e.result_id.clone(),
                version_id: series::version_id(&e.new_content_digest),
                task_id: task.into(),
                task_revision: e.task_revision as u64,
            }))
        })
    }
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error> {
        self.tasks
            .transition(task, revision, run, change)
            .map_err(Error::Task)
    }
    fn cover_gap(&self, covering: &crate::market_storage::GapCovering) -> Result<bool, Error> {
        covering.validate()?;
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let gap: Option<(String, i64)> = tx
                .query_row(
                    "SELECT symbol,period FROM proven_gaps WHERE gap_id=?1",
                    [&covering.gap_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(storage)?;
            let Some((gap_symbol, gap_period)) = gap else {
                return Err(Error::Invalid("covering references unknown gap".into()));
            };
            // A covering binds a published version only — on v21 that is a
            // `market_series` row (the single current series per scope).
            let scope: Option<(String, i64)> = tx
                .query_row(
                    "SELECT entity,period FROM market_series WHERE version_id=?1",
                    [&covering.version_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(storage)?;
            let Some((entity, code)) = scope else {
                return Err(Error::Invalid(
                    "covering references unknown or unpublished version".into(),
                ));
            };
            if entity != gap_symbol || code != gap_period {
                return Err(Error::Invalid("covering version scope mismatch".into()));
            }
            let existing: Option<String> = tx
                .query_row(
                    "SELECT covered_at FROM gap_coverings WHERE gap_id=?1 AND version_id=?2",
                    params![covering.gap_id, covering.version_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            let out = match existing {
                Some(at) if at == covering.covered_at.to_rfc3339() => Ok(false),
                Some(_) => Err(integrity("gap covering identity conflict")),
                None => tx
                    .execute(
                        "INSERT INTO gap_coverings VALUES(?1,?2,?3)",
                        params![
                            covering.gap_id,
                            covering.version_id,
                            covering.covered_at.to_rfc3339()
                        ],
                    )
                    .map_err(storage)
                    .map(|_| true),
            }?;
            tx.commit()
                .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
            Ok(out)
        })
    }
    fn gap_coverings(
        &self,
        gap_id: &str,
    ) -> Result<Vec<crate::market_storage::GapCovering>, Error> {
        self.read(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT gap_id,version_id,covered_at FROM gap_coverings WHERE gap_id=?1 ORDER BY covered_at,version_id",
                )
                .map_err(storage)?;
            let rows = stmt
                .query_map([gap_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(storage)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage)?;
            let mut out = Vec::with_capacity(rows.len());
            for (gap_id, version_id, covered_at) in rows {
                out.push(crate::market_storage::GapCovering {
                    gap_id,
                    version_id,
                    covered_at: covered_at.parse().map_err(|_| {
                        Error::Integrity("gap_coverings covered_at is not a timestamp".into())
                    })?,
                });
            }
            Ok(out)
        })
    }
}

fn update_base_facts_on(
    c: &Connection,
    base_version_id: &str,
) -> Result<Option<crate::market_update::UpdateBaseFacts>, Error> {
    let Some(row) = series::read_series_by_version_id(c, base_version_id)? else {
        return Ok(None);
    };
    let binding = evidence_store::read_source_binding(c, &row.source_binding_digest)?;
    let (kind, canonical) = evidence_store::read_evidence(c, &row.evidence_digest)?;
    if kind != EvidenceKind::Version {
        return Err(integrity("version evidence kind mismatch"));
    }
    let evidence: VersionEvidence = serde_json::from_str(&canonical).map_err(integrity)?;
    Ok(Some(crate::market_update::UpdateBaseFacts {
        version_id: row.version_id.clone(),
        symbol: row.entity.clone(),
        record_count: row.record_count as u32,
        first_day: compact_market::epoch_date(row.first_day).map_err(integrity)?,
        last_day: compact_market::epoch_date(row.last_day).map_err(integrity)?,
        integrity_sha256: row.version_id.clone(),
        coverage: CoverageEvidence {
            status: row.coverage_status,
            reason: evidence.coverage_reason,
        },
        unit_currency: evidence.unit_currency,
        source: evidence.source,
        generation_id: binding.generation_id,
        generation_manifest_sha256: hex_bytes(&binding.generation_manifest_sha256),
        day_member_sha256: hex_bytes(&binding.day_member_sha256),
        security_event_sha256: hex_bytes(&binding.security_event_sha256),
    }))
}

fn base_bars_in_range_on(
    c: &Connection,
    base_version_id: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Option<Vec<Bar>>, Error> {
    let Some(row) = series::read_series_by_version_id(c, base_version_id)? else {
        return Ok(None);
    };
    Ok(Some(
        series::bars_in_range(
            c,
            row.series_key,
            compact_market::epoch_day(start),
            compact_market::epoch_day(end),
        )?
        .iter()
        .map(|b| b.to_bar(row.period))
        .collect::<Result<Vec<_>, _>>()
        .map_err(integrity)?,
    ))
}

/// Per-item batch step for `commit_update_batch`: replay → start the item
/// → commit inside a savepoint → map failures to item outcomes.
/// Structural errors abort the whole batch transaction.
fn update_batch_item(
    c: &Connection,
    snapshot: &mut TaskSnapshot,
    item: &crate::market_update::UpdateBatchItem,
) -> Result<Result<Receipt, Error>, Error> {
    // The typed batch never stages; the id still names the scratch row an
    // earlier failed envelope attempt of this same item may have left.
    let staged_version_id = match &item.prepared {
        crate::market_update::UpdateBatchPrepared::Append {
            result_version_id, ..
        } => Some(result_version_id.as_str()),
        crate::market_update::UpdateBatchPrepared::NoChange { .. } => None,
    };
    if let Some(event) = series::event_by_result(c, &item.publication_id)? {
        if event.task_id != item.task_id || event.item_id != item.item_id {
            return Err(Error::Conflict(
                "update result id replays a different commit".into(),
            ));
        }
        series_commit::consume_staged(c, staged_version_id)?;
        return Ok(Ok(Receipt {
            publication_id: item.publication_id.clone(),
            version_id: series::version_id(&event.new_content_digest),
            task_id: item.task_id.clone(),
            task_revision: event.task_revision as u64,
        }));
    }
    crate::sqlite_tasks::transition_light_tx(
        c,
        snapshot,
        &item.run_id,
        Change::StartItem(item.item_id.clone()),
    )
    .map_err(Error::Task)?;
    let revision: i64 = c
        .query_row(
            "SELECT state_revision FROM tasks WHERE task_id=?1",
            [&item.task_id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    let expected_revision =
        u64::try_from(revision).map_err(|_| integrity("task revision out of range"))?;
    let committed = {
        c.execute_batch("SAVEPOINT update_batch_item")
            .map_err(storage)?;
        // The arms run inside a closure so a `?` early-return becomes the
        // item's Err outcome (savepoint rollback + FinishItem) instead of
        // aborting the whole batch.
        let outcome: Result<Receipt, Error> = (|| match &item.prepared {
            crate::market_update::UpdateBatchPrepared::NoChange {
                base_version_id,
                range_start_day,
                range_end_day,
                continuity,
            } => {
                let head = series::read_series_by_version_id(c, base_version_id)?
                    .ok_or(Error::NotFound)?;
                let window_bars =
                    series::bars_in_range(c, head.series_key, *range_start_day, *range_end_day)?;
                let evidence = series_commit::put_continuity_evidence(
                    c,
                    &ops_continuity_json(
                        "unchanged",
                        base_version_id,
                        &head.source_binding_digest,
                        Some(head.record_count as u64 * 32),
                        Some(continuity),
                    ),
                )?;
                series_commit::commit_series_tx(
                    c,
                    &series_commit::Commit {
                        result_id: &item.publication_id,
                        task_id: &item.task_id,
                        item_id: &item.item_id,
                        run_id: &item.run_id,
                        occupancy_key: &item.occupancy_key,
                        expected_task_revision: expected_revision,
                        entity: &item.symbol,
                        period: Period::Day,
                        base_version_id: Some(base_version_id),
                        change: series_commit::SeriesChange::NoChange,
                        range_start_day: *range_start_day,
                        range_end_day: *range_end_day,
                        algorithm_version: series_commit::UPDATE_ALGO,
                        window_digest: Some(series_commit::window_fold(&window_bars)?),
                        source_binding_digest: None,
                        evidence_digest: None,
                        coverage_status: None,
                        metadata_changed: false,
                        event_evidence_digest: evidence,
                        published_at: None,
                        staged_version_id: None,
                    },
                )
            }
            crate::market_update::UpdateBatchPrepared::Append {
                base_version_id,
                result_version_id: _,
                upserts,
                metadata,
                continuity,
            } => {
                let head = series::read_series_by_version_id(c, base_version_id)?
                    .ok_or(Error::NotFound)?;
                let compact_upserts = series_commit::to_compact_bars(upserts, Period::Day)?;
                let (binding, evidence_d, coverage) =
                    metadata_fields(c, &head, metadata.as_deref())?;
                let evidence = series_commit::put_continuity_evidence(
                    c,
                    &ops_continuity_json(
                        "append",
                        base_version_id,
                        &head.source_binding_digest,
                        Some(head.record_count as u64 * 32),
                        Some(continuity),
                    ),
                )?;
                series_commit::commit_series_tx(
                    c,
                    &series_commit::Commit {
                        result_id: &item.publication_id,
                        task_id: &item.task_id,
                        item_id: &item.item_id,
                        run_id: &item.run_id,
                        occupancy_key: &item.occupancy_key,
                        expected_task_revision: expected_revision,
                        entity: &item.symbol,
                        period: Period::Day,
                        base_version_id: Some(base_version_id),
                        change: series_commit::SeriesChange::Append {
                            upserts: compact_upserts.as_slice(),
                        },
                        range_start_day: upserts
                            .iter()
                            .map(|b| compact_market::epoch_day(b.date))
                            .min()
                            .unwrap_or(head.last_day),
                        range_end_day: upserts
                            .iter()
                            .map(|b| compact_market::epoch_day(b.date))
                            .max()
                            .unwrap_or(head.last_day),
                        algorithm_version: series_commit::UPDATE_ALGO,
                        window_digest: Some(series_commit::window_fold(&compact_upserts)?),
                        source_binding_digest: binding,
                        evidence_digest: evidence_d,
                        coverage_status: coverage,
                        metadata_changed: binding.is_some() || evidence_d.is_some(),
                        event_evidence_digest: evidence,
                        published_at: None,
                        staged_version_id,
                    },
                )
            }
        })();
        match outcome {
            Ok(receipt) => {
                c.execute_batch("RELEASE update_batch_item")
                    .map_err(storage)?;
                Ok(receipt)
            }
            Err(error) => {
                c.execute_batch("ROLLBACK TO update_batch_item; RELEASE update_batch_item")
                    .map_err(storage)?;
                Err(error)
            }
        }
    };
    match committed {
        Ok(receipt) => Ok(Ok(receipt)),
        Err(error) => {
            let change = if matches!(
                error,
                Error::OutcomeUnknown(_)
                    | Error::Storage(_)
                    | Error::Task(
                        task_control::Error::StorageUnavailable(_)
                            | task_control::Error::OutcomeUnknown(_)
                    )
            ) {
                Change::RequireVerification {
                    item_id: item.item_id.clone(),
                    reason: "item execution result not confirmed inside update batch; committed receipt not yet established".into(),
                }
            } else {
                Change::FinishItem(
                    item.item_id.clone(),
                    task_control::ItemOutcome {
                        success: false,
                        error: Some(error.to_string()),
                        evidence: None,
                    },
                )
            };
            crate::sqlite_tasks::transition_light_tx(c, snapshot, &item.run_id, change)
                .map_err(Error::Task)?;
            Ok(Err(error))
        }
    }
}

// ---------------------------------------------------------------------------
// TaskRepository — task-table plumbing delegates unchanged to SqliteTasks
// ---------------------------------------------------------------------------

impl TaskRepository for SqliteCompactMarket {
    fn read_request(&self, id: &str) -> Result<Option<RequestRecord>, task_control::Error> {
        self.tasks.read_request(id)
    }
    fn read_task(&self, id: &str) -> Result<Option<TaskSnapshot>, task_control::Error> {
        self.tasks.read_task(id)
    }
    fn accept(
        &self,
        id: &str,
        command: &Command,
        work: &FrozenWork,
        run: &str,
        retry: Option<&RetryGuard>,
    ) -> Result<Acceptance, task_control::Error> {
        self.tasks.accept(id, command, work, run, retry)
    }
    fn transition(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, task_control::Error> {
        self.tasks.transition(task, revision, run, change)
    }
    fn transition_light(
        &self,
        s: &mut TaskSnapshot,
        run: &str,
        change: Change,
    ) -> Result<(), task_control::Error> {
        self.tasks.transition_light(s, run, change)
    }
    fn candidates(&self, run: &str) -> Result<Vec<TaskSnapshot>, task_control::Error> {
        self.tasks.candidates(run)
    }
    fn reconcile(&self, run: &str) -> Result<usize, task_control::Error> {
        self.tasks.reconcile(run)
    }
}

// ---------------------------------------------------------------------------
// DailyCollectionRepository — collection first-publish commits
// ---------------------------------------------------------------------------

impl crate::official_market::DailyCollectionRepository for SqliteCompactMarket {
    fn collection_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        <Self as RangeRepository>::item_receipt(self, task, item)
    }
    fn effective_head(&self, scope: &Scope) -> Result<Option<String>, Error> {
        self.read(|c| head_version_id(c, &scope.symbol, scope.period))
    }
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error> {
        self.tasks
            .transition(task, revision, run, change)
            .map_err(Error::Task)
    }
    fn commit_collection_batch(
        &self,
        task_id: &str,
        items: &[CollectionCandidate],
    ) -> Result<Option<Vec<Result<Receipt, Error>>>, Error> {
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let mut snapshot = crate::sqlite_tasks::read_bound_task(&tx, task_id)
                .map_err(Error::Task)?
                .ok_or(Error::Task(task_control::Error::TaskNotFound))?;
            if snapshot.task.status == task_control::Status::Pending {
                let run_id = items
                    .first()
                    .map(|item| item.run_id.clone())
                    .ok_or_else(|| invalid("collection batch carries no items"))?;
                crate::sqlite_tasks::transition_light_tx(
                    &tx,
                    &mut snapshot,
                    &run_id,
                    Change::Claim,
                )
                .map_err(Error::Task)?;
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(collection_batch_item(&tx, &mut snapshot, item)?);
            }
            tx.commit().map_err(storage)?;
            Ok(Some(out))
        })
    }
}

fn collection_batch_item(
    c: &Connection,
    snapshot: &mut TaskSnapshot,
    item: &CollectionCandidate,
) -> Result<Result<Receipt, Error>, Error> {
    if let Some(event) = series::event_by_result(c, &item.publication_id)? {
        if event.task_id != item.task_id || event.item_id != item.item_id {
            return Err(Error::Conflict(
                "collection result id replays a different commit".into(),
            ));
        }
        series_commit::consume_staged(c, Some(item.version_id.as_str()))?;
        return Ok(Ok(Receipt {
            publication_id: item.publication_id.clone(),
            version_id: series::version_id(&event.new_content_digest),
            task_id: item.task_id.clone(),
            task_revision: event.task_revision as u64,
        }));
    }
    crate::sqlite_tasks::transition_light_tx(
        c,
        snapshot,
        &item.run_id,
        Change::StartItem(item.item_id.clone()),
    )
    .map_err(Error::Task)?;
    let committed = {
        c.execute_batch("SAVEPOINT collection_batch_item")
            .map_err(storage)?;
        let outcome = collection_item_commit(c, item);
        match outcome {
            Ok(receipt) => {
                c.execute_batch("RELEASE collection_batch_item")
                    .map_err(storage)?;
                Ok(receipt)
            }
            Err(error) => {
                c.execute_batch("ROLLBACK TO collection_batch_item; RELEASE collection_batch_item")
                    .map_err(storage)?;
                Err(error)
            }
        }
    };
    match committed {
        Ok(receipt) => Ok(Ok(receipt)),
        Err(error) => {
            crate::sqlite_tasks::transition_light_tx(
                c,
                snapshot,
                &item.run_id,
                Change::FinishItem(
                    item.item_id.clone(),
                    task_control::ItemOutcome {
                        success: false,
                        error: Some(error.to_string()),
                        evidence: None,
                    },
                ),
            )
            .map_err(Error::Task)?;
            Ok(Err(error))
        }
    }
}

fn collection_item_commit(c: &Connection, item: &CollectionCandidate) -> Result<Receipt, Error> {
    compact_market::canonical_entity(&item.symbol).map_err(integrity)?;
    let bars: Vec<CompactBar> = item
        .bars
        .iter()
        .map(|b| CompactBar::from_bar(b, Period::Day))
        .collect::<Result<Vec<_>, _>>()
        .map_err(integrity)?;
    if bars.is_empty() {
        return Err(integrity("collection candidate carries no bars"));
    }
    if bars.windows(2).any(|p| p[0].trade_day >= p[1].trade_day) {
        return Err(integrity("candidate bars are not strictly increasing"));
    }
    // Unit-evidence gate (v19 collection_candidate_check parity, F1):
    // confirmed price evidence always; volume/amount units required only
    // when the candidate actually carries those fields.
    let unit = &item.unit_currency;
    if !unit.price_currency.is_confirmed() || !unit.price_unit.is_confirmed() {
        return Err(integrity("price unit/currency evidence is not confirmed"));
    }
    if bars.iter().any(|b| b.volume.is_some()) && !unit.volume_unit.is_confirmed() {
        return Err(integrity("volume unit evidence missing for volume bars"));
    }
    if bars.iter().any(|b| b.amount.is_some())
        && !(unit.amount_currency.is_confirmed() && unit.amount_unit.is_confirmed())
    {
        return Err(integrity("amount unit evidence missing for amount bars"));
    }
    let binding = series_commit::put_generation_binding(
        c,
        &item.generation_id,
        &item.generation_manifest_sha256,
        &item.day_member_sha256,
        &item.security_event_sha256,
        &item.generation_evidence,
    )?;
    let evidence = evidence_store::put_evidence(
        c,
        EvidenceKind::Version,
        &json!(VersionEvidence {
            source: item.source.clone(),
            coverage_reason: item.coverage.reason.clone(),
            unit_currency: item.unit_currency.clone(),
            adjustment_evidence: item.adjustment_evidence.clone(),
        }),
    )?;
    let revision: i64 = c
        .query_row(
            "SELECT state_revision FROM tasks WHERE task_id=?1",
            [&item.task_id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    let scope = Scope {
        symbol: item.symbol.clone(),
        period: Period::Day,
    };
    series_commit::commit_series_tx(
        c,
        &series_commit::Commit {
            result_id: &item.publication_id,
            task_id: &item.task_id,
            item_id: &item.item_id,
            run_id: &item.run_id,
            occupancy_key: &scope.occupancy_key(),
            expected_task_revision: u64::try_from(revision)
                .map_err(|_| integrity("task revision out of range"))?,
            entity: &item.symbol,
            period: Period::Day,
            base_version_id: None,
            change: series_commit::SeriesChange::FirstPublish { bars: &bars },
            range_start_day: bars[0].trade_day,
            range_end_day: bars[bars.len() - 1].trade_day,
            algorithm_version: series_commit::COLLECTION_ALGO,
            window_digest: Some(series_commit::window_fold(&bars)?),
            source_binding_digest: Some(binding),
            evidence_digest: Some(evidence),
            coverage_status: Some(item.coverage.status),
            metadata_changed: false,
            event_evidence_digest: evidence,
            published_at: Some(item.published_at),
            staged_version_id: Some(item.version_id.as_str()),
        },
    )
}

// ---------------------------------------------------------------------------
// YearlyRepository — schema-21 yearly commits (S3)
// ---------------------------------------------------------------------------

impl crate::yearly_publication::results::YearlyRepository for SqliteCompactMarket {
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error> {
        self.tasks
            .transition(task, revision, run, change)
            .map_err(Error::Task)
    }
    fn commit_yearly(
        &self,
        plan: &crate::yearly_publication::results::YearlyResultPlan,
    ) -> Result<Receipt, Error> {
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let command = plan.command();
            // Replay first — the result-id anchor outranks every gate; a
            // repeated command returns its recorded receipt. A result id
            // bound to a different task/item is an identity conflict, not a
            // replay.
            if let Some(event) = series::event_by_result(&tx, &command.publication_id)? {
                if event.task_id != command.task_id || event.item_id != command.item_id {
                    return Err(Error::Conflict("result identity conflict".into()));
                }
                series_commit::consume_staged(&tx, Some(command.version_id.as_str()))?;
                let receipt = receipt_for(&tx, &command.publication_id)?
                    .ok_or_else(|| integrity("receipt event lacks result payload"))?;
                tx.commit().map_err(storage)?;
                return Ok(receipt);
            }
            let v = plan.publication.record.version();
            let scope = Scope {
                symbol: v.symbol.clone(),
                period: Period::Year,
            };
            let lineage = v
                .yearly_derivation
                .as_ref()
                .ok_or_else(|| integrity("yearly candidate lacks derivation lineage"))?;
            let bars: Vec<CompactBar> = v
                .records
                .iter()
                .map(|r| CompactBar::from_bar(r, Period::Year))
                .collect::<Result<_, _>>()
                .map_err(integrity)?;
            let daily_base = series_commit::hex_digest(&plan.base.version().version_id)?;
            let version_evidence = evidence_store::put_evidence(
                &tx,
                EvidenceKind::Version,
                &json!(crate::compact_market::VersionEvidence {
                    source: v.source.clone(),
                    coverage_reason: plan.publication.record.batch.coverage.reason.clone(),
                    unit_currency: plan.publication.record.batch.unit_currency.clone(),
                    adjustment_evidence: plan.publication.record.batch.adjustment_evidence.clone(),
                }),
            )?;
            let event_evidence = evidence_store::put_evidence(
                &tx,
                EvidenceKind::Yearly,
                &json!({
                    "base_daily_version_id": plan.base.version().version_id,
                    "algorithm_version": lineage.algorithm_version,
                    "years": v.records.iter().map(|r| r.date.year()).collect::<Vec<_>>(),
                }),
            )?;
            // `as_of_day` is the daily-content cutoff the derivation consumed
            // (`last_trading_date`), not the publish wall-clock — reads clip
            // the daily re-aggregation at it so a same-year append leaves the
            // row merely Behind rather than unclassifiable.
            let as_of = lineage.last_trading_date;
            let occupancy = scope.occupancy_key();
            let w = series_commit::Commit {
                result_id: &command.publication_id,
                task_id: &command.task_id,
                item_id: &command.item_id,
                run_id: &command.run_id,
                occupancy_key: &occupancy,
                expected_task_revision: command.expected_revision,
                entity: &v.symbol,
                period: Period::Year,
                base_version_id: plan.prior.as_ref().map(|p| p.version().version_id.as_str()),
                change: series_commit::SeriesChange::Yearly {
                    upserts: &bars,
                    daily_base_digest: daily_base,
                    algorithm_version: &lineage.algorithm_version,
                    as_of_day: compact_market::epoch_day(as_of),
                },
                range_start_day: 0,
                range_end_day: 0,
                algorithm_version: &lineage.algorithm_version,
                window_digest: None,
                // Yearly rows inherit the daily head's source binding —
                // the commit derives it from the daily series inside the tx.
                source_binding_digest: None,
                evidence_digest: Some(version_evidence),
                coverage_status: Some(plan.publication.record.batch.coverage.status),
                metadata_changed: false,
                event_evidence_digest: event_evidence,
                published_at: Some(v.published_at),
                staged_version_id: Some(command.version_id.as_str()),
            };
            let receipt = series_commit::commit_series_tx(&tx, &w)?;
            tx.commit().map_err(storage)?;
            Ok(receipt)
        })
    }
    fn yearly_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| receipt_for(c, id))
    }
    fn yearly_item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        <Self as RangeRepository>::item_receipt(self, task, item)
    }
}

// ---------------------------------------------------------------------------
// Publication-verification / default-range / gap stores — task tables and
// event-derived facts; no dropped v19 tables are consulted.
// ---------------------------------------------------------------------------

impl crate::publication_verification::decision::AuditStore for SqliteCompactMarket {
    fn commit_check(
        &self,
        plan: &crate::publication_verification::decision::Plan,
    ) -> Result<crate::publication_verification::decision::Receipt, Error> {
        self.write(|c| super::publication_checks::commit_check_tx(c, plan))
    }
    fn check_receipt(
        &self,
        id: &str,
    ) -> Result<Option<crate::publication_verification::decision::Receipt>, Error> {
        self.read(|c| {
            super::publication_checks::read_receipt(c, id).map(|r| r.map(|(_, receipt)| receipt))
        })
    }
}

impl crate::publication_verification::EvidenceRepository for SqliteCompactMarket {
    fn publication_facts(
        &self,
        task_id: &str,
    ) -> Result<crate::publication_verification::Facts, Error> {
        self.read(|c| super::verification::read_facts(c, task_id))
    }
}

impl crate::data_supply::default_range::DefaultRangeSource for SqliteCompactMarket {
    fn resolve_default_work(
        &self,
        op: crate::data_supply::default_range::DefaultOperation,
        today: NaiveDate,
    ) -> Result<crate::data_supply::default_range::DefaultWork, Error> {
        self.read(|c| {
            crate::data_supply::default_range::resolve_default_work_on(c, op, today).map_err(|e| {
                match e {
                    crate::data_supply::default_selection::SelectionError::EmptySelection => {
                        Error::Invalid("EMPTY_SELECTION".into())
                    }
                    crate::data_supply::default_selection::SelectionError::Invalid(m) => {
                        Error::Invalid(m)
                    }
                }
            })
        })
    }
}

impl crate::gap_verification::runner::GapTaskStore for SqliteCompactMarket {
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error> {
        MarketRepository::task(self, id)
    }
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, Error> {
        self.tasks
            .transition(task, revision, run, change)
            .map_err(Error::Task)
    }
}

impl crate::gap_verification::GapDecisionStore for SqliteCompactMarket {
    fn freeze_gap_subject(
        &self,
        target: &crate::gap_verification::GapTarget,
    ) -> Result<crate::gap_verification::SubjectBinding, Error> {
        self.read(|c| crate::gap_verification::freeze_subject_on(c, target))
    }
    fn check_gap_subject(
        &self,
        subject: &crate::gap_verification::SubjectBinding,
    ) -> Result<(), Error> {
        self.read(|c| crate::gap_verification::check_subject_on(c, subject))
    }
    fn commit_gap_decision(
        &self,
        decision: &crate::market_storage::GapVerificationDecision,
    ) -> Result<bool, Error> {
        self.tx(|c| super::gaps::record_gap_decision_tx(c, decision, true))
    }
    fn gap_decision(
        &self,
        decision_id: &str,
    ) -> Result<Option<crate::market_storage::GapVerificationDecision>, Error> {
        self.read(|c| super::gaps::read_decision(c, decision_id))
    }
}
