//! TDX security-fact writers (M2 PR-2/PR-3, issue-391 §5/§9.2/§16).
//!
//! Layer position (separation point 1): this module knows the two typed tables
//! and the version registration, not the TDX member names or the parser. The
//! adapter (`official_security`) hands over [`CapitalEventSet`] and
//! [`SecurityLifecycleFact`] values; the domain assembly
//! (`security_master::assembly`) hands over the merged [`Assembly`].
//!
//! `capital_events` is a whole-group replace per entity, exactly like
//! `adjustment_events`: delete+insert inside the caller's transaction, and an
//! empty set removes both the rows and the registration. `security_master` has
//! two writers: PR-2's [`upsert_security_master`] keeps the TDX-only rows for a
//! partial run, and PR-3's [`replace_security_master`] replaces the whole
//! assembled dataset and registers its version. The whole-dataset replace is
//! what makes a code that disappeared from both sources disappear from the
//! table instead of surviving as a stale interval (§3.10).

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::derived::{self, DerivedVersionRow};
use super::{evidence_store, integrity, series_commit, storage};
use crate::compact_market::{self, Digest, EvidenceKind};
use crate::market_storage::Error;
use crate::security_master::assembly::{
    self, Assembly, CodeMigrationFact, Conflict, MigrationCertainty, UnresolvedFact,
};
use crate::security_master::{
    ADMITTED_CATEGORIES, CapitalEvent, CapitalEventSet, DelistingReason, DelistingSource, Exchange,
    IssuerResolution, ListingSource, RiskStatus, RiskWarningInterval, SecurityLifecycleFact,
    SuspensionCertainty, SuspensionInterval, SuspensionReason, is_admitted_category, is_suspicious,
};

/// `derived_dataset_versions.dataset_kind` of the capital-event set.
pub(crate) const CAPITAL_EVENTS_KIND: &str = "capital_events";
/// Digest algorithm name in `derived_dataset_versions.digest_format`.
pub(crate) const CAPITAL_EVENTS_DIGEST_FORMAT: &str = "capital-events-v1";
/// Derivation label in `algorithm_version`: the 2dp 万股 -> 股 conversion.
pub(crate) const CAPITAL_EVENTS_ALGORITHM: &str = "gbbq-capital-2dp-v1";
/// `security_master` rows written by PR-2 are TDX-only; PR-3 owns the assembled
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
        event.known_at = read_text_day(raw.11, "capital_events.known_at")?;
        events.push(event);
    }
    Ok(events)
}

