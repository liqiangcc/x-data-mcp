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
//!
//! Two consequences of "the calendar ends at its last input bar" are enforced
//! here (#446): the coverage self-check gates only bars **inside**
//! `[first_day,last_day]` ([`measure_coverage`]) and reports the bars after
//! `last_day` as a stale tail instead of counting them outside; and a
//! derivation whose `last_day` is earlier than the stored calendar's is refused
//! with [`CALENDAR_REGRESSED`] instead of replacing the rows, because that
//! write would turn committed bars into "unknown" for every consumer.

use chrono::NaiveDate;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use super::derived::{self, DerivedVersionRow};
use super::{evidence_store, integrity, series_commit, storage};
use crate::compact_market::{self, EvidenceKind};
use crate::market_data::CalendarBatch;
use crate::market_storage::Error;
use crate::trading_calendar::{
    self, COVERAGE_BAR_LIMIT, COVERAGE_DAY_LIMIT, CalendarCoverage, DayStatus, TradingCalendar,
    TradingDay,
};

fn invalid(message: impl std::fmt::Display) -> Error {
    Error::Invalid(message.to_string())
}

/// Stable error code of the no-regression guard (#446). A calendar whose
/// `last_day` moves backwards would turn already committed stock bars into
/// "unknown" for every consumer (suspensions, tradability, price limits), so
/// the write is refused instead of replacing the table. The code is the first
/// token of the integrity message (right after the `MARKET_INTEGRITY:` prefix)
/// — the same shape the source adapters use for `source_store_unconfigured` —
/// so `task_items.outcome_json.error` can be aggregated by code without parsing
/// prose. There is no `force` override.
pub(crate) const CALENDAR_REGRESSED: &str = "calendar_regressed";

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
/// per-bar detail is only fetched for the (few) days outside the calendar or
/// after it, and is bounded by [`COVERAGE_DAY_LIMIT`] / [`COVERAGE_BAR_LIMIT`].
///
/// The split is [`TradingCalendar::day_status`], not `contains` (#446):
///
/// - `InCalendar` — the union proves the day; nothing to report;
/// - `NonTrading` — inside the covered span and absent: the union missed a
///   real trading day. Counted in `total_bars`/`outside_bars` and gated at
///   0.01% (§5.2, AC-M1-4);
/// - `BeyondCalendar` — after `last_day`: unknown by §3.8, because the
///   calendar does not predict. A stock bar there only says the stock inputs
///   are fresher than the index inputs, so it is reported as the stale tail
///   (`stale_bars`, `stale_days`, `stale_samples`) and never enters the gate.
///   Counting it as "outside" made a one-day index lag (~5.2k bars = 0.029%)
///   fail a threshold whose whole budget is ~1.8k bars (`calendar-suspensions-
///   attribution.md` §A).
///
/// `total_bars` is therefore the denominator of the ratio: **bars inside
/// [first_day, last_day]**, i.e. `InCalendar + NonTrading`. The tail has its
/// own count, so the ratio can never be diluted by unknown days.
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
    let mut stale_bars = 0i64;
    let mut stock_last_day: Option<NaiveDate> = None;
    let mut outside_days: Vec<(NaiveDate, i64)> = Vec::new();
    let mut stale_days: Vec<(NaiveDate, i64)> = Vec::new();
    let mut days_truncated = false;
    let mut stale_days_truncated = false;
    for row in rows {
        let (trade_day, bars) = row.map_err(storage)?;
        let day = compact_market::epoch_date(trade_day).map_err(integrity)?;
        stock_last_day = Some(stock_last_day.map_or(day, |last| last.max(day)));
        match calendar.day_status(day) {
            Some(DayStatus::InCalendar) => total_bars += bars,
            Some(DayStatus::NonTrading) => {
                total_bars += bars;
                outside_bars += bars;
                if outside_days.len() < COVERAGE_DAY_LIMIT {
                    outside_days.push((day, bars));
                } else {
                    days_truncated = true;
                }
            }
            Some(DayStatus::BeyondCalendar) => {
                stale_bars += bars;
                if stale_days.len() < COVERAGE_DAY_LIMIT {
                    stale_days.push((day, bars));
                } else {
                    stale_days_truncated = true;
                }
            }
            None => {
                return Err(integrity(
                    "the coverage check needs a calendar with at least one day",
                ));
            }
        }
    }
    let (outside_samples, samples_truncated) = if outside_days.is_empty() || days_truncated {
        (Vec::new(), false)
    } else {
        bar_samples_on(c, &outside_days)?
    };
    let (stale_samples, stale_samples_truncated) = if stale_days.is_empty() || stale_days_truncated
    {
        (Vec::new(), false)
    } else {
        bar_samples_on(c, &stale_days)?
    };
    Ok(CalendarCoverage {
        total_bars,
        outside_bars,
        outside_days,
        outside_samples,
        days_truncated,
        samples_truncated,
        stale_bars,
        stale_days,
        stale_samples,
        stale_days_truncated,
        stale_samples_truncated,
        stock_last_day,
        calendar_last_day: calendar.last_day(),
        calendar_digest: calendar.digest_hex(),
    })
}

