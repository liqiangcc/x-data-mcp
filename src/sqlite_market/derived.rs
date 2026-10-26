//! Non-bar derived datasets (issue-383 §3.6/§3.7, M1 PR-4): the parsed
//! factor-event table and the version registration shared by every dataset
//! that is not a bar series.
//!
//! Layer position (separation points 1/2): `adjustment_events` holds the
//! 解析事实 — the immutable f32 factor bits plus the 2dp business decimals
//! derived from them — and `derived_dataset_versions` holds only identity and
//! audit for datasets whose rows live in their own typed table. Nothing here
//! knows a source name: the adapter (`official_market`) hands over an
//! [`AdjustmentEventSet`].
//!
//! Identity rule (same as `market_series`, §3.10): the content digest is a
//! pure function of the values and their order, `version_id` is its hex, and
//! metadata (coverage/evidence/task/timestamps) never enters it. The derived
//! `c1..c4` are excluded on purpose — they are recomputable from `c_bits`, so
//! folding both would let the digest disagree with itself.

use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{evidence_store, integrity, storage};
use crate::compact_market::{self, Digest, EvidenceKind};
use crate::market_data::{AdjustmentEvent, AdjustmentEventSet};
use crate::market_storage::Error;

/// `derived_dataset_versions.dataset_kind` of the factor set (§3.7).
pub(crate) const ADJUSTMENT_EVENTS_KIND: &str = "adjustment_events";
/// Digest algorithm name in `derived_dataset_versions.digest_format` (§3.10).
pub(crate) const ADJUSTMENT_EVENTS_DIGEST_FORMAT: &str = "adjustment-events-v1";
/// Derivation label in `algorithm_version` (§4.4): the 2dp factor
/// normalization the digest input is built from.
pub(crate) const ADJUSTMENT_EVENTS_ALGORITHM: &str = "gbbq-cat1-2dp-v1";
/// Why every stored `known_at` is NULL (BR-M1-9). Part of the evidence text so
/// the limitation travels with the data instead of living in a doc only.
const KNOWN_AT_REASON: &str = "the source carries only the ex-rights (effective) date; no announcement date exists, \
     so the 知晓时间 is unknown and never guessed (BR-M1-9)";

fn push_text(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

/// Content digest of one entity's factor set (§3.10):
/// `SHA256("adjustment-events-v1" ‖ entity ‖ n ‖ (ordinal, effective_day,
/// category, c_bits_1..4)… ‖ excluded)`.
///
/// Order is the caller's slice order, so a reordered set is a different
/// version; `excluded` is folded last so the count that never reaches the
/// table still changes the identity when the ignored records change.
pub(crate) fn events_digest(
    entity: &str,
    events: &[AdjustmentEvent],
    excluded: usize,
) -> Result<Digest, Error> {
    compact_market::canonical_entity(entity).map_err(|e| integrity(e.to_string()))?;
    let count = u64::try_from(events.len()).map_err(|_| integrity("event count overflow"))?;
    let mut payload = Vec::with_capacity(64 + events.len() * 29);
    payload.extend_from_slice(b"adjustment-events-v1\0");
    push_text(&mut payload, entity);
    payload.extend_from_slice(&count.to_be_bytes());
    for event in events {
        payload.extend_from_slice(&event.ordinal.to_be_bytes());
        payload.extend_from_slice(&compact_market::epoch_day(event.effective_day).to_be_bytes());
        payload.push(event.category);
        for bits in event.c_bits {
            payload.extend_from_slice(&bits.to_be_bytes());
        }
    }
    payload.extend_from_slice(
        &u64::try_from(excluded)
            .map_err(|_| integrity("excluded count overflow"))?
            .to_be_bytes(),
    );
    Ok(Sha256::digest(&payload).into())
}

/// One `derived_dataset_versions` row as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedVersionRow {
    pub dataset_kind: String,
    pub dataset_key: String,
    pub version_id: String,
    pub content_digest: Digest,
    pub digest_format: String,
    pub record_count: i64,
    pub first_day: i64,
    pub last_day: i64,
    pub algorithm_version: String,
    pub source_binding_digest: Digest,
    pub evidence_digest: Digest,
    /// The task item whose commit last wrote the set (audit only).
    pub task_id: String,
    pub item_id: String,
}

const VERSION_COLUMNS: &str = "dataset_kind,dataset_key,version_id,content_digest,\
     digest_format,record_count,first_day,last_day,algorithm_version,\
     source_binding_digest,evidence_digest,task_id,item_id";

type RawVersion = (
    String,
    String,
    String,
    Vec<u8>,
    String,
    i64,
    i64,
    i64,
    String,
    Vec<u8>,
    Vec<u8>,
    String,
    String,
);