/// Every stored `capital_events` row as `(entity, event)`, ordered by entity
/// then ordinal. The export needs the whole table; the per-entity reader above
/// stays for the read model, and both share the same decoder so they cannot
/// drift.
pub(crate) fn read_all_capital_events(
    c: &Connection,
) -> Result<Vec<(String, CapitalEvent)>, Error> {
    let mut statement = c
        .prepare("SELECT exchange,code FROM capital_events GROUP BY exchange,code ORDER BY 1,2")
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(storage)?;
    let mut entities = Vec::new();
    for row in rows {
        entities.push(row.map_err(storage)?);
    }
    let mut all = Vec::new();
    for (exchange, code) in entities {
        let entity = format!("{exchange}.{code}");
        for event in read_capital_events(c, &entity)? {
            all.push((entity.clone(), event));
        }
    }
    Ok(all)
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
        if !is_admitted_category(event.category) {
            return Err(integrity(
                "only capital categories 2/3/5/7/8/9/10 are stored (D-50 R-3 revision)",
            ));
        }
        if !event.prev_is_lagged {
            return Err(integrity(
                "capital_events.prev_is_lagged is always true in schema 24",
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
        "included_categories": ADMITTED_CATEGORIES,
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
        insert_fact(c, fact, source_binding_digest, evidence_digest)?;
    }
    Ok(facts.len())
}

/// One `security_master` row on the identity `(exchange, code,
/// effective_from)`. Shared by PR-2's TDX-only upsert and PR-3's assembled
/// replace so the 22-column layout exists once.
fn insert_fact(
    c: &Connection,
    fact: &SecurityLifecycleFact,
    source_binding_digest: Digest,
    evidence_digest: Digest,
) -> Result<(), Error> {
    let entity = fact.entity();
    let (exchange, code) = split_entity(&entity)?;
    c.execute(
        "INSERT INTO security_master(exchange,code,effective_from,effective_to,\
         instrument_class,issuer_key,issuer_resolution,listing_date,listing_date_tdx,\
         listing_source,b_listing_date,delisting_date,delisting_date_tdx,\
         delisting_source,delisting_reason_class,arrangement_start,arrangement_end,\
         listing_conflict,delisting_conflict,known_at,source_binding_digest,\
         evidence_digest) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,\
         ?20,?21,?22) \
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
            i64::from(fact.instrument_class),
            fact.issuer_key.as_deref(),
            fact.issuer_resolution.as_str(),
            fact.listing_date.map(compact_market::epoch_day),
            fact.listing_date_tdx.map(compact_market::epoch_day),
            fact.listing_source.as_str(),
            fact.b_listing_date.map(compact_market::epoch_day),
            fact.delisting_date.map(compact_market::epoch_day),
            fact.delisting_date_tdx.map(compact_market::epoch_day),
            fact.delisting_source.as_str(),
            fact.delisting_reason_class.as_str(),
            fact.arrangement_start.map(compact_market::epoch_day),
            fact.arrangement_end.map(compact_market::epoch_day),
            fact.listing_conflict as i64,
            fact.delisting_conflict as i64,
            fact.known_at.map(|day| day.to_string()),
            source_binding_digest.as_slice(),
            evidence_digest.as_slice(),
        ],
    )
    .map_err(storage)?;
    Ok(())
}

/// Replace the whole assembled `security_master` dataset and its
/// `code_migrations`, then register the dataset version.
///
/// A whole-dataset delete+insert (not an upsert) is deliberate: the assembly is
/// the complete answer for one generation, so a code that disappeared from
/// both sources must disappear from the table instead of surviving as a stale
/// interval (§3.10). An empty assembly is rejected: the exchange branch may be
/// pending, but the TDX branch always has rows, and an empty table would claim
/// a version it does not hold.
pub(crate) fn replace_security_master(
    c: &Connection,
    assembled: &Assembly,
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<DerivedVersionRow, Error> {
    if assembled.rows.is_empty() {
        return Err(integrity(
            "security_master assembled produced no interval; refusing to publish an empty dataset",
        ));
    }
    let mut seen = BTreeSet::new();
    for fact in &assembled.rows {
        validate_fact(fact)?;
        if !seen.insert((
            fact.exchange.as_str(),
            fact.code.as_str(),
            fact.effective_from,
        )) {
            return Err(integrity(
                "duplicate security_master interval in one assembled",
            ));
        }
    }
    let mut migration_keys = BTreeSet::new();
    for migration in &assembled.migrations {
        validate_migration(migration)?;
        if !migration_keys.insert((
            migration.from_exchange.as_str(),
            migration.from_code.as_str(),
            migration.effective_from,
        )) {
            return Err(integrity("duplicate code_migrations row in one assembled"));
        }
    }

    let digest: Digest = assembly::assembly_digest(&assembled.rows, &assembled.migrations);
    let evidence =
        assembled_master_evidence(assembled, &digest, source_binding_digest, task_id, item_id)?;
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    let now = chrono::Utc::now().to_rfc3339();

    c.execute("DELETE FROM security_master", [])
        .map_err(storage)?;
    c.execute("DELETE FROM code_migrations", [])
        .map_err(storage)?;
    for fact in &assembled.rows {
        insert_fact(c, fact, source_binding_digest, evidence_digest)?;
    }
    for migration in &assembled.migrations {
        c.execute(
            "INSERT INTO code_migrations(from_exchange,from_code,to_exchange,to_code,\
             effective_from,certainty,source_kind,source_record_id,known_at,evidence_digest) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                migration.from_exchange.as_str(),
                migration.from_code,
                migration.to_exchange.as_str(),
                migration.to_code,
                compact_market::epoch_day(migration.effective_from),
                migration.certainty.as_str(),
                migration.source_kind.as_str(),
                migration.source_record_id.as_deref(),
                migration.known_at.map(|day| day.to_string()),
                evidence_digest.as_slice(),
            ],
        )
        .map_err(storage)?;
    }

    let first_day = assembled
        .rows
        .iter()
        .map(|fact| compact_market::epoch_day(fact.effective_from))
        .min()
        .ok_or_else(|| integrity("non-empty assembled checked above"))?;
    // An open interval has no end yet; the latest known day is its start. The
    // registration's CHECK requires first_day <= last_day.
    let last_day = assembled
        .rows
        .iter()
        .map(|fact| compact_market::epoch_day(fact.effective_to.unwrap_or(fact.effective_from)))
        .max()
        .ok_or_else(|| integrity("non-empty assembled checked above"))?
        .max(first_day);
    let record_count = i64::try_from(assembled.rows.len())
        .map_err(|_| integrity("security_master row count overflow"))?;
    let version_id = compact_market::digest_hex(&digest);
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
            assembly::DATASET_KIND,
            assembly::DATASET_KEY,
            version_id,
            digest.as_slice(),
            assembly::DIGEST_FORMAT,
            record_count,
            first_day,
            last_day,
            assembly::ALGORITHM_VERSION,
            source_binding_digest.as_slice(),
            evidence_digest.as_slice(),
            task_id,
            item_id,
            now,
        ],
    )
    .map_err(storage)?;
    derived::read_version(c, assembly::DATASET_KIND, assembly::DATASET_KEY)?
        .ok_or_else(|| integrity("security-master registration vanished after its insert"))
}

