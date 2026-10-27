//! TDX security-fact writers (M2 PR-2, issue-391 §5/§9.2/§16).
//!
//! Layer position (separation point 1): this module knows the two typed tables
//! and the version registration, not the TDX member names or the parser. The
//! adapter (`official_security`) hands over [`CapitalEventSet`] and
//! [`SecurityLifecycleFact`] values.
//!
//! `capital_events` is a whole-group replace per entity, exactly like
//! `adjustment_events`: delete+insert inside the caller's transaction, and an
//! empty set removes both the rows and the registration. `security_master` is
//! an upsert on the row identity `(exchange, code, effective_from)`, because
//! PR-3 will merge the exchange branch into the same table and must be able to
//! keep rows this PR-2 writer never saw.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::derived::{self, DerivedVersionRow};
use super::{evidence_store, integrity, storage};
use crate::compact_market::{self, Digest, EvidenceKind};
use crate::market_storage::Error;
use crate::security_master::{
    CapitalEvent, CapitalEventSet, DelistingSource, ListingSource, SecurityLifecycleFact,
    is_suspicious,
};

/// `derived_dataset_versions.dataset_kind` of the capital-event set.
pub(crate) const CAPITAL_EVENTS_KIND: &str = "capital_events";
/// Digest algorithm name in `derived_dataset_versions.digest_format`.
pub(crate) const CAPITAL_EVENTS_DIGEST_FORMAT: &str = "capital-events-v1";
/// Derivation label in `algorithm_version`: the 2dp 万股 -> 股 conversion.
pub(crate) const CAPITAL_EVENTS_ALGORITHM: &str = "gbbq-capital-2dp-v1";
/// `security_master` rows written here are TDX-only; PR-3 owns the assembled
/// dataset and its version registration.
const MASTER_ALGORITHM: &str = "tdx-base-dbf-v1";

/// Why every stored `capital_events.known_at` is NULL (BR-M1-9): the source
/// carries the ex-rights date only. Part of the evidence text so the
/// limitation travels with the data.
const CAPITAL_KNOWN_AT_REASON: &str = "the source carries only the ex-rights (effective) date; no announcement date exists, \
     so the 知晓时间 is unknown and never guessed (BR-M1-9)";

const MASTER_KNOWN_AT_REASON: &str = "base.dbf/infoharbor_spec.cfg carry no announcement or as-of timestamp; \
     the 知晓时间 is unknown and never proxied by the listing/effective day";

