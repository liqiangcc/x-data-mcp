//! Schema-19 compact repository adapter (#259): one service-lifetime
//! `WriteCoordinator`, compact facts authoritative, legacy-compatible
//! `CheckedRecord` envelopes synthesized in memory at read boundaries. No
//! legacy JSON envelope is ever persisted.
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::compact_results::{
    self, BindingCheck, HeadExpectation, MarketUpdateChange, MarketUpdateCommit, PublicationCommit,
    RequiredHead,
};
use super::compact_runtime::{CheckpointMetrics, SchemaIdentity, WriteCoordinator};
use super::compact_v19::{self, StoredClip, StoredDelta, StoredDeltaKind};
use super::compact_versions::{
    self, CompactVersionState, MaterializedVersion, VersionDraft, YearlyLineageDraft,
};
use super::{integrity, storage};
use crate::compact_market::{
    self, CompactBar, Digest, EvidenceKind, ManifestMode, PublicationKind, SourceBinding,
    UpdateOutcome, VersionEvidence,
};
use crate::local_runtime::DatabaseLease;
use crate::market_data::{
    Adjustment, Bar, CoverageEvidence, MarketDataBatch, Period, Provenance, SnapshotInputBinding,
};
use crate::market_storage::compat::{self, CheckedRecord};
use crate::market_storage::{
    Error, MarketRepository, Publication, PublishIntent, PublishPlan, Range, ReadPage, Receipt,
    Scope,
};
use crate::market_update::publication::{RangePlan, RangeRepository};
use crate::publish::EffectiveVersion;
use crate::task_control::{
    self, Acceptance, Change, Command, FrozenWork, RequestRecord, RetryGuard, TaskHead, TaskItem,
    TaskRepository, TaskSnapshot,
};

const BUSY: Duration = Duration::from_secs(5);
const SEGMENT_ROWS: usize = 512;
const UPDATE_ALGORITHM: &str = "local-range-update-v1";
const COLLECTION_ALGORITHM: &str = "local-collection-v1";

/// Schema-19 repository: `TaskRepository` + `MarketRepository` +
/// `RangeRepository` over the compact tables, fronted by the
/// `WriteCoordinator`'s lease/schema checks and WAL bounds.
#[derive(Clone)]
pub(crate) struct SqliteCompactMarket {
    coordinator: Arc<WriteCoordinator>,
}

/// Deterministic schema-19 identity (recomputes the schema_migrations digest
/// without opening the database).
pub(crate) fn schema19_identity() -> SchemaIdentity {
    const DOMAIN: &[u8] = b"x-data-schema-identity-v1\0";
    let mut payload = DOMAIN.to_vec();
    payload.extend_from_slice(&19u32.to_be_bytes());
    payload.extend_from_slice(&19u32.to_be_bytes());
    for (index, sql) in crate::sqlite_tasks::MIGRATIONS
        .iter()
        .chain(std::iter::once(&crate::sqlite_tasks::COMPACT_MIGRATION))
        .enumerate()
    {
        payload.extend_from_slice(&(index as u32 + 1).to_be_bytes());
        let checksum = crate::hash::sha256(sql.as_bytes());
        payload.extend_from_slice(&(checksum.len() as u32).to_be_bytes());
        payload.extend_from_slice(checksum.as_bytes());
    }
    SchemaIdentity {
        user_version: 19,
        digest: Sha256::digest(&payload).into(),
    }
}

impl SqliteCompactMarket {
    pub(crate) fn open(path: &Path) -> Result<Self, Error> {
        let lease = DatabaseLease::acquire(path)
            .map_err(|e| Error::Base(format!("database lease: {e}")))?;
        let coordinator =
            WriteCoordinator::open_existing(path, Arc::new(lease), schema19_identity(), BUSY)?;
        Ok(Self {
            coordinator: Arc::new(coordinator),
        })
    }

    pub(crate) fn coordinator(&self) -> &Arc<WriteCoordinator> {
        &self.coordinator
    }

    pub(crate) fn checkpoint_idle(&self) -> Result<CheckpointMetrics, Error> {
        self.coordinator.checkpoint_idle()
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T, Error>) -> Result<T, Error> {
        self.coordinator.read(f)
    }
    fn write<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T, Error>) -> Result<T, Error> {
        self.coordinator.write(f)
    }
    fn tx<T>(&self, f: impl FnOnce(&Connection) -> Result<T, Error>) -> Result<T, Error> {
        self.coordinator.transaction(f)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactSource {
    generation_id: String,
    generation_manifest_sha256: String,
    day_member_sha256: String,
    security_event_sha256: String,
    generation_evidence: Value,
}

fn invalid(error: impl std::fmt::Display) -> Error {
    Error::Invalid(error.to_string())
}

fn hex_digest(text: &str) -> Result<Digest, Error> {
    if text.len() != 64 || !crate::hash::is_sha256(text) {
        return Err(invalid("digest hex must be 32 bytes"));
    }
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(invalid)?;
    }
    Ok(digest)
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn period_code(period: Period) -> i64 {
    match period {
        Period::Day => 1,
        Period::Year => 2,
    }
}

fn period_name(period: Period) -> &'static str {
    match period {
        Period::Day => "day",
        Period::Year => "year",
    }
}