/// One whole-market `security_master` commit driven by the daily
/// `derived:security_master` item (issue-391 §12.1).
///
/// The generation binding is written in the same transaction as the rows, so a
/// dataset can never claim a generation whose members it did not read. The
/// member it names is `base.dbf` (the fact file), and the event digest is the
/// gbbq capital/pricing set the assembly consumed.
pub(crate) struct SecurityMasterWrite<'a> {
    pub assembled: &'a Assembly,
    pub task_id: &'a str,
    pub item_id: &'a str,
    pub generation_id: &'a str,
    pub generation_manifest_sha256: &'a str,
    pub member_sha256: &'a str,
    pub security_event_sha256: &'a str,
    pub generation_evidence: &'a Value,
}

/// Write the binding, then replace the whole assembled dataset, in the
/// caller's transaction.
pub(crate) fn commit_security_master(
    c: &Connection,
    write: &SecurityMasterWrite<'_>,
) -> Result<DerivedVersionRow, Error> {
    let binding = series_commit::put_generation_binding(
        c,
        write.generation_id,
        write.generation_manifest_sha256,
        write.member_sha256,
        write.security_event_sha256,
        write.generation_evidence,
    )?;
    replace_security_master(c, write.assembled, binding, write.task_id, write.item_id)
}

/// All stored `security_master` intervals, ordered by the row identity.
/// Used by the read model and by the round-trip tests.
pub(crate) fn read_security_master(c: &Connection) -> Result<Vec<SecurityLifecycleFact>, Error> {
    let mut statement = c
        .prepare(
            "SELECT exchange,code,effective_from,effective_to,instrument_class,issuer_key,\
             issuer_resolution,listing_date,listing_date_tdx,listing_source,b_listing_date,\
             delisting_date,delisting_date_tdx,delisting_source,delisting_reason_class,\
             arrangement_start,arrangement_end,listing_conflict,delisting_conflict,\
             known_at FROM security_master ORDER BY exchange,code,effective_from",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| {
            Ok(RawMasterRow {
                exchange: r.get(0)?,
                code: r.get(1)?,
                effective_from: r.get(2)?,
                effective_to: r.get(3)?,
                instrument_class: r.get(4)?,
                issuer_key: r.get(5)?,
                issuer_resolution: r.get(6)?,
                listing_date: r.get(7)?,
                listing_date_tdx: r.get(8)?,
                listing_source: r.get(9)?,
                b_listing_date: r.get(10)?,
                delisting_date: r.get(11)?,
                delisting_date_tdx: r.get(12)?,
                delisting_source: r.get(13)?,
                delisting_reason_class: r.get(14)?,
                arrangement_start: r.get(15)?,
                arrangement_end: r.get(16)?,
                listing_conflict: r.get(17)?,
                delisting_conflict: r.get(18)?,
                known_at: r.get(19)?,
            })
        })
        .map_err(storage)?;
    let mut facts = Vec::new();
    for row in rows {
        facts.push(row.map_err(storage)?.into_fact()?);
    }
    Ok(facts)
}