fn version_row(raw: RawVersion) -> Result<DerivedVersionRow, Error> {
    let content_digest: Digest = raw
        .3
        .as_slice()
        .try_into()
        .map_err(|_| integrity("content_digest column is not 32 bytes"))?;
    if raw.2 != compact_market::digest_hex(&content_digest) {
        return Err(integrity(
            "derived version_id does not match its content digest",
        ));
    }
    Ok(DerivedVersionRow {
        dataset_kind: raw.0,
        dataset_key: raw.1,
        version_id: raw.2,
        content_digest,
        digest_format: raw.4,
        record_count: raw.5,
        first_day: raw.6,
        last_day: raw.7,
        algorithm_version: raw.8,
        source_binding_digest: raw
            .9
            .as_slice()
            .try_into()
            .map_err(|_| integrity("source_binding_digest column is not 32 bytes"))?,
        evidence_digest: raw
            .10
            .as_slice()
            .try_into()
            .map_err(|_| integrity("evidence_digest column is not 32 bytes"))?,
        task_id: raw.11,
        item_id: raw.12,
    })
}

/// The registration row of one non-bar dataset, `None` when it has never been
/// written (or its last write carried an empty set).
pub(crate) fn read_version(
    c: &Connection,
    dataset_kind: &str,
    dataset_key: &str,
) -> Result<Option<DerivedVersionRow>, Error> {
    let raw: Option<RawVersion> = c
        .query_row(
            &format!(
                "SELECT {VERSION_COLUMNS} FROM derived_dataset_versions \
                 WHERE dataset_kind=?1 AND dataset_key=?2"
            ),
            params![dataset_kind, dataset_key],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    raw.map(version_row).transpose()
}

/// Stored factor set of one entity, in ordinal order. This is the accessor the
/// research read model (PR-8) consumes; `factors` is read back from the stored
/// `c1..c4` so a recomputation from `c_bits` can be compared against it.
pub(crate) fn read_adjustment_events(
    c: &Connection,
    entity: &str,
) -> Result<Vec<AdjustmentEvent>, Error> {
    let mut statement = c
        .prepare(
            "SELECT ordinal,effective_day,category,c_bits_1,c_bits_2,c_bits_3,c_bits_4,\
             c1,c2,c3,c4,known_at FROM adjustment_events WHERE entity=?1 ORDER BY ordinal",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![entity], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .map_err(storage)?;
    let mut events = Vec::new();
    for row in rows {
        let raw = row.map_err(storage)?;
        let day = compact_market::epoch_date(raw.1).map_err(integrity)?;
        let known_at = match raw.11 {
            Some(text) => Some(
                NaiveDate::parse_from_str(&text, "%Y-%m-%d")
                    .map_err(|_| integrity("known_at is not a date"))?,
            ),
            None => None,
        };
        let mut factors = [rust_decimal::Decimal::ZERO; 4];
        for (slot, text) in factors.iter_mut().zip([raw.7, raw.8, raw.9, raw.10]) {
            *slot = text
                .parse()
                .map_err(|_| integrity("stored factor is not a decimal"))?;
        }
        events.push(AdjustmentEvent {
            ordinal: raw.0,
            effective_day: day,
            category: u8::try_from(raw.2).map_err(|_| integrity("category out of range"))?,
            c_bits: [
                u32::try_from(raw.3).map_err(|_| integrity("c_bits_1 out of range"))?,
                u32::try_from(raw.4).map_err(|_| integrity("c_bits_2 out of range"))?,
                u32::try_from(raw.5).map_err(|_| integrity("c_bits_3 out of range"))?,
                u32::try_from(raw.6).map_err(|_| integrity("c_bits_4 out of range"))?,
            ],
            factors,
            known_at,
        });
    }
    Ok(events)
}

/// Replace one entity's whole factor set (§3.6 item 4: delete+insert, never an
/// incremental patch) and refresh its version registration, inside the
/// caller's transaction.
///
/// An empty set is a legitimate outcome (an entity whose verified events
/// disappeared): the stored rows and the registration are removed so the table
/// never claims a version it no longer has.
///
/// Returns the registration row written, `None` for an empty set.
pub(crate) fn replace_adjustment_events(
    c: &Connection,
    set: &AdjustmentEventSet,
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<Option<DerivedVersionRow>, Error> {
    compact_market::canonical_entity(&set.entity).map_err(|e| integrity(e.to_string()))?;
    for event in &set.events {
        if event.category != 1 {
            return Err(integrity(
                "only category-1 events are stored (unverified categories stay excluded)",
            ));
        }
        for factor in event.factors {
            if factor.round_dp(2) != factor {
                return Err(integrity("factor is not a 2dp business decimal"));
            }
        }
    }
    if set
        .events
        .windows(2)
        .any(|pair| pair[0].ordinal >= pair[1].ordinal)
    {
        return Err(integrity("event ordinals are not strictly increasing"));
    }
    let digest = events_digest(&set.entity, &set.events, set.excluded)?;
    let now = chrono::Utc::now().to_rfc3339();
    // Whole-group replace: the stored rows are this entity's current set only.
    c.execute(
        "DELETE FROM adjustment_events WHERE entity=?1",
        params![set.entity],
    )
    .map_err(storage)?;
    if set.is_empty() {
        c.execute(
            "DELETE FROM derived_dataset_versions WHERE dataset_kind=?1 AND dataset_key=?2",
            params![ADJUSTMENT_EVENTS_KIND, set.entity],
        )
        .map_err(storage)?;
        return Ok(None);
    }
    let evidence = factor_evidence(set, &digest)?;
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    for event in &set.events {
        c.execute(
            "INSERT INTO adjustment_events(entity,ordinal,effective_day,category,\
             c_bits_1,c_bits_2,c_bits_3,c_bits_4,c1,c2,c3,c4,\
             source_binding_digest,evidence_digest,known_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                set.entity,
                event.ordinal,
                compact_market::epoch_day(event.effective_day),
                i64::from(event.category),
                i64::from(event.c_bits[0]),
                i64::from(event.c_bits[1]),
                i64::from(event.c_bits[2]),
                i64::from(event.c_bits[3]),
                event.factors[0].to_string(),
                event.factors[1].to_string(),
                event.factors[2].to_string(),
                event.factors[3].to_string(),
                source_binding_digest.as_slice(),
                evidence_digest.as_slice(),
                event.known_at.map(|day| day.to_string()),
            ],
        )
        .map_err(storage)?;
    }
    // The covered span, not the ordinal order: decode order need not be
    // date order, and the registration must satisfy `first_day <= last_day`.
    let first_day = set
        .events
        .iter()
        .map(|event| compact_market::epoch_day(event.effective_day))
        .min()
        .ok_or_else(|| integrity("non-empty set checked above"))?;
    let last_day = set
        .events
        .iter()
        .map(|event| compact_market::epoch_day(event.effective_day))
        .max()
        .ok_or_else(|| integrity("non-empty set checked above"))?;
    let version_id = compact_market::digest_hex(&digest);
    let record_count =
        i64::try_from(set.events.len()).map_err(|_| integrity("event count overflow"))?;
    c.execute(
        "INSERT INTO derived_dataset_versions(dataset_kind,dataset_key,version_id,\
         content_digest,digest_format,record_count,first_day,last_day,algorithm_version,\
         source_binding_digest,evidence_digest,task_id,item_id,created_at,updated_at) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14) \
         ON CONFLICT(dataset_kind,dataset_key) DO UPDATE SET \
         version_id=excluded.version_id,content_digest=excluded.content_digest,\
         digest_format=excluded.digest_format,record_count=excluded.record_count,\
         first_day=excluded.first_day,last_day=excluded.last_day,\
         algorithm_version=excluded.algorithm_version,\
         source_binding_digest=excluded.source_binding_digest,\
         evidence_digest=excluded.evidence_digest,task_id=excluded.task_id,\
         item_id=excluded.item_id,updated_at=excluded.updated_at",
        params![
            ADJUSTMENT_EVENTS_KIND,
            set.entity,
            version_id,
            digest.as_slice(),
            ADJUSTMENT_EVENTS_DIGEST_FORMAT,
            record_count,
            first_day,
            last_day,
            ADJUSTMENT_EVENTS_ALGORITHM,
            source_binding_digest.as_slice(),
            evidence_digest.as_slice(),
            task_id,
            item_id,
            now,
        ],
    )
    .map_err(storage)?;
    Ok(Some(
        read_version(c, ADJUSTMENT_EVENTS_KIND, &set.entity)?
            .ok_or_else(|| integrity("factor-set registration vanished after its insert"))?,
    ))
}

/// The `derived` evidence bundle of one factor set: how it was interpreted,
/// what was deliberately excluded, and that the 知晓时间 is unknown.
fn factor_evidence(set: &AdjustmentEventSet, digest: &Digest) -> Result<Value, Error> {
    let first = set
        .events
        .first()
        .ok_or_else(|| integrity("evidence requires a non-empty event set"))?;
    let last = set
        .events
        .last()
        .ok_or_else(|| integrity("evidence requires a non-empty event set"))?;
    Ok(json!({
        "dataset": ADJUSTMENT_EVENTS_KIND,
        "dataset_key": set.entity,
        "digest_format": ADJUSTMENT_EVENTS_DIGEST_FORMAT,
        "algorithm_version": ADJUSTMENT_EVENTS_ALGORITHM,
        "content_digest": compact_market::digest_hex(digest),
        "event_count": set.events.len(),
        // Records this build refuses to interpret: counted, never stored.
        "excluded_non_category_1": set.excluded,
        "first_effective_day": first.effective_day.to_string(),
        "last_effective_day": last.effective_day.to_string(),
        // Separation point 3: the effective date exists, the known-at date
        // does not. Stated in the evidence so a point-in-time consumer cannot
        // read the factor set as "known when effective".
        "known_at": "unknown",
        "known_at_reason": KNOWN_AT_REASON,
    }))
}

#[cfg(test)]
mod tests;