fn push_text(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn split_entity(entity: &str) -> Result<(&str, &str), Error> {
    compact_market::canonical_entity(entity).map_err(|error| integrity(error.to_string()))?;
    entity
        .split_once('.')
        .ok_or_else(|| integrity("entity has no exchange separator"))
}

/// Content digest of one entity's capital-event set:
/// `SHA256("capital-events-v1" ‖ entity ‖ n ‖ (ordinal, effective_day,
/// category, *_bits_1..4)… ‖ excluded)`.
///
/// The derived share counts and the `suspicious` flag are excluded on purpose:
/// both are recomputable from the stored bits, so folding them would let the
/// digest disagree with itself when the gate is revisited.
pub(crate) fn capital_events_digest(set: &CapitalEventSet) -> Result<Digest, Error> {
    split_entity(&set.entity)?;
    let count = u64::try_from(set.events.len()).map_err(|_| integrity("event count overflow"))?;
    let mut payload = Vec::with_capacity(64 + set.events.len() * 29);
    payload.extend_from_slice(b"capital-events-v1\0");
    push_text(&mut payload, &set.entity);
    payload.extend_from_slice(&count.to_be_bytes());
    for event in &set.events {
        payload.extend_from_slice(&event.ordinal.to_be_bytes());
        payload.extend_from_slice(&compact_market::epoch_day(event.effective_day).to_be_bytes());
        payload.push(event.category);
        for bits in [
            event.float_before_bits,
            event.total_before_bits,
            event.float_after_bits,
            event.total_after_bits,
        ] {
            payload.extend_from_slice(&bits.to_be_bytes());
        }
    }
    payload.extend_from_slice(
        &u64::try_from(set.excluded)
            .map_err(|_| integrity("excluded count overflow"))?
            .to_be_bytes(),
    );
    Ok(Sha256::digest(&payload).into())
}

/// Stored capital-event set of one entity, in ordinal order. The derived
/// share counts and the gate are checked against the frozen rule on read, so
/// a corrupted derived column is an integrity error rather than a value the
/// read model would silently trust.
pub(crate) fn read_capital_events(
    c: &Connection,
    entity: &str,
) -> Result<Vec<CapitalEvent>, Error> {
    let (exchange, code) = split_entity(entity)?;
    let mut statement = c
        .prepare(
            "SELECT ordinal,effective_day,category,float_before_bits,total_before_bits,\
             float_after_bits,total_after_bits,float_after,total_after,\
             prev_is_lagged,suspicious,known_at FROM capital_events \
             WHERE exchange=?1 AND code=?2 ORDER BY ordinal",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![exchange, code], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .map_err(storage)?;
    let mut events = Vec::new();
    for row in rows {
        let raw = row.map_err(storage)?;
        let day = compact_market::epoch_date(raw.1).map_err(integrity)?;
        let category = u8::try_from(raw.2).map_err(|_| integrity("category out of range"))?;
        let bits = [
            u32::try_from(raw.3).map_err(|_| integrity("float_before_bits out of range"))?,
            u32::try_from(raw.4).map_err(|_| integrity("total_before_bits out of range"))?,
            u32::try_from(raw.5).map_err(|_| integrity("float_after_bits out of range"))?,
            u32::try_from(raw.6).map_err(|_| integrity("total_after_bits out of range"))?,
        ];
        let mut event = CapitalEvent::from_raw(raw.0, day, category, bits)
            .map_err(|error| integrity(error.to_string()))?;
        if event.float_after != raw.7 || event.total_after != raw.8 {
            return Err(integrity(
                "stored capital_events share counts disagree with the frozen 2dp rule",
            ));
        }
        if event.prev_is_lagged != (raw.9 != 0) || event.suspicious != (raw.10 != 0) {
            return Err(integrity(
                "stored capital_events flags disagree with the frozen gate",
            ));
        }
        event.known_at = match raw.11 {
            Some(text) => Some(
                NaiveDate::parse_from_str(&text, "%Y-%m-%d")
                    .map_err(|_| integrity("known_at is not a date"))?,
            ),
            None => None,
        };
        events.push(event);
    }
    Ok(events)
}

/// Replace one entity's whole capital-event set and refresh its version
/// registration, inside the caller's transaction.
///
/// An empty set is a real outcome: the rows and the registration are removed
/// so the table never claims a version it no longer holds.
pub(crate) fn replace_capital_events(
    c: &Connection,
    set: &CapitalEventSet,
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<Option<DerivedVersionRow>, Error> {
    let (exchange, code) = split_entity(&set.entity)?;
    for event in &set.events {
        if !matches!(event.category, 2 | 3 | 9 | 10) {
            return Err(integrity(
                "only capital categories 2/3/9/10 are stored (cat5/7/8 stay excluded)",
            ));
        }
        if !event.prev_is_lagged {
            return Err(integrity(
                "capital_events.prev_is_lagged is always true in schema 23",
            ));
        }
        if !event.shares_match_bits() {
            return Err(integrity(
                "capital event share counts do not match the stored f32 bits",
            ));
        }
        if event.suspicious
            != is_suspicious(
                event.float_before,
                event.total_before,
                event.float_after,
                event.total_after,
            )
        {
            return Err(integrity(
                "capital event suspicious flag does not match the §5.2 gate",
            ));
        }
    }
    if set
        .events
        .windows(2)
        .any(|pair| pair[0].ordinal >= pair[1].ordinal)
    {
        return Err(integrity(
            "capital event ordinals are not strictly increasing",
        ));
    }
    let digest = capital_events_digest(set)?;
    let now = chrono::Utc::now().to_rfc3339();
    c.execute(
        "DELETE FROM capital_events WHERE exchange=?1 AND code=?2",
        params![exchange, code],
    )
    .map_err(storage)?;
    if set.is_empty() {
        c.execute(
            "DELETE FROM derived_dataset_versions WHERE dataset_kind=?1 AND dataset_key=?2",
            params![CAPITAL_EVENTS_KIND, set.entity],
        )
        .map_err(storage)?;
        return Ok(None);
    }

    let evidence = capital_evidence(set, &digest, source_binding_digest)?;
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    for event in &set.events {
        c.execute(
            "INSERT INTO capital_events(exchange,code,ordinal,effective_day,category,\
             float_before_bits,total_before_bits,float_after_bits,total_after_bits,\
             float_after,total_after,prev_is_lagged,suspicious,known_at,\
             source_binding_digest,evidence_digest) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                exchange,
                code,
                event.ordinal,
                compact_market::epoch_day(event.effective_day),
                i64::from(event.category),
                i64::from(event.float_before_bits),
                i64::from(event.total_before_bits),
                i64::from(event.float_after_bits),
                i64::from(event.total_after_bits),
                event.float_after,
                event.total_after,
                event.prev_is_lagged as i64,
                event.suspicious as i64,
                event.known_at.map(|day| day.to_string()),
                source_binding_digest.as_slice(),
                evidence_digest.as_slice(),
            ],
        )
        .map_err(storage)?;
    }
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
            CAPITAL_EVENTS_KIND,
            set.entity,
            version_id,
            digest.as_slice(),
            CAPITAL_EVENTS_DIGEST_FORMAT,
            record_count,
            first_day,
            last_day,
            CAPITAL_EVENTS_ALGORITHM,
            source_binding_digest.as_slice(),
            evidence_digest.as_slice(),
            task_id,
            item_id,
            now,
        ],
    )
    .map_err(storage)?;
    derived::read_version(c, CAPITAL_EVENTS_KIND, &set.entity)?
        .ok_or_else(|| integrity("capital-event registration vanished after its insert"))
        .map(Some)
}