struct RawMasterRow {
    exchange: String,
    code: String,
    effective_from: i64,
    effective_to: Option<i64>,
    instrument_class: i64,
    issuer_key: Option<String>,
    issuer_resolution: String,
    listing_date: Option<i64>,
    listing_date_tdx: Option<i64>,
    listing_source: String,
    b_listing_date: Option<i64>,
    delisting_date: Option<i64>,
    delisting_date_tdx: Option<i64>,
    delisting_source: String,
    delisting_reason_class: String,
    arrangement_start: Option<i64>,
    arrangement_end: Option<i64>,
    listing_conflict: i64,
    delisting_conflict: i64,
    known_at: Option<String>,
}

impl RawMasterRow {
    fn into_fact(self) -> Result<SecurityLifecycleFact, Error> {
        let exchange = Exchange::parse(&self.exchange)
            .ok_or_else(|| integrity(format!("security_master exchange {:?}", self.exchange)))?;
        let instrument_class = u8::try_from(self.instrument_class)
            .map_err(|_| integrity("security_master instrument_class is out of range"))?;
        let issuer_resolution = IssuerResolution::parse(&self.issuer_resolution)
            .ok_or_else(|| integrity("security_master issuer_resolution is unregistered"))?;
        let listing_source = ListingSource::parse(&self.listing_source)
            .ok_or_else(|| integrity("security_master listing_source is unregistered"))?;
        let delisting_source = DelistingSource::parse(&self.delisting_source)
            .ok_or_else(|| integrity("security_master delisting_source is unregistered"))?;
        let delisting_reason_class = DelistingReason::parse(&self.delisting_reason_class)
            .ok_or_else(|| integrity("security_master delisting_reason_class is unregistered"))?;
        Ok(SecurityLifecycleFact {
            exchange,
            code: self.code,
            effective_from: read_day(self.effective_from)?,
            effective_to: read_opt_day(self.effective_to)?,
            instrument_class,
            issuer_key: self.issuer_key,
            issuer_resolution,
            listing_date: read_opt_day(self.listing_date)?,
            listing_date_tdx: read_opt_day(self.listing_date_tdx)?,
            listing_source,
            b_listing_date: read_opt_day(self.b_listing_date)?,
            delisting_date: read_opt_day(self.delisting_date)?,
            delisting_date_tdx: read_opt_day(self.delisting_date_tdx)?,
            delisting_source,
            delisting_reason_class,
            arrangement_start: read_opt_day(self.arrangement_start)?,
            arrangement_end: read_opt_day(self.arrangement_end)?,
            listing_conflict: self.listing_conflict != 0,
            delisting_conflict: self.delisting_conflict != 0,
            known_at: read_text_day(self.known_at, "security_master.known_at")?,
        })
    }
}

