//! `security_name_history` write path and dataset registration (issue-308 S-6,
//! issue-446 PR-12).
//!
//! Layer position (separation points 1/2/4): the domain
//! ([`crate::security_master::names`]) owns interval assembly and validation;
//! this module only persists a validated [`NameSet`] and registers its version.
//! It never fetches and never decides what a change means. The read side stays
//! in [`super::security::read_security_names`], so writer and reader share one
//! row decoder.
//!
//! Whole-dataset replace matches the other whole-market writers: the set is the
//! complete answer for one generation, so an interval that disappeared from the
//! source disappears from the table in the same commit.
//!
//! ## Two subsets, one registration (D-76 Q1)
//!
//! The table holds two disjoint subsets that are replaced independently:
//!
//! - the SZ A-share rows (`source_kind <> 'exchange_snapshot'`), replaced from
//!   the [`NameSet`] exactly as before, and
//! - the ETF observed-name rows (`source_kind = 'exchange_snapshot'`), replaced
//!   from an [`EtfNameSet`] only when the ETF source answered
//!   ([`EtfPart::Replace`]). [`EtfPart::Keep`] leaves them as they are: a
//!   failed, pending or absent ETF source never deletes a name, and an ETF
//!   write never deletes or edits an A-share row.
//!
//! Both subsets sit under the one `security_names`/`a_share` registration (a
//! second `dataset_key` of the same kind would make the reader refuse to
//! choose), whose digest covers every stored row. An empty set removes
//! the rows **and** the registration; the DDL forbids `record_count = 0`, and
//! "never written" must remain distinguishable from "written empty". The
//! adapter never admits an empty SZSE table (a whole-market snapshot with no
//! change row is a source defect), so this branch cannot erase a history on a
//! quiet day.
//!
//! A source failure never reaches this module: the caller leaves the previous
//! version readable instead of deleting it for an acquisition that did not
//! complete.

use rusqlite::{Connection, params};
use serde_json::json;

use super::derived::{self, DerivedVersionRow};
use super::{evidence_store, integrity, series_commit, storage};
use crate::compact_market::{self, Digest, EvidenceKind};
use crate::market_storage::Error;
use crate::security_master::Exchange;
use crate::security_master::names::{
    self, ALGORITHM_VERSION, DATASET_KEY, DATASET_KIND, DIGEST_FORMAT, ETF_ALGORITHM_VERSION,
    Error as NameError, EtfNameSet, NameInterval, NameSet, NameSourceKind,
};
use std::collections::BTreeSet;

fn name_error(error: NameError) -> Error {
    integrity(error.to_string())
}

fn sort_rows(rows: &mut [NameInterval]) {
    rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
}

/// What the writer does with the ETF subset.
#[derive(Debug, Clone, Copy)]
pub(crate) enum EtfPart<'a> {
    /// Leave the stored ETF rows as they are (ETF source failed, pending, or
    /// not part of this write).
    Keep,
    /// The ETF source answered: replace the ETF subset with these rows.
    Replace(&'a EtfNameSet),
}

