use serde_json::{Value, json};
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

/// Top-level fields of a `version` evidence document whose contents are
/// identical for every version resolved from one generation (#259 P0-D).
/// Each is stored once as a `shared` sub-bundle and replaced by an
/// `$evref` marker in the parent's stored form; `read_evidence` re-inlines
/// them so digests and canonical bytes are unchanged externally.
const SHARED_VERSION_FIELDS: [&str; 2] = ["unit_currency", "adjustment_evidence"];
const EVREF: &str = "$evref";

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
    if let Some((kind_raw, stored)) = existing {
        let stored_kind = parse_evidence_kind(&kind_raw)?;
        let canonical = inflate_evidence(c, stored_kind, &stored)?;
        compact_market::verify_evidence(stored_kind, &canonical, bundle.digest)
            .map_err(integrity)?;
        if stored_kind != kind || canonical != bundle.canonical_json {
            return Err(integrity(
                "evidence digest collision with different content",
            ));
        }
        return Ok(bundle.digest);
    }
    let stored_form = decompose_evidence(c, kind, &bundle.canonical_json)?;
    c.execute(
        "INSERT INTO evidence_bundles(evidence_digest,evidence_kind,canonical_json) VALUES(?1,?2,?3)",
        params![bundle.digest.as_slice(), kind.as_str(), stored_form],
    )
    .map_err(storage)?;
    Ok(bundle.digest)
}

/// Replace decomposable fields with `$evref` markers pointing at `shared`
/// sub-bundle rows. Returns the stored form (full canonical unchanged for
/// kinds without decomposable fields).
fn decompose_evidence(
    c: &Connection,
    kind: EvidenceKind,
    canonical: &str,
) -> Result<String, Error> {
    if kind != EvidenceKind::Version {
        return Ok(canonical.to_string());
    }
    let mut doc: Value = serde_json::from_str(canonical).map_err(integrity)?;
    let Some(map) = doc.as_object_mut() else {
        return Err(integrity("version evidence is not an object"));
    };
    for field in SHARED_VERSION_FIELDS {
        let Some(existing) = map.get(field) else {
            continue;
        };
        if !existing.is_object() || existing.get(EVREF).is_some() {
            continue;
        }
        let sub = compact_market::canonical_evidence(EvidenceKind::Shared, existing)
            .map_err(integrity)?;
        store_shared(c, &sub)?;
        map.insert(
            field.to_string(),
            json!({EVREF: compact_market::digest_hex(&sub.digest)}),
        );
    }
    serde_json::to_string(&doc).map_err(integrity)
}