/// All stored `code_migrations` facts, ordered by the row identity.
pub(crate) fn read_code_migrations(c: &Connection) -> Result<Vec<CodeMigrationFact>, Error> {
    let mut statement = c
        .prepare(
            "SELECT from_exchange,from_code,to_exchange,to_code,effective_from,certainty,\
             source_kind,source_record_id,known_at FROM code_migrations \
             ORDER BY from_exchange,from_code,effective_from",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        })
        .map_err(storage)?;
    let mut migrations = Vec::new();
    for row in rows {
        let (
            from_exchange,
            from_code,
            to_exchange,
            to_code,
            effective_from,
            certainty,
            source_kind,
            source_record_id,
            known_at,
        ) = row.map_err(storage)?;
        let from_exchange = Exchange::parse(&from_exchange)
            .ok_or_else(|| integrity("code_migrations from_exchange is unregistered"))?;
        let to_exchange = Exchange::parse(&to_exchange)
            .ok_or_else(|| integrity("code_migrations to_exchange is unregistered"))?;
        let certainty = MigrationCertainty::parse(&certainty)
            .ok_or_else(|| integrity("code_migrations certainty is unregistered"))?;
        migrations.push(CodeMigrationFact {
            from_exchange,
            from_code,
            to_exchange,
            to_code,
            effective_from: read_day(effective_from)?,
            certainty,
            source_kind,
            source_record_id,
            known_at: read_text_day(known_at, "known_at")?,
        });
    }
    Ok(migrations)
}

/// All stored `risk_warning_intervals` rows, ordered by the row identity.
/// The table has no writer in M2 until #308 admits the production source; the
/// read model still has to see rows so the fail-closed "no interval = unknown"
/// rule is exercised by fixtures (§6.3).
pub(crate) fn read_risk_warning_intervals(
    c: &Connection,
) -> Result<Vec<RiskWarningInterval>, Error> {
    let mut statement = c
        .prepare(
            "SELECT exchange,code,effective_from,effective_to,raw_status_code,\
             normalized_status,announced_at,known_at,source_kind,source_record_id,\
             source_document_sha256 FROM risk_warning_intervals \
             ORDER BY exchange,code,effective_from",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })
        .map_err(storage)?;
    let mut intervals = Vec::new();
    for row in rows {
        let (
            exchange,
            code,
            effective_from,
            effective_to,
            raw_status_code,
            normalized_status,
            announced_at,
            known_at,
            source_kind,
            source_record_id,
            source_document_sha256,
        ) = row.map_err(storage)?;
        let exchange = Exchange::parse(&exchange)
            .ok_or_else(|| integrity("risk_warning_intervals exchange is unregistered"))?;
        let status = RiskStatus::parse(&normalized_status)
            .ok_or_else(|| integrity("risk_warning_intervals status is unregistered"))?;
        intervals.push(RiskWarningInterval {
            exchange,
            code,
            effective_from: read_day(effective_from)?,
            effective_to: read_opt_day(effective_to)?,
            raw_status_code,
            status,
            announced_at,
            known_at: read_text_day(known_at, "risk_warning_intervals.known_at")?,
            source_kind,
            source_record_id,
            source_document_sha256,
        });
    }
    Ok(intervals)
}

/// One `security_name_history` row (§4.4). The writer
/// ([`super::security_names`], M3 PR-12) owns the encoding; this decoder is
/// shared with the export and with `source-store replay`, and validates the two
/// CHECK vocabularies against the domain enums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityNameRow {
    pub exchange: String,
    pub code: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub name: String,
    pub certainty: crate::security_master::names::NameCertainty,
    pub known_at: Option<NaiveDate>,
    pub source_kind: crate::security_master::names::NameSourceKind,
    pub source_record_id: Option<String>,
}

