//! Trading-calendar storage (issue-383 §3.8/§5.2, M1 PR-6): the derived
//! `trading_days` table, its `derived_dataset_versions` registration and the
//! coverage self-check that gates the write.
//!
//! Layer position (separation points 1/2/4): the adapter hands over
//! [`CalendarBatch`] (per registered input: dates, member digest, and the
//! empty corporate-action digest), the domain
//! ([`crate::trading_calendar`]) computes `cal-union-v1` and the threshold, and
//! this module only persists the result. No source name, file name or index
//! code appears here: the input list is the domain registration, and the
//! per-date mask comes from it.
//!
//! The table is replaced wholesale inside the caller's transaction (§3.8: ~8.8k
//! rows, delete+insert, no incremental patch), so a date that disappears from
//! the inputs disappears from the calendar in the same commit. Identity and
//! audit live in the registration row, not in the rows themselves (§3.10).

use chrono::NaiveDate;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use super::derived::{self, DerivedVersionRow};
use super::{evidence_store, integrity, series_commit, storage};
use crate::compact_market::{self, EvidenceKind};
use crate::market_data::CalendarBatch;
use crate::market_storage::Error;
use crate::trading_calendar::{
    self, COVERAGE_BAR_LIMIT, COVERAGE_DAY_LIMIT, CalendarCoverage, TradingCalendar, TradingDay,
};

fn invalid(message: impl std::fmt::Display) -> Error {
    Error::Invalid(message.to_string())
}

/// Why the calendar may not be used as a trading predicate (§5.2). Stored in
/// the evidence so the limitation travels with the data, not with a document.
const ONE_WAY_SEMANTICS: &str = "a row means at least one calendar input index \
     had a bar that day; a missing row inside [first_day,last_day] means a \
     non-trading day; a row does NOT mean a security could be traded that day \
     (suspension, before listing and after delisting are outside the calendar)";
const COMPLETENESS_LIMIT: &str = "there is no official calendar file: 'an index \
     had a bar' is the only evidence a date is a trading day, so the calendar is \
     derived data and must not be used to decide suspensions";

/// Every stored calendar day, ascending.
pub(crate) fn read_days(c: &Connection) -> Result<Vec<TradingDay>, Error> {
    let mut statement = c
        .prepare("SELECT trade_day,sources FROM trading_days ORDER BY trade_day")
        .map_err(storage)?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .map_err(storage)?;
    let mut days = Vec::new();
    for row in rows {
        let (day, sources) = row.map_err(storage)?;
        days.push(TradingDay {
            day: compact_market::epoch_date(day).map_err(integrity)?,
            sources,
        });
    }
    Ok(days)
}

/// The stored calendar, `None` when it was never derived. A calendar read back
/// this way carries no input members: it can be queried, not re-registered
/// ([`TradingCalendar::source_member_digest_hex`] fails closed).
pub(crate) fn read_calendar(c: &Connection) -> Result<Option<TradingCalendar>, Error> {
    let days = read_days(c)?;
    if days.is_empty() {
        return Ok(None);
    }
    TradingCalendar::from_stored(days)
        .map(Some)
        .map_err(invalid)
}

