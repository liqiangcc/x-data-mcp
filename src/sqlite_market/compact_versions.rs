use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, Datelike, Utc};

use super::compact_v19::{self, StoredClip, StoredDelta, StoredDeltaKind, StoredSegment};
use super::*;
use crate::compact_market::{
    self, CompactBar, DeltaKind, DeltaOp, Digest, EvidenceKind, ManifestMode, SegmentClip,
    SourceBinding, VersionEvidence, VersionIdentity, YearlyLineageIdentity,
};
use crate::market_data::{Adjustment, CoverageStatus, YearlyDerivation};
use crate::publish::{EffectiveVersion, ValidationSummary};

const DIGEST_FORMAT: &str = "compact-market-version-v1";
const MAX_DELTA_DEPTH: i64 = 256;
const MAX_CHAIN_NODES: usize = 257;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct YearlyLineageDraft {
    pub base_daily_version_key: i64,
    pub algorithm_version: String,
    pub first_trade_day: i64,
    pub last_trade_day: i64,
    pub as_of_day: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VersionDraft {
    pub version_id: String,
    pub entity: String,
    pub period: Period,
    pub source_binding_digest: Digest,
    pub coverage_status: CoverageStatus,
    pub evidence_digest: Digest,
    pub created_at: String,
    pub yearly_lineage: Option<YearlyLineageDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckpointBasis {
    pub parent_version_key: i64,
    pub delta_ops: Vec<StoredDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedVersion {
    pub key: i64,
    pub integrity_digest: Digest,
    pub version: EffectiveVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactVersionSummary {
    pub version_id: String,
    pub entity: String,
    pub period: Period,
    pub published_at: DateTime<Utc>,
    pub source: String,
    pub record_count: u64,
    pub yearly_derivation: Option<YearlyDerivation>,
    pub integrity_sha256: String,
}

pub(crate) fn put_evidence(
    c: &Connection,
    kind: EvidenceKind,
    value: &Value,
) -> Result<Digest, Error> {
    let bundle = compact_market::canonical_evidence(kind, value).map_err(integrity)?;
    let existing = c
        .query_row(
            "SELECT evidence_kind,canonical_json FROM evidence_bundles WHERE evidence_digest=?1",
            params![bundle.digest.as_slice()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(storage)?;
    if let Some((kind_raw, canonical)) = existing {
        let stored_kind = parse_evidence_kind(&kind_raw)?;
        compact_market::verify_evidence(stored_kind, &canonical, bundle.digest)
            .map_err(integrity)?;
        if stored_kind != kind || canonical != bundle.canonical_json {
            return Err(integrity(
                "evidence digest collision with different content",
            ));
        }
        return Ok(bundle.digest);
    }
    c.execute(
        "INSERT INTO evidence_bundles(evidence_digest,evidence_kind,canonical_json) VALUES(?1,?2,?3)",
        params![bundle.digest.as_slice(), kind.as_str(), bundle.canonical_json],
    )
    .map_err(storage)?;
    Ok(bundle.digest)
}

pub(crate) fn put_source_binding(c: &Connection, binding: &SourceBinding) -> Result<Digest, Error> {
    let digest = compact_market::source_binding_digest(binding).map_err(integrity)?;
    let (kind, _) = read_evidence(c, &binding.evidence_digest)?;
    if kind != EvidenceKind::Generation {
        return Err(integrity("source binding evidence is not generation"));
    }
    let existing = c
        .query_row(
            "SELECT generation_id,generation_manifest_sha256,day_member_sha256,security_event_sha256,evidence_digest FROM source_bindings WHERE source_binding_digest=?1",
            params![digest.as_slice()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    if let Some((generation_id, manifest, day, event, evidence)) = existing {
        let stored = SourceBinding {
            generation_id,
            generation_manifest_sha256: fixed_digest(&manifest)?,
            day_member_sha256: fixed_digest(&day)?,
            security_event_sha256: fixed_digest(&event)?,
            evidence_digest: fixed_digest(&evidence)?,
        };
        if compact_market::source_binding_digest(&stored).map_err(integrity)? != digest {
            return Err(integrity("stored source binding digest mismatch"));
        }
        if &stored != binding {
            return Err(integrity(
                "source binding digest collision with different content",
            ));
        }
        return Ok(digest);
    }
    c.execute(
        "INSERT INTO source_bindings(source_binding_digest,generation_id,generation_manifest_sha256,day_member_sha256,security_event_sha256,evidence_digest) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            digest.as_slice(),
            binding.generation_id,
            binding.generation_manifest_sha256.as_slice(),
            binding.day_member_sha256.as_slice(),
            binding.security_event_sha256.as_slice(),
            binding.evidence_digest.as_slice()
        ],
    )
    .map_err(storage)?;
    Ok(digest)
}

pub(crate) fn stage_checkpoint(
    c: &Connection,
    draft: &VersionDraft,
    clips: &[StoredClip],
    basis: Option<&CheckpointBasis>,
) -> Result<i64, Error> {
    check_draft_static(c, draft)?;
    let bars = flatten_clips(c, &draft.entity, draft.period, clips)?;
    let plain: Vec<SegmentClip> = clips.iter().map(|stored| stored.clip.clone()).collect();
    let projection =
        compact_market::checkpoint_projection_digest(&draft.entity, draft.period, &plain)
            .map_err(integrity)?;
    if let Some(basis) = basis {
        if basis.delta_ops.is_empty() {
            return Err(integrity("checkpoint basis requires at least one delta op"));
        }
        let (parent, parent_bars) = reconstruct_leaf(c, basis.parent_version_key)?;
        verify_published_node(c, &parent, &parent_bars)?;
        if parent.entity != draft.entity || parent.period != draft.period {
            return Err(integrity("checkpoint basis parent entity/period mismatch"));
        }
        if parent.delta_depth != MAX_DELTA_DEPTH {
            return Err(integrity(
                "checkpoint basis parent is not at delta depth bound",
            ));
        }
        let applied = apply_delta(
            c,
            &parent_bars,
            &basis.delta_ops,
            &draft.entity,
            draft.period,
        )?;
        if applied != bars {
            return Err(integrity(
                "checkpoint does not equal parent plus basis delta",
            ));
        }
    }
    check_draft_lineage(c, draft, &bars)?;
    check_draft_evidence(c, draft, &bars)?;
    let key = insert_version_row(c, draft, None, 1, 0, &bars, projection)?;
    if let Some(lineage) = &draft.yearly_lineage {
        insert_lineage_row(c, key, lineage)?;
    }
    let written = compact_v19::write_checkpoint(c, key, &draft.entity, draft.period, clips)?;
    if written != projection {
        return Err(integrity("persisted checkpoint digest mismatch"));
    }
    Ok(key)
}

pub(crate) fn stage_delta(
    c: &Connection,
    draft: &VersionDraft,
    parent_version_key: i64,
    ops: &[StoredDelta],
) -> Result<i64, Error> {
    check_draft_static(c, draft)?;
    if ops.is_empty() {
        return Err(integrity("delta version requires at least one op"));
    }
    let (parent, parent_bars) = reconstruct_leaf(c, parent_version_key)?;
    verify_published_node(c, &parent, &parent_bars)?;
    if parent.entity != draft.entity || parent.period != draft.period {
        return Err(integrity("delta parent entity/period mismatch"));
    }
    if parent.delta_depth >= MAX_DELTA_DEPTH {
        return Err(integrity("delta depth bound reached"));
    }
    let bars = apply_delta(c, &parent_bars, ops, &draft.entity, draft.period)?;
    if bars.is_empty() {
        return Err(integrity("delta projection is empty"));
    }
    let plain = delta_ops(ops);
    let (delta, projection) =
        compact_market::delta_projection_digest(parent.projection_digest, &plain)
            .map_err(integrity)?;
    check_draft_lineage(c, draft, &bars)?;
    check_draft_evidence(c, draft, &bars)?;
    let key = insert_version_row(
        c,
        draft,
        Some(parent_version_key),
        2,
        parent.delta_depth + 1,
        &bars,
        projection,
    )?;
    if let Some(lineage) = &draft.yearly_lineage {
        insert_lineage_row(c, key, lineage)?;
    }
    let (written_delta, written_projection) =
        compact_v19::write_delta(c, key, parent.projection_digest, ops)?;
    if written_delta != delta || written_projection != projection {
        return Err(integrity("persisted delta digest mismatch"));
    }
    Ok(key)
}

pub(crate) fn mark_validated(c: &Connection, version_key: i64) -> Result<(), Error> {
    let row = read_version_row(c, version_key)?.ok_or_else(|| integrity("version row missing"))?;
    if row.state != 1 {
        return Err(integrity("version is not staged"));
    }
    verify_staged(c, &row)?;
    let changed = c
        .execute(
            "UPDATE data_versions SET state=2 WHERE version_key=?1 AND state=1",
            params![version_key],
        )
        .map_err(storage)?;
    if changed != 1 {
        return Err(integrity("version state transition did not apply"));
    }
    Ok(())
}

pub(crate) fn publish(
    c: &Connection,
    version_key: i64,
    published_at: DateTime<Utc>,
) -> Result<Digest, Error> {
    let row = read_version_row(c, version_key)?.ok_or_else(|| integrity("version row missing"))?;
    if row.state != 2 {
        return Err(integrity("version is not validated"));
    }
    verify_staged(c, &row)?;
    let published = published_at.to_rfc3339();
    let identity = build_identity(c, &row, &published)?;
    let digest = compact_market::version_integrity_digest(&identity).map_err(integrity)?;
    let changed = c
        .execute(
            "UPDATE data_versions SET state=3,published_at=?2,integrity_digest=?3 WHERE version_key=?1 AND state=2 AND published_at IS NULL AND integrity_digest IS NULL",
            params![version_key, published, digest.as_slice()],
        )
        .map_err(storage)?;
    if changed != 1 {
        return Err(integrity("version publish transition did not apply"));
    }
    Ok(digest)
}

pub(crate) fn fixed(c: &Connection, version_id: &str) -> Result<VerifiedVersion, Error> {
    let row = read_version_row_by_id(c, version_id)?.ok_or(Error::NotFound)?;
    if row.state != 3 {
        return Err(Error::NotFound);
    }
    fixed_by_key(c, row.key)
}

pub(crate) fn range(
    c: &Connection,
    scope: &Scope,
    version_id: Option<&str>,
    range: &Range,
) -> Result<ReadPage, Error> {
    range.validate()?;
    let verified = match version_id {
        Some(id) => fixed(c, id)?,
        None => {
            let key: i64 = c
                .query_row(
                    "SELECT version_key FROM effective_heads WHERE entity=?1 AND period=?2 AND adjustment=1",
                    params![scope.symbol, period_code(scope.period)],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?
                .ok_or(Error::NotFound)?;
            fixed_by_key(c, key)?
        }
    };
    if verified.version.symbol != scope.symbol || verified.version.period != scope.period {
        return Err(integrity("version/scope mismatch"));
    }
    let mut rows: Vec<Bar> = verified
        .version
        .records
        .iter()
        .filter(|bar| {
            bar.date >= range.start
                && bar.date <= range.end
                && range.after.is_none_or(|after| bar.date > after)
        })
        .take(range.limit + 1)
        .cloned()
        .collect();
    let more = rows.len() > range.limit;
    rows.truncate(range.limit);
    let next_after = if more {
        rows.last().map(|bar| bar.date)
    } else {
        None
    };
    Ok(ReadPage {
        integrity_sha256: compact_market::digest_hex(&verified.integrity_digest),
        version: verified.version,
        rows,
        next_after,
    })
}

pub(crate) fn list(c: &Connection, scope: &Scope) -> Result<Vec<CompactVersionSummary>, Error> {
    let mut statement = c
        .prepare(
            "SELECT version_key FROM data_versions WHERE entity=?1 AND period=?2 AND adjustment=1 AND state=3 ORDER BY published_at DESC,version_key DESC",
        )
        .map_err(storage)?;
    let keys = statement
        .query_map(params![scope.symbol, period_code(scope.period)], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(statement);
    let mut summaries = Vec::with_capacity(keys.len());
    for key in keys {
        let row = read_version_row(c, key)?.ok_or_else(|| integrity("version row missing"))?;
        if row.state != 3 {
            return Err(integrity("published index references unpublished version"));
        }
        let evidence = verify_version_evidence(c, &row.evidence_digest)?;
        let _ = read_source_binding(c, &row.source_binding_digest)?;
        let lineage = verify_stored_lineage(c, &row)?;
        let published = row
            .published_at
            .clone()
            .ok_or_else(|| integrity("published version lacks published_at"))?;
        let integrity_digest = row
            .integrity_digest
            .ok_or_else(|| integrity("published version lacks integrity digest"))?;
        let identity = build_identity(c, &row, &published)?;
        if compact_market::version_integrity_digest(&identity).map_err(integrity)?
            != integrity_digest
        {
            return Err(integrity("version integrity digest mismatch"));
        }
        let yearly_derivation = match lineage {
            Some(l) => Some(YearlyDerivation {
                base_daily_version_id: l.base_daily_version_id,
                algorithm_version: l.algorithm_version,
                first_trading_date: compact_market::epoch_date(l.first_trade_day)
                    .map_err(integrity)?,
                last_trading_date: compact_market::epoch_date(l.last_trade_day)
                    .map_err(integrity)?,
            }),
            None => None,
        };
        summaries.push(CompactVersionSummary {
            version_id: row.version_id.clone(),
            entity: row.entity.clone(),
            period: row.period,
            published_at: DateTime::parse_from_rfc3339(&published)
                .map_err(integrity)?
                .with_timezone(&Utc),
            source: evidence.source,
            record_count: row.record_count as u64,
            yearly_derivation,
            integrity_sha256: compact_market::digest_hex(&integrity_digest),
        });
    }
    Ok(summaries)
}

struct VersionRow {
    key: i64,
    version_id: String,
    entity: String,
    period: Period,
    state: i64,
    source_binding_digest: Digest,
    parent_version_key: Option<i64>,
    manifest_mode: i64,
    delta_depth: i64,
    record_count: i64,
    first_day: i64,
    last_day: i64,
    coverage_status: i64,
    evidence_digest: Digest,
    projection_digest: Digest,
    integrity_digest: Option<Digest>,
    published_at: Option<String>,
}

fn read_version_row(c: &Connection, key: i64) -> Result<Option<VersionRow>, Error> {
    let row = c
        .query_row(
            "SELECT version_key,version_id,entity,period,state,source_binding_digest,parent_version_key,manifest_mode,delta_depth,record_count,first_day,last_day,coverage_status,evidence_digest,projection_digest,published_at,integrity_digest,adjustment,digest_format FROM data_versions WHERE version_key=?1",
            params![key],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Vec<u8>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, i64>(10)?,
                    r.get::<_, i64>(11)?,
                    r.get::<_, i64>(12)?,
                    r.get::<_, Vec<u8>>(13)?,
                    r.get::<_, Vec<u8>>(14)?,
                    r.get::<_, Option<String>>(15)?,
                    r.get::<_, Option<Vec<u8>>>(16)?,
                    r.get::<_, i64>(17)?,
                    r.get::<_, String>(18)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    let Some((
        key,
        version_id,
        entity,
        period_code,
        state,
        source_binding,
        parent_version_key,
        manifest_mode,
        delta_depth,
        record_count,
        first_day,
        last_day,
        coverage_status,
        evidence,
        projection,
        published_at,
        integrity_blob,
        adjustment,
        digest_format,
    )) = row
    else {
        return Ok(None);
    };
    if adjustment != 1 || digest_format != DIGEST_FORMAT {
        return Err(integrity("version adjustment/digest format mismatch"));
    }
    Ok(Some(VersionRow {
        key,
        version_id,
        entity,
        period: decode_period(period_code)?,
        state,
        source_binding_digest: fixed_digest(&source_binding)?,
        parent_version_key,
        manifest_mode,
        delta_depth,
        record_count,
        first_day,
        last_day,
        coverage_status,
        evidence_digest: fixed_digest(&evidence)?,
        projection_digest: fixed_digest(&projection)?,
        integrity_digest: integrity_blob.map(|blob| fixed_digest(&blob)).transpose()?,
        published_at,
    }))
}

fn read_version_row_by_id(c: &Connection, id: &str) -> Result<Option<VersionRow>, Error> {
    let key: Option<i64> = c
        .query_row(
            "SELECT version_key FROM data_versions WHERE version_id=?1",
            params![id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    match key {
        Some(key) => read_version_row(c, key),
        None => Ok(None),
    }
}

fn fixed_digest(raw: &[u8]) -> Result<Digest, Error> {
    raw.try_into()
        .map_err(|_| integrity("stored digest length mismatch"))
}

fn fixed_by_key(c: &Connection, key: i64) -> Result<VerifiedVersion, Error> {
    let chain = reconstruct_version(c, key)?;
    let mut leaf_evidence = None;
    for (row, bars) in &chain {
        leaf_evidence = Some(verify_published_node(c, row, bars)?);
    }
    let (leaf, bars) = chain.last().unwrap();
    let version = to_effective_version(c, leaf, bars, leaf_evidence.unwrap())?;
    Ok(VerifiedVersion {
        key: leaf.key,
        integrity_digest: leaf
            .integrity_digest
            .ok_or_else(|| integrity("published version lacks integrity digest"))?,
        version,
    })
}

fn reconstruct_leaf(c: &Connection, key: i64) -> Result<(VersionRow, Vec<CompactBar>), Error> {
    let mut chain = reconstruct_version(c, key)?;
    let (row, bars) = chain.pop().unwrap();
    Ok((row, bars))
}

fn reconstruct_version(
    c: &Connection,
    leaf_key: i64,
) -> Result<Vec<(VersionRow, Vec<CompactBar>)>, Error> {
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut key = leaf_key;
    loop {
        if !visited.insert(key) {
            return Err(integrity("version chain contains a cycle"));
        }
        let row =
            read_version_row(c, key)?.ok_or_else(|| integrity("version chain node missing"))?;
        match row.manifest_mode {
            1 => {
                if row.parent_version_key.is_some() || row.delta_depth != 0 {
                    return Err(integrity("checkpoint version carries parent/depth"));
                }
                chain.push(row);
                break;
            }
            2 => {
                if row.delta_depth < 1 || row.delta_depth > MAX_DELTA_DEPTH {
                    return Err(integrity("delta depth out of bounds"));
                }
                let parent = row
                    .parent_version_key
                    .ok_or_else(|| integrity("delta version lacks parent"))?;
                chain.push(row);
                key = parent;
            }
            _ => return Err(integrity("unknown manifest_mode")),
        }
        if chain.len() > MAX_CHAIN_NODES {
            return Err(integrity("version chain exceeds depth bound"));
        }
    }
    chain.reverse();
    let mut projections: Vec<(VersionRow, Vec<CompactBar>)> = Vec::with_capacity(chain.len());
    let mut bars: Vec<CompactBar> = Vec::new();
    for (index, row) in chain.into_iter().enumerate() {
        if index == 0 {
            let (clips, projection) =
                compact_v19::read_checkpoint(c, row.key, &row.entity, row.period)?;
            if projection != row.projection_digest {
                return Err(integrity("checkpoint projection digest mismatch"));
            }
            bars = flatten_clips(c, &row.entity, row.period, &clips)?;
        } else {
            let (parent_row, _) = projections.last().unwrap();
            if row.parent_version_key != Some(parent_row.key)
                || row.delta_depth != parent_row.delta_depth + 1
                || row.entity != parent_row.entity
                || row.period != parent_row.period
            {
                return Err(integrity("version chain linkage mismatch"));
            }
            if parent_row.state != 3 {
                return Err(integrity("delta parent is not published"));
            }
            let (ops, _, projection) =
                compact_v19::read_delta(c, row.key, parent_row.projection_digest)?;
            if projection != row.projection_digest {
                return Err(integrity("delta projection digest mismatch"));
            }
            bars = apply_delta(c, &bars, &ops, &row.entity, row.period)?;
            if bars.is_empty() {
                return Err(integrity("delta projection is empty"));
            }
        }
        projections.push((row, bars.clone()));
    }
    Ok(projections)
}

fn verify_staged(c: &Connection, leaf: &VersionRow) -> Result<(), Error> {
    let chain = reconstruct_version(c, leaf.key)?;
    for (index, (row, bars)) in chain.iter().enumerate() {
        verify_node_common(c, row, bars)?;
        if index + 1 < chain.len() {
            verify_published_fields(c, row)?;
        }
    }
    Ok(())
}

fn verify_published_node(
    c: &Connection,
    row: &VersionRow,
    bars: &[CompactBar],
) -> Result<VersionEvidence, Error> {
    verify_published_fields(c, row)?;
    verify_node_common(c, row, bars)
}

fn verify_published_fields(c: &Connection, row: &VersionRow) -> Result<(), Error> {
    if row.state != 3 {
        return Err(integrity("version chain parent is not published"));
    }
    let published = row
        .published_at
        .clone()
        .ok_or_else(|| integrity("published version lacks published_at"))?;
    let integrity_digest = row
        .integrity_digest
        .ok_or_else(|| integrity("published version lacks integrity digest"))?;
    let identity = build_identity(c, row, &published)?;
    if compact_market::version_integrity_digest(&identity).map_err(integrity)? != integrity_digest {
        return Err(integrity("version integrity digest mismatch"));
    }
    Ok(())
}

fn verify_node_common(
    c: &Connection,
    row: &VersionRow,
    bars: &[CompactBar],
) -> Result<VersionEvidence, Error> {
    let evidence = verify_version_evidence(c, &row.evidence_digest)?;
    let _ = read_source_binding(c, &row.source_binding_digest)?;
    check_bars_units(&evidence, bars)?;
    if bars.is_empty()
        || bars.len() as i64 != row.record_count
        || bars.first().unwrap().trade_day != row.first_day
        || bars.last().unwrap().trade_day != row.last_day
    {
        return Err(integrity("version metadata does not match projection"));
    }
    match row.period {
        Period::Day => {
            if stored_lineage(c, row.key)?.is_some() {
                return Err(integrity("daily version carries yearly lineage"));
            }
        }
        Period::Year => {
            let lineage = stored_lineage(c, row.key)?
                .ok_or_else(|| integrity("yearly version lacks lineage"))?;
            verify_lineage(
                c,
                &row.entity,
                row.period,
                &row.source_binding_digest,
                bars,
                &lineage,
            )?;
        }
    }
    Ok(evidence)
}

fn verify_version_evidence(c: &Connection, digest: &Digest) -> Result<VersionEvidence, Error> {
    let (kind, canonical) = read_evidence(c, digest)?;
    if kind != EvidenceKind::Version {
        return Err(integrity("version evidence kind mismatch"));
    }
    serde_json::from_str(&canonical).map_err(integrity)
}

fn verify_lineage(
    c: &Connection,
    entity: &str,
    period: Period,
    source_binding: &Digest,
    bars: &[CompactBar],
    lineage: &YearlyLineageDraft,
) -> Result<YearlyLineageIdentity, Error> {
    let base = read_version_row(c, lineage.base_daily_version_key)?
        .ok_or_else(|| integrity("yearly base version missing"))?;
    if base.state != 3 {
        return Err(integrity("yearly base version is not published"));
    }
    if base.period != Period::Day || base.entity != entity {
        return Err(integrity("yearly base entity/period mismatch"));
    }
    if base.source_binding_digest != *source_binding {
        return Err(integrity("yearly source binding differs from base"));
    }
    if lineage.algorithm_version != crate::yearly::ALGORITHM_VERSION {
        return Err(integrity("yearly lineage algorithm mismatch"));
    }
    if lineage.first_trade_day > lineage.last_trade_day
        || lineage.last_trade_day > lineage.as_of_day
    {
        return Err(integrity("yearly lineage bounds inverted"));
    }
    compact_market::epoch_date(lineage.first_trade_day).map_err(integrity)?;
    compact_market::epoch_date(lineage.last_trade_day).map_err(integrity)?;
    let as_of = compact_market::epoch_date(lineage.as_of_day).map_err(integrity)?;
    let base_version = fixed_by_key(c, lineage.base_daily_version_key)?.version;
    let records = bars
        .iter()
        .map(|bar| bar.to_bar(period))
        .collect::<Result<Vec<_>, _>>()
        .map_err(integrity)?;
    let years: BTreeSet<i32> = records.iter().map(|bar| bar.date.year()).collect();
    let annual = crate::yearly::aggregate(&base_version, Some(&years), as_of)
        .map_err(|e| integrity(format!("yearly aggregation failed: {e}")))?;
    if annual
        .records
        .iter()
        .map(|record| record.bar.clone())
        .collect::<Vec<_>>()
        != records
    {
        return Err(integrity(
            "yearly projection does not match base aggregation",
        ));
    }
    if annual
        .records
        .first()
        .map(|record| record.lineage.first_trading_date)
        != Some(compact_market::epoch_date(lineage.first_trade_day).map_err(integrity)?)
        || annual
            .records
            .last()
            .map(|record| record.lineage.last_trading_date)
            != Some(compact_market::epoch_date(lineage.last_trade_day).map_err(integrity)?)
    {
        return Err(integrity("yearly lineage bounds do not match aggregation"));
    }
    Ok(YearlyLineageIdentity {
        base_daily_version_id: base.version_id.clone(),
        algorithm_version: lineage.algorithm_version.clone(),
        first_trade_day: lineage.first_trade_day,
        last_trade_day: lineage.last_trade_day,
        as_of_day: lineage.as_of_day,
    })
}

fn stored_lineage(c: &Connection, key: i64) -> Result<Option<YearlyLineageDraft>, Error> {
    c.query_row(
        "SELECT base_daily_version_key,algorithm_version,first_trade_day,last_trade_day,as_of_day FROM yearly_lineage WHERE version_key=?1",
        params![key],
        |r| {
            Ok(YearlyLineageDraft {
                base_daily_version_key: r.get(0)?,
                algorithm_version: r.get(1)?,
                first_trade_day: r.get(2)?,
                last_trade_day: r.get(3)?,
                as_of_day: r.get(4)?,
            })
        },
    )
    .optional()
    .map_err(storage)
}

fn verify_stored_lineage(
    c: &Connection,
    row: &VersionRow,
) -> Result<Option<YearlyLineageIdentity>, Error> {
    match row.period {
        Period::Day => {
            if stored_lineage(c, row.key)?.is_some() {
                return Err(integrity("daily version carries yearly lineage"));
            }
            Ok(None)
        }
        Period::Year => {
            let lineage = stored_lineage(c, row.key)?
                .ok_or_else(|| integrity("yearly version lacks lineage"))?;
            let base = read_version_row(c, lineage.base_daily_version_key)?
                .ok_or_else(|| integrity("yearly base version missing"))?;
            if base.state != 3
                || base.period != Period::Day
                || base.entity != row.entity
                || base.source_binding_digest != row.source_binding_digest
                || lineage.algorithm_version != crate::yearly::ALGORITHM_VERSION
                || lineage.first_trade_day > lineage.last_trade_day
                || lineage.last_trade_day > lineage.as_of_day
            {
                return Err(integrity("yearly lineage metadata mismatch"));
            }
            compact_market::epoch_date(lineage.first_trade_day).map_err(integrity)?;
            compact_market::epoch_date(lineage.last_trade_day).map_err(integrity)?;
            compact_market::epoch_date(lineage.as_of_day).map_err(integrity)?;
            Ok(Some(YearlyLineageIdentity {
                base_daily_version_id: base.version_id.clone(),
                algorithm_version: lineage.algorithm_version,
                first_trade_day: lineage.first_trade_day,
                last_trade_day: lineage.last_trade_day,
                as_of_day: lineage.as_of_day,
            }))
        }
    }
}

fn build_identity(
    c: &Connection,
    row: &VersionRow,
    published_at: &str,
) -> Result<VersionIdentity, Error> {
    let parent_version_id = match row.parent_version_key {
        Some(key) => {
            let parent =
                read_version_row(c, key)?.ok_or_else(|| integrity("parent version missing"))?;
            if parent.state != 3
                || parent.entity != row.entity
                || parent.period != row.period
                || parent.delta_depth + 1 != row.delta_depth
            {
                return Err(integrity("version parent linkage mismatch"));
            }
            Some(parent.version_id)
        }
        None => None,
    };
    let yearly_lineage = verify_stored_lineage(c, row)?;
    let manifest_mode = match row.manifest_mode {
        1 => ManifestMode::Checkpoint,
        2 => ManifestMode::Delta,
        _ => return Err(integrity("unknown manifest_mode")),
    };
    let coverage_status = match row.coverage_status {
        1 => CoverageStatus::Complete,
        2 => CoverageStatus::Incomplete,
        3 => CoverageStatus::Unknown,
        _ => return Err(integrity("unknown coverage_status")),
    };
    Ok(VersionIdentity {
        version_id: row.version_id.clone(),
        entity: row.entity.clone(),
        period: row.period,
        source_binding_digest: row.source_binding_digest,
        parent_version_id,
        manifest_mode,
        delta_depth: row
            .delta_depth
            .try_into()
            .map_err(|_| integrity("delta_depth out of u32 range"))?,
        projection_digest: row.projection_digest,
        record_count: row
            .record_count
            .try_into()
            .map_err(|_| integrity("record_count out of u64 range"))?,
        first_day: row.first_day,
        last_day: row.last_day,
        coverage_status,
        evidence_digest: row.evidence_digest,
        published_at: published_at.to_string(),
        yearly_lineage,
    })
}

fn to_effective_version(
    c: &Connection,
    row: &VersionRow,
    bars: &[CompactBar],
    evidence: VersionEvidence,
) -> Result<EffectiveVersion, Error> {
    let records = bars
        .iter()
        .map(|bar| bar.to_bar(row.period))
        .collect::<Result<Vec<_>, _>>()
        .map_err(integrity)?;
    let yearly_derivation = match verify_stored_lineage(c, row)? {
        Some(l) => Some(YearlyDerivation {
            base_daily_version_id: l.base_daily_version_id,
            algorithm_version: l.algorithm_version,
            first_trading_date: compact_market::epoch_date(l.first_trade_day).map_err(integrity)?,
            last_trading_date: compact_market::epoch_date(l.last_trade_day).map_err(integrity)?,
        }),
        None => None,
    };
    let published_at = DateTime::parse_from_rfc3339(
        row.published_at
            .as_deref()
            .ok_or_else(|| integrity("published version lacks published_at"))?,
    )
    .map_err(integrity)?
    .with_timezone(&Utc);
    Ok(EffectiveVersion {
        version_id: row.version_id.clone(),
        symbol: row.entity.clone(),
        period: row.period,
        adjustment: Adjustment::Forward,
        source: evidence.source,
        records,
        published_at,
        validation: ValidationSummary {
            record_count: bars.len(),
            unique_record_count: bars.len(),
            duplicate_count: 0,
        },
        unit_currency: evidence.unit_currency,
        adjustment_evidence: evidence.adjustment_evidence,
        yearly_derivation,
    })
}

fn check_draft_static(c: &Connection, draft: &VersionDraft) -> Result<(), Error> {
    if draft.version_id.is_empty() {
        return Err(integrity("version_id is empty"));
    }
    compact_market::canonical_entity(&draft.entity).map_err(integrity)?;
    let created = DateTime::parse_from_rfc3339(&draft.created_at)
        .map_err(|_| integrity("created_at is not RFC 3339"))?;
    if created.offset().local_minus_utc() != 0
        || created.with_timezone(&Utc).to_rfc3339() != draft.created_at
    {
        return Err(integrity("created_at is not canonical UTC RFC 3339"));
    }
    match (&draft.period, &draft.yearly_lineage) {
        (Period::Day, None) | (Period::Year, Some(_)) => {}
        _ => return Err(integrity("draft period/lineage mismatch")),
    }
    let exists: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM data_versions WHERE version_id=?1)",
            params![draft.version_id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if exists {
        return Err(Error::Conflict("version_id already exists".into()));
    }
    Ok(())
}

fn check_draft_evidence(
    c: &Connection,
    draft: &VersionDraft,
    bars: &[CompactBar],
) -> Result<VersionEvidence, Error> {
    let evidence = verify_version_evidence(c, &draft.evidence_digest)?;
    let _ = read_source_binding(c, &draft.source_binding_digest)?;
    check_bars_units(&evidence, bars)?;
    Ok(evidence)
}

fn check_bars_units(evidence: &VersionEvidence, bars: &[CompactBar]) -> Result<(), Error> {
    let unit = &evidence.unit_currency;
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
    Ok(())
}

fn check_draft_lineage(
    c: &Connection,
    draft: &VersionDraft,
    bars: &[CompactBar],
) -> Result<(), Error> {
    if draft.period != Period::Year {
        return Ok(());
    }
    let lineage = draft
        .yearly_lineage
        .as_ref()
        .ok_or_else(|| integrity("yearly draft lacks lineage"))?;
    verify_lineage(
        c,
        &draft.entity,
        draft.period,
        &draft.source_binding_digest,
        bars,
        lineage,
    )?;
    Ok(())
}

fn insert_version_row(
    c: &Connection,
    draft: &VersionDraft,
    parent_version_key: Option<i64>,
    manifest_mode: i64,
    delta_depth: i64,
    bars: &[CompactBar],
    projection: Digest,
) -> Result<i64, Error> {
    let coverage = match draft.coverage_status {
        CoverageStatus::Complete => 1,
        CoverageStatus::Incomplete => 2,
        CoverageStatus::Unknown => 3,
    };
    c.execute(
        "INSERT INTO data_versions(version_id,entity,period,adjustment,state,source_binding_digest,parent_version_key,manifest_mode,delta_depth,record_count,first_day,last_day,coverage_status,evidence_digest,projection_digest,integrity_digest,digest_format,created_at,published_at) VALUES(?1,?2,?3,1,1,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,NULL,?14,?15,NULL)",
        params![
            draft.version_id,
            draft.entity,
            period_code(draft.period),
            draft.source_binding_digest.as_slice(),
            parent_version_key,
            manifest_mode,
            delta_depth,
            bars.len() as i64,
            bars.first().map(|b| b.trade_day).unwrap_or(0),
            bars.last().map(|b| b.trade_day).unwrap_or(0),
            coverage,
            draft.evidence_digest.as_slice(),
            projection.as_slice(),
            DIGEST_FORMAT,
            draft.created_at
        ],
    )
    .map_err(storage)?;
    Ok(c.last_insert_rowid())
}

fn insert_lineage_row(
    c: &Connection,
    version_key: i64,
    lineage: &YearlyLineageDraft,
) -> Result<(), Error> {
    c.execute(
        "INSERT INTO yearly_lineage(version_key,base_daily_version_key,algorithm_version,first_trade_day,last_trade_day,as_of_day) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            version_key,
            lineage.base_daily_version_key,
            lineage.algorithm_version,
            lineage.first_trade_day,
            lineage.last_trade_day,
            lineage.as_of_day
        ],
    )
    .map_err(storage)?;
    Ok(())
}

fn read_evidence(c: &Connection, digest: &Digest) -> Result<(EvidenceKind, String), Error> {
    let row = c
        .query_row(
            "SELECT evidence_kind,canonical_json FROM evidence_bundles WHERE evidence_digest=?1",
            params![digest.as_slice()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(|| integrity("evidence bundle missing"))?;
    let kind = parse_evidence_kind(&row.0)?;
    compact_market::verify_evidence(kind, &row.1, *digest).map_err(integrity)?;
    Ok((kind, row.1))
}

fn read_source_binding(c: &Connection, digest: &Digest) -> Result<SourceBinding, Error> {
    let row = c
        .query_row(
            "SELECT generation_id,generation_manifest_sha256,day_member_sha256,security_event_sha256,evidence_digest FROM source_bindings WHERE source_binding_digest=?1",
            params![digest.as_slice()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(|| integrity("source binding missing"))?;
    let binding = SourceBinding {
        generation_id: row.0,
        generation_manifest_sha256: fixed_digest(&row.1)?,
        day_member_sha256: fixed_digest(&row.2)?,
        security_event_sha256: fixed_digest(&row.3)?,
        evidence_digest: fixed_digest(&row.4)?,
    };
    if compact_market::source_binding_digest(&binding).map_err(integrity)? != *digest {
        return Err(integrity("source binding digest mismatch"));
    }
    let (kind, _) = read_evidence(c, &binding.evidence_digest)?;
    if kind != EvidenceKind::Generation {
        return Err(integrity("source binding evidence is not generation"));
    }
    Ok(binding)
}

fn check_stored_clip(
    segment: &StoredSegment,
    clip: &SegmentClip,
    entity: &str,
    period: Period,
) -> Result<(), Error> {
    if segment.entity != entity || segment.period != period {
        return Err(integrity("clip segment entity/period mismatch"));
    }
    if clip.segment_digest != segment.digest {
        return Err(integrity("clip segment digest mismatch"));
    }
    let (Some(first), Some(last)) = (segment.bars.first(), segment.bars.last()) else {
        return Err(integrity("segment is empty"));
    };
    if clip.clip_start_day < first.trade_day || clip.clip_end_day > last.trade_day {
        return Err(integrity("clip bounds outside segment"));
    }
    let actual = segment
        .bars
        .iter()
        .filter(|bar| bar.trade_day >= clip.clip_start_day && bar.trade_day <= clip.clip_end_day)
        .count();
    if actual as u32 != clip.row_count {
        return Err(integrity("clip row_count does not match segment rows"));
    }
    Ok(())
}

fn flatten_clips(
    c: &Connection,
    entity: &str,
    period: Period,
    clips: &[StoredClip],
) -> Result<Vec<CompactBar>, Error> {
    if clips.is_empty() {
        return Err(integrity("checkpoint requires at least one clip"));
    }
    let mut bars = Vec::new();
    let mut previous_end: Option<i64> = None;
    for stored in clips {
        let segment = compact_v19::read_segment(c, stored.segment_key)?;
        check_stored_clip(&segment, &stored.clip, entity, period)?;
        if previous_end.is_some_and(|end| stored.clip.clip_start_day <= end) {
            return Err(integrity("checkpoint clips overlap or unordered"));
        }
        previous_end = Some(stored.clip.clip_end_day);
        bars.extend(
            segment
                .bars
                .iter()
                .filter(|bar| {
                    bar.trade_day >= stored.clip.clip_start_day
                        && bar.trade_day <= stored.clip.clip_end_day
                })
                .cloned(),
        );
    }
    if bars.is_empty() {
        return Err(integrity("checkpoint projection is empty"));
    }
    Ok(bars)
}

fn delta_ops(ops: &[StoredDelta]) -> Vec<DeltaOp> {
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

fn apply_delta(
    c: &Connection,
    base: &[CompactBar],
    ops: &[StoredDelta],
    entity: &str,
    period: Period,
) -> Result<Vec<CompactBar>, Error> {
    let plain = delta_ops(ops);
    if !plain.is_empty() {
        compact_market::delta_digest(&plain).map_err(integrity)?;
    }
    let mut map: BTreeMap<i64, CompactBar> = base
        .iter()
        .map(|bar| (bar.trade_day, bar.clone()))
        .collect();
    for op in ops {
        match &op.kind {
            StoredDeltaKind::Upsert(stored) => {
                let segment = compact_v19::read_segment(c, stored.segment_key)?;
                check_stored_clip(&segment, &stored.clip, entity, period)?;
                for bar in segment.bars.iter().filter(|bar| {
                    bar.trade_day >= stored.clip.clip_start_day
                        && bar.trade_day <= stored.clip.clip_end_day
                }) {
                    map.insert(bar.trade_day, bar.clone());
                }
            }
            StoredDeltaKind::Remove => {
                map.retain(|day, _| !(*day >= op.range_start_day && *day <= op.range_end_day));
            }
        }
    }
    Ok(map.into_values().collect())
}

fn parse_evidence_kind(raw: &str) -> Result<EvidenceKind, Error> {
    Ok(match raw {
        "generation" => EvidenceKind::Generation,
        "batch_selection" => EvidenceKind::BatchSelection,
        "version" => EvidenceKind::Version,
        "continuity" => EvidenceKind::Continuity,
        "catalog" => EvidenceKind::Catalog,
        "yearly" => EvidenceKind::Yearly,
        _ => return Err(integrity("unknown evidence kind")),
    })
}

fn period_code(period: Period) -> i64 {
    match period {
        Period::Day => 1,
        Period::Year => 2,
    }
}

fn decode_period(code: i64) -> Result<Period, Error> {
    match code {
        1 => Ok(Period::Day),
        2 => Ok(Period::Year),
        _ => Err(integrity("unknown period code")),
    }
}

#[cfg(test)]
mod tests;