/// Replace the A-share name dataset and register its version, in the caller's
/// transaction. The ETF subset is kept. (Tests and fixtures; the daily item
/// and replay go through [`commit_security_names`].) `None` means nothing is left to
/// register: no A-share row and no kept ETF row.
#[cfg(test)]
pub(crate) fn replace_security_names(
    c: &Connection,
    set: &NameSet,
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<Option<DerivedVersionRow>, Error> {
    replace_security_names_with(
        c,
        set,
        EtfPart::Keep,
        source_binding_digest,
        task_id,
        item_id,
    )
}

/// [`replace_security_names`] with an explicit ETF subset decision.
pub(crate) fn replace_security_names_with(
    c: &Connection,
    set: &NameSet,
    etf: EtfPart<'_>,
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<Option<DerivedVersionRow>, Error> {
    // The A-share path first and on its own: an `exchange_snapshot` row or a
    // non-SZ row in the A-share set is refused here, whatever the ETF part is.
    names::validate_set(&set.rows).map_err(name_error)?;

    let (etf_rows, etf_entities, etf_new): (Vec<NameInterval>, BTreeSet<_>, bool) = match etf {
        EtfPart::Replace(etf) => (etf.rows.clone(), etf.entities.clone(), true),
        EtfPart::Keep => {
            let kept = stored_etf_rows(c)?;
            let entities = kept
                .iter()
                .map(|row| (row.exchange, row.code.clone()))
                .collect();
            (kept, entities, false)
        }
    };
    let mut rows = set.rows.clone();
    rows.extend(etf_rows.iter().cloned());
    sort_rows(&mut rows);
    // One validation over the whole table, with the ETF entities declared:
    // overlaps and duplicate keys across the two subsets are caught here.
    names::validate_set_for(&rows, &etf_entities).map_err(name_error)?;

    let digest = names::dataset_digest(&rows);
    c.execute(
        "DELETE FROM security_name_history WHERE source_kind <> ?1",
        params![NameSourceKind::ExchangeSnapshot.as_str()],
    )
    .map_err(storage)?;
    if etf_new {
        c.execute(
            "DELETE FROM security_name_history WHERE source_kind = ?1",
            params![NameSourceKind::ExchangeSnapshot.as_str()],
        )
        .map_err(storage)?;
    }
    if rows.is_empty() {
        c.execute(
            "DELETE FROM derived_dataset_versions WHERE dataset_kind=?1 AND dataset_key=?2",
            params![DATASET_KIND, DATASET_KEY],
        )
        .map_err(storage)?;
        return Ok(None);
    }
    let etf_json = etf_evidence(&rows);
    let evidence = json!({
        "dataset": DATASET_KIND,
        "dataset_key": DATASET_KEY,
        "digest_format": DIGEST_FORMAT,
        "algorithm_version": ALGORITHM_VERSION,
        "content_digest": compact_market::digest_hex(&digest),
        "record_count": rows.len(),
        "sz_rows": rows.iter().filter(|row| {
            row.exchange == Exchange::Sz && row.source_kind != NameSourceKind::ExchangeSnapshot
        }).count(),
        "interval_rows": rows.iter().filter(|row| row.certainty == names::NameCertainty::Interval).count(),
        "unbounded_rows": rows.iter().filter(|row| row.certainty == names::NameCertainty::Unbounded).count(),
        "observed_windows": set.observed_windows(),
        "not_observed_windows": set.not_observed_windows(),
        "coverage": set.coverage.iter().map(|window| json!({
            "source_kind": window.source_kind,
            "exchange": window.exchange.as_str(),
            "from": window.from.to_string(),
            "to": window.to.to_string(),
            "observed": window.observed,
            "rows": window.rows,
            "reason": window.reason,
        })).collect::<Vec<_>>(),
        "pending_sources": set.pending_sources.iter().map(|pending| json!({
            "dataset": pending.dataset,
            "source": pending.source,
            "reason": pending.reason,
        })).collect::<Vec<_>>(),
        // D-61: SH/BJ have no dated name source, so their absence is a
        // coverage window, never a row inferred from a current name.
        "market_tiers": {
            "sz": "APPROVED_SOURCE: SSGSGMXX/tab2 change dates",
            "sh": "NOT_AVAILABLE: no dated name history",
            "bj": "NOT_AVAILABLE: no canonical dated name source",
        },
        // §5.3/Q4: `known_at` is the day the first snapshot carrying the row was
        // stored (the adapter resolves it), never the change day and never the
        // write day. The two coverage claims stay separate: effective-time
        // history from the source start, knowledge-time PIT from the first
        // capture.
        "known_at_policy": "SZ name rows carry known_at = the day the first snapshot \
            containing the change row entered the source store; the change day is never \
            used as known_at",
        "known_at_coverage": {
            "effective_history": "effective-time name history is available from the \
                source start (SZSE SSGSGMXX/tab2, 1994-01-03)",
            "known_at_pit": "knowledge-time PIT (known_at filtering) is available from \
                the first capture day; rows written before this rule keep known_at = NULL \
                until a fresh init rewrites them",
        },
        "absence_policy": "a not_observed coverage window is not a name; a missing \
            interval stays unknown and no current name is back-filled",
    });
    // An A-share-only table registers exactly the evidence it always did; the
    // ETF block exists only when ETF rows do.
    let mut evidence = evidence;
    if let (Some(etf), Some(object)) = (etf_json, evidence.as_object_mut()) {
        object.insert("etf".into(), etf);
    }
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    let now = chrono::Utc::now().to_rfc3339();

    for row in rows
        .iter()
        .filter(|row| row.source_kind != NameSourceKind::ExchangeSnapshot || etf_new)
    {
        c.execute(
            "INSERT INTO security_name_history(exchange,code,effective_from,effective_to,\
             name,certainty,known_at,source_kind,source_record_id,evidence_digest) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                row.exchange.as_str(),
                row.code,
                compact_market::epoch_day(row.effective_from),
                row.effective_to.map(compact_market::epoch_day),
                row.name,
                row.certainty.as_str(),
                row.known_at.map(|day| day.to_string()),
                row.source_kind.as_str(),
                row.source_record_id,
                evidence_digest.as_slice(),
            ],
        )
        .map_err(storage)?;
    }

    let first_day = rows
        .iter()
        .map(|row| row.effective_from)
        .min()
        .ok_or_else(|| integrity("non-empty name set checked above"))?;
    let last_day = rows
        .iter()
        .map(|row| row.effective_to.unwrap_or(row.effective_from))
        .max()
        .ok_or_else(|| integrity("non-empty name set checked above"))?
        .max(first_day);
    let record_count =
        i64::try_from(rows.len()).map_err(|_| integrity("name-history row count overflow"))?;
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
            DATASET_KIND,
            DATASET_KEY,
            compact_market::digest_hex(&digest),
            digest.as_slice(),
            DIGEST_FORMAT,
            record_count,
            compact_market::epoch_day(first_day),
            compact_market::epoch_day(last_day),
            ALGORITHM_VERSION,
            source_binding_digest.as_slice(),
            evidence_digest.as_slice(),
            task_id,
            item_id,
            now,
        ],
    )
    .map_err(storage)?;
    derived::read_version(c, DATASET_KIND, DATASET_KEY)?
        .map(Some)
        .ok_or_else(|| integrity("name-history registration vanished after its insert"))
}