/// The coverage self-check of §5.2, measured against the **stored 不复权
/// series** — the bars AC-M1-1 pins to the `.day` members verbatim.
///
/// Basis and cost: the check counts every `instrument_class=0,
/// adjustment=0` bar, which is exactly "every A-share 不复权 bar" while no
/// other class-0 instrument exists. SQLite does the grouping and returns one
/// row per distinct date (~8.8k), so the caller never holds 16.7M bars; the
/// per-bar detail is only fetched for the (few) dates outside the calendar and
/// is bounded by [`COVERAGE_DAY_LIMIT`] / [`COVERAGE_BAR_LIMIT`].
///
/// It runs on a read connection, before the calendar transaction: a full scan
/// inside the write transaction would hold the write lock for the duration of
/// the scan. The daily run orders the calendar after the per-entity items
/// (§9.2), so the measurement and the commit see the same committed state.
pub(crate) fn measure_coverage(
    c: &Connection,
    calendar: &TradingCalendar,
) -> Result<CalendarCoverage, Error> {
    let mut statement = c
        .prepare(
            "SELECT b.trade_day,count(*) FROM series_bars b \
             JOIN market_series s ON s.series_key=b.series_key \
             WHERE s.instrument_class=?1 AND s.adjustment=?2 AND s.period=?3 \
             GROUP BY b.trade_day ORDER BY b.trade_day",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(
            params![
                crate::market_data::INSTRUMENT_CLASS_STOCK,
                crate::market_data::Adjustment::Raw.code(),
                super::compact_repo::period_code(crate::market_data::Period::Day),
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .map_err(storage)?;
    let mut total_bars = 0i64;
    let mut outside_bars = 0i64;
    let mut outside_days: Vec<(NaiveDate, i64)> = Vec::new();
    let mut days_truncated = false;
    for row in rows {
        let (trade_day, bars) = row.map_err(storage)?;
        total_bars += bars;
        let day = compact_market::epoch_date(trade_day).map_err(integrity)?;
        if calendar.contains(day) {
            continue;
        }
        outside_bars += bars;
        if outside_days.len() < COVERAGE_DAY_LIMIT {
            outside_days.push((day, bars));
        } else {
            days_truncated = true;
        }
    }
    let (outside_samples, samples_truncated) = if outside_days.is_empty() || days_truncated {
        (Vec::new(), false)
    } else {
        outside_bar_samples(c, &outside_days)?
    };
    Ok(CalendarCoverage {
        total_bars,
        outside_bars,
        outside_days,
        outside_samples,
        days_truncated,
        samples_truncated,
        calendar_digest: calendar.digest_hex(),
    })
}

/// The per-bar detail of the 清单: which entity's bar fell on an out-of-calendar
/// day, ascending by day then entity, bounded by [`COVERAGE_BAR_LIMIT`].
fn outside_bar_samples(
    c: &Connection,
    outside_days: &[(NaiveDate, i64)],
) -> Result<(Vec<(String, NaiveDate)>, bool), Error> {
    let placeholders = (1..=outside_days.len())
        .map(|index| format!("?{}", index + 3))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT s.entity,b.trade_day FROM series_bars b \
         JOIN market_series s ON s.series_key=b.series_key \
         WHERE s.instrument_class=?1 AND s.adjustment=?2 AND s.period=?3 \
         AND b.trade_day IN ({placeholders}) \
         ORDER BY b.trade_day,s.entity LIMIT {}",
        COVERAGE_BAR_LIMIT + 1
    );
    let mut statement = c.prepare(&sql).map_err(storage)?;
    let mut values: Vec<i64> = vec![
        crate::market_data::INSTRUMENT_CLASS_STOCK,
        crate::market_data::Adjustment::Raw.code(),
        super::compact_repo::period_code(crate::market_data::Period::Day),
    ];
    for (day, _) in outside_days {
        values.push(compact_market::epoch_day(*day));
    }
    let rows = statement
        .query_map(rusqlite::params_from_iter(values), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })
        .map_err(storage)?;
    let mut samples = Vec::new();
    let mut truncated = false;
    for row in rows {
        let (entity, trade_day) = row.map_err(storage)?;
        if samples.len() == COVERAGE_BAR_LIMIT {
            truncated = true;
            break;
        }
        samples.push((
            entity,
            compact_market::epoch_date(trade_day).map_err(integrity)?,
        ));
    }
    Ok((samples, truncated))
}

/// One calendar write: the adapter's facts, the coverage measurement taken
/// against them, and the audit identity of the item that produced them.
pub(crate) struct CalendarWrite<'a> {
    pub batch: &'a CalendarBatch,
    pub coverage: &'a CalendarCoverage,
    pub task_id: &'a str,
    pub item_id: &'a str,
    pub generation_id: &'a str,
    pub generation_manifest_sha256: &'a str,
    pub generation_evidence: &'a Value,
}