fn version_key(c: &Connection, version_id: &str) -> Result<i64, Error> {
    c.query_row(
        "SELECT version_key FROM data_versions WHERE version_id=?1",
        [version_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(storage)?
    .ok_or(Error::NotFound)
}

fn head_key(c: &Connection, entity: &str, period: Period) -> Result<Option<i64>, Error> {
    c.query_row(
        "SELECT version_key FROM effective_heads WHERE entity=?1 AND period=?2 AND adjustment=1",
        params![entity, period_code(period)],
        |r| r.get(0),
    )
    .optional()
    .map_err(storage)
}

fn head_id(c: &Connection, scope: &Scope) -> Result<Option<String>, Error> {
    c.query_row(
        "SELECT v.version_id FROM effective_heads h JOIN data_versions v ON v.version_key=h.version_key WHERE h.entity=?1 AND h.period=?2 AND h.adjustment=1",
        params![scope.symbol, period_code(scope.period)],
        |r| r.get(0),
    )
    .optional()
    .map_err(storage)
}

/// Rebuild the deterministic legacy-compatible envelope from verified compact
/// facts. Compact `integrity_digest`/`projection_digest` remain authoritative;
/// `integrity_sha256` in the projection is the in-memory legacy digest only.
fn synthesize(m: &MaterializedVersion) -> Result<CheckedRecord, Error> {
    let version = &m.version;
    let provenance = Provenance {
        source: version.source.clone(),
        source_period: if version.yearly_derivation.is_some() {
            "day".into()
        } else {
            period_name(version.period).into()
        },
        source_adjustment: "qfq".into(),
        symbol: version.symbol.clone(),
        acquired_at: version.published_at,
        adapter_version: "effective-publish".into(),
        yearly_derivation: version.yearly_derivation.clone(),
        snapshot_inputs: m
            .generation_evidence
            .get("snapshot_inputs")
            .cloned()
            .map(serde_json::from_value::<Vec<SnapshotInputBinding>>)
            .transpose()
            .map_err(super::integrity)?,
    };
    let batch = MarketDataBatch {
        symbol: version.symbol.clone(),
        period: version.period,
        adjustment: Adjustment::Forward,
        bars: version.records.clone(),
        provenance,
        coverage: CoverageEvidence {
            status: m.coverage_status,
            reason: m.coverage_reason.clone(),
        },
        unit_currency: version.unit_currency.clone(),
        adjustment_evidence: version.adjustment_evidence.clone(),
    };
    let mut raw = serde_json::to_value(version).map_err(super::integrity)?;
    raw["batch"] = serde_json::to_value(&batch).map_err(super::integrity)?;
    raw["compact_source"] = json!({
        "generation_id": m.source_binding.generation_id,
        "generation_manifest_sha256": hex_bytes(m.source_binding.generation_manifest_sha256.as_slice()),
        "day_member_sha256": hex_bytes(m.source_binding.day_member_sha256.as_slice()),
        "security_event_sha256": hex_bytes(m.source_binding.security_event_sha256.as_slice()),
        "generation_evidence": m.generation_evidence,
    });
    if let Some(parent) = &m.parent_version_id {
        raw["compact_parent_version_id"] = json!(parent);
    }
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw)?);
    Ok(CheckedRecord {
        raw,
        version: m.version.clone(),
        batch,
    })
}

fn materialize_checked(
    c: &Connection,
    version_id: &str,
    staged: bool,
) -> Result<CheckedRecord, Error> {
    let m = compact_versions::materialize(c, version_id)?;
    let allowed = match m.state {
        CompactVersionState::Published => !staged,
        CompactVersionState::Validated => true,
        CompactVersionState::Staged => staged,
    };
    if !allowed {
        return Err(Error::NotFound);
    }
    synthesize(&m)
}