/// One whole-market name-history commit driven by `derived:security_names`.
pub(crate) struct SecurityNameWrite<'a> {
    pub set: &'a NameSet,
    pub etf: EtfPart<'a>,
    pub task_id: &'a str,
    pub item_id: &'a str,
    pub generation_id: &'a str,
    pub generation_manifest_sha256: &'a str,
    pub member_sha256: &'a str,
    pub security_event_sha256: &'a str,
    pub generation_evidence: &'a serde_json::Value,
}

/// Write the generation binding, then replace the whole dataset, in the
/// caller's transaction.
pub(crate) fn commit_security_names(
    c: &Connection,
    write: &SecurityNameWrite<'_>,
) -> Result<Option<DerivedVersionRow>, Error> {
    let binding = series_commit::put_generation_binding(
        c,
        write.generation_id,
        write.generation_manifest_sha256,
        write.member_sha256,
        write.security_event_sha256,
        write.generation_evidence,
    )?;
    replace_security_names_with(
        c,
        write.set,
        write.etf,
        binding,
        write.task_id,
        write.item_id,
    )
}

/// The ETF subset as the table holds it now.
fn stored_etf_rows(c: &Connection) -> Result<Vec<NameInterval>, Error> {
    super::security::read_security_names(c)?
        .iter()
        .filter(|row| row.source_kind == NameSourceKind::ExchangeSnapshot)
        .map(super::security::SecurityNameRow::to_interval)
        .collect()
}