/// All stored `security_name_history` rows, ordered by the row identity.
pub(crate) fn read_security_names(c: &Connection) -> Result<Vec<SecurityNameRow>, Error> {
    let mut statement = c
        .prepare(
            "SELECT exchange,code,effective_from,effective_to,name,certainty,known_at,\
             source_kind,source_record_id FROM security_name_history \
             ORDER BY exchange,code,effective_from",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        })
        .map_err(storage)?;
    let mut names = Vec::new();
    for row in rows {
        let (exchange, code, from, to, name, certainty, known_at, source_kind, record) =
            row.map_err(storage)?;
        // The DDL CHECKs already restrict both columns; validating them here
        // means a database written by another binary cannot be read as a
        // vocabulary this binary does not understand (fail closed).
        let certainty = crate::security_master::names::NameCertainty::parse(&certainty)
            .ok_or_else(|| integrity("security_name_history certainty is unregistered"))?;
        let source_kind = crate::security_master::names::NameSourceKind::parse(&source_kind)
            .ok_or_else(|| integrity("security_name_history source_kind is unregistered"))?;
        names.push(SecurityNameRow {
            exchange,
            code,
            effective_from: read_day(from)?,
            effective_to: read_opt_day(to)?,
            name,
            certainty,
            known_at: read_text_day(known_at, "security_name_history.known_at")?,
            source_kind,
            source_record_id: record,
        });
    }
    Ok(names)
}

/// All stored `suspension_intervals` rows, ordered by the row identity. The
/// writer, its validation and the `unresolved_gap` derivation are
/// [`super::suspensions`] (PR-6); this decoder is shared with both so the
/// writer and every reader cannot drift on encoding.
pub(crate) fn read_suspension_intervals(c: &Connection) -> Result<Vec<SuspensionInterval>, Error> {
    let mut statement = c
        .prepare(
            "SELECT exchange,code,start_day,end_day,certainty,missing_days,reason,\
             known_at,source_kind,source_record_id FROM suspension_intervals \
             ORDER BY exchange,code,start_day",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })
        .map_err(storage)?;
    let mut intervals = Vec::new();
    for row in rows {
        let (
            exchange,
            code,
            start_day,
            end_day,
            certainty,
            missing_days,
            reason,
            known_at,
            source_kind,
            source_record_id,
        ) = row.map_err(storage)?;
        let exchange = Exchange::parse(&exchange)
            .ok_or_else(|| integrity("suspension_intervals exchange is unregistered"))?;
        let certainty = SuspensionCertainty::parse(&certainty)
            .ok_or_else(|| integrity("suspension_intervals certainty is unregistered"))?;
        let reason = SuspensionReason::parse(&reason)
            .ok_or_else(|| integrity("suspension_intervals reason is unregistered"))?;
        intervals.push(SuspensionInterval {
            exchange,
            code,
            start_day: read_day(start_day)?,
            end_day: read_day(end_day)?,
            certainty,
            missing_days,
            reason,
            known_at: read_text_day(known_at, "suspension_intervals.known_at")?,
            source_kind,
            source_record_id,
        });
    }
    Ok(intervals)
}

fn read_day(value: i64) -> Result<NaiveDate, Error> {
    compact_market::epoch_date(value).map_err(integrity)
}

fn read_opt_day(value: Option<i64>) -> Result<Option<NaiveDate>, Error> {
    value.map(read_day).transpose()
}

/// A `known_at`/`announced_at` TEXT column read at day granularity. The column
/// keeps the source's ISO8601 text (the DDL stores seconds when a source has
/// them); the M2 read model promises day granularity only (§6.2), so the date
/// prefix is what bounds a point-in-time query.
fn read_text_day(value: Option<String>, field: &str) -> Result<Option<NaiveDate>, Error> {
    value
        .map(|text| {
            let day = text.get(..10).unwrap_or(&text);
            NaiveDate::parse_from_str(day, "%Y-%m-%d")
                .map_err(|_| integrity(format!("{field} {text:?} is not a date")))
        })
        .transpose()
}

