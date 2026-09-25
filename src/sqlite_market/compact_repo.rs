//! Schema-19 compact repository adapter (#259): one service-lifetime
//! `WriteCoordinator`, compact facts authoritative, legacy-compatible
//! `CheckedRecord` envelopes synthesized in memory at read boundaries. No
//! legacy JSON envelope is ever persisted.
use std::collections::BTreeSet;
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
    Adjustment, Bar, CoverageEvidence, CoverageStatus, MarketDataBatch, Period, Provenance,
    SnapshotInputBinding,
};
use crate::market_storage::compat::{self, CheckedRecord};
use crate::market_storage::{
    Error, MarketRepository, Publication, PublishIntent, PublishPlan, Range, ReadPage, Receipt,
    Scope,
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
const SEGMENT_ROWS: usize = 512;
const UPDATE_ALGORITHM: &str = "local-range-update-v1";
const COLLECTION_ALGORITHM: &str = "local-collection-v1";

/// Schema-19 repository: `TaskRepository` + `MarketRepository` +
/// `RangeRepository` over the compact tables, fronted by the
/// `WriteCoordinator`'s lease/schema checks and WAL bounds.
#[derive(Clone)]
pub struct SqliteCompactMarket {
    coordinator: Arc<WriteCoordinator>,
    tasks: SqliteTasks,
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
    pub fn open(path: &Path) -> Result<Self, Error> {
        let lease = DatabaseLease::acquire(path)
            .map_err(|e| Error::Base(format!("database lease: {e}")))?;
        Self::open_shared(path, Arc::new(lease))
    }

    /// Opens the compact repository on an already-held database lease —
    /// the service process owns the flock, so the backend must share it
    /// rather than acquire a second one.
    pub fn open_shared(path: &Path, lease: Arc<DatabaseLease>) -> Result<Self, Error> {
        let coordinator = Arc::new(WriteCoordinator::open_existing(
            path,
            lease,
            schema19_identity(),
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

    /// Ops-first delta stage: the update runner supplies the changed bars
    /// and removed days for an existing published head instead of a
    /// full-history envelope. The draft reuses the base version's
    /// source binding / coverage / version evidence (same generation).
    /// Returns the staged version key.
    pub(crate) fn stage_ops_delta(
        &self,
        entity: &str,
        base_version_id: &str,
        version_id: &str,
        upserts: &[Bar],
        removes: &[NaiveDate],
    ) -> Result<i64, Error> {
        self.tx(|c| {
            Self::stage_ops_delta_tx(c, entity, base_version_id, version_id, upserts, removes)
        })
    }

    /// Transaction-level `stage_ops_delta` (#259 S5G-6): the batch
    /// update commit drives the same staging inside one transaction.
    #[allow(clippy::too_many_arguments)]
    fn stage_ops_delta_tx(
        c: &Connection,
        entity: &str,
        base_version_id: &str,
        version_id: &str,
        upserts: &[Bar],
        removes: &[NaiveDate],
    ) -> Result<i64, Error> {
        {
            let base_key = version_key(c, base_version_id)?;
            let base_facts = compact_versions::result_facts(c, base_key)?;
            if base_facts.state != CompactVersionState::Published {
                return Err(integrity("ops-delta base version is not published"));
            }
            if base_facts.entity != entity || base_facts.period != Period::Day {
                return Err(integrity("ops-delta base entity/period mismatch"));
            }
            let (cov, evd): (i64, Vec<u8>) = c
                .query_row(
                    "SELECT coverage_status, evidence_digest FROM data_versions \
                     WHERE version_key=?1",
                    [base_key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(storage)?;
            let coverage_status = match cov {
                1 => CoverageStatus::Complete,
                2 => CoverageStatus::Incomplete,
                _ => CoverageStatus::Unknown,
            };
            let mut bars: Vec<CompactBar> = upserts
                .iter()
                .map(|b| CompactBar::from_bar(b, Period::Day).map_err(integrity))
                .collect::<Result<_, _>>()?;
            bars.sort_by_key(|b| b.trade_day);
            if bars.windows(2).any(|w| w[0].trade_day >= w[1].trade_day) {
                return Err(integrity("ops-delta upserts are not strictly increasing"));
            }
            let mut ops = Vec::new();
            for chunk in bars.chunks(512) {
                let segment = compact_v19::insert_segment(c, entity, Period::Day, chunk)?;
                ops.push(StoredDelta {
                    range_start_day: chunk[0].trade_day,
                    range_end_day: chunk[chunk.len() - 1].trade_day,
                    kind: StoredDeltaKind::Upsert(StoredClip {
                        segment_key: segment.key,
                        clip: compact_market::SegmentClip {
                            segment_digest: segment.digest,
                            clip_start_day: chunk[0].trade_day,
                            clip_end_day: chunk[chunk.len() - 1].trade_day,
                            row_count: chunk.len() as u32,
                        },
                    }),
                });
            }
            for day in removes {
                let trade_day = compact_market::epoch_day(*day);
                ops.push(StoredDelta {
                    range_start_day: trade_day,
                    range_end_day: trade_day,
                    kind: StoredDeltaKind::Remove,
                });
            }
            ops.sort_by_key(|o| o.range_start_day);
            let draft = VersionDraft {
                version_id: version_id.into(),
                entity: entity.into(),
                period: Period::Day,
                source_binding_digest: base_facts.source_binding_digest,
                coverage_status,
                evidence_digest: evd
                    .try_into()
                    .map_err(|_| integrity("stored evidence digest is malformed"))?,
                created_at: Utc::now().to_rfc3339(),
                yearly_lineage: None,
            };
            // O(ops) staging when the delta is a pure tail append
            // (#259 S5G-8): no parent leaf reconstruction. Anything else
            // (removes, revisions, overlapping upserts) keeps the full
            // projection check.
            let append_only = !bars.is_empty() && removes.is_empty() && {
                let first = bars.first().unwrap().trade_day;
                let parent_last = base_facts.last_day;
                first > parent_last
            };
            if append_only {
                compact_versions::stage_delta_append(c, &draft, base_key, &ops, &bars)
            } else {
                compact_versions::stage_delta(c, &draft, base_key, &ops)
            }
        }
    }

    /// Marks a staged ops-delta version validated (mirrors `mark_validated`
    /// on the envelope path without materializing a candidate). Runs the
    /// O(ops) append-delta check when the staged shape qualifies
    /// (#259 S5G-8), full chain verification otherwise.
    pub(crate) fn validate_staged(&self, version_id: &str) -> Result<(), Error> {
        self.tx(|c| compact_versions::mark_validated_delta(c, version_key(c, version_id)?))
    }

    /// Continuity-evidence JSON shared by the ops-first commit paths
    /// (#259 S5G): carries the probe's `ContinuityEvidence` so a typed
    /// commit records the same proof fields as the plan-based path.
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

    /// Commits a staged ops-delta update with the same binding / required
    /// heads / head CAS semantics as the envelope commit path.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_ops_update(
        &self,
        result_id: &str,
        task_id: &str,
        item_id: &str,
        base_version_id: &str,
        result_version_id: &str,
        expected_revision: u64,
        run_id: &str,
        occupancy_key: &str,
        continuity_evidence: Option<&Value>,
    ) -> Result<Receipt, Error> {
        self.write(|c| {
            Self::commit_ops_update_tx(
                c,
                result_id,
                task_id,
                item_id,
                base_version_id,
                result_version_id,
                expected_revision,
                run_id,
                occupancy_key,
                continuity_evidence,
            )
        })
    }

    /// Transaction-level `commit_ops_update` (#259 S5G-6): the batch
    /// update commit drives the same commit inside one transaction.
    #[allow(clippy::too_many_arguments)]
    fn commit_ops_update_tx(
        c: &Connection,
        result_id: &str,
        task_id: &str,
        item_id: &str,
        base_version_id: &str,
        result_version_id: &str,
        expected_revision: u64,
        run_id: &str,
        occupancy_key: &str,
        continuity_evidence: Option<&Value>,
    ) -> Result<Receipt, Error> {
        {
            // Replay: a committed result may precede a lost response —
            // return its receipt instead of conflicting on re-register.
            if let Some(kind) = registry_kind(c, result_id)? {
                if kind != 1 {
                    return Err(Error::Conflict("result identity conflict".into()));
                }
                let stored = compact_results::read_market_update(c, result_id)?;
                if stored.identity.task_id != task_id
                    || stored.identity.item_id != item_id
                    || stored.identity.result_version_id != result_version_id
                {
                    return Err(Error::Conflict("result identity conflict".into()));
                }
                return Ok(Receipt {
                    publication_id: result_id.into(),
                    version_id: stored.identity.result_version_id.clone(),
                    task_id: task_id.into(),
                    task_revision: stored.identity.task_revision,
                });
            }
            let base_key = version_key(c, base_version_id)?;
            let result_key = version_key(c, result_version_id)?;
            let facts = compact_versions::result_facts(c, result_key)?;
            if facts.state != CompactVersionState::Validated {
                return Err(Error::Conflict("candidate is not validated".into()));
            }
            let (change, range_start, range_end) = match facts.manifest_mode {
                ManifestMode::Delta => {
                    if facts.parent_version_key != Some(base_key) {
                        return Err(Error::Conflict(
                            "update result delta is not based on the expected head".into(),
                        ));
                    }
                    let parent = compact_versions::result_facts(c, base_key)?;
                    let (ops, _, _) =
                        compact_v19::read_delta(c, result_key, parent.projection_digest)?;
                    (
                        MarketUpdateChange::Append {
                            prefix_bytes: base_count(c, base_key)? * 32,
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
            let continuity = Self::ops_continuity_json(
                if matches!(change, MarketUpdateChange::Append { .. }) {
                    "append"
                } else {
                    "full"
                },
                base_version_id,
                &facts.source_binding_digest,
                match &change {
                    MarketUpdateChange::Append { prefix_bytes, .. } => Some(*prefix_bytes),
                    _ => None,
                },
                continuity_evidence,
            );
            let evidence =
                compact_versions::put_evidence(c, EvidenceKind::Continuity, &continuity)?;
            let entity = facts.entity.clone();
            let stored = compact_results::commit_market_update_tx(
                c,
                &MarketUpdateCommit {
                    result_id: result_id.into(),
                    task_id: task_id.into(),
                    item_id: item_id.into(),
                    base_version_key: base_key,
                    result_version_key: result_key,
                    range_start_day: range_start,
                    range_end_day: range_end,
                    algorithm_version: UPDATE_ALGORITHM.into(),
                    source_binding_digest: facts.source_binding_digest,
                    evidence_digest: evidence,
                    expected_task_revision: expected_revision,
                    published_at: Some(Utc::now()),
                    head: HeadExpectation::Version(base_key),
                    required_heads: vec![RequiredHead {
                        entity: entity.clone(),
                        period: Period::Day,
                        expected: Some(base_key),
                    }],
                    binding: BindingCheck {
                        run_id: run_id.into(),
                        entity,
                        period: Period::Day,
                        occupancy_key: occupancy_key.into(),
                    },
                    change,
                },
            )?;
            Ok(Receipt {
                publication_id: result_id.into(),
                version_id: stored.identity.result_version_id.clone(),
                task_id: task_id.into(),
                task_revision: stored.identity.task_revision,
            })
        }
    }

    /// Replay check for the typed commit entry points (#259 S5G): a
    /// committed market-update result under this publication_id returns
    /// its receipt; a foreign result kind or mismatched identity fails
    /// closed, matching `commit_range_tx`'s registry behavior.
    fn typed_replay_receipt(&self, command: &Publication) -> Result<Option<Receipt>, Error> {
        self.read(|c| match registry_kind(c, &command.publication_id)? {
            Some(1) => {
                let stored = compact_results::read_market_update(c, &command.publication_id)?;
                if stored.identity.task_id != command.task_id
                    || stored.identity.item_id != command.item_id
                    || stored.identity.result_version_id != command.version_id
                {
                    return Err(Error::Conflict("result identity conflict".into()));
                }
                Ok(Some(Receipt {
                    publication_id: command.publication_id.clone(),
                    version_id: stored.identity.result_version_id.clone(),
                    task_id: command.task_id.clone(),
                    task_revision: stored.identity.task_revision,
                }))
            }
            Some(_) => Err(Error::Conflict("result identity conflict".into())),
            None => Ok(None),
        })
    }

    /// No-change publication for the ops-first update path (#259 S5G):
    /// the continuity probe proved the member prefix is byte-identical
    /// to the stored base, so the result reuses the base version — no
    /// staging, no new version, the published head stays put.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_ops_no_change(
        &self,
        result_id: &str,
        task_id: &str,
        item_id: &str,
        base_version_id: &str,
        expected_revision: u64,
        run_id: &str,
        occupancy_key: &str,
        range_start_day: i64,
        range_end_day: i64,
        continuity_evidence: Option<&Value>,
    ) -> Result<Receipt, Error> {
        self.write(|c| {
            Self::commit_ops_no_change_tx(
                c,
                result_id,
                task_id,
                item_id,
                base_version_id,
                expected_revision,
                run_id,
                occupancy_key,
                range_start_day,
                range_end_day,
                continuity_evidence,
            )
        })
    }

    /// Transaction-level `commit_ops_no_change` (#259 S5G-6): the update
    /// batch writer drives the same commit inside one transaction.
    #[allow(clippy::too_many_arguments)]
    fn commit_ops_no_change_tx(
        c: &Connection,
        result_id: &str,
        task_id: &str,
        item_id: &str,
        base_version_id: &str,
        expected_revision: u64,
        run_id: &str,
        occupancy_key: &str,
        range_start_day: i64,
        range_end_day: i64,
        continuity_evidence: Option<&Value>,
    ) -> Result<Receipt, Error> {
        {
            if let Some(kind) = registry_kind(c, result_id)? {
                if kind != 1 {
                    return Err(Error::Conflict("result identity conflict".into()));
                }
                let stored = compact_results::read_market_update(c, result_id)?;
                if stored.identity.task_id != task_id
                    || stored.identity.item_id != item_id
                    || stored.identity.result_version_id != base_version_id
                {
                    return Err(Error::Conflict("result identity conflict".into()));
                }
                return Ok(Receipt {
                    publication_id: result_id.into(),
                    version_id: stored.identity.result_version_id.clone(),
                    task_id: task_id.into(),
                    task_revision: stored.identity.task_revision,
                });
            }
            let base_key = version_key(c, base_version_id)?;
            let facts = compact_versions::result_facts(c, base_key)?;
            if facts.state != CompactVersionState::Published {
                return Err(integrity("no-change base is not published"));
            }
            let prefix_bytes = facts.record_count * 32;
            let continuity = Self::ops_continuity_json(
                "unchanged",
                base_version_id,
                &facts.source_binding_digest,
                Some(prefix_bytes as u64),
                continuity_evidence,
            );
            let evidence =
                compact_versions::put_evidence(c, EvidenceKind::Continuity, &continuity)?;
            let stored = compact_results::commit_market_update_tx(
                c,
                &MarketUpdateCommit {
                    result_id: result_id.into(),
                    task_id: task_id.into(),
                    item_id: item_id.into(),
                    base_version_key: base_key,
                    result_version_key: base_key,
                    range_start_day,
                    range_end_day,
                    algorithm_version: UPDATE_ALGORITHM.into(),
                    source_binding_digest: facts.source_binding_digest,
                    evidence_digest: evidence,
                    expected_task_revision: expected_revision,
                    published_at: None,
                    head: HeadExpectation::Keep,
                    required_heads: vec![RequiredHead {
                        entity: facts.entity.clone(),
                        period: Period::Day,
                        expected: Some(base_key),
                    }],
                    binding: BindingCheck {
                        run_id: run_id.into(),
                        entity: facts.entity.clone(),
                        period: Period::Day,
                        occupancy_key: occupancy_key.into(),
                    },
                    change: MarketUpdateChange::NoChange {
                        prefix_bytes: prefix_bytes as u64,
                    },
                },
            )?;
            Ok(Receipt {
                publication_id: result_id.into(),
                version_id: stored.identity.result_version_id.clone(),
                task_id: task_id.into(),
                task_revision: stored.identity.task_revision,
            })
        }
    }

    /// Typed base facts for the ops-first update path (#259 S5G): version
    /// row + source binding + version evidence — no bar materialization.
    /// The caller gets exactly what `probe`/`prepare` read off the legacy
    /// `CheckedRecord`: bindings, coverage, units, integrity, bounds.
    pub(crate) fn update_base_facts(
        &self,
        base_version_id: &str,
    ) -> Result<crate::market_update::UpdateBaseFacts, Error> {
        self.read(|c| {
            let key = version_key(c, base_version_id)?;
            let facts = compact_versions::result_facts(c, key)?;
            if facts.state != CompactVersionState::Published {
                return Err(integrity("ops base version is not published"));
            }
            if facts.period != Period::Day {
                return Err(integrity("ops base is not a daily version"));
            }
            let binding = compact_versions::read_source_binding(c, &facts.source_binding_digest)?;
            let (kind, evidence_json) = compact_versions::read_evidence(c, &facts.evidence_digest)?;
            if kind != EvidenceKind::Version {
                return Err(integrity("base version evidence kind mismatch"));
            }
            let evidence: compact_market::VersionEvidence = serde_json::from_str(&evidence_json)
                .map_err(|e| integrity(format!("version evidence malformed: {e}")))?;
            let integrity_digest = facts
                .integrity_digest
                .ok_or_else(|| integrity("published base lacks integrity digest"))?;
            let coverage_status = match facts.coverage_status {
                1 => CoverageStatus::Complete,
                2 => CoverageStatus::Incomplete,
                _ => CoverageStatus::Unknown,
            };
            Ok(crate::market_update::UpdateBaseFacts {
                version_id: facts.version_id,
                symbol: facts.entity,
                record_count: u32::try_from(facts.record_count)
                    .map_err(|_| integrity("base record_count out of range"))?,
                first_day: compact_market::epoch_date(facts.first_day).map_err(integrity)?,
                last_day: compact_market::epoch_date(facts.last_day).map_err(integrity)?,
                integrity_sha256: hex_bytes(&integrity_digest),
                coverage: crate::market_data::CoverageEvidence {
                    status: coverage_status,
                    reason: evidence.coverage_reason,
                },
                unit_currency: evidence.unit_currency,
                source: evidence.source,
                generation_id: binding.generation_id,
                generation_manifest_sha256: hex_bytes(&binding.generation_manifest_sha256),
                day_member_sha256: hex_bytes(&binding.day_member_sha256),
                security_event_sha256: hex_bytes(&binding.security_event_sha256),
            })
        })
    }

    /// Windowed base bars for the ops-diff path (#259 S5G).
    pub(crate) fn base_bars_in_range(
        &self,
        base_version_id: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Bar>, Error> {
        self.read(|c| {
            let key = version_key(c, base_version_id)?;
            let bars = compact_versions::leaf_bars_in_range(
                c,
                key,
                compact_market::epoch_day(start),
                compact_market::epoch_day(end),
            )?;
            bars.iter()
                .map(|bar| bar.to_bar(Period::Day).map_err(integrity))
                .collect()
        })
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

pub(crate) fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn period_code(period: Period) -> i64 {
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

pub(crate) fn version_key(c: &Connection, version_id: &str) -> Result<i64, Error> {
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
    // Published records keep the typed columns authoritative: the stored
    // integrity digest already binds the full content, and no reader of a
    // published envelope consumes `records`/`batch.bars` (only digests,
    // coverage, lineage and `compact_source`). Serializing thousands of
    // bars into the envelope per read is pure waste — omit them. Staged
    // and validated candidates keep the complete envelope because
    // `compat::check` and replay equality need the self-contained form.
    let mut raw = if m.state == CompactVersionState::Published {
        let mut slim_version = version.clone();
        slim_version.records = Vec::new();
        let mut raw = serde_json::to_value(&slim_version).map_err(super::integrity)?;
        let mut slim_batch = batch.clone();
        slim_batch.bars = Vec::new();
        raw["batch"] = serde_json::to_value(&slim_batch).map_err(super::integrity)?;
        raw
    } else {
        let mut raw = serde_json::to_value(version).map_err(super::integrity)?;
        raw["batch"] = serde_json::to_value(&batch).map_err(super::integrity)?;
        raw
    };
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
    // Yearly derivation metadata the typed store keeps outside the slim
    // envelope: publication equivalence (`results::equivalent`) compares
    // the stored as_of at year granularity, and readers re-aggregate
    // per-record lineage from it instead of storing the derived block.
    if m.state == CompactVersionState::Published
        && version.period == Period::Year
        && let Some(as_of_day) = m.yearly_as_of_day
    {
        raw["compact_yearly_lineage_v1"] = json!({
            "as_of": compact_market::epoch_date(as_of_day).map_err(super::integrity)?.to_string(),
        });
    }
    // Published versions report the stored typed integrity digest — the
    // authoritative content binding verified at commit — so every
    // frozen/derived digest comparison reads the same value cheap reads
    // expose. Staged/validated candidates keep the legacy envelope digest:
    // `compat::check` requires the staged envelope to be self-consistent.
    raw["integrity_sha256"] = match m.state {
        CompactVersionState::Published => json!(hex_bytes(
            &m.integrity_digest
                .ok_or_else(|| integrity("published version lacks integrity digest"))?
        )),
        _ => json!(compat::legacy_digest(&raw)?),
    };
    Ok(CheckedRecord {
        raw,
        version: m.version.clone(),
        batch,
    })
}

pub(crate) fn materialize_checked(
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
    // Full-candidate clips are only materialized for checkpoint staging;
    // content-addressed segments let unchanged chunks reuse existing rows.
    let full_clips = |c: &mut Connection| -> Result<Vec<StoredClip>, Error> {
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
        Ok(clips)
    };
    // A yearly record carries no generation source of its own: it derives
    // entirely from a published daily base, so it inherits that base's
    // source binding. Daily records must carry `compact_source`.
    let (yearly_lineage, binding_digest) = match period {
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
            (
                Some(YearlyLineageDraft {
                    base_daily_version_key: base_key,
                    algorithm_version: derivation.algorithm_version.clone(),
                    first_trade_day: compact_market::epoch_day(derivation.first_trading_date),
                    last_trade_day: compact_market::epoch_day(derivation.last_trading_date),
                    as_of_day: base.last_day,
                }),
                base.source_binding_digest,
            )
        }
        Period::Day => {
            if record.version.yearly_derivation.is_some() {
                return Err(invalid("daily record carries yearly derivation"));
            }
            let source: CompactSource = serde_json::from_value(
                raw.get("compact_source")
                    .cloned()
                    .ok_or_else(|| invalid("compact_source required for compact staging"))?,
            )
            .map_err(invalid)?;
            let evidence = compact_versions::put_evidence(
                c,
                EvidenceKind::Generation,
                &source.generation_evidence,
            )?;
            let binding = SourceBinding {
                generation_id: source.generation_id.clone(),
                generation_manifest_sha256: hex_digest(&source.generation_manifest_sha256)?,
                day_member_sha256: hex_digest(&source.day_member_sha256)?,
                security_event_sha256: hex_digest(&source.security_event_sha256)?,
                evidence_digest: evidence,
            };
            (None, compact_versions::put_source_binding(c, &binding)?)
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
        match plan_delta(&parent_bars, &bars)? {
            DeltaPlan::Equal => {
                return Err(Error::Conflict(
                    "candidate identical to declared parent; no-change must not stage".into(),
                ));
            }
            DeltaPlan::Upsert(changed) => {
                // Delta segments carry ONLY the changed bars; a daily append
                // therefore stores ~1 bar row per security, never a copied
                // tail chunk. This is what bounds per-day DB growth.
                let mut ops = Vec::new();
                for chunk in changed.chunks(SEGMENT_ROWS) {
                    let segment = compact_v19::insert_segment(c, &entity, period, chunk)?;
                    ops.push(StoredDelta {
                        range_start_day: chunk.first().unwrap().trade_day,
                        range_end_day: chunk.last().unwrap().trade_day,
                        kind: StoredDeltaKind::Upsert(StoredClip {
                            segment_key: segment.key,
                            clip: crate::compact_market::SegmentClip {
                                segment_digest: segment.digest,
                                clip_start_day: chunk.first().unwrap().trade_day,
                                clip_end_day: chunk.last().unwrap().trade_day,
                                row_count: chunk.len() as u32,
                            },
                        }),
                    });
                }
                if parent.delta_depth < 256 {
                    compact_versions::stage_delta(c, &draft, parent_key, &ops)?;
                } else {
                    let clips = full_clips(c)?;
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
                let clips = full_clips(c)?;
                compact_versions::stage_checkpoint(c, &draft, &clips, None)?;
            }
        }
    } else {
        let clips = full_clips(c)?;
        compact_versions::stage_checkpoint(c, &draft, &clips, None)?;
    }
    Ok(())
}

enum DeltaPlan {
    Equal,
    /// Only the changed bars (inserts + value changes), ordered by day.
    Upsert(Vec<CompactBar>),
    Full,
}

/// Delta planning is mechanical: the candidate must retain every parent day
/// (deletions force a checkpoint/full revision); each candidate bar absent
/// from or differing from the parent becomes an upsert row.
fn plan_delta(parent: &[CompactBar], candidate: &[CompactBar]) -> Result<DeltaPlan, Error> {
    use std::collections::{BTreeMap, BTreeSet};
    let candidate_days: BTreeSet<i64> = candidate.iter().map(|b| b.trade_day).collect();
    if parent
        .iter()
        .any(|b| !candidate_days.contains(&b.trade_day))
    {
        return Ok(DeltaPlan::Full);
    }
    let parent_by_day: BTreeMap<i64, &CompactBar> =
        parent.iter().map(|b| (b.trade_day, b)).collect();
    let changed: Vec<CompactBar> = candidate
        .iter()
        .filter(|b| parent_by_day.get(&b.trade_day) != Some(b))
        .cloned()
        .collect();
    Ok(if changed.is_empty() {
        DeltaPlan::Equal
    } else {
        DeltaPlan::Upsert(changed)
    })
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
    // Compact storage keeps only typed yearly facts; the derived
    // `local_yearly_derivation_v1` envelope projection is not reproducible,
    // so yearly candidates compare the facts the commit actually publishes.
    let same = match plan.record.version.period {
        Period::Year => {
            stored.version.version_id == plan.record.version.version_id
                && stored.version.symbol == plan.record.version.symbol
                && stored.version.records == plan.record.version.records
                && stored.version.yearly_derivation == plan.record.version.yearly_derivation
                && stored.version.source == plan.record.version.source
                && stored.version.unit_currency == plan.record.version.unit_currency
                && stored.batch.coverage == plan.record.batch.coverage
        }
        Period::Day => stored.raw == plan.record.raw,
    };
    if !same {
        return Err(integrity("validated candidate changed since plan"));
    }
    if let Some(digest) = &plan.base_digest {
        let lineage = plan
            .record
            .version
            .yearly_derivation
            .as_ref()
            .ok_or_else(|| integrity("base digest without yearly lineage"))?;
        // Typed facts only: the stored integrity digest is the authoritative
        // binding — no need to materialize thousands of base bars to compare
        // it.
        let base_key = version_key(c, &lineage.base_daily_version_id)?;
        let base = compact_versions::result_facts(c, base_key)?;
        if base.state != CompactVersionState::Published
            || base
                .integrity_digest
                .map(|d| compact_market::digest_hex(&d) == *digest)
                != Some(true)
        {
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
            // Published versions take the windowed leaf read — the range
            // bounds already scope which segments must be decoded. Validated
            // (unpublished) versions keep full staged materialization.
            let m = match compact_versions::materialize_windowed(
                c,
                &id,
                compact_market::epoch_day(range.start),
                compact_market::epoch_day(range.end),
            ) {
                Ok(m) => m,
                Err(Error::NotFound) => compact_versions::materialize(c, &id)?,
                Err(e) => return Err(e),
            };
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
    fn fixed_facts(&self, id: &str) -> Result<crate::market_storage::FixedFacts, Error> {
        self.read(|c| {
            let key = version_key(c, id)?;
            let facts = compact_versions::result_facts(c, key)?;
            if facts.state != CompactVersionState::Published {
                return Err(Error::NotFound);
            }
            let integrity_digest = facts
                .integrity_digest
                .ok_or_else(|| integrity("published version lacks integrity digest"))?;
            Ok(crate::market_storage::FixedFacts {
                version_id: facts.version_id,
                symbol: facts.entity,
                period: facts.period,
                integrity_sha256: hex_bytes(&integrity_digest),
                first_day: compact_market::epoch_date(facts.first_day)
                    .map_err(|e| integrity(e.to_string()))?,
                last_day: compact_market::epoch_date(facts.last_day)
                    .map_err(|e| integrity(e.to_string()))?,
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
            let m = compact_versions::materialize_windowed(
                c,
                id,
                compact_market::epoch_day(first_day),
                compact_market::epoch_day(last_day),
            )?;
            // `synthesize` reports the stored integrity digest for published
            // versions — the complete version's authoritative binding even
            // though `records` holds only the window.
            synthesize(&m)
        })
    }
    fn fixed_window_years(
        &self,
        id: &str,
        first_day: NaiveDate,
        last_day: NaiveDate,
    ) -> Result<BTreeSet<i32>, Error> {
        self.read(|c| {
            compact_versions::window_years(
                c,
                id,
                compact_market::epoch_day(first_day),
                compact_market::epoch_day(last_day),
            )
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
    /// Ops-first daily commit: stage the candidate as a parent-delta
    /// version, bind it to the approved merge by leaf equality (the content
    /// binding replacing the envelope `combined_integrity_sha256` digest
    /// check — the verifier still binds base + segment digests), then
    /// commit with the same required-heads/binding/head-CAS semantics.
    fn commit_range_ops(
        &self,
        command: &Publication,
        scope: &Scope,
        base_version_id: &str,
        upserts: &[Bar],
        removes: &[NaiveDate],
        expected: &[Bar],
    ) -> Result<Option<Receipt>, Error> {
        if command.intent
            != (PublishIntent::Daily {
                expected_head: Some(base_version_id.into()),
            })
        {
            return Err(Error::Invalid(
                "ops commit must bind the expected daily head".into(),
            ));
        }
        let version_id = command.version_id.clone();
        // Replay-tolerant stage: an already-staged version is re-verified by
        // the leaf equality check below instead of failing on re-stage.
        let exists = self.tx(|c| {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM data_versions WHERE version_id=?1)",
                [&version_id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(storage)
        })?;
        if !exists {
            self.stage_ops_delta(
                &scope.symbol,
                base_version_id,
                &version_id,
                upserts,
                removes,
            )?;
        }
        self.tx(|c| {
            let key = version_key(c, &version_id)?;
            let facts = compact_versions::result_facts(c, key)?;
            if facts.parent_version_key != Some(version_key(c, base_version_id)?)
                || facts.state == CompactVersionState::Published
            {
                return Err(Error::Conflict(
                    "staged ops version is not a delta on the expected base".into(),
                ));
            }
            let leaf = compact_versions::leaf_bars(c, key)?;
            let approved = expected
                .iter()
                .map(|b| CompactBar::from_bar(b, Period::Day).map_err(integrity))
                .collect::<Result<Vec<_>, _>>()?;
            if leaf != approved {
                return Err(Error::Integrity(
                    "staged ops projection differs from approved candidate".into(),
                ));
            }
            Ok(())
        })?;
        self.validate_staged(&version_id)?;
        self.commit_ops_update(
            &command.publication_id,
            &command.task_id,
            &command.item_id,
            base_version_id,
            &version_id,
            command.expected_revision,
            &command.run_id,
            &scope.occupancy_key(),
            None,
        )
        .map(Some)
    }
    fn update_base_facts(
        &self,
        base_version_id: &str,
    ) -> Result<Option<crate::market_update::UpdateBaseFacts>, Error> {
        // A base published before snapshot bindings existed cannot drive
        // the continuity probe — the caller falls back to the envelope
        // path, whose `Unverified` continuity still covers it.
        let bound = self.read(|c| {
            let key = version_key(c, base_version_id)?;
            let facts = compact_versions::result_facts(c, key)?;
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM source_bindings WHERE source_binding_digest=?1)",
                params![facts.source_binding_digest.as_slice()],
                |r| r.get::<_, bool>(0),
            )
            .map_err(storage)
        })?;
        if !bound {
            return Ok(None);
        }
        SqliteCompactMarket::update_base_facts(self, base_version_id).map(Some)
    }
    fn base_bars_in_range(
        &self,
        base_version_id: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Option<Vec<Bar>>, Error> {
        SqliteCompactMarket::base_bars_in_range(self, base_version_id, start, end).map(Some)
    }
    /// Typed ops commit (#259 S5G): identical staging/CAS semantics to
    /// `commit_range_ops`, but the content binding is O(ops) — the
    /// staged delta clips are read back and compared to the requested
    /// upserts/removes under the same normalization `stage_ops_delta`
    /// applies — instead of reconstructing the whole projected leaf.
    fn commit_range_ops_typed(
        &self,
        command: &Publication,
        scope: &Scope,
        base_version_id: &str,
        upserts: &[Bar],
        removes: &[NaiveDate],
        continuity_evidence: Option<&Value>,
    ) -> Result<Option<Receipt>, Error> {
        if command.intent
            != (PublishIntent::Daily {
                expected_head: Some(base_version_id.into()),
            })
        {
            return Err(Error::Invalid(
                "ops commit must bind the expected daily head".into(),
            ));
        }
        if let Some(receipt) = self.typed_replay_receipt(command)? {
            return Ok(Some(receipt));
        }
        let version_id = command.version_id.clone();
        let exists = self.tx(|c| {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM data_versions WHERE version_id=?1)",
                [&version_id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(storage)
        })?;
        if !exists {
            self.stage_ops_delta(
                &scope.symbol,
                base_version_id,
                &version_id,
                upserts,
                removes,
            )?;
        }
        self.tx(|c| {
            let key = version_key(c, &version_id)?;
            let facts = compact_versions::result_facts(c, key)?;
            if facts.parent_version_key != Some(version_key(c, base_version_id)?)
                || facts.state == CompactVersionState::Published
            {
                return Err(Error::Conflict(
                    "staged ops version is not a delta on the expected base".into(),
                ));
            }
            let base = compact_versions::result_facts(
                c,
                facts
                    .parent_version_key
                    .expect("delta parent checked above"),
            )?;
            let (staged, _, _) = compact_v19::read_delta(c, key, base.projection_digest)?;
            let mut bars: Vec<CompactBar> = upserts
                .iter()
                .map(|b| CompactBar::from_bar(b, Period::Day).map_err(integrity))
                .collect::<Result<_, _>>()?;
            bars.sort_by_key(|b| b.trade_day);
            if bars.windows(2).any(|w| w[0].trade_day >= w[1].trade_day) {
                return Err(integrity("typed ops upserts are not strictly increasing"));
            }
            let mut expected: Vec<(i64, i64, Option<compact_market::SegmentClip>)> = Vec::new();
            for chunk in bars.chunks(SEGMENT_ROWS) {
                expected.push((
                    chunk[0].trade_day,
                    chunk[chunk.len() - 1].trade_day,
                    Some(compact_market::SegmentClip {
                        segment_digest: compact_market::segment_digest(
                            &scope.symbol,
                            Period::Day,
                            chunk,
                        )
                        .map_err(integrity)?,
                        clip_start_day: chunk[0].trade_day,
                        clip_end_day: chunk[chunk.len() - 1].trade_day,
                        row_count: chunk.len() as u32,
                    }),
                ));
            }
            for day in removes {
                let trade_day = compact_market::epoch_day(*day);
                expected.push((trade_day, trade_day, None));
            }
            expected.sort_by_key(|e| e.0);
            let actual: Vec<(i64, i64, Option<compact_market::SegmentClip>)> = staged
                .iter()
                .map(|op| match &op.kind {
                    StoredDeltaKind::Upsert(clip) => (
                        op.range_start_day,
                        op.range_end_day,
                        Some(clip.clip.clone()),
                    ),
                    StoredDeltaKind::Remove => (op.range_start_day, op.range_end_day, None),
                })
                .collect();
            if actual != expected {
                return Err(Error::Integrity(
                    "staged ops differ from the requested delta".into(),
                ));
            }
            Ok(())
        })?;
        self.validate_staged(&version_id)?;
        self.commit_ops_update(
            &command.publication_id,
            &command.task_id,
            &command.item_id,
            base_version_id,
            &version_id,
            command.expected_revision,
            &command.run_id,
            &scope.occupancy_key(),
            continuity_evidence,
        )
        .map(Some)
    }
    fn commit_range_no_change_typed(
        &self,
        command: &Publication,
        base_version_id: &str,
        range: &crate::market_update::DateRange,
        continuity_evidence: Option<&Value>,
    ) -> Result<Option<Receipt>, Error> {
        if command.intent
            != (PublishIntent::Daily {
                expected_head: Some(base_version_id.into()),
            })
        {
            return Err(Error::Invalid(
                "no-change commit must bind the expected daily head".into(),
            ));
        }
        if command.version_id != base_version_id {
            return Err(Error::Invalid(
                "no-change result must reuse the base version".into(),
            ));
        }
        let scope_symbol = self.read(|c| {
            let key = version_key(c, base_version_id)?;
            Ok::<_, Error>(compact_versions::result_facts(c, key)?.entity)
        })?;
        self.commit_ops_no_change(
            &command.publication_id,
            &command.task_id,
            &command.item_id,
            base_version_id,
            command.expected_revision,
            &command.run_id,
            &crate::market_storage::Scope {
                symbol: scope_symbol,
                period: Period::Day,
            }
            .occupancy_key(),
            compact_market::epoch_day(range.start),
            compact_market::epoch_day(range.end),
            continuity_evidence,
        )
        .map(Some)
    }
    fn commit_update_batch(
        &self,
        task_id: &str,
        items: &[crate::market_update::UpdateBatchItem],
    ) -> Result<Option<Vec<Result<Receipt, Error>>>, Error> {
        SqliteCompactMarket::commit_update_batch(self, task_id, items).map(Some)
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

fn publication_receipt(stored: &compact_results::StoredPublicationResult) -> Receipt {
    Receipt {
        publication_id: stored.identity.publication_id.clone(),
        version_id: stored.identity.result_version_id.clone(),
        task_id: stored.identity.task_id.clone(),
        task_revision: stored.identity.task_revision,
    }
}

fn head_version_id(c: &Connection, entity: &str, period: Period) -> Result<Option<String>, Error> {
    c.query_row(
        "SELECT v.version_id FROM effective_heads h \
         JOIN data_versions v ON v.version_key=h.version_key \
         WHERE h.entity=?1 AND h.period=?2 AND h.adjustment=?3",
        params![entity, period_code(period), 1],
        |r| r.get(0),
    )
    .optional()
    .map_err(storage)
}

impl crate::official_market::DailyCollectionRepository for SqliteCompactMarket {
    fn collection_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| {
            let id: Option<String> = c
                .query_row(
                    "SELECT publication_id FROM publication_results \
                     WHERE task_id=?1 AND item_id=?2 AND publication_kind=1",
                    params![task, item],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            id.map(|id| compact_results::read_publication(c, &id).map(|s| publication_receipt(&s)))
                .transpose()
        })
    }
    fn effective_head(&self, scope: &Scope) -> Result<Option<String>, Error> {
        self.read(|c| head_version_id(c, &scope.symbol, scope.period))
    }
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<crate::task_control::TaskSnapshot, Error> {
        use crate::task_control::TaskRepository;
        Ok(self.transition(task, revision, run, change)?)
    }
    fn commit_collection_batch(
        &self,
        task_id: &str,
        items: &[CollectionCandidate],
    ) -> Result<Option<Vec<Result<Receipt, Error>>>, Error> {
        SqliteCompactMarket::commit_collection_batch(self, task_id, items).map(Some)
    }
}

impl crate::yearly_publication::results::YearlyRepository for SqliteCompactMarket {
    fn transition_task(
        &self,
        task: &str,
        revision: u64,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<crate::task_control::TaskSnapshot, Error> {
        use crate::task_control::TaskRepository;
        Ok(self.transition(task, revision, run, change)?)
    }
    fn commit_yearly(
        &self,
        plan: &crate::yearly_publication::results::YearlyResultPlan,
    ) -> Result<Receipt, Error> {
        self.write(|c| {
            if plan.no_change {
                yearly_no_change_tx(c, plan)
            } else {
                commit_tx(c, &plan.publication)
            }
        })
    }
    fn yearly_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| {
            let kind: Option<i64> = c
                .query_row(
                    "SELECT publication_kind FROM publication_results WHERE publication_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            match kind {
                Some(2) => {
                    compact_results::read_publication(c, id).map(|s| Some(publication_receipt(&s)))
                }
                Some(_) => Ok(None),
                None => Ok(None),
            }
        })
    }
    fn yearly_item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        self.read(|c| {
            let id: Option<String> = c
                .query_row(
                    "SELECT publication_id FROM publication_results \
                     WHERE task_id=?1 AND item_id=?2 AND publication_kind=2",
                    params![task, item],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            id.map(|id| compact_results::read_publication(c, &id).map(|s| publication_receipt(&s)))
                .transpose()
        })
    }
}

/// Compact-path yearly no-change commit: the reused yearly version is
/// already published, so the commit writes only a typed
/// `publication_results` (kind=yearly) receipt with head=Keep — no new
/// version, segment, or head movement.
fn yearly_no_change_tx(
    c: &mut Connection,
    plan: &crate::yearly_publication::results::YearlyResultPlan,
) -> Result<Receipt, Error> {
    let command = plan.command();
    if let Some(kind) = registry_kind(c, &command.publication_id)? {
        return replay_receipt(c, kind, command);
    }
    let result_key = version_key(c, &command.version_id)?;
    let facts = compact_versions::result_facts(c, result_key)?;
    if facts.period != Period::Year {
        return Err(integrity("yearly no-change result is not a yearly version"));
    }
    let lineage = plan
        .publication
        .record
        .version
        .yearly_derivation
        .as_ref()
        .ok_or_else(|| integrity("yearly no-change result lacks lineage"))?;
    let evidence = compact_versions::put_evidence(
        c,
        EvidenceKind::Yearly,
        &json!({
            "base_daily_version_id": lineage.base_daily_version_id,
            "algorithm_version": lineage.algorithm_version,
            "mode": "no_change",
        }),
    )?;
    let stored = compact_results::commit_publication(
        c,
        &PublicationCommit {
            publication_id: command.publication_id.clone(),
            task_id: command.task_id.clone(),
            item_id: command.item_id.clone(),
            publication_kind: PublicationKind::Yearly,
            base_version_key: Some(result_key),
            result_version_key: result_key,
            outcome: UpdateOutcome::NoChange,
            algorithm_version: lineage.algorithm_version.clone(),
            source_binding_digest: facts.source_binding_digest,
            evidence_digest: evidence,
            expected_task_revision: command.expected_revision,
            published_at: None,
            head: HeadExpectation::Keep,
            required_heads: required_heads(c, &plan.publication.required_heads)?,
            binding: binding_check(&plan.publication),
        },
    )?;
    Ok(publication_receipt(&stored))
}

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
        use crate::task_control::TaskRepository;
        Ok(self.transition(task, revision, run, change)?)
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod capacity;

/// One batch entry validated by `collection_candidate_check`: the encoded
/// bars plus the evidence/binding digests are produced in the same order
/// `stage_tx` derives them, so every stored row is byte-identical.
struct TypedCollection {
    bars: Vec<CompactBar>,
    source_binding: SourceBinding,
    version_evidence: VersionEvidence,
}

impl SqliteCompactMarket {
    /// Bulk first-collection commit (#259 S5F): one WriteCoordinator
    /// transaction covers every item of a single collection task — typed
    /// stage (no legacy envelope), validate, publish, typed result,
    /// `task_publications`, head CAS and item finish — sharing the batch's
    /// fsync. Content-level candidate failures finish the item as failed
    /// without aborting the batch; structural/transaction errors roll the
    /// whole batch back and the caller retries per item to isolate the bad
    /// item.
    pub fn commit_collection_batch(
        &self,
        task_id: &str,
        items: &[CollectionCandidate],
    ) -> Result<Vec<Result<Receipt, Error>>, Error> {
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let mut snapshot = crate::sqlite_tasks::read_bound_task(&tx, task_id)?
                .ok_or(Error::Task(task_control::Error::TaskNotFound))?;
            // Claim inside the batch transaction mirrors run_item's
            // Pending -> Running transition: the task's first worker turn
            // can commit the whole batch atomically.
            if snapshot.task.status == crate::task_control::Status::Pending {
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
            Ok(out)
        })
    }

    /// Batched update commit (#259 S5G-6): one write transaction covers
    /// the whole prepared item set — claim, per-item start, delta
    /// staging, publication commit and item finish — replacing the
    /// per-item stage+validate+commit transaction chain. Structural
    /// errors roll the batch back; the caller re-runs items individually.
    pub fn commit_update_batch(
        &self,
        task_id: &str,
        items: &[crate::market_update::UpdateBatchItem],
    ) -> Result<Vec<Result<Receipt, Error>>, Error> {
        self.write(|c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let mut snapshot = crate::sqlite_tasks::read_bound_task(&tx, task_id)?
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
            Ok(out)
        })
    }
}

/// Per-item batch step for `commit_update_batch`: replay → start the
/// item → stage+commit inside a savepoint → map a failed commit to the
/// same per-item outcome `run_item` records (verification marker for
/// outcome-unknown classes, a failed finish otherwise). Structural
/// errors — the transitions themselves — abort the whole batch.
fn update_batch_item(
    c: &Connection,
    snapshot: &mut TaskSnapshot,
    item: &crate::market_update::UpdateBatchItem,
) -> Result<Result<Receipt, Error>, Error> {
    if let Some(kind) = registry_kind(c, &item.publication_id)? {
        let stored = compact_results::read_market_update(c, &item.publication_id)?;
        if kind != compact_results::REGISTRY_MARKET_UPDATE {
            return Err(Error::Conflict("update result id kind mismatch".into()));
        }
        let expected_version = match &item.prepared {
            crate::market_update::UpdateBatchPrepared::NoChange {
                base_version_id, ..
            } => base_version_id,
            crate::market_update::UpdateBatchPrepared::Append {
                result_version_id, ..
            } => result_version_id,
        };
        if stored.identity.task_id != item.task_id
            || stored.identity.item_id != item.item_id
            || stored.identity.result_version_id != *expected_version
        {
            return Err(Error::Conflict(
                "update result id replays a different commit".into(),
            ));
        }
        return Ok(Ok(Receipt {
            publication_id: item.publication_id.clone(),
            version_id: stored.identity.result_version_id.clone(),
            task_id: item.task_id.clone(),
            task_revision: stored.identity.task_revision,
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
        // The savepoint confines one item's staged writes: a content or
        // commit failure rolls back only this item, never its siblings.
        c.execute_batch("SAVEPOINT update_batch_item")
            .map_err(storage)?;
        let outcome = match &item.prepared {
            crate::market_update::UpdateBatchPrepared::NoChange {
                base_version_id,
                range_start_day,
                range_end_day,
                continuity,
            } => SqliteCompactMarket::commit_ops_no_change_tx(
                c,
                &item.publication_id,
                &item.task_id,
                &item.item_id,
                base_version_id,
                expected_revision,
                &item.run_id,
                &item.occupancy_key,
                *range_start_day,
                *range_end_day,
                Some(continuity),
            ),
            crate::market_update::UpdateBatchPrepared::Append {
                base_version_id,
                result_version_id,
                upserts,
                continuity,
            } => SqliteCompactMarket::stage_ops_delta_tx(
                c,
                &item.symbol,
                base_version_id,
                result_version_id,
                upserts,
                &[],
            )
            .and_then(|_| {
                compact_versions::mark_validated_delta(c, version_key(c, result_version_id)?)?;
                SqliteCompactMarket::commit_ops_update_tx(
                    c,
                    &item.publication_id,
                    &item.task_id,
                    &item.item_id,
                    base_version_id,
                    result_version_id,
                    expected_revision,
                    &item.run_id,
                    &item.occupancy_key,
                    Some(continuity),
                )
            }),
        };
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

/// Per-item batch step: replay → start the item → typed validate+stage →
/// finish failed items inside the batch → commit. Structural errors abort
/// the whole batch transaction.
fn collection_batch_item(
    c: &Connection,
    snapshot: &mut TaskSnapshot,
    item: &CollectionCandidate,
) -> Result<Result<Receipt, Error>, Error> {
    if let Some(kind) = registry_kind(c, &item.publication_id)? {
        let stored = compact_results::read_publication(c, &item.publication_id)?;
        if kind != compact_results::REGISTRY_PUBLICATION {
            return Err(Error::Conflict("collection result id kind mismatch".into()));
        }
        let receipt = publication_receipt(&stored);
        if receipt.task_id != item.task_id || receipt.version_id != item.version_id {
            return Err(Error::Conflict(
                "collection result id replays a different commit".into(),
            ));
        }
        return Ok(Ok(receipt));
    }
    crate::sqlite_tasks::transition_light_tx(
        c,
        snapshot,
        &item.run_id,
        Change::StartItem(item.item_id.clone()),
    )
    .map_err(Error::Task)?;
    let typed = match collection_candidate_check(c, item) {
        Ok(typed) => typed,
        Err(error) => {
            // Content-level candidate failure: record the item outcome
            // inside the batch instead of aborting it.
            crate::sqlite_tasks::transition_light_tx(
                c,
                snapshot,
                &item.run_id,
                Change::FinishItem(
                    item.item_id.clone(),
                    crate::task_control::ItemOutcome {
                        success: false,
                        error: Some(error.to_string()),
                        evidence: None,
                    },
                ),
            )
            .map_err(Error::Task)?;
            return Ok(Err(error));
        }
    };
    let receipt = collection_item_commit_tx(c, item, &typed)?;
    Ok(Ok(receipt))
}

/// Pure-ish candidate validation + typed staging inputs: bar encoding,
/// ordering, unit evidence and source binding digests — the same checks
/// `compat::check` + `check_bars_units` enforce on the envelope path, in
/// one pass over the bars.
fn collection_candidate_check(
    c: &Connection,
    item: &CollectionCandidate,
) -> Result<TypedCollection, Error> {
    compact_market::canonical_entity(&item.symbol).map_err(super::integrity)?;
    let source_binding = SourceBinding {
        generation_id: item.generation_id.clone(),
        generation_manifest_sha256: hex_digest(&item.generation_manifest_sha256)?,
        day_member_sha256: hex_digest(&item.day_member_sha256)?,
        security_event_sha256: hex_digest(&item.security_event_sha256)?,
        evidence_digest: compact_versions::put_evidence(
            c,
            EvidenceKind::Generation,
            &item.generation_evidence,
        )?,
    };
    let version_evidence = VersionEvidence {
        source: item.source.clone(),
        coverage_reason: item.coverage.reason.clone(),
        unit_currency: item.unit_currency.clone(),
        adjustment_evidence: item.adjustment_evidence.clone(),
    };
    let mut bars = Vec::with_capacity(item.bars.len());
    for bar in &item.bars {
        bars.push(CompactBar::from_bar(bar, Period::Day).map_err(super::integrity)?);
    }
    if bars.is_empty() {
        return Err(integrity("collection candidate carries no bars"));
    }
    if bars
        .windows(2)
        .any(|pair| pair[0].trade_day >= pair[1].trade_day)
    {
        return Err(integrity("candidate bars are not strictly increasing"));
    }
    // `check_bars_units` semantics on the single encode pass.
    let unit = &version_evidence.unit_currency;
    if !unit.price_currency.is_confirmed() || !unit.price_unit.is_confirmed() {
        return Err(integrity("price unit/currency evidence is not confirmed"));
    }
    if bars.iter().any(|bar| bar.volume.is_some()) && !unit.volume_unit.is_confirmed() {
        return Err(integrity("volume unit evidence missing for volume bars"));
    }
    if bars.iter().any(|bar| bar.amount.is_some())
        && !(unit.amount_currency.is_confirmed() && unit.amount_unit.is_confirmed())
    {
        return Err(integrity("amount unit evidence missing for amount bars"));
    }
    Ok(TypedCollection {
        bars,
        source_binding,
        version_evidence,
    })
}

/// Stage+publish+commit one validated collection candidate inside the
/// batch transaction. Equivalent to `stage_tx` + `mark_validated` +
/// `commit_publication` minus the redundant stored-chain re-verification —
/// the rows were just written and digest-verified in this transaction, so
/// the facts cannot change between stage and commit.
fn collection_item_commit_tx(
    c: &Connection,
    item: &CollectionCandidate,
    typed: &TypedCollection,
) -> Result<Receipt, Error> {
    let binding_digest = compact_versions::put_source_binding(c, &typed.source_binding)?;
    let evidence_digest =
        compact_versions::put_evidence(c, EvidenceKind::Version, &json!(typed.version_evidence))?;
    let mut clips = Vec::new();
    for chunk in typed.bars.chunks(SEGMENT_ROWS) {
        let segment = compact_v19::insert_segment(c, &item.symbol, Period::Day, chunk)?;
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
    let plain: Vec<crate::compact_market::SegmentClip> =
        clips.iter().map(|clip| clip.clip.clone()).collect();
    let projection =
        compact_market::checkpoint_projection_digest(&item.symbol, Period::Day, &plain)
            .map_err(integrity)?;
    let draft = VersionDraft {
        version_id: item.version_id.clone(),
        entity: item.symbol.clone(),
        period: Period::Day,
        source_binding_digest: binding_digest,
        coverage_status: item.coverage.status,
        evidence_digest,
        created_at: item.published_at.to_rfc3339(),
        yearly_lineage: None,
    };
    compact_versions::check_draft_static(c, &draft)?;
    let key = compact_versions::insert_version_row(c, &draft, None, 1, 0, &typed.bars, projection)?;
    let written = compact_v19::write_checkpoint(c, key, &item.symbol, Period::Day, &clips)?;
    if written != projection {
        return Err(integrity("persisted checkpoint digest mismatch"));
    }
    // Staged -> validated: the stage above already verified every digest;
    // the stored rows cannot change inside this transaction.
    let changed = c
        .execute(
            "UPDATE data_versions SET state=2 WHERE version_key=?1 AND state=1",
            [key],
        )
        .map_err(storage)?;
    if changed != 1 {
        return Err(integrity("batch validation transition did not apply"));
    }
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
    let stored = compact_results::commit_publication_tx(
        c,
        &PublicationCommit {
            publication_id: item.publication_id.clone(),
            task_id: item.task_id.clone(),
            item_id: item.item_id.clone(),
            publication_kind: PublicationKind::Collection,
            base_version_key: None,
            result_version_key: key,
            outcome: UpdateOutcome::Published,
            algorithm_version: COLLECTION_ALGORITHM.into(),
            source_binding_digest: binding_digest,
            evidence_digest,
            expected_task_revision: u64::try_from(revision)
                .map_err(|_| integrity("task revision out of range"))?,
            published_at: Some(item.published_at),
            head: HeadExpectation::Missing,
            required_heads: vec![RequiredHead {
                entity: item.symbol.clone(),
                period: Period::Day,
                expected: None,
            }],
            binding: BindingCheck {
                run_id: item.run_id.clone(),
                entity: item.symbol.clone(),
                period: Period::Day,
                occupancy_key: scope.occupancy_key(),
            },
        },
        false,
    )?;
    Ok(Receipt {
        publication_id: item.publication_id.clone(),
        version_id: stored.identity.result_version_id.clone(),
        task_id: item.task_id.clone(),
        task_revision: stored.identity.task_revision,
    })
}