/// Replace **only** the `exchange_snapshot` (ETF) subset, preserving the
/// A-share rows and their registration evidence (PR-4 replay scope).
///
/// The delisted-name replay is an explicit, database-dependent scope: it has
/// no SZSE short-name page set of its own, so it cannot call
/// [`replace_security_names_with`] without rebuilding (and re-dating) the
/// A-share half. This writer reads the stored A-share rows, validates the
/// combined table with the ETF entities declared, and re-registers the version
/// with the existing evidence object's coverage/pending fields left untouched.
/// Only the digest, counts and the ETF evidence block change.
pub(crate) fn replace_etf_names_only(
    c: &Connection,
    etf: &EtfNameSet,
    task_id: &str,
    item_id: &str,
) -> Result<Option<DerivedVersionRow>, Error> {
    let stored: Vec<NameInterval> = super::security::read_security_names(c)?
        .iter()
        .map(super::security::SecurityNameRow::to_interval)
        .collect::<Result<Vec<_>, _>>()?;
    let a_share: Vec<NameInterval> = stored
        .iter()
        .filter(|row| row.source_kind != NameSourceKind::ExchangeSnapshot)
        .cloned()
        .collect();
    names::validate_set(&a_share).map_err(name_error)?;
    let mut rows = a_share;
    rows.extend(etf.rows.iter().cloned());
    sort_rows(&mut rows);
    names::validate_set_for(&rows, &etf.entities).map_err(name_error)?;
    let digest = names::dataset_digest(&rows);
    c.execute(
        "DELETE FROM security_name_history WHERE source_kind = ?1",
        params![NameSourceKind::ExchangeSnapshot.as_str()],
    )
    .map_err(storage)?;
    if rows.is_empty() {
        c.execute(
            "DELETE FROM derived_dataset_versions WHERE dataset_kind=?1 AND dataset_key=?2",
            params![DATASET_KIND, DATASET_KEY],
        )
        .map_err(storage)?;
        return Ok(None);
    }
    let mut evidence = derived::read_version(c, DATASET_KIND, DATASET_KEY)
        .map_err(storage)?
        .and_then(|version| {
            let (kind, text) = evidence_store::read_evidence(c, &version.evidence_digest).ok()?;
            (kind == EvidenceKind::Derived)
                .then(|| serde_json::from_str::<serde_json::Value>(&text).ok())
                .flatten()
        })
        .unwrap_or_else(|| {
            json!({
                "dataset": DATASET_KIND,
                "dataset_key": DATASET_KEY,
                "digest_format": DIGEST_FORMAT,
                "algorithm_version": ALGORITHM_VERSION,
                "coverage": [],
                "pending_sources": [],
            })
        });
    if let Some(object) = evidence.as_object_mut() {
        object.insert(
            "content_digest".into(),
            json!(compact_market::digest_hex(&digest)),
        );
        object.insert("record_count".into(), json!(rows.len()));
        object.insert(
            "interval_rows".into(),
            json!(
                rows.iter()
                    .filter(|row| row.certainty == names::NameCertainty::Interval)
                    .count()
            ),
        );
        object.insert(
            "unbounded_rows".into(),
            json!(
                rows.iter()
                    .filter(|row| row.certainty == names::NameCertainty::Unbounded)
                    .count()
            ),
        );
        if let Some(etf_json) = etf_evidence(&rows) {
            object.insert("etf".into(), etf_json);
        } else {
            object.remove("etf");
        }
        object.insert("etf_replay_scope".into(), json!("delisted_etf_names"));
    }
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    for row in rows
        .iter()
        .filter(|row| row.source_kind == NameSourceKind::ExchangeSnapshot)
    {
        c.execute(
            "INSERT INTO security_name_history(exchange,code,effective_from,effective_to,\
             name,certainty,known_at,source_kind,source_record_id,evidence_digest) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                row.exchange.as_str(),
                row.code,
                compact_market::epoch_day(row.effective_from),
                row.effective_to.map(compact_market::epoch_day),
                row.name,
                row.certainty.as_str(),
                row.known_at.map(|day| day.to_string()),
                row.source_kind.as_str(),
                row.source_record_id,
                evidence_digest.as_slice(),
            ],
        )
        .map_err(storage)?;
    }
    let now = chrono::Utc::now().to_rfc3339();
    c.execute(
        "UPDATE derived_dataset_versions SET version_id=?3, content_digest=?4, \
         record_count=?5, evidence_digest=?6, task_id=?7, item_id=?8, updated_at=?9 \
         WHERE dataset_kind=?1 AND dataset_key=?2",
        params![
            DATASET_KIND,
            DATASET_KEY,
            compact_market::digest_hex(&digest),
            digest.as_slice(),
            i64::try_from(rows.len()).unwrap_or(i64::MAX),
            evidence_digest.as_slice(),
            task_id,
            item_id,
            now,
        ],
    )
    .map_err(storage)?;
    derived::read_version(c, DATASET_KIND, DATASET_KEY)?
        .map(Some)
        .ok_or_else(|| integrity("name-history registration vanished after the ETF replay"))
}

/// Registration evidence of the ETF subset. A pure function of the rows, so a
/// daily write and a replay of the same store register identical evidence.
/// What only the daily item knows (the selected list, unresolved codes) goes
/// on the item outcome instead.
fn etf_evidence(rows: &[NameInterval]) -> Option<serde_json::Value> {
    let etf: Vec<&NameInterval> = rows
        .iter()
        .filter(|row| row.source_kind == NameSourceKind::ExchangeSnapshot)
        .collect();
    if etf.is_empty() {
        return None;
    }
    Some(json!({
        "source_kind": NameSourceKind::ExchangeSnapshot.as_str(),
        "algorithm_version": ETF_ALGORITHM_VERSION,
        "basis": "fetch_time",
        "rows": etf.len(),
        "codes": etf.iter().map(|row| (row.exchange, row.code.as_str())).collect::<BTreeSet<_>>().len(),
        "interval_rows": etf.iter().filter(|row| row.certainty == names::NameCertainty::Interval).count(),
        "unbounded_rows": etf.iter().filter(|row| row.certainty == names::NameCertainty::Unbounded).count(),
        "effective_from": "the first observation day of the name in the frozen ETF list \
            snapshots, not the exchange's rename day; no row exists before a code's first \
            observation and nothing is back-filled",
        "known_at": "the Shanghai day the observing snapshot entered the source store",
        "source_record_id": "<list source id>:<snapshot sha256> of the observing snapshot",
    }))
}

/// All stored name rows, decoded by the read model's decoder (writer tests;
/// the `security_names` replay scope reads through the read model directly).
#[cfg(test)]
pub(crate) fn read_names(
    c: &Connection,
) -> Result<Vec<crate::sqlite_market::security::SecurityNameRow>, Error> {
    super::security::read_security_names(c)
}

/// The dataset's registration row, `None` when it was never written or its
/// last write was empty.
#[cfg(test)]
pub(crate) fn read_registration(c: &Connection) -> Result<Option<DerivedVersionRow>, Error> {
    derived::read_version(c, DATASET_KIND, DATASET_KEY)
}

#[cfg(test)]
mod tests;
