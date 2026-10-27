//! `suspension_intervals` write path and dataset registration (issue-391 §16
//! PR-6).
//!
//! Layer position (separation points 1/2/4): the domain
//! ([`crate::security_master::suspensions`]) owns the rules — "no bar is not a
//! suspension", the gap run merge, the confirmed/derived vocabulary. This
//! module only persists a validated [`SuspensionSet`] and registers its
//! version; it never decides why a day has no bar and never talks to a source.
//! The read side already lives in [`super::security::read_suspension_intervals`]
//! (PR-4) so the read model and the writer share one row decoder.
//!
//! Whole-dataset replace matches the other M2 writers (`security_master`,
//! `capital_events`, `price_limit_rules`): the set is the complete answer for
//! one generation, so a gap that disappeared from the bars disappears from the
//! table in the same commit, and "never written" stays distinguishable from
//! "written empty" (an empty set removes the rows and the registration; the
//! DDL forbids a `record_count = 0` registration).

use rusqlite::{Connection, params};
use serde_json::json;

use super::derived::{self, DerivedVersionRow};
use super::{evidence_store, integrity, storage};
use crate::compact_market::{self, Digest, EvidenceKind};
use crate::market_storage::Error;
use crate::security_master::suspensions::{
    self, ALGORITHM_VERSION, DATASET_KEY, DATASET_KIND, DIGEST_FORMAT, Error as SuspensionError,
    SuspensionSet,
};
use crate::security_master::{SuspensionCertainty, SuspensionInterval};

fn suspension_error(error: SuspensionError) -> Error {
    integrity(error.to_string())
}

fn sort_rows(rows: &mut [SuspensionInterval]) {
    rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.start_day).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.start_day,
        ))
    });
}

/// Replace the whole suspension dataset and register its version, in the
/// caller's transaction. `None` means the set was empty and both the rows and
/// the registration were removed.
///
/// The source binding is the generation's, exactly like the main table: a gap
/// is only meaningful relative to the bars that were stored for that
/// generation. `confirmed` rows keep their own `source_kind` per row, so the
/// per-row provenance survives the dataset-level binding.
pub(crate) fn replace_suspensions(
    c: &Connection,
    set: &SuspensionSet,
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<Option<DerivedVersionRow>, Error> {
    suspensions::validate_set(&set.rows).map_err(suspension_error)?;

    let mut rows = set.rows.clone();
    sort_rows(&mut rows);

    let digest = suspensions::dataset_digest(&rows);
    // The evidence must not claim counts the rows do not have (H3): the split
    // is recomputed here and a hand-built set that disagrees is refused
    // *before* anything is deleted.
    let confirmed_rows = rows
        .iter()
        .filter(|row| row.certainty == SuspensionCertainty::Confirmed)
        .count();
    let single_day_gap_rows = rows
        .iter()
        .filter(|row| row.certainty == SuspensionCertainty::UnresolvedGap && row.missing_days == 1)
        .count();
    let multi_day_gap_rows = rows
        .iter()
        .filter(|row| row.certainty == SuspensionCertainty::UnresolvedGap && row.missing_days > 1)
        .count();
    let gap_rows = single_day_gap_rows + multi_day_gap_rows;
    if (
        set.confirmed_rows,
        set.gap_rows,
        set.single_day_gap_rows,
        set.multi_day_gap_rows,
    ) != (
        confirmed_rows,
        gap_rows,
        single_day_gap_rows,
        multi_day_gap_rows,
    ) {
        return Err(integrity(
            "suspension set counters disagree with its rows; refusing to register \
             evidence that does not describe the stored rows",
        ));
    }

    c.execute("DELETE FROM suspension_intervals", [])
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
        "confirmed_rows": confirmed_rows,
        "gap_rows": gap_rows,
        // §7.3: single-day holes and long outages stay countable apart.
        "single_day_gap_rows": single_day_gap_rows,
        "multi_day_gap_rows": multi_day_gap_rows,
        "gap_missing_days": rows
            .iter()
            .filter(|row| row.certainty == SuspensionCertainty::UnresolvedGap)
            .map(|row| row.missing_days)
            .sum::<i64>(),
        "pending_sources": set.pending_sources.iter().map(|pending| json!({
            "dataset": pending.dataset,
            "source": pending.source,
            "reason": pending.reason,
        })).collect::<Vec<_>>(),
        // A gap is derived and carries no announcement, so its `known_at` is
        // NULL and a `known_at` filter excludes it (separation point 3). The
        // limitation travels with the data instead of being implied.
        "known_at_policy": "unresolved_gap rows carry known_at = NULL; a \
            point-in-time query with known_at excludes them (fail-closed). \
            confirmed rows carry the announcement day when the source has one",
    });
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    let now = chrono::Utc::now().to_rfc3339();

    for row in &rows {
        c.execute(
            "INSERT INTO suspension_intervals(exchange,code,start_day,end_day,certainty,\
             missing_days,reason,known_at,source_kind,source_record_id,evidence_digest) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                row.exchange.as_str(),
                row.code,
                compact_market::epoch_day(row.start_day),
                compact_market::epoch_day(row.end_day),
                row.certainty.as_str(),
                row.missing_days,
                row.reason.as_str(),
                row.known_at.map(|day| day.to_string()),
                row.source_kind,
                row.source_record_id,
                evidence_digest.as_slice(),
            ],
        )
        .map_err(storage)?;
    }

    let first_day = rows
        .iter()
        .map(|row| row.start_day)
        .min()
        .ok_or_else(|| integrity("non-empty suspension set checked above"))?;
    let last_day = rows
        .iter()
        .map(|row| row.end_day)
        .max()
        .ok_or_else(|| integrity("non-empty suspension set checked above"))?;
    let record_count =
        i64::try_from(rows.len()).map_err(|_| integrity("suspension row count overflow"))?;
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
        .ok_or_else(|| integrity("suspension registration vanished after its insert"))
}

/// Every stored suspension row in row-identity order. The decoder is PR-4's,
/// so the writer and the read model cannot drift on encoding.
pub(crate) fn read_suspensions(c: &Connection) -> Result<Vec<SuspensionInterval>, Error> {
    super::security::read_suspension_intervals(c)
}

/// The suspension dataset's registration row, `None` when it was never written
/// or its last write was empty.
pub(crate) fn read_registration(c: &Connection) -> Result<Option<DerivedVersionRow>, Error> {
    derived::read_version(c, DATASET_KIND, DATASET_KEY)
}

#[cfg(test)]
mod tests;