/// The per-bar detail of a day 清单: which entity's bar fell on one of `days`,
/// ascending by day then entity, bounded by [`COVERAGE_BAR_LIMIT`].
fn bar_samples_on(
    c: &Connection,
    days: &[(NaiveDate, i64)],
) -> Result<(Vec<(String, NaiveDate)>, bool), Error> {
    let placeholders = (1..=days.len())
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
    for (day, _) in days {
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
/// derived span must not move backwards against the stored calendar
/// ([`CALENDAR_REGRESSED`], reported before anything else because it is the
/// root cause a tail failure would otherwise hide); the coverage must be
/// measured against **this** calendar and be within the 0.01% limit
/// (AC-M1-4); then the rows, the `derived` evidence bundle and the
/// `derived_dataset_versions('trading_calendar','exchange_union')` row are
/// written together, so a stored calendar never disagrees with the version it
/// claims.
pub(crate) fn commit_calendar(
    c: &Connection,
    w: &CalendarWrite<'_>,
) -> Result<DerivedVersionRow, Error> {
    let calendar = TradingCalendar::union(w.batch).map_err(invalid)?;
    let first_day = calendar
        .first_day()
        .ok_or_else(|| integrity("calendar union produced no day"))?;
    let last_day = calendar
        .last_day()
        .ok_or_else(|| integrity("calendar union produced no day"))?;
    // No-regression guard (#446): the stored calendar is what consumers read,
    // and §3.8 says days after its `last_day` are unknown. Letting a derivation
    // whose index inputs are older replace it silently downgrades every bar
    // already committed past that day, so refuse the whole write. An empty
    // database has nothing to protect and commits as before; equal `last_day`
    // is a normal re-derivation, not a regression.
    let stored = read_calendar(c)?;
    let stored_last = stored.as_ref().and_then(TradingCalendar::last_day);
    if let Some(stored_last) = stored_last
        && last_day < stored_last
    {
        let stored_days = stored.as_ref().map_or(0, TradingCalendar::len);
        return Err(Error::Integrity(format!(
            "{CALENDAR_REGRESSED}: the derived calendar ends at {last_day}, before the stored \
             calendar's last day {stored_last} ({stored_days} stored days); the stored calendar \
             is kept — the index inputs are older than the committed calendar, so refreshing \
             them is the fix (there is no force override)"
        )));
    }
    w.coverage.require_within_limit().map_err(invalid)?;
    let digest_hex = calendar.digest_hex();
    if w.coverage.calendar_digest != digest_hex {
        return Err(integrity(
            "coverage was measured against a different calendar than the one committed",
        ));
    }
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
             does not predict future trading days; stock bars there are reported as \
             coverage.stale_bars and are not counted in the coverage ratio",
        "coverage": {
            "basis": "every stored 不复权 security bar (instrument_class=0, \
                 adjustment=0), which AC-M1-1 pins to the .day member verbatim; \
                 total_bars counts only the bars inside [first_day,last_day] (the \
                 ratio's denominator), while bars after last_day are the unknown \
                 tail counted in stale_bars",
            "total_bars": coverage.total_bars,
            "total_bars_scope": "inside [first_day,last_day] (InCalendar + NonTrading)",
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
            // The tail (#446): bars after last_day are unknown (§3.8), so they
            // are visible here and never in the ratio above.
            "stale_bars": coverage.stale_bars,
            "stale_days": coverage
                .stale_days
                .iter()
                .map(|(day, bars)| json!({"day": day.to_string(), "bars": bars}))
                .collect::<Vec<_>>(),
            "stale_bars_detail": coverage
                .stale_samples
                .iter()
                .map(|(entity, day)| json!({"entity": entity, "day": day.to_string()}))
                .collect::<Vec<_>>(),
            "stale_days_truncated": coverage.stale_days_truncated,
            "stale_samples_truncated": coverage.stale_samples_truncated,
            "stock_last_day": coverage.stock_last_day.map(|day| day.to_string()),
            "calendar_last_day": coverage.calendar_last_day.map(|day| day.to_string()),
        },
        "limitations": [
            COMPLETENESS_LIMIT,
            "derived data: the calendar is recomputable from the two input .day members",
        ],
    }))
}

#[cfg(test)]
mod tests;
