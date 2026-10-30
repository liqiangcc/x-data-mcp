//! `risk_warning_intervals` write path and dataset registration (issue-308,
//! issue-446 PR-10).
//!
//! Layer position (separation points 1/2/4): the domain
//! ([`crate::security_master::risk_warnings`]) owns interval assembly and
//! validation; this module only persists a validated [`RiskWarningSet`] and
//! registers its version. It never fetches and never decides what the source
//! meant. The read side stays in
//! [`super::security::read_risk_warning_intervals`], so writer and reader share
//! one row decoder.
//!
//! Whole-dataset replace matches the other whole-market writers: the set is the
//! complete answer for one generation, so an interval that disappeared from the
//! source disappears from the table in the same commit. An empty set removes
//! the rows **and** the registration; the DDL forbids `record_count = 0`, and
//! "never written" must remain distinguishable from "written empty".
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
use crate::security_master::RiskWarningInterval;
use crate::security_master::risk_warnings::{
    self, ALGORITHM_VERSION, DATASET_KEY, DATASET_KIND, DIGEST_FORMAT, Error as RiskWarningError,
    RiskWarningSet,
};

fn risk_error(error: RiskWarningError) -> Error {
    integrity(error.to_string())
}

fn sort_rows(rows: &mut [RiskWarningInterval]) {
    rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
}

/// Replace the whole risk-warning dataset and register its version, in the
/// caller's transaction. `None` means the set was empty and both the rows and
/// the registration were removed.
pub(crate) fn replace_risk_warnings(
    c: &Connection,
    set: &RiskWarningSet,
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<Option<DerivedVersionRow>, Error> {
    risk_warnings::validate_set(&set.rows).map_err(risk_error)?;

    let mut rows = set.rows.clone();
    sort_rows(&mut rows);

    let digest = risk_warnings::dataset_digest(&rows);
    c.execute("DELETE FROM risk_warning_intervals", [])
        .map_err(storage)?;
    if rows.is_empty() {
        c.execute(
            "DELETE FROM derived_dataset_versions WHERE dataset_kind=?1 AND dataset_key=?2",
            params![DATASET_KIND, DATASET_KEY],
        )
        .map_err(storage)?;
        return Ok(None);
    }

    let evidence = json!({
        "dataset": DATASET_KIND,
        "dataset_key": DATASET_KEY,
        "digest_format": DIGEST_FORMAT,
        "algorithm_version": ALGORITHM_VERSION,
        "content_digest": compact_market::digest_hex(&digest),
        "record_count": rows.len(),
        "sz_rows": rows.iter().filter(|row| row.exchange == crate::security_master::Exchange::Sz).count(),
        "sh_rows": rows.iter().filter(|row| row.exchange == crate::security_master::Exchange::Sh).count(),
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
        // D-58: absence of an announcement is `unknown`, never `normal`.
        "absence_policy": "no row is inferred as normal; a missing interval or a \
            not_observed coverage window is unknown (fail-closed)",
        "known_at_policy": "SZ name-history rows carry known_at = NULL (the table has \
            effective dates but no publication time); SH event rows carry the announcement \
            day because it is the earliest time a consumer could know the event",
    });
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    let now = chrono::Utc::now().to_rfc3339();

    for row in &rows {
        c.execute(
            "INSERT INTO risk_warning_intervals(exchange,code,effective_from,effective_to,\
             raw_status_code,normalized_status,announced_at,known_at,source_kind,\
             source_record_id,source_document_sha256,evidence_digest) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                row.exchange.as_str(),
                row.code,
                compact_market::epoch_day(row.effective_from),
                row.effective_to.map(compact_market::epoch_day),
                row.raw_status_code,
                row.status.as_str(),
                row.announced_at,
                row.known_at.map(|day| day.to_string()),
                row.source_kind,
                row.source_record_id,
                row.source_document_sha256,
                evidence_digest.as_slice(),
            ],
        )
        .map_err(storage)?;
    }

    let first_day = rows
        .iter()
        .map(|row| row.effective_from)
        .min()
        .ok_or_else(|| integrity("non-empty risk-warning set checked above"))?;
    let last_day = rows
        .iter()
        .map(|row| row.effective_to.unwrap_or(row.effective_from))
        .max()
        .ok_or_else(|| integrity("non-empty risk-warning set checked above"))?
        .max(first_day);
    let record_count =
        i64::try_from(rows.len()).map_err(|_| integrity("risk-warning row count overflow"))?;
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
        .ok_or_else(|| integrity("risk-warning registration vanished after its insert"))
}

/// One whole-market risk-warning commit driven by `derived:risk_warnings`.
pub(crate) struct RiskWarningWrite<'a> {
    pub set: &'a RiskWarningSet,
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
pub(crate) fn commit_risk_warnings(
    c: &Connection,
    write: &RiskWarningWrite<'_>,
) -> Result<Option<DerivedVersionRow>, Error> {
    let binding = series_commit::put_generation_binding(
        c,
        write.generation_id,
        write.generation_manifest_sha256,
        write.member_sha256,
        write.security_event_sha256,
        write.generation_evidence,
    )?;
    replace_risk_warnings(c, write.set, binding, write.task_id, write.item_id)
}

/// Every stored risk-warning row, decoded by the read model's decoder.
pub(crate) fn read_risk_warnings(c: &Connection) -> Result<Vec<RiskWarningInterval>, Error> {
    super::security::read_risk_warning_intervals(c)
}

/// The dataset's registration row, `None` when it was never written or its
/// last write was empty.
pub(crate) fn read_registration(c: &Connection) -> Result<Option<DerivedVersionRow>, Error> {
    derived::read_version(c, DATASET_KIND, DATASET_KEY)
}

#[cfg(test)]
mod tests;