/// The `derived` evidence bundle of one capital-event set.
fn capital_evidence(
    set: &CapitalEventSet,
    digest: &Digest,
    source_binding_digest: Digest,
) -> Result<Value, Error> {
    let first = set
        .events
        .first()
        .ok_or_else(|| integrity("evidence requires a non-empty event set"))?;
    let last = set
        .events
        .last()
        .ok_or_else(|| integrity("evidence requires a non-empty event set"))?;
    let excluded_by_category: BTreeMap<String, usize> = set
        .excluded_by_category
        .iter()
        .map(|(category, count)| (category.to_string(), *count))
        .collect();
    // §5.2 diagnostics: the source's "before" is the previous record's state,
    // so a break in the chain is counted, not treated as a parse error.
    let chain_gap_count = set
        .events
        .windows(2)
        .filter(|pair| pair[0].float_after != pair[1].float_before)
        .count();
    let same_day_multi_event_count = set
        .events
        .windows(2)
        .filter(|pair| pair[0].effective_day == pair[1].effective_day)
        .count();
    Ok(json!({
        "dataset": CAPITAL_EVENTS_KIND,
        "dataset_key": set.entity,
        "digest_format": CAPITAL_EVENTS_DIGEST_FORMAT,
        "algorithm_version": CAPITAL_EVENTS_ALGORITHM,
        "content_digest": compact_market::digest_hex(digest),
        "source_binding_digest": compact_market::digest_hex(&source_binding_digest),
        "event_count": set.events.len(),
        "included_categories": [2, 3, 9, 10],
        "excluded_categories": set.excluded,
        "excluded_by_category": excluded_by_category,
        "suspicious_count": set.events.iter().filter(|event| event.suspicious).count(),
        "chain_gap_count": chain_gap_count,
        "same_day_multi_event_count": same_day_multi_event_count,
        "prev_is_lagged": "all rows: the source's before values are the previous record's state and may be stale",
        "shares_unit": "integer shares; the source stores 万股 at 2dp, converted by the frozen 2dp normalization × 10,000",
        "first_effective_day": first.effective_day.to_string(),
        "last_effective_day": last.effective_day.to_string(),
        "known_at": "unknown",
        "known_at_reason": CAPITAL_KNOWN_AT_REASON,
    }))
}