/// The `derived` evidence bundle of the assembled dataset.
fn assembled_master_evidence(
    assembled: &Assembly,
    digest: &Digest,
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<Value, Error> {
    let conflicts: Vec<Value> = assembled.conflicts.iter().map(conflict_json).collect();
    let pending_sources: Vec<Value> = assembled
        .pending_sources
        .iter()
        .map(|pending| {
            json!({
                "dataset": pending.dataset,
                "source": pending.source,
                "reason": pending.reason,
            })
        })
        .collect();
    let unresolved: Vec<Value> = assembled.unresolved.iter().map(unresolved_json).collect();
    let first_day = assembled
        .rows
        .iter()
        .map(|fact| fact.effective_from)
        .min()
        .ok_or_else(|| integrity("evidence requires a non-empty assembled"))?;
    let last_day = assembled
        .rows
        .iter()
        .map(|fact| fact.effective_to.unwrap_or(fact.effective_from))
        .max()
        .ok_or_else(|| integrity("evidence requires a non-empty assembled"))?;
    Ok(json!({
        "dataset": assembly::DATASET_KIND,
        "dataset_key": assembly::DATASET_KEY,
        "digest_format": assembly::DIGEST_FORMAT,
        "algorithm_version": assembly::ALGORITHM_VERSION,
        "content_digest": compact_market::digest_hex(digest),
        "source_binding_digest": compact_market::digest_hex(&source_binding_digest),
        "task_id": task_id,
        "item_id": item_id,
        "row_count": assembled.rows.len(),
        "migration_count": assembled.migrations.len(),
        "confirmed_migration_count": assembled
            .migrations
            .iter()
            .filter(|migration| migration.certainty == MigrationCertainty::Confirmed)
            .count(),
        "candidate_migration_count": assembled
            .migrations
            .iter()
            .filter(|migration| migration.certainty == MigrationCertainty::Candidate)
            .count(),
        "ambiguous_code_reuse_count": assembled.ambiguous_code_reuse_count(),
        "listing_conflict_count": assembled
            .rows
            .iter()
            .filter(|fact| fact.listing_conflict)
            .count(),
        "delisting_conflict_count": assembled
            .rows
            .iter()
            .filter(|fact| fact.delisting_conflict)
            .count(),
        "first_day": first_day.to_string(),
        "last_day": last_day.to_string(),
        "exchange_as_of": assembled.exchange_as_of.map(|day| day.to_string()),
        "listing_sources": counts(assembled.rows.iter().map(|fact| fact.listing_source.as_str())),
        "delisting_sources": counts(assembled.rows.iter().map(|fact| fact.delisting_source.as_str())),
        "issuer_resolutions": counts(
            assembled.rows.iter().map(|fact| fact.issuer_resolution.as_str())
        ),
        "conflicts": conflicts,
        "pending_sources": pending_sources,
        "unresolved": unresolved,
        "known_at": known_at_coverage(assembled),
        "known_at_reason": ASSEMBLED_KNOWN_AT_REASON,
        "limitations": [
            "listing_conflict=1 rows keep both the exchange (code-level) and TDX (subject-level) listing dates; listing_date follows §4.2 and the disagreement is enumerated in conflicts",
            "issuer_resolution=ambiguous marks an entity whose history crosses a subject-change boundary; strict universe excludes it (Q3)",
            "candidate code migrations are leads only and never collapse two code histories (§9.2)",
            "unresolved facts are counted here, never written as a guessed interval"
        ],
    }))
}

const ASSEMBLED_KNOWN_AT_REASON: &str = "the exchange list and the TDX files carry no announcement timestamp; a row's known_at is stored only when a source provides one and stays NULL otherwise, never proxied by the effective day";

/// How much of the assembled main table carries a 知晓时间. Reported in the
/// evidence bundle so a reader cannot mistake "the sources give none today" for
/// "the column is unused" (§12.3).
fn known_at_coverage(assembled: &Assembly) -> &'static str {
    let dated = assembled
        .rows
        .iter()
        .filter(|row| row.known_at.is_some())
        .count();
    if dated == 0 {
        "unknown"
    } else if dated == assembled.rows.len() {
        "complete"
    } else {
        "partial"
    }
}

fn counts<'a>(values: impl Iterator<Item = &'a str>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for value in values {
        *counts.entry(value.to_string()).or_insert(0) += 1;
    }
    counts
}

