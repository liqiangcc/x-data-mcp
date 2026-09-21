use chrono::{DateTime, Utc};
use rusqlite::{ErrorCode, TransactionBehavior};

use super::compact_v19::{self, StoredClip, StoredDelta, StoredDeltaKind};
use super::compact_versions::{self, CompactVersionState, VersionResultFacts};
use super::*;
use crate::compact_market::{
    self, CompactBar, ContinuityMode, DeltaKind, DeltaOp, Digest, EvidenceKind, ManifestMode,
    MarketUpdateResultIdentity, PublicationKind, PublicationResultIdentity, SegmentClip,
    UpdateOutcome,
};

const MAX_DEPTH: u32 = 256;
const OUTCOME_LIMIT: usize = 192;
const REGISTRY_MARKET_UPDATE: i64 = 1;
const REGISTRY_PUBLICATION: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HeadExpectation {
    Keep,
    Missing,
    Version(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MarketUpdateChange {
    NoChange {
        prefix_bytes: u64,
    },
    Append {
        prefix_bytes: u64,
        ops: Vec<StoredDelta>,
    },
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarketUpdateCommit {
    pub result_id: String,
    pub task_id: String,
    pub item_id: String,
    pub base_version_key: i64,
    pub result_version_key: i64,
    pub range_start_day: i64,
    pub range_end_day: i64,
    pub algorithm_version: String,
    pub source_binding_digest: Digest,
    pub evidence_digest: Digest,
    pub expected_task_revision: u64,
    pub published_at: Option<DateTime<Utc>>,
    pub head: HeadExpectation,
    pub change: MarketUpdateChange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublicationCommit {
    pub publication_id: String,
    pub task_id: String,
    pub item_id: String,
    pub publication_kind: PublicationKind,
    pub base_version_key: Option<i64>,
    pub result_version_key: i64,
    pub outcome: UpdateOutcome,
    pub algorithm_version: String,
    pub source_binding_digest: Digest,
    pub evidence_digest: Digest,
    pub expected_task_revision: u64,
    pub published_at: Option<DateTime<Utc>>,
    pub head: HeadExpectation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredMarketUpdateResult {
    pub result_key: i64,
    pub base_version_key: i64,
    pub result_version_key: i64,
    pub identity: MarketUpdateResultIdentity,
    pub result_digest: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredPublicationResult {
    pub result_key: i64,
    pub base_version_key: Option<i64>,
    pub result_version_key: i64,
    pub identity: PublicationResultIdentity,
    pub result_digest: Digest,
}

pub(crate) fn commit_market_update(
    c: &mut Connection,
    command: &MarketUpdateCommit,
) -> Result<StoredMarketUpdateResult, Error> {
    compact_market::epoch_date(command.range_start_day).map_err(integrity)?;
    compact_market::epoch_date(command.range_end_day).map_err(integrity)?;
    if command.range_start_day > command.range_end_day {
        return Err(integrity("update range inverted"));
    }
    let expected_revision_i64 = i64::try_from(command.expected_task_revision)
        .map_err(|_| integrity("task revision out of range"))?;
    let task_revision = command
        .expected_task_revision
        .checked_add(1)
        .ok_or_else(|| integrity("task revision overflow"))?;
    let task_revision_i64 =
        i64::try_from(task_revision).map_err(|_| integrity("task revision out of range"))?;
    let outcome_json = result_outcome_json(&command.result_id, "market_update")?;
    let prefix_i64 = match &command.change {
        MarketUpdateChange::NoChange { prefix_bytes }
        | MarketUpdateChange::Append { prefix_bytes, .. } => {
            Some(i64::try_from(*prefix_bytes).map_err(|_| integrity("prefix_bytes out of range"))?)
        }
        MarketUpdateChange::Full => None,
    };

    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let base = compact_versions::result_facts(&tx, command.base_version_key)?;
    if base.state != CompactVersionState::Published {
        return Err(integrity("update base version is not published"));
    }
    let base_fixed = compact_versions::fixed(&tx, &base.version_id)?;
    compact_versions::verified_source_binding(&tx, &command.source_binding_digest)?;
    if compact_versions::verified_evidence_kind(&tx, &command.evidence_digest)?
        != EvidenceKind::Continuity
    {
        return Err(integrity("market update evidence is not continuity"));
    }
    let base_compact = compact_bars(&base_fixed)?;
    let plan = plan_market_update(&tx, command, &base)?;
    register_result(&tx, &command.result_id, REGISTRY_MARKET_UPDATE)?;
    require_item_unused(&tx, &command.task_id, &command.item_id)?;
    if plan.outcome == UpdateOutcome::Published {
        let published_at = command
            .published_at
            .ok_or_else(|| integrity("published update lacks published_at"))?;
        compact_versions::publish(&tx, plan.result.key, published_at)?;
        compact_versions::fixed(&tx, &plan.result.version_id)?;
        if plan.continuity == ContinuityMode::Append
            && plan.result.manifest_mode == ManifestMode::Checkpoint
        {
            let applied = compact_versions::apply_stored_delta(
                &tx,
                &base_compact,
                &plan.append_ops,
                &plan.result.entity,
                plan.result.period,
            )?;
            let actual = compact_bars(&compact_versions::fixed(&tx, &plan.result.version_id)?)?;
            if applied != actual {
                return Err(integrity("checkpoint append does not match base plus ops"));
            }
        }
    }
    let identity = MarketUpdateResultIdentity {
        result_id: command.result_id.clone(),
        task_id: command.task_id.clone(),
        item_id: command.item_id.clone(),
        base_version_id: base.version_id.clone(),
        result_version_id: plan.result.version_id.clone(),
        outcome: plan.outcome,
        range_start_day: command.range_start_day,
        range_end_day: command.range_end_day,
        algorithm_version: command.algorithm_version.clone(),
        source_binding_digest: command.source_binding_digest,
        continuity_mode: plan.continuity,
        prefix_bytes: plan.prefix_bytes,
        base_projection_digest: base.projection_digest,
        result_projection_digest: plan.result.projection_digest,
        delta_digest: plan.delta_digest,
        evidence_digest: command.evidence_digest,
        task_revision,
    };
    let result_digest =
        compact_market::market_update_result_digest(&identity).map_err(integrity)?;
    tx.execute(
        "INSERT INTO market_update_results(result_id,task_id,item_id,base_version_key,result_version_key,outcome,range_start_day,range_end_day,algorithm_version,source_binding_digest,continuity_mode,prefix_bytes,base_projection_digest,result_projection_digest,delta_digest,evidence_digest,task_revision,result_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        params![
            command.result_id,
            command.task_id,
            command.item_id,
            base.key,
            plan.result.key,
            outcome_code(plan.outcome),
            command.range_start_day,
            command.range_end_day,
            command.algorithm_version,
            command.source_binding_digest.as_slice(),
            continuity_code(plan.continuity),
            prefix_i64,
            base.projection_digest.as_slice(),
            plan.result.projection_digest.as_slice(),
            plan.delta_digest.map(|d| d.to_vec()),
            command.evidence_digest.as_slice(),
            task_revision_i64,
            result_digest.as_slice()
        ],
    )
    .map_err(conflict_or_storage)?;
    let result_key = tx.last_insert_rowid();
    for (seq, op) in plan.append_ops.iter().enumerate() {
        let StoredDeltaKind::Upsert(stored) = &op.kind else {
            return Err(integrity("append result op is not an upsert"));
        };
        tx.execute(
            "INSERT INTO market_update_delta_clips(result_key,seq,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                result_key,
                seq as i64,
                op.range_start_day,
                op.range_end_day,
                stored.segment_key,
                stored.clip.clip_start_day,
                stored.clip.clip_end_day,
                stored.clip.row_count
            ],
        )
        .map_err(storage)?;
    }
    if plan.outcome == UpdateOutcome::Published {
        tx.execute(
            "INSERT INTO task_publications(task_id,version_key,result_id) VALUES(?1,?2,?3)",
            params![command.task_id, plan.result.key, command.result_id],
        )
        .map_err(conflict_or_storage)?;
        apply_head(
            &tx,
            &command.head,
            &plan.result.entity,
            plan.result.period,
            Some(base.key),
            plan.result.key,
            false,
        )?;
    }
    finish_item_and_task(
        &tx,
        &command.task_id,
        &command.item_id,
        expected_revision_i64,
        task_revision_i64,
        &outcome_json,
    )?;
    tx.commit().map_err(storage)?;
    Ok(StoredMarketUpdateResult {
        result_key,
        base_version_key: base.key,
        result_version_key: plan.result.key,
        identity,
        result_digest,
    })
}

pub(crate) fn commit_publication(
    c: &mut Connection,
    command: &PublicationCommit,
) -> Result<StoredPublicationResult, Error> {
    let expected_revision_i64 = i64::try_from(command.expected_task_revision)
        .map_err(|_| integrity("task revision out of range"))?;
    let task_revision = command
        .expected_task_revision
        .checked_add(1)
        .ok_or_else(|| integrity("task revision overflow"))?;
    let task_revision_i64 =
        i64::try_from(task_revision).map_err(|_| integrity("task revision out of range"))?;
    let outcome_json = result_outcome_json(&command.publication_id, "publication")?;

    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let result = compact_versions::result_facts(&tx, command.result_version_key)?;
    let expected_evidence = match command.publication_kind {
        PublicationKind::Collection | PublicationKind::Republish => EvidenceKind::Version,
        PublicationKind::Yearly => EvidenceKind::Yearly,
    };
    if compact_versions::verified_evidence_kind(&tx, &command.evidence_digest)? != expected_evidence
    {
        return Err(integrity("publication evidence kind mismatch"));
    }
    compact_versions::verified_source_binding(&tx, &command.source_binding_digest)?;
    match command.publication_kind {
        PublicationKind::Collection if result.period != Period::Day => {
            return Err(integrity("collection publication requires daily result"));
        }
        PublicationKind::Yearly if result.period != Period::Year => {
            return Err(integrity("yearly publication requires yearly result"));
        }
        _ => {}
    }
    let base = match command.base_version_key {
        Some(key) => {
            let facts = compact_versions::result_facts(&tx, key)?;
            if facts.state != CompactVersionState::Published {
                return Err(integrity("publication base version is not published"));
            }
            if facts.entity != result.entity || facts.period != result.period {
                return Err(integrity("publication base scope mismatch"));
            }
            compact_versions::fixed(&tx, &facts.version_id)?;
            Some(facts)
        }
        None => None,
    };
    match command.outcome {
        UpdateOutcome::NoChange => {
            let base = base
                .as_ref()
                .ok_or_else(|| integrity("no-change publication lacks base"))?;
            if base.key != result.key {
                return Err(integrity("no-change publication base differs from result"));
            }
            if result.state != CompactVersionState::Published {
                return Err(integrity("no-change publication result is not published"));
            }
            if command.published_at.is_some() || command.head != HeadExpectation::Keep {
                return Err(integrity("no-change publication carries publish effects"));
            }
            compact_versions::fixed(&tx, &result.version_id)?;
        }
        UpdateOutcome::Published => {
            if result.state != CompactVersionState::Validated {
                return Err(integrity("publication result version is not validated"));
            }
            if result.source_binding_digest != command.source_binding_digest {
                return Err(integrity("publication source binding mismatch"));
            }
            if command.published_at.is_none() {
                return Err(integrity("published publication lacks published_at"));
            }
            if let Some(base) = &base
                && base.key == result.key
            {
                return Err(integrity("published publication reuses base version"));
            }
        }
    }
    register_result(&tx, &command.publication_id, REGISTRY_PUBLICATION)?;
    require_item_unused(&tx, &command.task_id, &command.item_id)?;
    if command.outcome == UpdateOutcome::Published {
        compact_versions::publish(
            &tx,
            result.key,
            command
                .published_at
                .ok_or_else(|| integrity("published publication lacks published_at"))?,
        )?;
        compact_versions::fixed(&tx, &result.version_id)?;
    }
    let identity = PublicationResultIdentity {
        publication_id: command.publication_id.clone(),
        task_id: command.task_id.clone(),
        item_id: command.item_id.clone(),
        publication_kind: command.publication_kind,
        base_version_id: base.as_ref().map(|facts| facts.version_id.clone()),
        result_version_id: result.version_id.clone(),
        outcome: command.outcome,
        algorithm_version: command.algorithm_version.clone(),
        source_binding_digest: command.source_binding_digest,
        evidence_digest: command.evidence_digest,
        task_revision,
    };
    let result_digest = compact_market::publication_result_digest(&identity).map_err(integrity)?;
    tx.execute(
        "INSERT INTO publication_results(publication_id,task_id,item_id,publication_kind,base_version_key,result_version_key,outcome,algorithm_version,source_binding_digest,evidence_digest,task_revision,result_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            command.publication_id,
            command.task_id,
            command.item_id,
            publication_kind_code(command.publication_kind),
            command.base_version_key,
            result.key,
            outcome_code(command.outcome),
            command.algorithm_version,
            command.source_binding_digest.as_slice(),
            command.evidence_digest.as_slice(),
            task_revision_i64,
            result_digest.as_slice()
        ],
    )
    .map_err(conflict_or_storage)?;
    let result_key = tx.last_insert_rowid();
    if command.outcome == UpdateOutcome::Published {
        tx.execute(
            "INSERT INTO task_publications(task_id,version_key,result_id) VALUES(?1,?2,?3)",
            params![command.task_id, result.key, command.publication_id],
        )
        .map_err(conflict_or_storage)?;
        apply_head(
            &tx,
            &command.head,
            &result.entity,
            result.period,
            command.base_version_key,
            result.key,
            true,
        )?;
    }
    finish_item_and_task(
        &tx,
        &command.task_id,
        &command.item_id,
        expected_revision_i64,
        task_revision_i64,
        &outcome_json,
    )?;
    tx.commit().map_err(storage)?;
    Ok(StoredPublicationResult {
        result_key,
        base_version_key: command.base_version_key,
        result_version_key: result.key,
        identity,
        result_digest,
    })
}

pub(crate) fn read_market_update(
    c: &Connection,
    result_id: &str,
) -> Result<StoredMarketUpdateResult, Error> {
    let kind: Option<i64> = c
        .query_row(
            "SELECT result_kind FROM result_registry WHERE result_id=?1",
            params![result_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    match kind {
        None => return Err(Error::NotFound),
        Some(REGISTRY_MARKET_UPDATE) => {}
        Some(_) => return Err(integrity("registry kind mismatch")),
    }
    let row = c
        .query_row(
            "SELECT result_key,task_id,item_id,base_version_key,result_version_key,outcome,range_start_day,range_end_day,algorithm_version,source_binding_digest,continuity_mode,prefix_bytes,base_projection_digest,result_projection_digest,delta_digest,evidence_digest,task_revision,result_digest FROM market_update_results WHERE result_id=?1",
            params![result_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, Vec<u8>>(9)?,
                    r.get::<_, i64>(10)?,
                    r.get::<_, Option<i64>>(11)?,
                    r.get::<_, Vec<u8>>(12)?,
                    r.get::<_, Vec<u8>>(13)?,
                    r.get::<_, Option<Vec<u8>>>(14)?,
                    r.get::<_, Vec<u8>>(15)?,
                    r.get::<_, i64>(16)?,
                    r.get::<_, Vec<u8>>(17)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(|| integrity("market update result row missing"))?;
    let (
        result_key,
        task_id,
        item_id,
        base_version_key,
        result_version_key,
        outcome_raw,
        range_start_day,
        range_end_day,
        algorithm_version,
        source_binding,
        continuity_raw,
        prefix_bytes,
        base_projection,
        result_projection,
        delta_raw,
        evidence_digest,
        task_revision,
        stored_digest,
    ) = row;
    let outcome = decode_outcome(outcome_raw)?;
    let continuity = decode_continuity(continuity_raw)?;
    let prefix_bytes = match prefix_bytes {
        Some(prefix) if prefix >= 0 => Some(prefix as u64),
        Some(_) => return Err(integrity("negative prefix_bytes")),
        None => None,
    };
    let base_projection_digest = fixed_digest(&base_projection)?;
    let result_projection_digest = fixed_digest(&result_projection)?;
    let stored_delta = delta_raw.map(|raw| fixed_digest(&raw)).transpose()?;
    if task_revision <= 0
        || range_start_day > range_end_day
        || compact_market::epoch_date(range_start_day).is_err()
        || compact_market::epoch_date(range_end_day).is_err()
    {
        return Err(integrity("market update result metadata invalid"));
    }
    match (outcome, continuity) {
        (UpdateOutcome::NoChange, ContinuityMode::Unchanged)
            if base_version_key == result_version_key
                && stored_delta.is_none()
                && prefix_bytes.is_some() => {}
        (UpdateOutcome::Published, ContinuityMode::Append | ContinuityMode::Full)
            if base_version_key != result_version_key && stored_delta.is_some() => {}
        _ => return Err(integrity("market update result combination invalid")),
    }
    if continuity == ContinuityMode::Full && prefix_bytes.is_some() {
        return Err(integrity("full update result carries prefix_bytes"));
    }
    let (base_facts, base_fixed) = require_published_facts(c, base_version_key)?;
    let (result_facts, result_fixed) = require_published_facts(c, result_version_key)?;
    if base_projection_digest != base_facts.projection_digest
        || result_projection_digest != result_facts.projection_digest
    {
        return Err(integrity("update result projection linkage mismatch"));
    }
    if base_facts.entity != result_facts.entity || base_facts.period != result_facts.period {
        return Err(integrity("update result scope mismatch"));
    }
    compact_versions::verified_source_binding(c, &fixed_digest(&source_binding)?)?;
    if outcome == UpdateOutcome::Published
        && result_facts.source_binding_digest != fixed_digest(&source_binding)?
    {
        return Err(integrity("update result source binding mismatch"));
    }
    if compact_versions::verified_evidence_kind(c, &fixed_digest(&evidence_digest)?)?
        != EvidenceKind::Continuity
    {
        return Err(integrity("market update evidence kind mismatch"));
    }
    let ops = read_update_delta_clips(c, result_key)?;
    let delta_digest = match continuity {
        ContinuityMode::Unchanged => {
            if !ops.is_empty() {
                return Err(integrity("no-change update carries delta clips"));
            }
            None
        }
        ContinuityMode::Append => {
            if ops.is_empty() {
                return Err(integrity("append update lacks delta clips"));
            }
            for op in &ops {
                if op.range_start_day <= base_facts.last_day {
                    return Err(integrity("append update op is not tail-only"));
                }
            }
            let ops_digest = compact_market::delta_digest(&codec_ops(&ops)).map_err(integrity)?;
            match result_facts.manifest_mode {
                ManifestMode::Delta => {
                    if result_facts.parent_version_key != Some(base_facts.key) {
                        return Err(integrity("append delta parent mismatch"));
                    }
                    let (persisted, persisted_delta, projection) =
                        compact_v19::read_delta(c, result_facts.key, base_facts.projection_digest)?;
                    if persisted != ops
                        || persisted_delta != ops_digest
                        || projection != result_facts.projection_digest
                    {
                        return Err(integrity("append delta ops mismatch persisted"));
                    }
                }
                ManifestMode::Checkpoint => {
                    if base_facts.delta_depth != MAX_DEPTH {
                        return Err(integrity(
                            "append checkpoint rollover requires depth-256 base",
                        ));
                    }
                    let applied = compact_versions::apply_stored_delta(
                        c,
                        &compact_bars(&base_fixed)?,
                        &ops,
                        &result_facts.entity,
                        result_facts.period,
                    )?;
                    if applied != compact_bars(&result_fixed)? {
                        return Err(integrity("checkpoint append does not match base plus ops"));
                    }
                }
            }
            Some(ops_digest)
        }
        ContinuityMode::Full => {
            if !ops.is_empty() {
                return Err(integrity("full update carries delta clips"));
            }
            if result_facts.manifest_mode != ManifestMode::Checkpoint
                || result_facts.parent_version_key.is_some()
                || result_facts.delta_depth != 0
            {
                return Err(integrity("full revision result is not a root checkpoint"));
            }
            Some(compact_market::full_revision_digest(
                base_facts.projection_digest,
                result_facts.projection_digest,
            ))
        }
    };
    if delta_digest != stored_delta {
        return Err(integrity("update delta digest mismatch"));
    }
    let identity = MarketUpdateResultIdentity {
        result_id: result_id.into(),
        task_id: task_id.clone(),
        item_id: item_id.clone(),
        base_version_id: base_facts.version_id.clone(),
        result_version_id: result_facts.version_id.clone(),
        outcome,
        range_start_day,
        range_end_day,
        algorithm_version,
        source_binding_digest: fixed_digest(&source_binding)?,
        continuity_mode: continuity,
        prefix_bytes,
        base_projection_digest,
        result_projection_digest,
        delta_digest,
        evidence_digest: fixed_digest(&evidence_digest)?,
        task_revision: task_revision as u64,
    };
    if compact_market::market_update_result_digest(&identity).map_err(integrity)?
        != fixed_digest(&stored_digest)?
    {
        return Err(integrity("update result digest mismatch"));
    }
    verify_task_publication(
        c,
        &task_id,
        result_facts.key,
        result_id,
        outcome == UpdateOutcome::Published,
    )?;
    verify_task_item(
        c,
        &task_id,
        &item_id,
        result_id,
        "market_update",
        task_revision,
    )?;
    Ok(StoredMarketUpdateResult {
        result_key,
        base_version_key,
        result_version_key,
        identity,
        result_digest: fixed_digest(&stored_digest)?,
    })
}

pub(crate) fn read_publication(
    c: &Connection,
    publication_id: &str,
) -> Result<StoredPublicationResult, Error> {
    let kind: Option<i64> = c
        .query_row(
            "SELECT result_kind FROM result_registry WHERE result_id=?1",
            params![publication_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    match kind {
        None => return Err(Error::NotFound),
        Some(REGISTRY_PUBLICATION) => {}
        Some(_) => return Err(integrity("registry kind mismatch")),
    }
    let row = c
        .query_row(
            "SELECT result_key,task_id,item_id,publication_kind,base_version_key,result_version_key,outcome,algorithm_version,source_binding_digest,evidence_digest,task_revision,result_digest FROM publication_results WHERE publication_id=?1",
            params![publication_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Vec<u8>>(8)?,
                    r.get::<_, Vec<u8>>(9)?,
                    r.get::<_, i64>(10)?,
                    r.get::<_, Vec<u8>>(11)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(|| integrity("publication result row missing"))?;
    let (
        result_key,
        task_id,
        item_id,
        publication_kind_raw,
        base_version_key,
        result_version_key,
        outcome_raw,
        algorithm_version,
        source_binding,
        evidence_digest,
        task_revision,
        stored_digest,
    ) = row;
    let publication_kind = match publication_kind_raw {
        1 => PublicationKind::Collection,
        2 => PublicationKind::Yearly,
        3 => PublicationKind::Republish,
        _ => return Err(integrity("unknown publication_kind")),
    };
    let outcome = decode_outcome(outcome_raw)?;
    if task_revision <= 0 {
        return Err(integrity("publication task revision invalid"));
    }
    match (outcome, base_version_key) {
        (UpdateOutcome::NoChange, Some(base)) if base == result_version_key => {}
        (UpdateOutcome::Published, base) if base.is_none_or(|base| base != result_version_key) => {}
        _ => return Err(integrity("publication result combination invalid")),
    }
    let (result_facts, _) = require_published_facts(c, result_version_key)?;
    match publication_kind {
        PublicationKind::Collection if result_facts.period != Period::Day => {
            return Err(integrity("collection publication result is not daily"));
        }
        PublicationKind::Yearly if result_facts.period != Period::Year => {
            return Err(integrity("yearly publication result is not yearly"));
        }
        _ => {}
    }
    let expected_evidence = match publication_kind {
        PublicationKind::Collection | PublicationKind::Republish => EvidenceKind::Version,
        PublicationKind::Yearly => EvidenceKind::Yearly,
    };
    if compact_versions::verified_evidence_kind(c, &fixed_digest(&evidence_digest)?)?
        != expected_evidence
    {
        return Err(integrity("publication evidence kind mismatch"));
    }
    compact_versions::verified_source_binding(c, &fixed_digest(&source_binding)?)?;
    if outcome == UpdateOutcome::Published
        && result_facts.source_binding_digest != fixed_digest(&source_binding)?
    {
        return Err(integrity("publication result source binding mismatch"));
    }
    let base_facts = match base_version_key {
        Some(key) => {
            let (facts, _) = require_published_facts(c, key)?;
            if facts.entity != result_facts.entity || facts.period != result_facts.period {
                return Err(integrity("publication base scope mismatch"));
            }
            Some(facts)
        }
        None => None,
    };
    let identity = PublicationResultIdentity {
        publication_id: publication_id.into(),
        task_id: task_id.clone(),
        item_id: item_id.clone(),
        publication_kind,
        base_version_id: base_facts.as_ref().map(|facts| facts.version_id.clone()),
        result_version_id: result_facts.version_id.clone(),
        outcome,
        algorithm_version,
        source_binding_digest: fixed_digest(&source_binding)?,
        evidence_digest: fixed_digest(&evidence_digest)?,
        task_revision: task_revision as u64,
    };
    if compact_market::publication_result_digest(&identity).map_err(integrity)?
        != fixed_digest(&stored_digest)?
    {
        return Err(integrity("publication result digest mismatch"));
    }
    verify_task_publication(
        c,
        &task_id,
        result_facts.key,
        publication_id,
        outcome == UpdateOutcome::Published,
    )?;
    verify_task_item(
        c,
        &task_id,
        &item_id,
        publication_id,
        "publication",
        task_revision,
    )?;
    Ok(StoredPublicationResult {
        result_key,
        base_version_key,
        result_version_key,
        identity,
        result_digest: fixed_digest(&stored_digest)?,
    })
}

struct MarketPlan {
    result: VersionResultFacts,
    outcome: UpdateOutcome,
    continuity: ContinuityMode,
    prefix_bytes: Option<u64>,
    delta_digest: Option<Digest>,
    append_ops: Vec<StoredDelta>,
}

fn plan_market_update(
    c: &Connection,
    command: &MarketUpdateCommit,
    base: &VersionResultFacts,
) -> Result<MarketPlan, Error> {
    match &command.change {
        MarketUpdateChange::NoChange { prefix_bytes } => {
            if command.result_version_key != base.key {
                return Err(integrity("no-change update result differs from base"));
            }
            if command.published_at.is_some() || command.head != HeadExpectation::Keep {
                return Err(integrity("no-change update carries publish effects"));
            }
            Ok(MarketPlan {
                result: base.clone(),
                outcome: UpdateOutcome::NoChange,
                continuity: ContinuityMode::Unchanged,
                prefix_bytes: Some(*prefix_bytes),
                delta_digest: None,
                append_ops: Vec::new(),
            })
        }
        MarketUpdateChange::Append { prefix_bytes, ops } => {
            if ops.is_empty() {
                return Err(integrity("append update requires ops"));
            }
            for op in ops {
                if !matches!(op.kind, StoredDeltaKind::Upsert(_)) {
                    return Err(integrity("append update op is not an upsert"));
                }
                if op.range_start_day <= base.last_day {
                    return Err(integrity("append update op is not tail-only"));
                }
            }
            let delta = compact_market::delta_digest(&codec_ops(ops)).map_err(integrity)?;
            let result = compact_versions::result_facts(c, command.result_version_key)?;
            require_validated_result(command, &result, base)?;
            match result.manifest_mode {
                ManifestMode::Delta => {
                    if result.parent_version_key != Some(base.key) {
                        return Err(integrity("append delta parent mismatch"));
                    }
                    let (persisted, stored_delta, projection) =
                        compact_v19::read_delta(c, result.key, base.projection_digest)?;
                    if persisted != *ops || stored_delta != delta {
                        return Err(integrity("append delta ops mismatch persisted"));
                    }
                    if projection != result.projection_digest {
                        return Err(integrity("append result projection mismatch"));
                    }
                }
                ManifestMode::Checkpoint => {
                    if base.delta_depth != MAX_DEPTH {
                        return Err(integrity(
                            "append checkpoint rollover requires depth-256 base",
                        ));
                    }
                }
            }
            Ok(MarketPlan {
                result,
                outcome: UpdateOutcome::Published,
                continuity: ContinuityMode::Append,
                prefix_bytes: Some(*prefix_bytes),
                delta_digest: Some(delta),
                append_ops: ops.clone(),
            })
        }
        MarketUpdateChange::Full => {
            let result = compact_versions::result_facts(c, command.result_version_key)?;
            require_validated_result(command, &result, base)?;
            if result.manifest_mode != ManifestMode::Checkpoint
                || result.parent_version_key.is_some()
                || result.delta_depth != 0
            {
                return Err(integrity("full revision result is not a root checkpoint"));
            }
            let delta = compact_market::full_revision_digest(
                base.projection_digest,
                result.projection_digest,
            );
            Ok(MarketPlan {
                result,
                outcome: UpdateOutcome::Published,
                continuity: ContinuityMode::Full,
                prefix_bytes: None,
                delta_digest: Some(delta),
                append_ops: Vec::new(),
            })
        }
    }
}

fn require_validated_result(
    command: &MarketUpdateCommit,
    result: &VersionResultFacts,
    base: &VersionResultFacts,
) -> Result<(), Error> {
    if result.key == base.key {
        return Err(integrity("published update reuses base version"));
    }
    if result.state != CompactVersionState::Validated {
        return Err(integrity("update result version is not validated"));
    }
    if result.entity != base.entity || result.period != base.period {
        return Err(integrity("update result scope mismatch"));
    }
    if result.source_binding_digest != command.source_binding_digest {
        return Err(integrity("update source binding mismatch"));
    }
    if command.published_at.is_none() {
        return Err(integrity("published update lacks published_at"));
    }
    Ok(())
}

fn register_result(c: &Connection, result_id: &str, kind: i64) -> Result<(), Error> {
    if result_id.is_empty() {
        return Err(integrity("result id is empty"));
    }
    let exists: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM result_registry WHERE result_id=?1)",
            params![result_id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if exists {
        return Err(Error::Conflict("result id already consumed".into()));
    }
    c.execute(
        "INSERT INTO result_registry(result_id,result_kind) VALUES(?1,?2)",
        params![result_id, kind],
    )
    .map_err(conflict_or_storage)?;
    Ok(())
}

fn require_item_unused(c: &Connection, task_id: &str, item_id: &str) -> Result<(), Error> {
    let exists: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM market_update_results WHERE task_id=?1 AND item_id=?2) OR EXISTS(SELECT 1 FROM publication_results WHERE task_id=?1 AND item_id=?2)",
            params![task_id, item_id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if exists {
        return Err(Error::Conflict("task item already has a result".into()));
    }
    Ok(())
}

fn apply_head(
    c: &Connection,
    head: &HeadExpectation,
    entity: &str,
    period: Period,
    base_key: Option<i64>,
    result_key: i64,
    allow_missing: bool,
) -> Result<(), Error> {
    let period_code: i64 = match period {
        Period::Day => 1,
        Period::Year => 2,
    };
    match head {
        HeadExpectation::Keep => Ok(()),
        HeadExpectation::Missing => {
            if !allow_missing {
                return Err(Error::Conflict("update cannot create a head".into()));
            }
            let changed = c
                .execute(
                    "INSERT INTO effective_heads(entity,period,adjustment,version_key) SELECT ?1,?2,1,?3 WHERE NOT EXISTS(SELECT 1 FROM effective_heads WHERE entity=?1 AND period=?2 AND adjustment=1)",
                    params![entity, period_code, result_key],
                )
                .map_err(storage)?;
            if changed != 1 {
                return Err(Error::Conflict("effective head already exists".into()));
            }
            Ok(())
        }
        HeadExpectation::Version(expected) => {
            if base_key != Some(*expected) {
                return Err(Error::Conflict(
                    "head expectation does not match base".into(),
                ));
            }
            let changed = c
                .execute(
                    "UPDATE effective_heads SET version_key=?1 WHERE entity=?2 AND period=?3 AND adjustment=1 AND version_key=?4",
                    params![result_key, entity, period_code, expected],
                )
                .map_err(storage)?;
            if changed != 1 {
                return Err(Error::Conflict(
                    "effective head does not match expectation".into(),
                ));
            }
            Ok(())
        }
    }
}

fn finish_item_and_task(
    c: &Connection,
    task_id: &str,
    item_id: &str,
    expected_revision: i64,
    new_revision: i64,
    outcome_json: &str,
) -> Result<(), Error> {
    let changed = c
        .execute(
            "UPDATE task_items SET stage='finished',outcome_json=?1 WHERE task_id=?2 AND item_id=?3 AND stage='running' AND outcome_json IS NULL",
            params![outcome_json, task_id, item_id],
        )
        .map_err(storage)?;
    if changed != 1 {
        return Err(Error::Conflict("task item is not running".into()));
    }
    let changed = c
        .execute(
            "UPDATE tasks SET state_revision=?1,items_running=items_running-1,items_finished=items_finished+1,items_succeeded=items_succeeded+1,status=CASE WHEN items_finished+1=item_count THEN CASE WHEN items_succeeded+1=item_count THEN 'succeeded' ELSE 'partially_failed' END ELSE status END WHERE task_id=?2 AND status='running' AND state_revision=?3 AND items_running>0",
            params![new_revision, task_id, expected_revision],
        )
        .map_err(storage)?;
    if changed != 1 {
        return Err(Error::Conflict(
            "task state does not match expectation".into(),
        ));
    }
    Ok(())
}

fn verify_task_item(
    c: &Connection,
    task_id: &str,
    item_id: &str,
    result_id: &str,
    result_kind: &str,
    result_revision: i64,
) -> Result<(), Error> {
    let item = c
        .query_row(
            "SELECT stage,outcome_json FROM task_items WHERE task_id=?1 AND item_id=?2",
            params![task_id, item_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(|| integrity("task item missing"))?;
    let expected = result_outcome_json(result_id, result_kind)?;
    if item.0 != "finished" || item.1.as_deref() != Some(expected.as_str()) {
        return Err(integrity("task item outcome mismatch"));
    }
    let revision: i64 = c
        .query_row(
            "SELECT state_revision FROM tasks WHERE task_id=?1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(|| integrity("task missing"))?;
    if revision < result_revision {
        return Err(integrity("task revision regressed"));
    }
    Ok(())
}

fn read_update_delta_clips(c: &Connection, result_key: i64) -> Result<Vec<StoredDelta>, Error> {
    let mut statement = c
        .prepare(
            "SELECT seq,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count FROM market_update_delta_clips WHERE result_key=?1 ORDER BY seq",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![result_key], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(statement);
    let mut ops = Vec::with_capacity(rows.len());
    for (index, (seq, range_start, range_end, segment_key, clip_start, clip_end, row_count)) in
        rows.into_iter().enumerate()
    {
        if seq != index as i64 || row_count <= 0 {
            return Err(integrity("update delta clip rows malformed"));
        }
        let segment = compact_v19::read_segment(c, segment_key)?;
        ops.push(StoredDelta {
            range_start_day: range_start,
            range_end_day: range_end,
            kind: StoredDeltaKind::Upsert(StoredClip {
                segment_key,
                clip: SegmentClip {
                    segment_digest: segment.digest,
                    clip_start_day: clip_start,
                    clip_end_day: clip_end,
                    row_count: row_count as u32,
                },
            }),
        });
    }
    Ok(ops)
}

fn require_published_facts(
    c: &Connection,
    key: i64,
) -> Result<(VersionResultFacts, compact_versions::VerifiedVersion), Error> {
    let facts = compact_versions::result_facts(c, key)?;
    if facts.state != CompactVersionState::Published {
        return Err(integrity("referenced version is not published"));
    }
    let verified = compact_versions::fixed(c, &facts.version_id)?;
    Ok((facts, verified))
}

fn verify_task_publication(
    c: &Connection,
    task_id: &str,
    version_key: i64,
    result_id: &str,
    published: bool,
) -> Result<(), Error> {
    if published {
        let count: i64 = c
            .query_row(
                "SELECT count(*) FROM task_publications WHERE task_id=?1 AND version_key=?2 AND result_id=?3",
                params![task_id, version_key, result_id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if count != 1 {
            return Err(integrity("task publication row missing"));
        }
    } else {
        let count: i64 = c
            .query_row(
                "SELECT count(*) FROM task_publications WHERE result_id=?1",
                params![result_id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if count != 0 {
            return Err(integrity("no-change result carries task publication"));
        }
    }
    Ok(())
}

fn compact_bars(verified: &compact_versions::VerifiedVersion) -> Result<Vec<CompactBar>, Error> {
    verified
        .version
        .records
        .iter()
        .map(|bar| CompactBar::from_bar(bar, verified.version.period))
        .collect::<Result<Vec<_>, _>>()
        .map_err(integrity)
}

fn codec_ops(ops: &[StoredDelta]) -> Vec<DeltaOp> {
    ops.iter()
        .map(|op| DeltaOp {
            range_start_day: op.range_start_day,
            range_end_day: op.range_end_day,
            kind: match &op.kind {
                StoredDeltaKind::Upsert(stored) => DeltaKind::Upsert(stored.clip.clone()),
                StoredDeltaKind::Remove => DeltaKind::Remove,
            },
        })
        .collect()
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

fn outcome_code(outcome: UpdateOutcome) -> i64 {
    match outcome {
        UpdateOutcome::NoChange => 1,
        UpdateOutcome::Published => 2,
    }
}

fn continuity_code(mode: ContinuityMode) -> i64 {
    match mode {
        ContinuityMode::Unchanged => 1,
        ContinuityMode::Append => 2,
        ContinuityMode::Full => 3,
    }
}

fn publication_kind_code(kind: PublicationKind) -> i64 {
    match kind {
        PublicationKind::Collection => 1,
        PublicationKind::Yearly => 2,
        PublicationKind::Republish => 3,
    }
}

fn decode_outcome(code: i64) -> Result<UpdateOutcome, Error> {
    match code {
        1 => Ok(UpdateOutcome::NoChange),
        2 => Ok(UpdateOutcome::Published),
        _ => Err(integrity("unknown outcome")),
    }
}

fn decode_continuity(code: i64) -> Result<ContinuityMode, Error> {
    match code {
        1 => Ok(ContinuityMode::Unchanged),
        2 => Ok(ContinuityMode::Append),
        3 => Ok(ContinuityMode::Full),
        _ => Err(integrity("unknown continuity_mode")),
    }
}

fn fixed_digest(raw: &[u8]) -> Result<Digest, Error> {
    raw.try_into()
        .map_err(|_| integrity("stored digest length mismatch"))
}

fn conflict_or_storage(error: rusqlite::Error) -> Error {
    match error {
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == ErrorCode::ConstraintViolation =>
        {
            Error::Conflict("result uniqueness violated".into())
        }
        other => storage(other),
    }
}

#[cfg(test)]
mod tests;