/// Insert a `shared` sub-document row verbatim, or verify an identical row
/// already stored under its digest. Part rows live in `evidence_parts` and
/// are never decomposed, so marker expansion cannot recurse.
fn store_shared(c: &Connection, bundle: &compact_market::EvidenceBundle) -> Result<(), Error> {
    let existing = c
        .query_row(
            "SELECT canonical_json FROM evidence_parts WHERE part_digest=?1",
            params![bundle.digest.as_slice()],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(storage)?;
    if let Some(canonical) = existing {
        compact_market::verify_evidence(EvidenceKind::Shared, &canonical, bundle.digest)
            .map_err(integrity)?;
        if canonical != bundle.canonical_json {
            return Err(integrity(
                "evidence digest collision with different content",
            ));
        }
        return Ok(());
    }
    c.execute(
        "INSERT INTO evidence_parts(part_digest,canonical_json) VALUES(?1,?2)",
        params![bundle.digest.as_slice(), bundle.canonical_json],
    )
    .map_err(storage)?;
    Ok(())
}

/// Re-inline `$evref` markers back into a stored evidence document and
/// return its canonical form. Kinds without decomposable fields — and rows
/// written before decomposition existed — pass through unchanged.
fn inflate_evidence(c: &Connection, kind: EvidenceKind, stored: &str) -> Result<String, Error> {
    if kind != EvidenceKind::Version {
        return Ok(stored.to_string());
    }
    let mut doc: Value = serde_json::from_str(stored).map_err(integrity)?;
    let Some(map) = doc.as_object_mut() else {
        return Err(integrity("stored version evidence is not an object"));
    };
    for field in SHARED_VERSION_FIELDS {
        let Some(marker) = map
            .get(field)
            .and_then(|v| v.get(EVREF))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let digest = digest_from_hex(marker)?;
        let canonical: String = c
            .query_row(
                "SELECT canonical_json FROM evidence_parts WHERE part_digest=?1",
                params![digest.as_slice()],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| integrity("shared evidence part missing"))?;
        compact_market::verify_evidence(EvidenceKind::Shared, &canonical, digest)
            .map_err(integrity)?;
        map.insert(
            field.to_string(),
            serde_json::from_str(&canonical).map_err(integrity)?,
        );
    }
    serde_json_canonicalizer::to_string(&doc).map_err(|_| integrity("evidence not canonicalizable"))
}

fn digest_from_hex(hex: &str) -> Result<Digest, Error> {
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| integrity("malformed shared evidence reference"))?;
    bytes
        .try_into()
        .map_err(|_| integrity("malformed shared evidence reference"))
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

/// O(ops) append-only delta staging (#259 S5G-8): every op must be a
/// pure upsert strictly after the parent's last day, so the result
/// metadata is arithmetic over the parent row and the projection digest
/// chains O(ops) off `parent.projection_digest` — no parent leaf
/// materialization, no full merge. The parent's committed digests are
/// the binding fact (`verify_published_fields` re-checks its row-level
/// integrity); deep leaf re-verification stays with reads and the
/// integrity verifier. Non-append shapes must use `stage_delta`.
pub(crate) fn stage_delta_append(
    c: &Connection,
    draft: &VersionDraft,
    parent_version_key: i64,
    ops: &[StoredDelta],
    new_bars: &[CompactBar],
) -> Result<i64, Error> {
    check_draft_static(c, draft)?;
    if ops.is_empty() {
        return Err(integrity("delta version requires at least one op"));
    }
    if draft.period != Period::Day {
        // Yearly staging needs lineage verification over the full leaf.
        return Err(integrity("append fast path is daily-only"));
    }
    let parent = read_version_row(c, parent_version_key)?
        .ok_or_else(|| integrity("delta parent version row missing"))?;
    verify_published_fields(c, &parent)?;
    if parent.entity != draft.entity || parent.period != draft.period {
        return Err(integrity("delta parent entity/period mismatch"));
    }
    if parent.delta_depth >= MAX_DELTA_DEPTH {
        return Err(integrity("delta depth bound reached"));
    }
    let mut added: i64 = 0;
    let mut prev_end = parent.last_day;
    for op in ops {
        let StoredDeltaKind::Upsert(stored) = &op.kind else {
            return Err(integrity("append delta carries a non-upsert op"));
        };
        if op.range_start_day <= prev_end
            || op.range_start_day > op.range_end_day
            || op.range_start_day != stored.clip.clip_start_day
            || op.range_end_day != stored.clip.clip_end_day
        {
            return Err(integrity("append ops overlap the base or each other"));
        }
        let segment = compact_v19::read_segment(c, stored.segment_key)?;
        check_stored_clip(&segment, &stored.clip, &draft.entity, draft.period)?;
        added = added
            .checked_add(i64::from(stored.clip.row_count))
            .ok_or_else(|| integrity("append record_count overflow"))?;
        prev_end = op.range_end_day;
    }
    let plain = delta_ops(ops);
    let (delta, projection) =
        compact_market::delta_projection_digest(parent.projection_digest, &plain)
            .map_err(integrity)?;
    check_draft_evidence(c, draft, new_bars)?;
    let key = insert_version_row_stats(
        c,
        draft,
        Some(parent_version_key),
        2,
        parent.delta_depth + 1,
        parent.record_count + added,
        parent.first_day,
        prev_end,
        projection,
    )?;
    let (written_delta, written_projection) =
        compact_v19::write_delta(c, key, parent.projection_digest, ops)?;
    if written_delta != delta || written_projection != projection {
        return Err(integrity("persisted delta digest mismatch"));
    }
    Ok(key)
}

/// O(ops) append-only delta verification (#259 S5G-8): decodes the
/// delta's own ops, re-checks clip/segment digests, the chained
/// projection and the arithmetic metadata against the parent row, plus
/// the parent's row-level published integrity. Ancestors stay trusted
/// as committed facts — reads and the integrity verifier re-verify the
/// full leaf. `Ok(false)` means the node is not an append-only delta;
/// the caller must run full chain verification.
pub(crate) fn verify_delta_append(c: &Connection, version_key: i64) -> Result<bool, Error> {
    let row = read_version_row(c, version_key)?.ok_or_else(|| integrity("version row missing"))?;
    if row.manifest_mode != 2 {
        return Ok(false);
    }
    let parent_key = row
        .parent_version_key
        .ok_or_else(|| integrity("delta version lacks parent"))?;
    let parent =
        read_version_row(c, parent_key)?.ok_or_else(|| integrity("delta parent missing"))?;
    let (ops, _, projection) = compact_v19::read_delta(c, row.key, parent.projection_digest)?;
    let mut added: i64 = 0;
    let mut prev_end = parent.last_day;
    for op in &ops {
        let StoredDeltaKind::Upsert(stored) = &op.kind else {
            return Ok(false);
        };
        if op.range_start_day <= prev_end
            || op.range_start_day > op.range_end_day
            || op.range_start_day != stored.clip.clip_start_day
            || op.range_end_day != stored.clip.clip_end_day
        {
            return Ok(false);
        }
        added += i64::from(stored.clip.row_count);
        prev_end = op.range_end_day;
    }
    verify_published_fields(c, &parent)?;
    if row.delta_depth != parent.delta_depth + 1
        || row.entity != parent.entity
        || row.period != parent.period
    {
        return Err(integrity("version chain linkage mismatch"));
    }
    if projection != row.projection_digest {
        return Err(integrity("delta projection digest mismatch"));
    }
    if row.record_count != parent.record_count + added
        || row.first_day != parent.first_day
        || row.last_day != prev_end
    {
        return Err(integrity(
            "version metadata does not match append projection",
        ));
    }
    let evidence = verify_version_evidence(c, &row.evidence_digest)?;
    let _ = read_source_binding(c, &row.source_binding_digest)?;
    let mut new_bars = Vec::new();
    for op in &ops {
        let StoredDeltaKind::Upsert(stored) = &op.kind else {
            unreachable!("checked above")
        };
        let segment = compact_v19::read_segment(c, stored.segment_key)?;
        check_stored_clip(&segment, &stored.clip, &row.entity, row.period)?;
        new_bars.extend(
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
    check_bars_units(&evidence, &new_bars)?;
    if row.period == Period::Day && stored_lineage(c, row.key)?.is_some() {
        return Err(integrity("daily version carries yearly lineage"));
    }
    Ok(true)
}

/// Validation for an append-only delta staged by `stage_delta_append`
/// (#259 S5G-8): the O(ops) node check instead of the full chain
/// replay. Falls back to `verify_staged` for any other delta shape.
pub(crate) fn mark_validated_delta(c: &Connection, version_key: i64) -> Result<(), Error> {
    let row = read_version_row(c, version_key)?.ok_or_else(|| integrity("version row missing"))?;
    if row.state != 1 {
        return Err(integrity("version is not staged"));
    }
    if !verify_delta_append(c, version_key)? {
        verify_staged(c, &row)?;
    }
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

/// Publish for an append-only delta (#259 S5G-8): O(ops) node check then
/// `publish_presumed`. `Ok(None)` means the staged node is not an
/// append-only delta — the caller must run the full `publish` +
/// leaf-projection verification path instead.
pub(crate) fn publish_delta(
    c: &Connection,
    version_key: i64,
    published_at: DateTime<Utc>,
) -> Result<Option<Digest>, Error> {
    let row = read_version_row(c, version_key)?.ok_or_else(|| integrity("version row missing"))?;
    if row.state != 2 {
        return Err(integrity("version is not validated"));
    }
    if !verify_delta_append(c, version_key)? {
        return Ok(None);
    }
    Ok(Some(publish_presumed(c, version_key, published_at)?))
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
    publish_presumed(c, version_key, published_at)
}

/// Publish transition without re-verifying the stored chain: only valid
/// when the caller verified the same facts inside this transaction (#259
/// S5F batch staging — the just-written rows cannot change within the tx).
pub(crate) fn publish_presumed(
    c: &Connection,
    version_key: i64,
    published_at: DateTime<Utc>,
) -> Result<Digest, Error> {
    let row = read_version_row(c, version_key)?.ok_or_else(|| integrity("version row missing"))?;
    if row.state != 2 {
        return Err(integrity("version is not validated"));
    }
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
    created_at: String,
}

fn read_version_row(c: &Connection, key: i64) -> Result<Option<VersionRow>, Error> {
    let row = c
        .query_row(
            "SELECT version_key,version_id,entity,period,state,source_binding_digest,parent_version_key,manifest_mode,delta_depth,record_count,first_day,last_day,coverage_status,evidence_digest,projection_digest,published_at,integrity_digest,adjustment,digest_format,created_at FROM data_versions WHERE version_key=?1",
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
                    r.get::<_, String>(19)?,
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
        created_at,
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
        created_at,
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

/// Clip-level verified read of a leaf version: reconstructs the
/// projection and verifies the stored published-node digests without
/// materializing the legacy JSON envelope.
pub(crate) fn verify_leaf_projection(c: &Connection, key: i64) -> Result<(), Error> {
    let (row, bars) = reconstruct_leaf(c, key)?;
    verify_published_node(c, &row, &bars)?;
    Ok(())
}

/// Leaf projection bars at clip level (no legacy JSON envelope).
pub(crate) fn leaf_bars(c: &Connection, key: i64) -> Result<Vec<CompactBar>, Error> {
    Ok(reconstruct_leaf(c, key)?.1)
}

/// Windowed leaf read (#259 S5G): leaf bars restricted to
/// `[start_day, end_day]`, decoding only segments that overlap the window.
/// Cost is O(chain metadata + overlapping segments), not O(history) —
/// used by the ops-diff path to compare a typed candidate against the
/// base head without materializing it. Chain linkage, published-state
/// and clip/segment digests are still enforced; malformed rows fail
/// closed exactly like full reconstruction.
pub(crate) fn leaf_bars_in_range(
    c: &Connection,
    leaf_key: i64,
    start_day: i64,
    end_day: i64,
) -> Result<Vec<CompactBar>, Error> {
    if start_day > end_day {
        return Ok(Vec::new());
    }
    // Chain metadata walk, leaf -> root; no segment decode here.
    let mut nodes = Vec::new();
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
                nodes.push(row);
                break;
            }
            2 => {
                if row.delta_depth < 1 || row.delta_depth > MAX_DELTA_DEPTH {
                    return Err(integrity("delta depth out of bounds"));
                }
                let parent = row
                    .parent_version_key
                    .ok_or_else(|| integrity("delta version lacks parent"))?;
                nodes.push(row);
                key = parent;
            }
            _ => return Err(integrity("unknown manifest_mode")),
        }
        if nodes.len() > MAX_CHAIN_NODES {
            return Err(integrity("version chain exceeds depth bound"));
        }
    }
    nodes.reverse();
    // Root checkpoint: decode only clips overlapping the window.
    let mut map: BTreeMap<i64, CompactBar> = BTreeMap::new();
    {
        let root = &nodes[0];
        let mut statement = c
            .prepare(
                "SELECT segment_key,clip_start_day,clip_end_day,row_count \
                 FROM version_checkpoint_clips \
                 WHERE version_key=?1 AND clip_start_day<=?2 AND clip_end_day>=?3 \
                 ORDER BY seq",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map(params![root.key, end_day, start_day], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        drop(statement);
        for (segment_key, clip_start, clip_end, row_count) in rows {
            if row_count <= 0 || row_count > u32::MAX as i64 {
                return Err(integrity("checkpoint clip row_count out of bounds"));
            }
            let segment = compact_v19::read_segment(c, segment_key)?;
            let clip = SegmentClip {
                segment_digest: segment.digest,
                clip_start_day: clip_start,
                clip_end_day: clip_end,
                row_count: row_count as u32,
            };
            check_stored_clip(&segment, &clip, &root.entity, root.period)?;
            for bar in segment.bars.iter().filter(|bar| {
                bar.trade_day >= clip_start.max(start_day) && bar.trade_day <= clip_end.min(end_day)
            }) {
                map.insert(bar.trade_day, bar.clone());
            }
        }
    }
    // Delta ops overlapping the window, applied root -> leaf. Ops outside
    // the window are metadata-only and never decode their segment.
    for row in nodes.iter().skip(1) {
        let mut statement = c
            .prepare(
                "SELECT delta_kind,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count \
                 FROM version_delta_clips \
                 WHERE version_key=?1 AND range_start_day<=?2 AND range_end_day>=?3 \
                 ORDER BY seq",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map(params![row.key, end_day, start_day], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        drop(statement);
        for (kind, range_start, range_end, segment_key, clip_start, clip_end, row_count) in rows {
            match kind {
                1 => {
                    let (segment_key, clip_start, clip_end) =
                        match (segment_key, clip_start, clip_end) {
                            (Some(key), Some(start), Some(end)) => (key, start, end),
                            _ => return Err(integrity("upsert delta missing clip fields")),
                        };
                    if row_count <= 0 || row_count > u32::MAX as i64 {
                        return Err(integrity("upsert delta row_count out of bounds"));
                    }
                    let segment = compact_v19::read_segment(c, segment_key)?;
                    let clip = SegmentClip {
                        segment_digest: segment.digest,
                        clip_start_day: clip_start,
                        clip_end_day: clip_end,
                        row_count: row_count as u32,
                    };
                    check_stored_clip(&segment, &clip, &row.entity, row.period)?;
                    for bar in segment.bars.iter().filter(|bar| {
                        bar.trade_day >= clip_start.max(start_day)
                            && bar.trade_day <= clip_end.min(end_day)
                    }) {
                        map.insert(bar.trade_day, bar.clone());
                    }
                }
                2 => {
                    let (drop_start, drop_end) =
                        (range_start.max(start_day), range_end.min(end_day));
                    map.retain(|day, _| !(*day >= drop_start && *day <= drop_end));
                }
                _ => return Err(integrity("unknown delta_kind")),
            }
        }
    }
    Ok(map.into_values().collect())
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
    if stored_lineage(c, base.key)?.is_some() {
        return Err(integrity("yearly base carries a lineage row"));
    }
    let records = bars
        .iter()
        .map(|bar| bar.to_bar(period))
        .collect::<Result<Vec<_>, _>>()
        .map_err(integrity)?;
    let years: BTreeSet<i32> = records.iter().map(|bar| bar.date.year()).collect();
    // The windowed base re-aggregation is a pure function of the published
    // base content — bound by its stored integrity digest — plus years and
    // as_of. The derivation already computed it, so staged/validated/
    // published re-verification of the same inputs reuses the memoized
    // series instead of re-decoding the window; the projection equality
    // below still runs on every call. On a miss, lineage bounds enclose
    // every base bar of the selected years so only overlapping segments
    // decode — a truncated or shifted window changes the aggregation
    // output and fails the equality check: fail-closed.
    let base_digest_hex = compact_market::digest_hex(
        &base
            .integrity_digest
            .ok_or_else(|| integrity("published base lacks integrity digest"))?,
    );
    let key = crate::yearly::aggregate_key(&base_digest_hex, &years, as_of);
    let annual = crate::yearly::aggregate_memoized(key, || {
        let base_bars = leaf_bars_in_range(
            c,
            lineage.base_daily_version_key,
            lineage.first_trade_day,
            lineage.last_trade_day,
        )?;
        let base_evidence = verify_version_evidence(c, &base.evidence_digest)?;
        let base_version = to_effective_version(c, &base, &base_bars, base_evidence)?;
        crate::yearly::aggregate(&base_version, Some(&years), as_of)
            .map_err(|e| integrity(format!("yearly aggregation failed: {e}")))
    })?;
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

/// The published daily base version_id of a yearly version's stored
/// lineage, or None when the version has no lineage row.
pub(crate) fn daily_base_version_id(
    c: &Connection,
    version_key: i64,
) -> Result<Option<String>, Error> {
    let Some(lineage) = stored_lineage(c, version_key)? else {
        return Ok(None);
    };
    let base = read_version_row(c, lineage.base_daily_version_key)?
        .ok_or_else(|| integrity("yearly lineage base version missing"))?;
    Ok(Some(base.version_id))
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

/// Recompute the integrity digest for `published_at` — used by the S5F
/// equivalence test to compare rows across paths where the publish stamp
/// legitimately differs (envelope commits stamp at commit time).
#[cfg(test)]
pub(crate) fn recompute_integrity(
    c: &Connection,
    key: i64,
    published_at: &str,
) -> Result<Digest, Error> {
    let row = read_version_row(c, key)?.ok_or_else(|| integrity("version row missing"))?;
    let identity = build_identity(c, &row, published_at)?;
    compact_market::version_integrity_digest(&identity).map_err(integrity)
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

fn unpublished_version(
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
    let published_at = DateTime::parse_from_rfc3339(&row.created_at)
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

/// Verified materialization of any stored version: published versions run the
/// full chain/integrity verification; staged and validated versions run the
/// staged-projection verification. The projected `EffectiveVersion.published_at`
/// of an unpublished version is its immutable `created_at` stamp.
#[derive(Debug, Clone)]
pub(crate) struct MaterializedVersion {
    pub key: i64,
    pub state: CompactVersionState,
    pub version: EffectiveVersion,
    pub coverage_status: CoverageStatus,
    pub coverage_reason: String,
    pub source_binding: SourceBinding,
    pub generation_evidence: Value,
    pub parent_version_id: Option<String>,
    /// Stored `yearly_lineage.as_of_day` for yearly versions — derivation
    /// metadata publication reuses; None for daily or lineage-less rows.
    pub yearly_as_of_day: Option<i64>,
    pub projection_digest: Digest,
    pub integrity_digest: Option<Digest>,
}

pub(crate) fn materialize(c: &Connection, version_id: &str) -> Result<MaterializedVersion, Error> {
    let row = read_version_row_by_id(c, version_id)?.ok_or(Error::NotFound)?;
    let state = match row.state {
        1 => CompactVersionState::Staged,
        2 => CompactVersionState::Validated,
        3 => CompactVersionState::Published,
        _ => return Err(integrity("malformed version state")),
    };
    let evidence = verify_version_evidence(c, &row.evidence_digest)?;
    let version = match state {
        CompactVersionState::Published => fixed_by_key(c, row.key)?.version,
        _ => {
            verify_staged(c, &row)?;
            let (leaf, bars) = reconstruct_leaf(c, row.key)?;
            unpublished_version(c, &leaf, &bars, evidence.clone())?
        }
    };
    let source_binding = read_source_binding(c, &row.source_binding_digest)?;
    let (kind, canonical) = read_evidence(c, &source_binding.evidence_digest)?;
    if kind != EvidenceKind::Generation {
        return Err(integrity("source binding evidence is not generation"));
    }
    let generation_evidence: Value = serde_json::from_str(&canonical).map_err(integrity)?;
    let parent_version_id = match row.parent_version_key {
        Some(parent) => Some(
            read_version_row(c, parent)?
                .ok_or_else(|| integrity("parent version missing"))?
                .version_id,
        ),
        None => None,
    };
    Ok(MaterializedVersion {
        key: row.key,
        state,
        version,
        coverage_status: match row.coverage_status {
            1 => CoverageStatus::Complete,
            2 => CoverageStatus::Incomplete,
            3 => CoverageStatus::Unknown,
            _ => return Err(integrity("unknown coverage_status")),
        },
        coverage_reason: evidence.coverage_reason,
        source_binding,
        generation_evidence,
        parent_version_id,
        yearly_as_of_day: stored_lineage(c, row.key)?.map(|l| l.as_of_day),
        projection_digest: row.projection_digest,
        integrity_digest: row.integrity_digest,
    })
}

/// Header-only materialization of a published version: the same
/// `MaterializedVersion` fields `materialize` produces — identity digest
/// verification, version header, evidence, source binding, generation
/// evidence, parent link, lineage — with `version.records` left empty and
/// `validation` counts taken from the stored row (the same values the
/// verified projection would contain). Chain reconstruction and per-clip
/// bar decoding are skipped: for a published row the stored
/// `integrity_digest` is the authoritative content binding
/// (`verify_published_fields` still recomputes it here), and `synthesize`
/// already emits the slim envelope without bars, so the resulting
/// `CheckedRecord.raw()` is byte-identical to the full path's.
/// `Ok(None)` when the row is absent or not published, letting callers keep
/// the `materialize` fallback for exact state/error semantics.
pub(crate) fn materialize_header(
    c: &Connection,
    version_id: &str,
) -> Result<Option<MaterializedVersion>, Error> {
    let Some(row) = read_version_row_by_id(c, version_id)? else {
        return Ok(None);
    };
    if row.state != 3 {
        return Ok(None);
    }
    let evidence = verify_version_evidence(c, &row.evidence_digest)?;
    verify_published_fields(c, &row)?;
    let version = header_version(c, &row, evidence.clone())?;
    let source_binding = read_source_binding(c, &row.source_binding_digest)?;
    let (kind, canonical) = read_evidence(c, &source_binding.evidence_digest)?;
    if kind != EvidenceKind::Generation {
        return Err(integrity("source binding evidence is not generation"));
    }
    let generation_evidence: Value = serde_json::from_str(&canonical).map_err(integrity)?;
    let parent_version_id = match row.parent_version_key {
        Some(parent) => Some(
            read_version_row(c, parent)?
                .ok_or_else(|| integrity("parent version missing"))?
                .version_id,
        ),
        None => None,
    };
    Ok(Some(MaterializedVersion {
        key: row.key,
        state: CompactVersionState::Published,
        version,
        coverage_status: match row.coverage_status {
            1 => CoverageStatus::Complete,
            2 => CoverageStatus::Incomplete,
            3 => CoverageStatus::Unknown,
            _ => return Err(integrity("unknown coverage_status")),
        },
        coverage_reason: evidence.coverage_reason,
        source_binding,
        generation_evidence,
        parent_version_id,
        yearly_as_of_day: stored_lineage(c, row.key)?.map(|l| l.as_of_day),
        projection_digest: row.projection_digest,
        integrity_digest: row.integrity_digest,
    }))
}

/// `to_effective_version` without the record list: identical header fields
/// (identity, lineage, published stamp, evidence) for a published row, with
/// `records` empty and `validation` populated from the stored counts.
fn header_version(
    c: &Connection,
    row: &VersionRow,
    evidence: VersionEvidence,
) -> Result<EffectiveVersion, Error> {
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
    let record_count: usize = row
        .record_count
        .try_into()
        .map_err(|_| integrity("record_count out of usize range"))?;
    Ok(EffectiveVersion {
        version_id: row.version_id.clone(),
        symbol: row.entity.clone(),
        period: row.period,
        adjustment: Adjustment::Forward,
        source: evidence.source,
        records: vec![],
        published_at,
        validation: ValidationSummary {
            record_count,
            unique_record_count: record_count,
            duplicate_count: 0,
        },
        unit_currency: evidence.unit_currency,
        adjustment_evidence: evidence.adjustment_evidence,
        yearly_derivation,
    })
}

/// Windowed published-version materialization (#262 yearly read path):
/// identical header/binding/lineage reads as `materialize`, but the leaf
/// bars come from `leaf_bars_in_range`, decoding only segments that overlap
/// `[start_day, end_day]`. Chain linkage and clip digests are still
/// enforced; the row's stored `integrity_digest` remains the authoritative
/// content binding — callers must not re-digest the partial projection.
/// Non-published versions are `NotFound`, matching `fixed()` semantics.
pub(crate) fn materialize_windowed(
    c: &Connection,
    version_id: &str,
    start_day: i64,
    end_day: i64,
) -> Result<MaterializedVersion, Error> {
    let row = read_version_row_by_id(c, version_id)?.ok_or(Error::NotFound)?;
    let state = match row.state {
        1 => CompactVersionState::Staged,
        2 => CompactVersionState::Validated,
        3 => CompactVersionState::Published,
        _ => return Err(integrity("malformed version state")),
    };
    if state != CompactVersionState::Published {
        return Err(Error::NotFound);
    }
    let evidence = verify_version_evidence(c, &row.evidence_digest)?;
    let bars = leaf_bars_in_range(c, row.key, start_day, end_day)?;
    let version = to_effective_version(c, &row, &bars, evidence.clone())?;
    let source_binding = read_source_binding(c, &row.source_binding_digest)?;
    let (kind, canonical) = read_evidence(c, &source_binding.evidence_digest)?;
    if kind != EvidenceKind::Generation {
        return Err(integrity("source binding evidence is not generation"));
    }
    let generation_evidence: Value = serde_json::from_str(&canonical).map_err(integrity)?;
    let parent_version_id = match row.parent_version_key {
        Some(parent) => Some(
            read_version_row(c, parent)?
                .ok_or_else(|| integrity("parent version missing"))?
                .version_id,
        ),
        None => None,
    };
    Ok(MaterializedVersion {
        key: row.key,
        state,
        version,
        coverage_status: match row.coverage_status {
            1 => CoverageStatus::Complete,
            2 => CoverageStatus::Incomplete,
            3 => CoverageStatus::Unknown,
            _ => return Err(integrity("unknown coverage_status")),
        },
        coverage_reason: evidence.coverage_reason,
        source_binding,
        generation_evidence,
        parent_version_id,
        yearly_as_of_day: stored_lineage(c, row.key)?.map(|l| l.as_of_day),
        projection_digest: row.projection_digest,
        integrity_digest: row.integrity_digest,
    })
}

/// Distinct calendar years with stored bars in `[start_day, end_day]` for
/// a published version — decodes only overlapping segments (clip digests
/// enforced) without materializing `Bar`s. Same published-only `NotFound`
/// semantics as `materialize_windowed`.
pub(crate) fn window_years(
    c: &Connection,
    version_id: &str,
    start_day: i64,
    end_day: i64,
) -> Result<BTreeSet<i32>, Error> {
    let row = read_version_row_by_id(c, version_id)?.ok_or(Error::NotFound)?;
    if row.state != 3 {
        return Err(Error::NotFound);
    }
    verify_version_evidence(c, &row.evidence_digest)?;
    leaf_bars_in_range(c, row.key, start_day, end_day)?
        .iter()
        .map(|bar| {
            compact_market::epoch_date(bar.trade_day)
                .map(|date| date.year())
                .map_err(integrity)
        })
        .collect()
}

pub(crate) fn check_draft_static(c: &Connection, draft: &VersionDraft) -> Result<(), Error> {
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

pub(crate) fn insert_version_row(
    c: &Connection,
    draft: &VersionDraft,
    parent_version_key: Option<i64>,
    manifest_mode: i64,
    delta_depth: i64,
    bars: &[CompactBar],
    projection: Digest,
) -> Result<i64, Error> {
    insert_version_row_stats(
        c,
        draft,
        parent_version_key,
        manifest_mode,
        delta_depth,
        bars.len() as i64,
        bars.first().map(|b| b.trade_day).unwrap_or(0),
        bars.last().map(|b| b.trade_day).unwrap_or(0),
        projection,
    )
}

/// `insert_version_row` with explicit leaf statistics (#259 S5G-8): the
/// append-only delta path derives them arithmetically from the parent
/// row without materializing the projection.
#[allow(clippy::too_many_arguments)]
fn insert_version_row_stats(
    c: &Connection,
    draft: &VersionDraft,
    parent_version_key: Option<i64>,
    manifest_mode: i64,
    delta_depth: i64,
    record_count: i64,
    first_day: i64,
    last_day: i64,
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
            record_count,
            first_day,
            last_day,
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

pub(crate) fn read_evidence(
    c: &Connection,
    digest: &Digest,
) -> Result<(EvidenceKind, String), Error> {
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
    let canonical = inflate_evidence(c, kind, &row.1)?;
    compact_market::verify_evidence(kind, &canonical, *digest).map_err(integrity)?;
    Ok((kind, canonical))
}

pub(crate) fn read_source_binding(c: &Connection, digest: &Digest) -> Result<SourceBinding, Error> {
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
        "shared" => EvidenceKind::Shared,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactVersionState {
    Staged,
    Validated,
    Published,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VersionResultFacts {
    pub key: i64,
    pub version_id: String,
    pub entity: String,
    pub period: Period,
    pub state: CompactVersionState,
    pub source_binding_digest: Digest,
    pub parent_version_key: Option<i64>,
    pub manifest_mode: ManifestMode,
    pub delta_depth: u32,
    pub projection_digest: Digest,
    pub first_day: i64,
    pub last_day: i64,
    pub record_count: i64,
    pub coverage_status: i64,
    pub evidence_digest: Digest,
    pub integrity_digest: Option<Digest>,
}

pub(crate) fn result_facts(c: &Connection, key: i64) -> Result<VersionResultFacts, Error> {
    let row = read_version_row(c, key)?.ok_or_else(|| integrity("version row missing"))?;
    let state = match row.state {
        1 => CompactVersionState::Staged,
        2 => CompactVersionState::Validated,
        3 => CompactVersionState::Published,
        _ => return Err(integrity("malformed version state")),
    };
    let manifest_mode = match row.manifest_mode {
        1 => ManifestMode::Checkpoint,
        2 => ManifestMode::Delta,
        _ => return Err(integrity("malformed manifest_mode")),
    };
    let delta_depth: u32 = row
        .delta_depth
        .try_into()
        .map_err(|_| integrity("malformed delta_depth"))?;
    match manifest_mode {
        ManifestMode::Checkpoint if row.parent_version_key.is_some() || delta_depth != 0 => {
            return Err(integrity("checkpoint version carries parent/depth"));
        }
        ManifestMode::Delta
            if row.parent_version_key.is_none()
                || !(1..=MAX_DELTA_DEPTH as u32).contains(&delta_depth) =>
        {
            return Err(integrity("delta version lacks parent/depth"));
        }
        _ => {}
    }
    Ok(VersionResultFacts {
        key: row.key,
        version_id: row.version_id,
        entity: row.entity,
        period: row.period,
        state,
        source_binding_digest: row.source_binding_digest,
        parent_version_key: row.parent_version_key,
        manifest_mode,
        delta_depth,
        projection_digest: row.projection_digest,
        first_day: row.first_day,
        last_day: row.last_day,
        record_count: row.record_count,
        coverage_status: row.coverage_status,
        evidence_digest: row.evidence_digest,
        integrity_digest: row.integrity_digest,
    })
}

pub(crate) fn verified_evidence_kind(
    c: &Connection,
    digest: &Digest,
) -> Result<EvidenceKind, Error> {
    Ok(read_evidence(c, digest)?.0)
}

pub(crate) fn verified_source_binding(
    c: &Connection,
    digest: &Digest,
) -> Result<SourceBinding, Error> {
    read_source_binding(c, digest)
}

pub(crate) fn apply_stored_delta(
    c: &Connection,
    base: &[CompactBar],
    ops: &[StoredDelta],
    entity: &str,
    period: Period,
) -> Result<Vec<CompactBar>, Error> {
    apply_delta(c, base, ops, entity, period)
}

#[cfg(test)]
mod tests;