fn stage_tx(c: &mut Connection, raw: &Value) -> Result<(), Error> {
    let record = compat::check(raw)?;
    let source: CompactSource = serde_json::from_value(
        raw.get("compact_source")
            .cloned()
            .ok_or_else(|| invalid("compact_source required for compact staging"))?,
    )
    .map_err(invalid)?;
    let evidence =
        compact_versions::put_evidence(c, EvidenceKind::Generation, &source.generation_evidence)?;
    let binding = SourceBinding {
        generation_id: source.generation_id.clone(),
        generation_manifest_sha256: hex_digest(&source.generation_manifest_sha256)?,
        day_member_sha256: hex_digest(&source.day_member_sha256)?,
        security_event_sha256: hex_digest(&source.security_event_sha256)?,
        evidence_digest: evidence,
    };
    let binding_digest = compact_versions::put_source_binding(c, &binding)?;
    let version_evidence = VersionEvidence {
        source: record.version.source.clone(),
        coverage_reason: record.batch.coverage.reason.clone(),
        unit_currency: record.batch.unit_currency.clone(),
        adjustment_evidence: record.batch.adjustment_evidence.clone(),
    };
    let evidence_digest =
        compact_versions::put_evidence(c, EvidenceKind::Version, &json!(version_evidence))?;
    let period = record.version.period;
    let entity = record.version.symbol.clone();
    let bars = record
        .version
        .records
        .iter()
        .map(|bar| CompactBar::from_bar(bar, period))
        .collect::<Result<Vec<_>, _>>()
        .map_err(super::integrity)?;
    let mut clips = Vec::new();
    for chunk in bars.chunks(SEGMENT_ROWS) {
        let segment = compact_v19::insert_segment(c, &entity, period, chunk)?;
        clips.push(StoredClip {
            segment_key: segment.key,
            clip: crate::compact_market::SegmentClip {
                segment_digest: segment.digest,
                clip_start_day: chunk.first().unwrap().trade_day,
                clip_end_day: chunk.last().unwrap().trade_day,
                row_count: chunk.len() as u32,
            },
        });
    }
    let yearly_lineage = match period {
        Period::Year => {
            let derivation = record
                .version
                .yearly_derivation
                .clone()
                .ok_or_else(|| invalid("yearly record lacks derivation"))?;
            let base_key = version_key(c, &derivation.base_daily_version_id)?;
            let base = compact_versions::result_facts(c, base_key)?;
            if base.state != CompactVersionState::Published
                || base.period != Period::Day
                || base.entity != entity
            {
                return Err(invalid("yearly base is not a published daily version"));
            }
            Some(YearlyLineageDraft {
                base_daily_version_key: base_key,
                algorithm_version: derivation.algorithm_version.clone(),
                first_trade_day: compact_market::epoch_day(derivation.first_trading_date),
                last_trade_day: compact_market::epoch_day(derivation.last_trading_date),
                as_of_day: base.last_day,
            })
        }
        Period::Day => {
            if record.version.yearly_derivation.is_some() {
                return Err(invalid("daily record carries yearly derivation"));
            }
            None
        }
    };
    let draft = VersionDraft {
        version_id: record.version.version_id.clone(),
        entity: entity.clone(),
        period,
        source_binding_digest: binding_digest,
        coverage_status: record.batch.coverage.status,
        evidence_digest,
        created_at: record.version.published_at.to_rfc3339(),
        yearly_lineage,
    };
    if let Some(parent_id) = raw.get("compact_parent_version_id").and_then(Value::as_str) {
        let parent_key = version_key(c, parent_id)?;
        let parent = compact_versions::result_facts(c, parent_key)?;
        if parent.state != CompactVersionState::Published
            || parent.entity != entity
            || parent.period != period
        {
            return Err(invalid(
                "compact parent is not a published same-scope version",
            ));
        }
        let parent_bars = compact_versions::fixed(c, parent_id)?
            .version
            .records
            .iter()
            .map(|bar| CompactBar::from_bar(bar, period))
            .collect::<Result<Vec<_>, _>>()
            .map_err(super::integrity)?;
        match plan_delta(&parent_bars, &bars, &clips)? {
            DeltaPlan::Equal => {
                return Err(Error::Conflict(
                    "candidate identical to declared parent; no-change must not stage".into(),
                ));
            }
            DeltaPlan::Ops(ops) => {
                if parent.delta_depth < 256 {
                    compact_versions::stage_delta(c, &draft, parent_key, &ops)?;
                } else {
                    compact_versions::stage_checkpoint(
                        c,
                        &draft,
                        &clips,
                        Some(&compact_versions::CheckpointBasis {
                            parent_version_key: parent_key,
                            delta_ops: ops,
                        }),
                    )?;
                }
            }
            DeltaPlan::Full => {
                compact_versions::stage_checkpoint(c, &draft, &clips, None)?;
            }
        }
    } else {
        compact_versions::stage_checkpoint(c, &draft, &clips, None)?;
    }
    Ok(())
}

enum DeltaPlan {
    Equal,
    Ops(Vec<StoredDelta>),
    Full,
}

/// Delta ops are pure upserts: every parent day must still exist in the
/// candidate; the upsert range covers [min changed day, max changed day] and
/// carries the candidate's own segment clips.
fn plan_delta(
    parent: &[CompactBar],
    candidate: &[CompactBar],
    clips: &[StoredClip],
) -> Result<DeltaPlan, Error> {
    use std::collections::BTreeMap;
    let parent_by_day: BTreeMap<i64, &CompactBar> =
        parent.iter().map(|b| (b.trade_day, b)).collect();
    let candidate_by_day: BTreeMap<i64, &CompactBar> =
        candidate.iter().map(|b| (b.trade_day, b)).collect();
    if parent
        .iter()
        .any(|b| !candidate_by_day.contains_key(&b.trade_day))
    {
        return Ok(DeltaPlan::Full);
    }
    let mut changed = Vec::new();
    for bar in candidate {
        if parent_by_day.get(&bar.trade_day) != Some(&bar) {
            changed.push(bar.trade_day);
        }
    }
    if changed.is_empty() {
        return Ok(DeltaPlan::Equal);
    }
    let range_start = *changed.iter().min().unwrap();
    let range_end = *changed.iter().max().unwrap();
    let mut ops = Vec::new();
    for stored in clips {
        let clip = &stored.clip;
        if clip.clip_end_day < range_start || clip.clip_start_day > range_end {
            continue;
        }
        let start = clip.clip_start_day.max(range_start);
        let count = candidate
            .iter()
            .filter(|b| b.trade_day >= start && b.trade_day <= clip.clip_end_day)
            .count() as u32;
        if count == 0 {
            continue;
        }
        ops.push(StoredDelta {
            range_start_day: start,
            range_end_day: clip.clip_end_day,
            kind: StoredDeltaKind::Upsert(StoredClip {
                segment_key: stored.segment_key,
                clip: crate::compact_market::SegmentClip {
                    segment_digest: clip.segment_digest,
                    clip_start_day: start,
                    clip_end_day: clip.clip_end_day,
                    row_count: count,
                },
            }),
        });
    }
    Ok(DeltaPlan::Ops(ops))
}