fn conflict_json(conflict: &Conflict) -> Value {
    match conflict {
        Conflict::ListingDate {
            exchange,
            code,
            exchange_date,
            tdx_date,
        } => json!({
            "kind": "listing_date",
            "exchange": exchange.as_str(),
            "code": code,
            "exchange_date": exchange_date.to_string(),
            "tdx_date": tdx_date.to_string(),
        }),
        Conflict::DelistingDate {
            exchange,
            code,
            exchange_date,
            tdx_date,
        } => json!({
            "kind": "delisting_date",
            "exchange": exchange.as_str(),
            "code": code,
            "exchange_date": exchange_date.to_string(),
            "tdx_date": tdx_date.to_string(),
        }),
        Conflict::AmbiguousCodeReuse {
            exchange,
            code,
            from,
            to,
        } => json!({
            "kind": "ambiguous_code_reuse",
            "exchange": exchange.as_str(),
            "code": code,
            "from": from.to_string(),
            "to": to.map(|day| day.to_string()),
        }),
    }
}

fn unresolved_json(fact: &UnresolvedFact) -> Value {
    match fact {
        UnresolvedFact::ListingWithoutDate { exchange, code } => json!({
            "kind": "listing_without_date",
            "exchange": exchange.as_str(),
            "code": code,
        }),
        UnresolvedFact::DelistingWithoutInterval {
            exchange,
            code,
            date,
        } => json!({
            "kind": "delisting_without_interval",
            "exchange": exchange.as_str(),
            "code": code,
            "date": date.to_string(),
        }),
        UnresolvedFact::MigrationWithoutFromInterval {
            from,
            to,
            effective_from,
        } => json!({
            "kind": "migration_without_from_interval",
            "from": from,
            "to": to,
            "effective_from": effective_from.to_string(),
        }),
        UnresolvedFact::MigrationWithoutToInterval {
            from,
            to,
            effective_from,
        } => json!({
            "kind": "migration_without_to_interval",
            "from": from,
            "to": to,
            "effective_from": effective_from.to_string(),
        }),
    }
}

fn validate_fact(fact: &SecurityLifecycleFact) -> Result<(), Error> {
    let entity = fact.entity();
    compact_market::canonical_entity(&entity).map_err(|error| integrity(error.to_string()))?;
    if fact.code.len() != 6 || !fact.code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(integrity("security_master code is not six digits"));
    }
    if fact.instrument_class > 2 {
        return Err(integrity(
            "security_master instrument_class is outside 0/1/2",
        ));
    }
    if fact.issuer_key.as_deref() == Some("") {
        return Err(integrity("security_master issuer_key is empty"));
    }
    if fact.issuer_key.is_some() && fact.issuer_resolution == IssuerResolution::CodeDate {
        return Err(integrity(
            "security_master has an issuer_key but issuer_resolution=code_date",
        ));
    }
    if let Some(to) = fact.effective_to
        && to <= fact.effective_from
    {
        return Err(integrity("effective_to is not after effective_from"));
    }
    if fact.listing_date.is_some() != (fact.listing_source != ListingSource::Unknown) {
        return Err(integrity("listing_date and listing_source disagree"));
    }
    if fact.delisting_date.is_some() != (fact.delisting_source != DelistingSource::Unknown) {
        return Err(integrity("delisting_date and delisting_source disagree"));
    }
    if let (Some(start), Some(end)) = (fact.arrangement_start, fact.arrangement_end)
        && start > end
    {
        return Err(integrity("arrangement_start is after arrangement_end"));
    }
    Ok(())
}

fn validate_migration(migration: &CodeMigrationFact) -> Result<(), Error> {
    if migration.from_code.len() != 6
        || !migration
            .from_code
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        || migration.to_code.len() != 6
        || !migration.to_code.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(integrity("code_migrations code is not six digits"));
    }
    if migration.from_exchange == migration.to_exchange && migration.from_code == migration.to_code
    {
        return Err(integrity("code_migrations does not change the code"));
    }
    if migration.source_kind.is_empty() {
        return Err(integrity("code_migrations source_kind is empty"));
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