/// Replace the whole calendar and register its version, in the caller's
/// transaction.
///
/// Gates, in order: the batch must form a valid union (§5.2 inputs); the
/// coverage must be measured against **this** calendar and be within the
/// 0.01% limit (AC-M1-4); then the rows, the `derived` evidence bundle and the
/// `derived_dataset_versions('trading_calendar','exchange_union')` row are
/// written together, so a stored calendar never disagrees with the version it
/// claims.
pub(crate) fn commit_calendar(
    c: &Connection,
    w: &CalendarWrite<'_>,
) -> Result<DerivedVersionRow, Error> {
    let calendar = TradingCalendar::union(w.batch).map_err(invalid)?;
    w.coverage.require_within_limit().map_err(invalid)?;
    let digest_hex = calendar.digest_hex();
    if w.coverage.calendar_digest != digest_hex {
        return Err(integrity(
            "coverage was measured against a different calendar than the one committed",
        ));
    }
    let first_day = calendar
        .first_day()
        .ok_or_else(|| integrity("calendar union produced no day"))?;
    let last_day = calendar
        .last_day()
        .ok_or_else(|| integrity("calendar union produced no day"))?;
    let member_digest = calendar.source_member_digest_hex().map_err(invalid)?;
    let binding = series_commit::put_generation_binding(
        c,
        w.generation_id,
        w.generation_manifest_sha256,
        &member_digest,
        &w.batch.security_event_sha256,
        w.generation_evidence,
    )?;
    let evidence = calendar_evidence(&calendar, w.coverage)?;
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    // Whole-table replace (§3.8): the stored rows are this derivation's rows
    // only, so a date the inputs no longer carry cannot linger.
    c.execute("DELETE FROM trading_days", []).map_err(storage)?;
    for day in calendar.days() {
        c.execute(
            "INSERT INTO trading_days(trade_day,sources) VALUES(?1,?2)",
            params![compact_market::epoch_day(day.day), day.sources],
        )
        .map_err(storage)?;
    }
    let digest = series_commit::hex_digest(&digest_hex)?;
    let now = chrono::Utc::now().to_rfc3339();
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
            trading_calendar::DATASET_KIND,
            trading_calendar::DATASET_KEY,
            digest_hex,
            digest.as_slice(),
            trading_calendar::DIGEST_FORMAT,
            calendar.len() as i64,
            compact_market::epoch_day(first_day),
            compact_market::epoch_day(last_day),
            trading_calendar::ALGORITHM_VERSION,
            binding.as_slice(),
            evidence_digest.as_slice(),
            w.task_id,
            w.item_id,
            now,
        ],
    )
    .map_err(storage)?;
    derived::read_version(
        c,
        trading_calendar::DATASET_KIND,
        trading_calendar::DATASET_KEY,
    )?
    .ok_or_else(|| integrity("calendar registration vanished after its insert"))
}

/// The `derived` evidence bundle of one calendar (§5.2): which inputs and
/// members produced it, how the coverage check went, and the limitations that
/// keep it from being read as "tradable".
fn calendar_evidence(
    calendar: &TradingCalendar,
    coverage: &CalendarCoverage,
) -> Result<Value, Error> {
    let first_day = calendar
        .first_day()
        .ok_or_else(|| integrity("calendar evidence requires a non-empty calendar"))?;
    let last_day = calendar
        .last_day()
        .ok_or_else(|| integrity("calendar evidence requires a non-empty calendar"))?;
    Ok(json!({
        "dataset": trading_calendar::DATASET_KIND,
        "dataset_key": trading_calendar::DATASET_KEY,
        "digest_format": trading_calendar::DIGEST_FORMAT,
        "algorithm_version": trading_calendar::ALGORITHM_VERSION,
        "content_digest": calendar.digest_hex(),
        "day_count": calendar.len(),
        "first_day": first_day.to_string(),
        "last_day": last_day.to_string(),
        // The input list is part of the answer (AC-M1-5): which indices, which
        // members, how many dates each contributed.
        "inputs": calendar
            .inputs()
            .iter()
            .map(|input| json!({
                "entity": input.entity,
                "sources_bit": input.bit,
                "day_member_sha256": input.day_member_sha256,
                "day_count": input.day_count,
            }))
            .collect::<Vec<_>>(),
        "semantics": ONE_WAY_SEMANTICS,
        "beyond_last_day": "unknown: the calendar ends at the last input bar and \
             does not predict future trading days",
        "coverage": {
            "basis": "every stored 不复权 security bar (instrument_class=0, \
                 adjustment=0), which AC-M1-1 pins to the .day member verbatim",
            "total_bars": coverage.total_bars,
            "outside_bars": coverage.outside_bars,
            "ratio": coverage.ratio_text(),
            "limit": "1/10000 (0.01%)",
            "outside_days": coverage
                .outside_days
                .iter()
                .map(|(day, bars)| json!({"day": day.to_string(), "bars": bars}))
                .collect::<Vec<_>>(),
            "outside_bars_detail": coverage
                .outside_samples
                .iter()
                .map(|(entity, day)| json!({"entity": entity, "day": day.to_string()}))
                .collect::<Vec<_>>(),
            "days_truncated": coverage.days_truncated,
            "samples_truncated": coverage.samples_truncated,
        },
        "limitations": [
            COMPLETENESS_LIMIT,
            "derived data: the calendar is recomputable from the two input .day members",
        ],
    }))
}

#[cfg(test)]
mod tests;