fn binding_check(plan: &PublishPlan) -> BindingCheck {
    let scope = Scope::of(&plan.record.version);
    BindingCheck {
        run_id: plan.command.run_id.clone(),
        entity: scope.symbol.clone(),
        period: scope.period,
        occupancy_key: scope.occupancy_key(),
    }
}

fn required_heads(
    c: &Connection,
    heads: &[(Scope, Option<String>)],
) -> Result<Vec<RequiredHead>, Error> {
    heads
        .iter()
        .map(|(scope, expected)| {
            expected
                .as_ref()
                .map(|id| version_key(c, id))
                .transpose()
                .map(|key| RequiredHead {
                    entity: scope.symbol.clone(),
                    period: scope.period,
                    expected: key,
                })
        })
        .collect()
}

fn registry_kind(c: &Connection, result_id: &str) -> Result<Option<i64>, Error> {
    c.query_row(
        "SELECT result_kind FROM result_registry WHERE result_id=?1",
        [result_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(storage)
}

fn replay_receipt(c: &Connection, kind: i64, command: &Publication) -> Result<Receipt, Error> {
    match kind {
        1 => {
            let stored = compact_results::read_market_update(c, &command.publication_id)?;
            if stored.identity.task_id != command.task_id
                || stored.identity.item_id != command.item_id
                || stored.identity.result_version_id != command.version_id
            {
                return Err(Error::Conflict("result identity conflict".into()));
            }
            Ok(Receipt {
                publication_id: command.publication_id.clone(),
                version_id: stored.identity.result_version_id.clone(),
                task_id: command.task_id.clone(),
                task_revision: stored.identity.task_revision,
            })
        }
        2 => {
            let stored = compact_results::read_publication(c, &command.publication_id)?;
            if stored.identity.task_id != command.task_id
                || stored.identity.item_id != command.item_id
                || stored.identity.result_version_id != command.version_id
            {
                return Err(Error::Conflict("result identity conflict".into()));
            }
            Ok(Receipt {
                publication_id: command.publication_id.clone(),
                version_id: stored.identity.result_version_id.clone(),
                task_id: command.task_id.clone(),
                task_revision: stored.identity.task_revision,
            })
        }
        _ => Err(integrity("unknown result kind")),
    }
}

fn commit_tx(c: &mut Connection, plan: &PublishPlan) -> Result<Receipt, Error> {
    let command = &plan.command;
    if let Some(kind) = registry_kind(c, &command.publication_id)? {
        return replay_receipt(c, kind, command);
    }
    let result_key = version_key(c, &command.version_id)?;
    let facts = compact_versions::result_facts(c, result_key)?;
    if facts.state != CompactVersionState::Validated {
        return Err(Error::Conflict("candidate is not validated".into()));
    }
    // The sealed plan's record must still equal the stored verified facts.
    let stored = materialize_checked(c, &command.version_id, true)?;
    if stored.raw != plan.record.raw {
        return Err(integrity("validated candidate changed since plan"));
    }
    if let Some(digest) = &plan.base_digest {
        let lineage = plan
            .record
            .version
            .yearly_derivation
            .as_ref()
            .ok_or_else(|| integrity("base digest without yearly lineage"))?;
        let base = materialize_checked(c, &lineage.base_daily_version_id, false)?;
        if base.raw["integrity_sha256"].as_str() != Some(digest.as_str()) {
            return Err(Error::Base("fixed base changed since validation".into()));
        }
    }
    let scope = Scope::of(&plan.record.version);
    let binding = binding_check(plan);
    let required = required_heads(c, &plan.required_heads)?;
    match &command.intent {
        PublishIntent::Daily {
            expected_head: Some(expected),
        } => {
            let base_key = version_key(c, expected)?;
            let base = compact_versions::result_facts(c, base_key)?;
            if base.state != CompactVersionState::Published {
                return Err(integrity("update base is not published"));
            }
            let (change, range_start, range_end) = match facts.manifest_mode {
                ManifestMode::Delta => {
                    if facts.parent_version_key != Some(base_key) {
                        return Err(Error::Conflict(
                            "update result delta is not based on the expected head".into(),
                        ));
                    }
                    let parent = compact_versions::result_facts(
                        c,
                        facts
                            .parent_version_key
                            .ok_or_else(|| integrity("delta lacks parent"))?,
                    )?;
                    let (ops, _, _) =
                        compact_v19::read_delta(c, result_key, parent.projection_digest)?;
                    let base_count: i64 = c
                        .query_row(
                            "SELECT record_count FROM data_versions WHERE version_key=?1",
                            [base_key],
                            |r| r.get(0),
                        )
                        .map_err(storage)?;
                    (
                        MarketUpdateChange::Append {
                            prefix_bytes: (base_count as u64) * 32,
                            ops: ops.clone(),
                        },
                        ops.first().map(|o| o.range_start_day).unwrap_or(0),
                        ops.last().map(|o| o.range_end_day).unwrap_or(0),
                    )
                }
                ManifestMode::Checkpoint => {
                    (MarketUpdateChange::Full, facts.first_day, facts.last_day)
                }
            };
            let evidence = compact_versions::put_evidence(
                c,
                EvidenceKind::Continuity,
                &json!({
                    "mode": if matches!(change, MarketUpdateChange::Append{..}) {"append"} else {"full"},
                    "base_version_id": expected,
                    "source_binding_digest": hex_bytes(&facts.source_binding_digest),
                }),
            )?;
            let stored = compact_results::commit_market_update(
                c,
                &MarketUpdateCommit {
                    result_id: command.publication_id.clone(),
                    task_id: command.task_id.clone(),
                    item_id: command.item_id.clone(),
                    base_version_key: base_key,
                    result_version_key: result_key,
                    range_start_day: range_start,
                    range_end_day: range_end,
                    algorithm_version: UPDATE_ALGORITHM.into(),
                    source_binding_digest: facts.source_binding_digest,
                    evidence_digest: evidence,
                    expected_task_revision: command.expected_revision,
                    published_at: Some(Utc::now()),
                    head: HeadExpectation::Version(base_key),
                    required_heads: required,
                    binding,
                    change,
                },
            )?;
            Ok(Receipt {
                publication_id: command.publication_id.clone(),
                version_id: stored.identity.result_version_id.clone(),
                task_id: command.task_id.clone(),
                task_revision: stored.identity.task_revision,
            })
        }
        PublishIntent::Daily {
            expected_head: None,
        }
        | PublishIntent::HistoricalFixedBase
        | PublishIntent::CurrentHead { .. } => {
            let (kind, head, base_key, required) = match &command.intent {
                PublishIntent::Daily { .. } => (
                    PublicationKind::Collection,
                    HeadExpectation::Missing,
                    None,
                    required,
                ),
                PublishIntent::HistoricalFixedBase => (
                    PublicationKind::Yearly,
                    HeadExpectation::Keep,
                    head_key(c, &scope.symbol, scope.period)?,
                    required,
                ),
                PublishIntent::CurrentHead {
                    expected_yearly_head,
                    ..
                } => {
                    // `required` already binds the expected daily head via
                    // `plan.required_heads`; the yearly head expectation is
                    // enforced by `head`/`apply_head` below.
                    let yearly_key = expected_yearly_head
                        .as_ref()
                        .map(|id| version_key(c, id))
                        .transpose()?;
                    (
                        PublicationKind::Yearly,
                        match yearly_key {
                            Some(key) => HeadExpectation::Version(key),
                            None => HeadExpectation::Missing,
                        },
                        yearly_key,
                        required,
                    )
                }
            };
            let evidence = match kind {
                PublicationKind::Yearly => {
                    let lineage = stored
                        .version
                        .yearly_derivation
                        .as_ref()
                        .ok_or_else(|| integrity("yearly result lacks derivation"))?;
                    compact_versions::put_evidence(
                        c,
                        EvidenceKind::Yearly,
                        &json!({
                            "base_daily_version_id": lineage.base_daily_version_id,
                            "algorithm_version": lineage.algorithm_version,
                        }),
                    )?
                }
                _ => {
                    let digest: Vec<u8> = c
                        .query_row(
                            "SELECT evidence_digest FROM data_versions WHERE version_key=?1",
                            [result_key],
                            |r| r.get(0),
                        )
                        .map_err(storage)?;
                    fixed_digest(&digest)?
                }
            };
            let algorithm = match kind {
                PublicationKind::Yearly => stored
                    .version
                    .yearly_derivation
                    .as_ref()
                    .map(|l| l.algorithm_version.clone())
                    .unwrap_or_else(|| COLLECTION_ALGORITHM.into()),
                _ => COLLECTION_ALGORITHM.into(),
            };
            let stored_result = compact_results::commit_publication(
                c,
                &PublicationCommit {
                    publication_id: command.publication_id.clone(),
                    task_id: command.task_id.clone(),
                    item_id: command.item_id.clone(),
                    publication_kind: kind,
                    base_version_key: base_key,
                    result_version_key: result_key,
                    outcome: UpdateOutcome::Published,
                    algorithm_version: algorithm,
                    source_binding_digest: facts.source_binding_digest,
                    evidence_digest: evidence,
                    expected_task_revision: command.expected_revision,
                    published_at: Some(Utc::now()),
                    head,
                    required_heads: required,
                    binding,
                },
            )?;
            Ok(Receipt {
                publication_id: command.publication_id.clone(),
                version_id: stored_result.identity.result_version_id.clone(),
                task_id: command.task_id.clone(),
                task_revision: stored_result.identity.task_revision,
            })
        }
    }
}

fn fixed_digest(blob: &[u8]) -> Result<Digest, Error> {
    if blob.len() != 32 {
        return Err(integrity("stored digest is not 32 bytes"));
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(blob);
    Ok(digest)
}

fn commit_range_tx(c: &mut Connection, plan: &RangePlan) -> Result<Receipt, Error> {
    let publication = &plan.publication;
    let command = publication.publication();
    if let Some(kind) = registry_kind(c, &command.publication_id)? {
        return replay_receipt(c, kind, command);
    }
    let base_key = version_key(c, &plan.base.version.version_id)?;
    let base = compact_versions::result_facts(c, base_key)?;
    if base.state != CompactVersionState::Published {
        return Err(integrity("range base is not published"));
    }
    let binding = binding_check(publication);
    let required = required_heads(c, &publication.required_heads)?;
    let range_start = compact_market::epoch_day(
        NaiveDate::parse_from_str(
            plan.audit["range"]["start"].as_str().unwrap_or_default(),
            "%Y-%m-%d",
        )
        .map_err(super::integrity)?,
    );
    let range_end = compact_market::epoch_day(
        NaiveDate::parse_from_str(
            plan.audit["range"]["end"].as_str().unwrap_or_default(),
            "%Y-%m-%d",
        )
        .map_err(super::integrity)?,
    );
    let evidence_value = plan
        .audit
        .get("evidence")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let (change, result_key) = if plan.no_change {
        let base_count: i64 = c
            .query_row(
                "SELECT record_count FROM data_versions WHERE version_key=?1",
                [base_key],
                |r| r.get(0),
            )
            .map_err(storage)?;
        (
            MarketUpdateChange::NoChange {
                prefix_bytes: (base_count as u64) * 32,
            },
            base_key,
        )
    } else {
        let key = version_key(c, &command.version_id)?;
        let facts = compact_versions::result_facts(c, key)?;
        if facts.state != CompactVersionState::Validated {
            return Err(Error::Conflict("candidate is not validated".into()));
        }
        let stored = materialize_checked(c, &command.version_id, true)?;
        if stored.raw != publication.record.raw {
            return Err(integrity("validated candidate changed since plan"));
        }
        let change = match facts.manifest_mode {
            ManifestMode::Delta => {
                if facts.parent_version_key != Some(base_key) {
                    return Err(Error::Conflict(
                        "update result delta is not based on the frozen base".into(),
                    ));
                }
                let (ops, _, _) = compact_v19::read_delta(c, key, base.projection_digest)?;
                MarketUpdateChange::Append {
                    prefix_bytes: base_count(c, base_key)? * 32,
                    ops,
                }
            }
            ManifestMode::Checkpoint => MarketUpdateChange::Full,
        };
        (change, key)
    };
    let mut continuity = json!({
        "mode": match &change {
            MarketUpdateChange::NoChange{..} => "unchanged",
            MarketUpdateChange::Append{..} => "append",
            MarketUpdateChange::Full => "full",
        },
        "base_version_id": plan.base.version.version_id,
        "source_binding_digest": hex_bytes(
            &compact_versions::result_facts(c, result_key)?.source_binding_digest,
        ),
    });
    if let Some(proof) = evidence_value.get("continuity") {
        continuity["proof"] = proof.clone();
    }
    if let Some(basis) = evidence_value.get("same_qfq_basis_ref") {
        continuity["basis_ref"] = basis.clone();
    }
    if let Some(revision) = evidence_value.get("revision_ref") {
        continuity["revision_ref"] = revision.clone();
    }
    if let MarketUpdateChange::Append { prefix_bytes, .. }
    | MarketUpdateChange::NoChange { prefix_bytes } = &change
    {
        continuity["prefix_bytes"] = json!(prefix_bytes);
    }
    let evidence = compact_versions::put_evidence(c, EvidenceKind::Continuity, &continuity)?;
    let stored = compact_results::commit_market_update(
        c,
        &MarketUpdateCommit {
            result_id: command.publication_id.clone(),
            task_id: command.task_id.clone(),
            item_id: command.item_id.clone(),
            base_version_key: base_key,
            result_version_key: result_key,
            range_start_day: range_start,
            range_end_day: range_end,
            algorithm_version: plan
                .audit
                .get("algorithm_version")
                .and_then(Value::as_str)
                .unwrap_or(UPDATE_ALGORITHM)
                .into(),
            source_binding_digest: compact_versions::result_facts(c, result_key)?
                .source_binding_digest,
            evidence_digest: evidence,
            expected_task_revision: command.expected_revision,
            published_at: if plan.no_change {
                None
            } else {
                Some(Utc::now())
            },
            head: if plan.no_change {
                HeadExpectation::Keep
            } else {
                HeadExpectation::Version(base_key)
            },
            required_heads: required,
            binding,
            change,
        },
    )?;
    Ok(Receipt {
        publication_id: command.publication_id.clone(),
        version_id: stored.identity.result_version_id.clone(),
        task_id: command.task_id.clone(),
        task_revision: stored.identity.task_revision,
    })
}

fn base_count(c: &Connection, key: i64) -> Result<u64, Error> {
    let count: i64 = c
        .query_row(
            "SELECT record_count FROM data_versions WHERE version_key=?1",
            [key],
            |r| r.get(0),
        )
        .map_err(storage)?;
    Ok(count as u64)
}

impl MarketRepository for SqliteCompactMarket {
    fn stage(&self, raw: &Value) -> Result<(), Error> {
        self.write(|c| stage_tx(c, raw))
    }
    fn candidate(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.read(|c| materialize_checked(c, id, true))
    }
    fn fixed(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.read(|c| materialize_checked(c, id, false))
    }
    fn range(&self, scope: &Scope, id: Option<&str>, range: &Range) -> Result<ReadPage, Error> {
        range.validate()?;
        self.read(|c| {
            let id = match id {
                Some(id) => id.to_owned(),
                None => head_id(c, scope)?.ok_or(Error::NotFound)?,
            };
            let m = compact_versions::materialize(c, &id)?;
            if !matches!(
                m.state,
                CompactVersionState::Validated | CompactVersionState::Published
            ) {
                return Err(Error::NotFound);
            }
            if Scope::of(&m.version) != *scope {
                return Err(integrity("version scope mismatch"));
            }
            let record = synthesize(&m)?;
            let mut rows: Vec<Bar> = Vec::new();
            for bar in &m.version.records {
                if bar.date < range.start || bar.date > range.end {
                    continue;
                }
                if let Some(after) = range.after
                    && bar.date <= after
                {
                    continue;
                }
                rows.push(bar.clone());
            }
            let more = rows.len() > range.limit;
            rows.truncate(range.limit);
            let next_after = more.then(|| rows.last().map(|b| b.date)).flatten();
            Ok(ReadPage {
                integrity_sha256: record.raw["integrity_sha256"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                version: m.version,
                rows,
                next_after,
            })
        })
    }
    fn versions(&self, scope: &Scope) -> Result<Vec<EffectiveVersion>, Error> {
        self.read(|c| {
            let orphan: bool = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM task_publications tp LEFT JOIN data_versions v ON v.version_key=tp.version_key WHERE v.version_key IS NULL)",
                    [],
                    |r| r.get(0),
                )
                .map_err(storage)?;
            if orphan {
                return Err(integrity("task publication references missing version"));
            }
            let summaries = compact_versions::list(c, scope)?;
            summaries
                .iter()
                .map(|summary| {
                    compact_versions::materialize(c, &summary.version_id).map(|m| m.version)
                })
                .collect()
        })
    }
    fn fixed_many(&self, ids: &[String]) -> Result<Vec<CheckedRecord>, Error> {
        self.read(|c| {
            ids.iter()
                .map(|id| materialize_checked(c, id, false))
                .collect()
        })
    }
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error> {
        self.read(|c| {
            crate::sqlite_tasks::read_bound_task(c, id)?
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
    ) -> Result<Option<task_control::ItemBinding>, Error> {
        self.read(|c| crate::sqlite_tasks::read_item_binding(c, task, item).map_err(Error::Task))
    }
    fn transition_task_head(
        &self,
        head: &mut TaskHead,
        item: Option<&mut TaskItem>,
        run: &str,
        change: Change,
    ) -> Result<(), Error> {
        self.tx(|c| {
            crate::sqlite_tasks::transition_head_tx(c, head, item, run, change, Utc::now())
                .map_err(Error::Task)
        })
    }
    fn mark_validated(&self, record: &CheckedRecord) -> Result<(), Error> {
        self.write(|c| {
            let m = compact_versions::materialize(c, &record.version.version_id)?;
            let stored = synthesize(&m)?;
            if stored.raw != record.raw {
                return Err(Error::Conflict("validated record differs".into()));
            }
            match m.state {
                CompactVersionState::Staged => compact_versions::mark_validated(c, m.key),
                CompactVersionState::Validated => Ok(()),
                CompactVersionState::Published => {
                    Err(Error::Conflict("candidate already published".into()))
                }
            }
        })
    }
    fn receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| match registry_kind(c, id)? {
            Some(1) => {
                let stored = compact_results::read_market_update(c, id)?;
                Ok(Some(Receipt {
                    publication_id: stored.identity.result_id.clone(),
                    version_id: stored.identity.result_version_id.clone(),
                    task_id: stored.identity.task_id.clone(),
                    task_revision: stored.identity.task_revision,
                }))
            }
            Some(2) => {
                let stored = compact_results::read_publication(c, id)?;
                Ok(Some(Receipt {
                    publication_id: stored.identity.publication_id.clone(),
                    version_id: stored.identity.result_version_id.clone(),
                    task_id: stored.identity.task_id.clone(),
                    task_revision: stored.identity.task_revision,
                }))
            }
            Some(_) => Err(integrity("unknown result kind")),
            None => Ok(None),
        })
    }
    fn commit(&self, plan: &PublishPlan) -> Result<Receipt, Error> {
        self.write(|c| commit_tx(c, plan))
    }
}

impl RangeRepository for SqliteCompactMarket {
    fn commit_range(&self, plan: &RangePlan) -> Result<Receipt, Error> {
        self.write(|c| commit_range_tx(c, plan))
    }
    fn range_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| match registry_kind(c, id)? {
            Some(1) => {
                let stored = compact_results::read_market_update(c, id)?;
                Ok(Some(Receipt {
                    publication_id: stored.identity.result_id.clone(),
                    version_id: stored.identity.result_version_id.clone(),
                    task_id: stored.identity.task_id.clone(),
                    task_revision: stored.identity.task_revision,
                }))
            }
            Some(_) => Ok(None),
            None => Ok(None),
        })
    }
    fn item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| {
            let id: Option<String> = c
                .query_row(
                    "SELECT result_id FROM market_update_results WHERE task_id=?1 AND item_id=?2",
                    params![task, item],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            let Some(id) = id else { return Ok(None) };
            let stored = compact_results::read_market_update(c, &id)?;
            Ok(Some(Receipt {
                publication_id: stored.identity.result_id.clone(),
                version_id: stored.identity.result_version_id.clone(),
                task_id: stored.identity.task_id.clone(),
                task_revision: stored.identity.task_revision,
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
        <Self as TaskRepository>::transition(self, task, revision, run, change).map_err(Error::Task)
    }
    fn cover_gap(&self, covering: &crate::market_storage::GapCovering) -> Result<bool, Error> {
        covering.validate()?;
        self.write(|c| {
            let gap = c
                .query_row(
                    "SELECT gap_id,symbol,period,start_date,end_date,evidence_kind,evidence_id,evidence_digest,observed_at FROM proven_gaps WHERE gap_id=?1",
                    [&covering.gap_id],
                    |r| {
                        Ok(crate::market_storage::ProvenGap {
                            gap_id: r.get(0)?,
                            symbol: r.get(1)?,
                            period: match r.get::<_, String>(2)?.as_str() {
                                "day" => Period::Day,
                                _ => Period::Year,
                            },
                            start_date: r.get::<_, String>(3)?.parse().map_err(|_| {
                                rusqlite::Error::InvalidQuery
                            })?,
                            end_date: r.get::<_, String>(4)?.parse().map_err(|_| {
                                rusqlite::Error::InvalidQuery
                            })?,
                            evidence_kind: r.get(5)?,
                            evidence_id: r.get(6)?,
                            evidence_digest: r.get(7)?,
                            observed_at: r
                                .get::<_, String>(8)?
                                .parse()
                                .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        })
                    },
                )
                .optional()
                .map_err(storage)?
                .ok_or_else(|| Error::Invalid("covering references unknown gap".into()))?;
            let scope: Option<(String, i64)> = c
                .query_row(
                    "SELECT v.entity,v.period FROM data_versions v WHERE v.version_id=?1 AND (v.state=3 OR EXISTS(SELECT 1 FROM task_publications b WHERE b.version_key=v.version_key))",
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
            if entity != gap.symbol || code != period_code(gap.period) {
                return Err(Error::Invalid("covering version scope mismatch".into()));
            }
            let existing: Option<String> = c
                .query_row(
                    "SELECT covered_at FROM gap_coverings WHERE gap_id=?1 AND version_id=?2",
                    params![covering.gap_id, covering.version_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            match existing {
                Some(at) if at == covering.covered_at.to_rfc3339() => Ok(false),
                Some(_) => Err(integrity("gap covering identity conflict")),
                None => c
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
            }
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
            rows.into_iter()
                .map(|(gap_id, version_id, at)| {
                    Ok(crate::market_storage::GapCovering {
                        gap_id,
                        version_id,
                        covered_at: DateTime::parse_from_rfc3339(&at)
                            .map_err(super::integrity)?
                            .with_timezone(&Utc),
                    })
                })
                .collect()
        })
    }
}

impl TaskRepository for SqliteCompactMarket {
    fn read_request(&self, id: &str) -> Result<Option<RequestRecord>, task_control::Error> {
        self.coordinator
            .read(|c| {
                crate::sqlite_tasks::read_request(c, id).map_err(|e| {
                    Error::Task(task_control::Error::StorageUnavailable(e.to_string()))
                })
            })
            .map_err(|e| match e {
                Error::Task(inner) => inner,
                other => task_control::Error::StorageUnavailable(other.to_string()),
            })
    }
    fn read_task(&self, id: &str) -> Result<Option<TaskSnapshot>, task_control::Error> {
        self.coordinator
            .read(|c| {
                crate::sqlite_tasks::read_bound_task(c, id).map_err(|e| {
                    Error::Task(task_control::Error::StorageUnavailable(e.to_string()))
                })
            })
            .map_err(|e| match e {
                Error::Task(inner) => inner,
                other => task_control::Error::StorageUnavailable(other.to_string()),
            })
    }
    fn accept(
        &self,
        id: &str,
        command: &Command,
        work: &FrozenWork,
        run: &str,
        retry: Option<&RetryGuard>,
    ) -> Result<Acceptance, task_control::Error> {
        work.validate()?;
        if id.trim().is_empty() || run.trim().is_empty() {
            return Err(task_control::Error::InvalidCommand("empty identity".into()));
        }
        self.coordinator
            .write(|c| {
                crate::sqlite_tasks::accept_tx(c, id, command, work, run, retry)
                    .map_err(Error::Task)
            })
            .map_err(|e| match e {
                Error::Task(inner) => inner,
                other => task_control::Error::StorageUnavailable(other.to_string()),
            })
    }
    fn transition(
        &self,
        id: &str,
        revision: u64,
        run: &str,
        change: Change,
    ) -> Result<TaskSnapshot, task_control::Error> {
        self.coordinator
            .write(|c| {
                crate::sqlite_tasks::transition_tx(c, id, revision, run, change)
                    .map_err(Error::Task)
            })
            .map_err(|e| match e {
                Error::Task(inner) => inner,
                other => task_control::Error::StorageUnavailable(other.to_string()),
            })
    }
    fn transition_light(
        &self,
        s: &mut TaskSnapshot,
        run: &str,
        change: Change,
    ) -> Result<(), task_control::Error> {
        self.coordinator
            .write(|c| {
                crate::sqlite_tasks::transition_light_tx(c, s, run, change).map_err(Error::Task)
            })
            .map_err(|e| match e {
                Error::Task(inner) => inner,
                other => task_control::Error::StorageUnavailable(other.to_string()),
            })
    }
    fn candidates(&self, run: &str) -> Result<Vec<TaskSnapshot>, task_control::Error> {
        self.coordinator
            .read(|c| {
                crate::sqlite_tasks::candidates_tx(c, run).map_err(|e| {
                    Error::Task(task_control::Error::StorageUnavailable(e.to_string()))
                })
            })
            .map_err(|e| match e {
                Error::Task(inner) => inner,
                other => task_control::Error::StorageUnavailable(other.to_string()),
            })
    }
    fn reconcile(&self, run: &str) -> Result<usize, task_control::Error> {
        self.coordinator
            .write(|c| crate::sqlite_tasks::reconcile_tx(c, run).map_err(Error::Task))
            .map_err(|e| match e {
                Error::Task(inner) => inner,
                other => task_control::Error::StorageUnavailable(other.to_string()),
            })
    }
}

#[cfg(test)]
mod tests;
