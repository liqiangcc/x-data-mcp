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
use super::{evidence_store, integrity, series_commit, storage};
use crate::compact_market::{self, Digest, EvidenceKind};
use crate::market_storage::Error;
use crate::security_master::suspensions::{
    self, ALGORITHM_VERSION, DATASET_KEY, DATASET_KIND, DIGEST_FORMAT, Error as SuspensionError,
    SkippedInterval, SuspensionSet,
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

/// Confirmed intervals the domain skipped because the calendar cannot place
/// them (XDM-134). The counters stay complete; the sample list is already
/// truncated by the domain, so this only shapes the evidence.
fn skipped_samples_json(samples: &[SkippedInterval]) -> Vec<serde_json::Value> {
    samples
        .iter()
        .map(|sample| {
            json!({
                "entity": sample.entity,
                "start_day": sample.start_day.format("%Y-%m-%d").to_string(),
                "end_day": sample.end_day.format("%Y-%m-%d").to_string(),
                "source_record_id": sample.source_record_id,
            })
        })
        .collect()
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
    replace_suspensions_with_evidence(c, set, source_binding_digest, task_id, item_id, None)
}

/// The same writer with an optional replay-input digest. `source_store replay
/// --scope suspensions` passes it so a later `--verify-only` can tell a changed
/// master/calendar (the digest names the inputs) from changed stored rows. The
/// daily item leaves it `None`; the rows and registration are otherwise
/// identical.
pub(crate) fn replace_suspensions_with_evidence(
    c: &Connection,
    set: &SuspensionSet,
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
    replay_inputs_digest: Option<&str>,
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
    // XDM-134: a skipped interval has no row, so the H3 reconciliation above
    // cannot see it. The samples must still be a subset of what was counted,
    // otherwise the stored evidence claims samples for intervals it did not
    // skip.
    if set.deferred_samples.len() > set.deferred_beyond_calendar
        || set.skipped_samples.len() > set.skipped_no_trading_day
    {
        return Err(integrity(
            "suspension skip samples outnumber their counters; refusing to register \
             evidence that does not describe the skipped intervals",
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
    let mut evidence = json!({
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
        // XDM-134: confirmed intervals that wrote no row because the calendar
        // cannot place them. Counted and sampled so a deferred weekend or an
        // interval past `last_day` is visible instead of silently absent.
        "deferred_beyond_calendar": set.deferred_beyond_calendar,
        "deferred_samples": skipped_samples_json(&set.deferred_samples),
        "skipped_no_trading_day": set.skipped_no_trading_day,
        "skipped_samples": skipped_samples_json(&set.skipped_samples),
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
    if let (Some(digest), Some(object)) = (replay_inputs_digest, evidence.as_object_mut()) {
        object.insert("replay_inputs_digest".to_string(), json!(digest));
    }
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

/// One whole-market suspension commit driven by the daily
/// `derived:suspensions` item (issue-391 §12.1).
///
/// `master` is the registration of the `security_master` dataset the gaps were
/// derived from. The binding must match the one this commit writes, so a run
/// that reads another generation's master fails closed instead of binding
/// those rows to this generation's members.
pub(crate) struct SuspensionWrite<'a> {
    pub set: &'a SuspensionSet,
    pub master: &'a DerivedVersionRow,
    pub task_id: &'a str,
    pub item_id: &'a str,
    pub generation_id: &'a str,
    pub generation_manifest_sha256: &'a str,
    pub member_sha256: &'a str,
    pub security_event_sha256: &'a str,
    pub generation_evidence: &'a serde_json::Value,
    /// `Some` only for `source_store replay --scope suspensions`: the digest of
    /// the database inputs (master/calendar/bar days) the rows were assembled
    /// from, registered as evidence for a later `--verify-only`.
    pub replay_inputs_digest: Option<&'a str>,
}

/// Write the binding, verify the master came from the same inputs, then
/// replace the whole suspension dataset, in the caller's transaction.
pub(crate) fn commit_suspensions(
    c: &Connection,
    write: &SuspensionWrite<'_>,
) -> Result<Option<DerivedVersionRow>, Error> {
    let binding = series_commit::put_generation_binding(
        c,
        write.generation_id,
        write.generation_manifest_sha256,
        write.member_sha256,
        write.security_event_sha256,
        write.generation_evidence,
    )?;
    if write.master.source_binding_digest != binding {
        return Err(integrity(
            "suspensions derive from a security_master assembled from another generation",
        ));
    }
    replace_suspensions_with_evidence(
        c,
        write.set,
        binding,
        write.task_id,
        write.item_id,
        write.replay_inputs_digest,
    )
}

/// The four inputs `derived:suspensions` derives from, read in one snapshot
/// (issue-391 §12.1). Reading them separately would let the interval list and
/// the bars it is bracketed against come from different commits; taking them
/// on one connection makes "the master this run wrote plus the bars it was
/// written against" a single read.
pub(crate) struct SuspensionInputs {
    pub intervals: Vec<crate::security_master::SecurityLifecycleFact>,
    /// The `security_master` registration the gaps are derived from. Its
    /// binding must match the generation this item commits.
    pub master: DerivedVersionRow,
    pub calendar: Vec<chrono::NaiveDate>,
    pub bar_days: BarDays,
}

/// Entity → raw bar days. A named alias keeps the nested generic out of the
/// struct field (the pre-push shape checker splits it as a field named `Vec`).
pub(crate) type BarDays = std::collections::BTreeMap<String, Vec<chrono::NaiveDate>>;

/// Why [`read_inputs_if_ready`] could not read the four inputs. The replay
/// maps both to the stable `suspension_replay_not_ready` code; the daily item
/// keeps its existing fail-closed messages.
#[derive(Debug)]
pub(crate) enum SuspensionInputsReadiness {
    Ready(SuspensionInputs),
    MissingMaster,
    EmptyCalendar,
}

/// Read [`SuspensionInputs`], reporting which prerequisite is absent instead
/// of collapsing both into one error. A missing master or an empty calendar
/// would otherwise be read as "no gaps", so neither is ever treated as an
/// empty dataset.
pub(crate) fn read_inputs_if_ready(c: &Connection) -> Result<SuspensionInputsReadiness, Error> {
    let intervals = super::security::read_security_master(c)?;
    let Some(master) = derived::read_version(
        c,
        crate::security_master::assembly::DATASET_KIND,
        crate::security_master::assembly::DATASET_KEY,
    )?
    else {
        return Ok(SuspensionInputsReadiness::MissingMaster);
    };
    let calendar: Vec<chrono::NaiveDate> = super::calendar::read_days(c)?
        .into_iter()
        .map(|day| day.day)
        .collect();
    if calendar.is_empty() {
        return Ok(SuspensionInputsReadiness::EmptyCalendar);
    }
    let bar_days = super::series::read_raw_bar_days(c)?;
    Ok(SuspensionInputsReadiness::Ready(SuspensionInputs {
        intervals,
        master,
        calendar,
        bar_days,
    }))
}

/// Read [`SuspensionInputs`], failing closed when an input is absent: a
/// missing master or an empty calendar would otherwise be read as "no gaps".
pub(crate) fn read_inputs(c: &Connection) -> Result<SuspensionInputs, Error> {
    match read_inputs_if_ready(c)? {
        SuspensionInputsReadiness::Ready(inputs) => Ok(inputs),
        SuspensionInputsReadiness::MissingMaster => Err(integrity(
            "suspensions need a stored security_master; none exists for this run",
        )),
        SuspensionInputsReadiness::EmptyCalendar => Err(integrity(
            "suspensions need the derived trading calendar; it is empty",
        )),
    }
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