/// Insert or update TDX-only `security_master` rows on the row identity.
///
/// No version registration: the dataset is not assembled until PR-3 merges
/// the exchange branch (§9.3). The rows still carry a `derived` evidence
/// bundle so the TDX-only limitation travels with the data.
pub(crate) fn upsert_security_master(
    c: &Connection,
    facts: &[SecurityLifecycleFact],
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<usize, Error> {
    if facts.is_empty() {
        return Ok(0);
    }
    let mut seen = BTreeSet::new();
    for fact in facts {
        validate_fact(fact)?;
        if !seen.insert((
            fact.exchange.as_str(),
            fact.code.as_str(),
            fact.effective_from,
        )) {
            return Err(integrity(
                "duplicate security_master interval in one write batch",
            ));
        }
    }
    let evidence = master_evidence(facts, source_binding_digest, task_id, item_id)?;
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    for fact in facts {
        let entity = fact.entity();
        let (exchange, code) = split_entity(&entity)?;
        c.execute(
            "INSERT INTO security_master(exchange,code,effective_from,effective_to,\
             instrument_class,issuer_key,issuer_resolution,listing_date,listing_date_tdx,\
             listing_source,b_listing_date,delisting_date,delisting_date_tdx,\
             delisting_source,delisting_reason_class,arrangement_start,arrangement_end,\
             listing_conflict,delisting_conflict,known_at,source_binding_digest,\
             evidence_digest) \
             VALUES(?1,?2,?3,?4,0,NULL,?5,?6,?7,?8,NULL,?9,?10,?11,?12,?13,?14,?15,?16,\
             NULL,?17,?18) \
             ON CONFLICT(exchange,code,effective_from) DO UPDATE SET \
             effective_to=excluded.effective_to,instrument_class=excluded.instrument_class,\
             issuer_key=excluded.issuer_key,issuer_resolution=excluded.issuer_resolution,\
             listing_date=excluded.listing_date,listing_date_tdx=excluded.listing_date_tdx,\
             listing_source=excluded.listing_source,b_listing_date=excluded.b_listing_date,\
             delisting_date=excluded.delisting_date,delisting_date_tdx=excluded.delisting_date_tdx,\
             delisting_source=excluded.delisting_source,\
             delisting_reason_class=excluded.delisting_reason_class,\
             arrangement_start=excluded.arrangement_start,\
             arrangement_end=excluded.arrangement_end,\
             listing_conflict=excluded.listing_conflict,\
             delisting_conflict=excluded.delisting_conflict,known_at=excluded.known_at,\
             source_binding_digest=excluded.source_binding_digest,\
             evidence_digest=excluded.evidence_digest",
            params![
                exchange,
                code,
                compact_market::epoch_day(fact.effective_from),
                fact.effective_to.map(compact_market::epoch_day),
                fact.issuer_resolution.as_str(),
                fact.listing_date.map(compact_market::epoch_day),
                fact.listing_date_tdx.map(compact_market::epoch_day),
                fact.listing_source.as_str(),
                fact.delisting_date.map(compact_market::epoch_day),
                fact.delisting_date_tdx.map(compact_market::epoch_day),
                fact.delisting_source.as_str(),
                fact.delisting_reason_class.as_str(),
                fact.arrangement_start.map(compact_market::epoch_day),
                fact.arrangement_end.map(compact_market::epoch_day),
                fact.listing_conflict as i64,
                fact.delisting_conflict as i64,
                source_binding_digest.as_slice(),
                evidence_digest.as_slice(),
            ],
        )
        .map_err(storage)?;
    }
    Ok(facts.len())
}

fn validate_fact(fact: &SecurityLifecycleFact) -> Result<(), Error> {
    let entity = fact.entity();
    compact_market::canonical_entity(&entity).map_err(|error| integrity(error.to_string()))?;
    if fact.code.len() != 6 || !fact.code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(integrity("security_master code is not six digits"));
    }
    if let Some(to) = fact.effective_to {
        if to <= fact.effective_from {
            return Err(integrity("effective_to is not after effective_from"));
        }
    }
    if fact.listing_date.is_some() != (fact.listing_source != ListingSource::Unknown) {
        return Err(integrity("listing_date and listing_source disagree"));
    }
    if fact.delisting_date.is_some() != (fact.delisting_source != DelistingSource::Unknown) {
        return Err(integrity("delisting_date and delisting_source disagree"));
    }
    if let (Some(start), Some(end)) = (fact.arrangement_start, fact.arrangement_end) {
        if start > end {
            return Err(integrity("arrangement_start is after arrangement_end"));
        }
    }
    Ok(())
}

/// The `derived` evidence bundle of one TDX-only `security_master` batch.
fn master_evidence(
    facts: &[SecurityLifecycleFact],
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<Value, Error> {
    let listing_sources: BTreeSet<&str> = facts
        .iter()
        .map(|fact| fact.listing_source.as_str())
        .collect();
    let delisting_sources: BTreeSet<&str> = facts
        .iter()
        .map(|fact| fact.delisting_source.as_str())
        .collect();
    let reason_classes: BTreeSet<&str> = facts
        .iter()
        .map(|fact| fact.delisting_reason_class.as_str())
        .collect();
    Ok(json!({
        "dataset": "security_master",
        "dataset_key": "tdx_only",
        "algorithm_version": MASTER_ALGORITHM,
        "row_count": facts.len(),
        "source_binding_digest": compact_market::digest_hex(&source_binding_digest),
        "task_id": task_id,
        "item_id": item_id,
        "instrument_class": 0,
        "issuer_resolution": "code_date",
        "listing_sources": listing_sources,
        "delisting_sources": delisting_sources,
        "delisting_reason_classes": reason_classes,
        "known_at": "unknown",
        "known_at_reason": MASTER_KNOWN_AT_REASON,
        "limitations": [
            "TDX-only: the exchange listing/delisting branch is PR-3; this batch must not be read as the assembled security master",
            "SSDATE is subject-level; a code reused by a new subject has no separate interval until the exchange branch supplies a boundary",
            "rows with SSDATE='0' are not written: §4.1 defines no defensible effective_from for an unknown listing day",
            "delisting_reason_class is unknown until the exchange list or announcement is admitted (PR-3); arrangement bounds do not prove the class",
            "no derived_dataset_versions row: the dataset is incomplete until PR-3 assembles both branches"
        ],
    }))
}

#[cfg(test)]
mod tests;
