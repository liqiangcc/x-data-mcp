//! Schema-21 latest-only market storage (SPEC-LATEST-ONLY §1–§3;
//! SPEC-RULINGS-LATEST-ONLY A1–A5).
//!
//! Every `(entity, period, adjustment)` keeps exactly one current row in
//! `market_series`; bars live in `series_bars` keyed by `(series_key,
//! trade_day)`; every state transition appends one `market_events` row inside
//! the same transaction that updates the series row (§3.4 invariant).
//!
//! Content identity is a sequential fold digest (SPEC §2.2):
//! `h_0 = seed(entity, period)`, `h_n = SHA256(h_{n-1} || bar_encoding(bar_n))`
//! — path-independent, O(k) to extend on append, O(n) to rebuild on replace.
//! `version_id = lower(hex(content_digest))`; a version id that existed but
//! was superseded resolves to `Error::Stale`, one that never existed to
//! `Error::NotFound` (SPEC §6.1).

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{integrity, storage};
use crate::compact_market::{
    self, CompactBar, Digest, EvidenceKind, SourceBinding, VersionEvidence,
};
use crate::market_data::{
    Adjustment, CoverageEvidence, CoverageStatus, MarketDataBatch, Period, Provenance,
    SnapshotInputBinding, YearlyDerivation,
};
use crate::market_storage::{Error, compat::CheckedRecord};
use crate::publish::{EffectiveVersion, ValidationSummary};

fn invalid(error: impl std::fmt::Display) -> Error {
    Error::Invalid(error.to_string())
}

fn push_text(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

pub(crate) const DIGEST_FORMAT: &str = "series-fold-v1";

/// Domain-separated fold seed (SPEC §2.2): binds the digest to
/// (entity, period) so identical bar lists under different identities cannot
/// collide.
pub(crate) fn seed_digest(entity: &str, period: Period) -> Result<Digest, Error> {
    compact_market::canonical_entity(entity).map_err(|e| invalid(e.to_string()))?;
    let mut payload = Vec::with_capacity(64);
    payload.extend_from_slice(b"x-data-series-v1\0");
    push_text(&mut payload, entity);
    payload.push(compact_market::period_code(period));
    Ok(Sha256::digest(&payload).into())
}

/// One fold step: `h_n = SHA256(h_{n-1} || canonical(bar_n))`, where the
/// canonical encoding is `bar_encoding` (compact_market.rs:886).
pub(crate) fn fold_bar(state: Digest, bar: &CompactBar) -> Digest {
    let mut payload = Vec::with_capacity(128);
    payload.extend_from_slice(&state);
    compact_market::bar_encoding(&mut payload, bar);
    Sha256::digest(&payload).into()
}

/// Full ordered fold over `bars`; strictly increasing `trade_day` is enforced
/// so the digest is defined only over canonical content (SPEC §2.2).
pub(crate) fn fold_bars(
    entity: &str,
    period: Period,
    bars: &[CompactBar],
) -> Result<Digest, Error> {
    fold_more(seed_digest(entity, period)?, bars)
}

/// Continue a stored fold with new bars — the append path is O(#new bars) and
/// never rescans stored content.
pub(crate) fn fold_more(from: Digest, bars: &[CompactBar]) -> Result<Digest, Error> {
    let mut digest = from;
    let mut previous: Option<i64> = None;
    for bar in bars {
        if previous.is_some_and(|p| bar.trade_day <= p) {
            return Err(invalid("series bars not strictly increasing"));
        }
        previous = Some(bar.trade_day);
        digest = fold_bar(digest, bar);
    }
    Ok(digest)
}

/// `version_id = lower(hex(content_digest))`; the table CHECK binds them.
pub(crate) fn version_id(digest: &Digest) -> String {
    compact_market::digest_hex(digest)
}

/// Decode a 64-char lowercase-hex version id back to its digest; `None` for
/// any other shape (legacy `version-*` ids, malformed input, uppercase).
pub(crate) fn decode_version_id(version_id: &str) -> Option<Digest> {
    if version_id.len() != 64
        || !version_id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&version_id[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(digest)
}

// ---------------------------------------------------------------------------
// Series rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct SeriesRow {
    pub series_key: i64,
    pub entity: String,
    pub period: Period,
    pub version_id: String,
    pub content_digest: Digest,
    pub record_count: i64,
    pub first_day: i64,
    pub last_day: i64,
    pub coverage_status: CoverageStatus,
    pub source_binding_digest: Digest,
    pub evidence_digest: Digest,
    pub last_event_seq: i64,
    #[allow(dead_code)] // creation timestamp retained for audit surfaces
    pub created_at: String,
    pub updated_at: String,
}

const SERIES_COLUMNS: &str = "series_key,entity,period,version_id,content_digest,record_count,first_day,last_day,coverage_status,source_binding_digest,evidence_digest,last_event_seq,created_at,updated_at";

type RawSeries = (
    i64,
    String,
    i64,
    String,
    Vec<u8>,
    i64,
    i64,
    i64,
    i64,
    Vec<u8>,
    Vec<u8>,
    i64,
    String,
    String,
);

fn decode_period(code: i64) -> Result<Period, Error> {
    match code {
        1 => Ok(Period::Day),
        2 => Ok(Period::Year),
        _ => Err(integrity("unknown series period")),
    }
}

fn decode_coverage(code: i64) -> Result<CoverageStatus, Error> {
    match code {
        1 => Ok(CoverageStatus::Complete),
        2 => Ok(CoverageStatus::Incomplete),
        3 => Ok(CoverageStatus::Unknown),
        _ => Err(integrity("unknown coverage_status")),
    }
}

fn coverage_code(status: CoverageStatus) -> i64 {
    match status {
        CoverageStatus::Complete => 1,
        CoverageStatus::Incomplete => 2,
        CoverageStatus::Unknown => 3,
    }
}

fn digest32(raw: Vec<u8>) -> Result<Digest, Error> {
    <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| integrity("digest column is not 32 bytes"))
}

fn digest32_opt(raw: Option<Vec<u8>>) -> Result<Option<Digest>, Error> {
    raw.map(digest32).transpose()
}

fn series_row(raw: RawSeries) -> Result<SeriesRow, Error> {
    let content_digest = digest32(raw.4)?;
    let vid = raw.3;
    if vid != version_id(&content_digest) {
        return Err(integrity("series version_id does not match content digest"));
    }
    Ok(SeriesRow {
        series_key: raw.0,
        entity: raw.1,
        period: decode_period(raw.2)?,
        version_id: vid,
        content_digest,
        record_count: raw.5,
        first_day: raw.6,
        last_day: raw.7,
        coverage_status: decode_coverage(raw.8)?,
        source_binding_digest: digest32(raw.9)?,
        evidence_digest: digest32(raw.10)?,
        last_event_seq: raw.11,
        created_at: raw.12,
        updated_at: raw.13,
    })
}

fn raw_series(r: &rusqlite::Row<'_>) -> Result<RawSeries, rusqlite::Error> {
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
        r.get(13)?,
    ))
}

fn read_series_row(
    c: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Option<SeriesRow>, Error> {
    c.query_row(sql, args, raw_series)
        .optional()
        .map_err(storage)?
        .map(series_row)
        .transpose()
}

/// The current series row for one (entity, period); `None` when never
/// published.
pub(crate) fn read_series(
    c: &Connection,
    entity: &str,
    period: Period,
) -> Result<Option<SeriesRow>, Error> {
    read_series_row(
        c,
        &format!(
            "SELECT {SERIES_COLUMNS} FROM market_series WHERE entity=?1 AND period=?2 AND adjustment=1"
        ),
        params![entity, super::compact_repo::period_code(period)],
    )
}

pub(crate) fn read_series_by_version_id(
    c: &Connection,
    version_id: &str,
) -> Result<Option<SeriesRow>, Error> {
    read_series_row(
        c,
        &format!("SELECT {SERIES_COLUMNS} FROM market_series WHERE version_id=?1"),
        params![version_id],
    )
}

pub(crate) fn read_series_by_key(
    c: &Connection,
    series_key: i64,
) -> Result<Option<SeriesRow>, Error> {
    read_series_row(
        c,
        &format!("SELECT {SERIES_COLUMNS} FROM market_series WHERE series_key=?1"),
        params![series_key],
    )
}

/// Explicit-pin resolver (SPEC §6.1): a version id that no longer names the
/// current row but whose digest was committed before resolves `Stale`; any
/// other miss is `NotFound`. Head-resolved ids always hit, so the probe is a
/// no-op on that path.
pub(crate) fn require_series(c: &Connection, version_id: &str) -> Result<SeriesRow, Error> {
    if let Some(row) = read_series_by_version_id(c, version_id)? {
        return Ok(row);
    }
    if let Some(digest) = decode_version_id(version_id)
        && digest_seen(c, &digest)?
    {
        return Err(Error::Stale(format!("version superseded: {version_id}")));
    }
    Err(Error::NotFound)
}

// ---------------------------------------------------------------------------
// Bars
// ---------------------------------------------------------------------------

const BAR_COLUMNS: &str =
    "trade_day,open_cents,high_cents,low_cents,close_cents,volume,amount,r7_raw";

fn read_bars(
    c: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<CompactBar>, Error> {
    c.prepare(sql)
        .map_err(storage)?
        .query_map(args, |r| {
            Ok(CompactBar {
                trade_day: r.get(0)?,
                open_cents: r.get(1)?,
                high_cents: r.get(2)?,
                low_cents: r.get(3)?,
                close_cents: r.get(4)?,
                volume: r.get(5)?,
                amount: r.get(6)?,
                r7_raw: r.get(7)?,
            })
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)
}

/// All bars of a series, PK order — the whole current sequence.
pub(crate) fn all_bars(c: &Connection, series_key: i64) -> Result<Vec<CompactBar>, Error> {
    read_bars(
        c,
        &format!("SELECT {BAR_COLUMNS} FROM series_bars WHERE series_key=?1 ORDER BY trade_day"),
        params![series_key],
    )
}

/// Bars inside [start_day, end_day] (inclusive epoch days), PK order.
pub(crate) fn bars_in_range(
    c: &Connection,
    series_key: i64,
    start_day: i64,
    end_day: i64,
) -> Result<Vec<CompactBar>, Error> {
    read_bars(
        c,
        &format!(
            "SELECT {BAR_COLUMNS} FROM series_bars WHERE series_key=?1 AND trade_day BETWEEN ?2 AND ?3 ORDER BY trade_day"
        ),
        params![series_key, start_day, end_day],
    )
}

/// Trade days inside [start_day, end_day] — the presence/year-set probe.
pub(crate) fn days_in_range(
    c: &Connection,
    series_key: i64,
    start_day: i64,
    end_day: i64,
) -> Result<Vec<i64>, Error> {
    c.prepare(
        "SELECT trade_day FROM series_bars WHERE series_key=?1 AND trade_day BETWEEN ?2 AND ?3 ORDER BY trade_day",
    )
    .map_err(storage)?
    .query_map(params![series_key, start_day, end_day], |r| r.get(0))
    .map_err(storage)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(storage)
}

/// (count, first_day, last_day) inside a window — the availability facts.
pub(crate) fn window_stats(
    c: &Connection,
    series_key: i64,
    start_day: i64,
    end_day: i64,
) -> Result<(usize, Option<i64>, Option<i64>), Error> {
    let (count, first, last) = c
        .query_row(
            "SELECT COUNT(*), MIN(trade_day), MAX(trade_day) FROM series_bars WHERE series_key=?1 AND trade_day BETWEEN ?2 AND ?3",
            params![series_key, start_day, end_day],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .map_err(storage)?;
    let count =
        usize::try_from(count).map_err(|_| integrity("series_bars count out of usize range"))?;
    Ok((count, first, last))
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)] // event fields projected selectively per reader
pub(crate) struct EventRow {
    pub seq: i64,
    pub entity: String,
    pub period: i64,
    pub kind: String,
    pub outcome: i64,
    pub metadata_changed: bool,
    pub continuity_mode: Option<i64>,
    pub prefix_bytes: Option<i64>,
    pub old_content_digest: Option<Digest>,
    pub new_content_digest: Digest,
    pub range_start_day: Option<i64>,
    pub range_end_day: Option<i64>,
    pub window_digest: Option<Digest>,
    /// `yearly_rederive` events: the daily base digest derived from.
    pub base_content_digest: Option<Digest>,
    pub algorithm_version: Option<String>,
    pub source_binding_digest: Option<Digest>,
    pub evidence_digest: Digest,
    pub task_id: String,
    pub item_id: String,
    pub task_revision: i64,
    pub result_id: String,
    pub result_digest: Digest,
    pub created_at: String,
    /// C3: 1 when the commit replaced the head's version-evidence
    /// semantics ({source, unit_currency, adjustment_evidence}).
    pub semantics_changed: i64,
}

const EVENT_COLUMNS: &str = "seq,entity,period,kind,outcome,metadata_changed,continuity_mode,prefix_bytes,old_content_digest,new_content_digest,range_start_day,range_end_day,window_digest,base_content_digest,algorithm_version,source_binding_digest,evidence_digest,task_id,item_id,task_revision,result_id,result_digest,created_at";

type RawEvent = (
    i64,
    String,
    i64,
    String,
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    Option<Vec<u8>>,
    Vec<u8>,
    Option<i64>,
    Option<i64>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<String>,
    Option<Vec<u8>>,
    Vec<u8>,
    String,
    String,
    i64,
    String,
    Vec<u8>,
    String,
    i64,
);

fn event_row(r: &rusqlite::Row<'_>) -> Result<RawEvent, rusqlite::Error> {
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
        r.get(13)?,
        r.get(14)?,
        r.get(15)?,
        r.get(16)?,
        r.get(17)?,
        r.get(18)?,
        r.get(19)?,
        r.get(20)?,
        r.get(21)?,
        r.get(22)?,
        r.get(23)?,
    ))
}

fn event(raw: RawEvent) -> Result<EventRow, Error> {
    Ok(EventRow {
        seq: raw.0,
        entity: raw.1,
        period: raw.2,
        kind: raw.3,
        outcome: raw.4,
        metadata_changed: raw.5 != 0,
        continuity_mode: raw.6,
        prefix_bytes: raw.7,
        old_content_digest: digest32_opt(raw.8)?,
        new_content_digest: digest32(raw.9)?,
        range_start_day: raw.10,
        range_end_day: raw.11,
        window_digest: digest32_opt(raw.12)?,
        base_content_digest: digest32_opt(raw.13)?,
        algorithm_version: raw.14,
        source_binding_digest: digest32_opt(raw.15)?,
        evidence_digest: digest32(raw.16)?,
        task_id: raw.17,
        item_id: raw.18,
        task_revision: raw.19,
        result_id: raw.20,
        result_digest: digest32(raw.21)?,
        created_at: raw.22,
        semantics_changed: raw.23,
    })
}

fn read_event_row(
    c: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Option<EventRow>, Error> {
    c.query_row(sql, args, event_row)
        .optional()
        .map_err(storage)?
        .map(event)
        .transpose()
}

#[allow(dead_code)]
pub(crate) fn event_by_seq(c: &Connection, seq: i64) -> Result<Option<EventRow>, Error> {
    read_event_row(
        c,
        &format!("SELECT {EVENT_COLUMNS} FROM market_events WHERE seq=?1"),
        params![seq],
    )
}

/// The newest event of one (entity, period) — one probe against the
/// `(entity, period, seq DESC)` index.
#[allow(dead_code)]
pub(crate) fn latest_event(
    c: &Connection,
    entity: &str,
    period: Period,
) -> Result<Option<EventRow>, Error> {
    read_event_row(
        c,
        &format!(
            "SELECT {EVENT_COLUMNS} FROM market_events WHERE entity=?1 AND period=?2 ORDER BY seq DESC LIMIT 1"
        ),
        params![entity, super::compact_repo::period_code(period)],
    )
}

/// Idempotent-replay anchor: the committed event carrying `result_id`.
pub(crate) fn event_by_result(c: &Connection, result_id: &str) -> Result<Option<EventRow>, Error> {
    read_event_row(
        c,
        &format!("SELECT {EVENT_COLUMNS} FROM market_events WHERE result_id=?1"),
        params![result_id],
    )
}

/// Events of one (entity, period) strictly after `seq`, ascending.
/// Replay/read anchor for the `UNIQUE(task_id, item_id)` pair.
pub(crate) fn event_by_task_item(
    c: &Connection,
    task_id: &str,
    item_id: &str,
) -> Result<Option<EventRow>, Error> {
    read_event_row(
        c,
        &format!("SELECT {EVENT_COLUMNS} FROM market_events WHERE task_id=?1 AND item_id=?2"),
        params![task_id, item_id],
    )
}

pub(crate) fn events_after(
    c: &Connection,
    entity: &str,
    period: Period,
    seq: i64,
) -> Result<Vec<EventRow>, Error> {
    c.prepare(&format!("SELECT {EVENT_COLUMNS} FROM market_events WHERE entity=?1 AND period=?2 AND seq>?3 ORDER BY seq"))
    .map_err(storage)?
    .query_map(
        params![entity, super::compact_repo::period_code(period), seq],
        event_row,
    )
    .map_err(storage)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(storage)?
    .into_iter()
    .map(event)
    .collect()
}

/// SPEC §6.1: was this content digest ever committed as a series state.
pub(crate) fn digest_seen(c: &Connection, digest: &Digest) -> Result<bool, Error> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM market_events WHERE new_content_digest=?1)",
        params![digest.as_slice()],
        |r| r.get(0),
    )
    .map_err(storage)
}

// ---------------------------------------------------------------------------
// Yearly lineage + stale classification (RULINGS A1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)] // fields read by yearly tasks (S3)
pub(crate) struct SeriesLineage {
    pub series_key: i64,
    pub base_daily_digest: Digest,
    pub base_event_seq: i64,
    pub algorithm_version: String,
    pub first_trade_day: i64,
    pub last_trade_day: i64,
    pub as_of_day: i64,
}

pub(crate) fn yearly_lineage(
    c: &Connection,
    series_key: i64,
) -> Result<Option<SeriesLineage>, Error> {
    c.query_row(
        "SELECT series_key,base_daily_digest,base_event_seq,algorithm_version,first_trade_day,last_trade_day,as_of_day FROM yearly_series_lineage WHERE series_key=?1",
        params![series_key],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        },
    )
    .optional()
    .map_err(storage)?
    .map(|row| {
        Ok(SeriesLineage {
            series_key: row.0,
            base_daily_digest: digest32(row.1)?,
            base_event_seq: row.2,
            algorithm_version: row.3,
            first_trade_day: row.4,
            last_trade_day: row.5,
            as_of_day: row.6,
        })
    })
    .transpose()
}

/// Stale classification of one yearly series (RULINGS A1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum YearlyState {
    /// The recorded base still equals the current daily state.
    Fresh,
    /// Only `append`/`no_change`/`metadata` events landed since derivation:
    /// the stored yearly rows remain correct for `as_of_day` — serve them.
    Behind { as_of_day: i64 },
    /// A `replace` or a semantics-changing commit landed after the
    /// recorded base: the stored yearly content is derived from data the
    /// current daily no longer agrees with — row-level absent, not served.
    Invalid(YearlyInvalidReason),
}

/// Why a yearly row is `YearlyState::Invalid` — the wire `reason` enum
/// values are `reason.code()` (fixed strings per the contract ruling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum YearlyInvalidReason {
    /// A daily `replace` event landed after the recorded base.
    DailyReplaced,
    /// A daily commit changed {source, unit_currency, adjustment_evidence}.
    SemanticsChanged,
}
impl YearlyInvalidReason {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::DailyReplaced => "daily_replaced_since_derivation",
            Self::SemanticsChanged => "value_semantics_changed",
        }
    }
}

/// Classify `series` (must be period=Year) against its daily counterpart.
/// `Error::Integrity` when the lineage is forged/broken: missing lineage row,
/// missing daily series, `base_daily_digest` absent from the daily event
/// chain, or `base_event_seq` ahead of the daily tail.
pub(crate) fn yearly_state(c: &Connection, series: &SeriesRow) -> Result<YearlyState, Error> {
    if series.period != Period::Year {
        return Err(integrity("yearly_state requires a yearly series"));
    }
    let lineage = yearly_lineage(c, series.series_key)?
        .ok_or_else(|| integrity("yearly series lacks lineage"))?;
    let daily = read_series(c, &series.entity, Period::Day)?
        .ok_or_else(|| integrity("yearly series without daily base"))?;
    if daily.last_event_seq < lineage.base_event_seq {
        return Err(integrity("yearly lineage base event ahead of daily tail"));
    }
    // The recorded base must appear in this entity's daily event chain —
    // otherwise the lineage was forged or the chain is corrupt.
    if !digest_seen_for(c, &series.entity, Period::Day, &lineage.base_daily_digest)? {
        return Err(integrity(
            "yearly lineage base digest not in daily event chain",
        ));
    }
    if daily.content_digest == lineage.base_daily_digest
        && daily.last_event_seq == lineage.base_event_seq
    {
        return Ok(YearlyState::Fresh);
    }
    let tail = events_after(c, &series.entity, Period::Day, lineage.base_event_seq)?;
    if tail.is_empty() {
        // Digest differs but no events follow the recorded base — the chain
        // and the series row disagree; corrupt.
        return Err(integrity("yearly lineage trails unrecorded daily state"));
    }
    if tail.iter().any(|e| e.kind == "replace") {
        return Ok(YearlyState::Invalid(YearlyInvalidReason::DailyReplaced));
    }
    if tail.iter().any(|e| e.semantics_changed != 0) {
        return Ok(YearlyState::Invalid(YearlyInvalidReason::SemanticsChanged));
    }
    Ok(YearlyState::Behind {
        as_of_day: lineage.as_of_day,
    })
}

/// Lineage fields written by the yearly commit (kept to clippy's arg cap).
pub(crate) struct LineageWrite<'a> {
    pub base_daily_digest: Digest,
    pub base_event_seq: i64,
    pub algorithm_version: &'a str,
    pub first_trade_day: i64,
    pub last_trade_day: i64,
    pub as_of_day: i64,
}

/// UPSERT the yearly derivation lineage (write path S3): the base digest
/// and event seq move forward even on no_change commits so a yearly row
/// stays `Fresh` instead of permanently `Behind`.
pub(crate) fn put_lineage(
    c: &Connection,
    series_key: i64,
    w: &LineageWrite<'_>,
) -> Result<(), Error> {
    c.execute(
        "INSERT OR REPLACE INTO yearly_series_lineage(series_key,base_daily_digest,base_event_seq,algorithm_version,first_trade_day,last_trade_day,as_of_day) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            series_key,
            w.base_daily_digest.as_slice(),
            w.base_event_seq,
            w.algorithm_version,
            w.first_trade_day,
            w.last_trade_day,
            w.as_of_day,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

fn digest_seen_for(
    c: &Connection,
    entity: &str,
    period: Period,
    digest: &Digest,
) -> Result<bool, Error> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM market_events WHERE entity=?1 AND period=?2 AND new_content_digest=?3)",
        params![
            entity,
            super::compact_repo::period_code(period),
            digest.as_slice()
        ],
        |r| r.get(0),
    )
    .map_err(storage)
}

// ---------------------------------------------------------------------------
// CheckedRecord synthesis — the same envelope surface `synthesize` emits for
// published compact versions (compact_repo.rs:1067), sourced from series rows.
// ---------------------------------------------------------------------------

fn read_source_binding(c: &Connection, digest: &Digest) -> Result<SourceBinding, Error> {
    super::evidence_store::read_source_binding(c, digest)
}

fn period_name(period: Period) -> &'static str {
    match period {
        Period::Day => "day",
        Period::Year => "year",
    }
}

/// Build the legacy-shaped `CheckedRecord` from series facts. `full` loads
/// `version().records`/`batch.bars`; header mode leaves them empty exactly
/// like `slim_published` — callers must take bars through the range APIs.
pub(crate) fn checked_record(
    c: &Connection,
    series: &SeriesRow,
    full: bool,
) -> Result<CheckedRecord, Error> {
    let (kind, canonical) =
        crate::sqlite_market::evidence_store::read_evidence(c, &series.evidence_digest)?;
    if kind != EvidenceKind::Version {
        return Err(integrity("version evidence kind mismatch"));
    }
    let evidence: VersionEvidence = serde_json::from_str(&canonical).map_err(integrity)?;
    let records = if full {
        all_bars(c, series.series_key)?
            .iter()
            .map(|b| b.to_bar(series.period))
            .collect::<Result<Vec<_>, _>>()
            .map_err(integrity)?
    } else {
        Vec::new()
    };
    let lineage = if series.period == Period::Year {
        yearly_lineage(c, series.series_key)?
    } else {
        None
    };
    let yearly_derivation = lineage
        .as_ref()
        .map(|l| {
            Ok::<_, Error>(YearlyDerivation {
                base_daily_version_id: version_id(&l.base_daily_digest),
                algorithm_version: l.algorithm_version.clone(),
                first_trading_date: compact_market::epoch_date(l.first_trade_day)
                    .map_err(integrity)?,
                last_trading_date: compact_market::epoch_date(l.last_trade_day)
                    .map_err(integrity)?,
            })
        })
        .transpose()?;
    let published_at = DateTime::parse_from_rfc3339(&series.updated_at)
        .map_err(integrity)?
        .with_timezone(&Utc);
    let record_count = usize::try_from(series.record_count)
        .map_err(|_| integrity("record_count out of usize range"))?;
    let version = EffectiveVersion {
        version_id: series.version_id.clone(),
        symbol: series.entity.clone(),
        period: series.period,
        adjustment: Adjustment::Forward,
        source: evidence.source.clone(),
        records,
        published_at,
        validation: ValidationSummary {
            record_count,
            unique_record_count: record_count,
            duplicate_count: 0,
        },
        unit_currency: evidence.unit_currency.clone(),
        adjustment_evidence: evidence.adjustment_evidence.clone(),
        yearly_derivation,
    };
    let source_binding = read_source_binding(c, &series.source_binding_digest)?;
    let (kind, canonical) =
        crate::sqlite_market::evidence_store::read_evidence(c, &source_binding.evidence_digest)?;
    if kind != EvidenceKind::Generation {
        return Err(integrity("source binding evidence is not generation"));
    }
    let generation_evidence: Value = serde_json::from_str(&canonical).map_err(integrity)?;
    let provenance = Provenance {
        source: version.source.clone(),
        source_period: if version.yearly_derivation.is_some() {
            "day".into()
        } else {
            period_name(series.period).into()
        },
        source_adjustment: "qfq".into(),
        symbol: series.entity.clone(),
        acquired_at: published_at,
        adapter_version: "effective-publish".into(),
        yearly_derivation: version.yearly_derivation.clone(),
        snapshot_inputs: generation_evidence
            .get("snapshot_inputs")
            .cloned()
            .map(serde_json::from_value::<Vec<SnapshotInputBinding>>)
            .transpose()
            .map_err(integrity)?,
    };
    let batch = MarketDataBatch {
        symbol: series.entity.clone(),
        period: series.period,
        adjustment: Adjustment::Forward,
        bars: version.records.clone(),
        provenance,
        coverage: CoverageEvidence {
            status: series.coverage_status,
            reason: evidence.coverage_reason.clone(),
        },
        unit_currency: evidence.unit_currency,
        adjustment_evidence: evidence.adjustment_evidence,
    };
    // Same slim envelope as the published-tier `synthesize`: no bar arrays in
    // `raw`, `compact_source` bound to the source binding, integrity_sha256 =
    // the content digest hex (= version_id on schema 21).
    let mut slim_version = version.clone();
    slim_version.records = Vec::new();
    let mut raw = serde_json::to_value(&slim_version).map_err(integrity)?;
    let mut slim_batch = batch.clone();
    slim_batch.bars = Vec::new();
    raw["batch"] = serde_json::to_value(&slim_batch).map_err(integrity)?;
    raw["compact_source"] = json!({
        "generation_id": source_binding.generation_id,
        "generation_manifest_sha256": super::compact_repo::hex_bytes(&source_binding.generation_manifest_sha256),
        "day_member_sha256": super::compact_repo::hex_bytes(&source_binding.day_member_sha256),
        "security_event_sha256": super::compact_repo::hex_bytes(&source_binding.security_event_sha256),
        "generation_evidence": generation_evidence,
    });
    if series.period == Period::Year
        && let Some(l) = &lineage
    {
        raw["compact_yearly_lineage_v1"] = json!({
            "as_of": compact_market::epoch_date(l.as_of_day)
                .map_err(integrity)?
                .to_string(),
        });
    }
    raw["integrity_sha256"] = json!(series.version_id);
    Ok(CheckedRecord {
        raw,
        version,
        batch,
    })
}

/// Header-only read by version id (`slim_published` parity): `None` when the
/// id does not name a current series row.
pub(crate) fn materialize_header(
    c: &Connection,
    version_id: &str,
) -> Result<Option<CheckedRecord>, Error> {
    let Some(series) = read_series_by_version_id(c, version_id)? else {
        return Ok(None);
    };
    checked_record(c, &series, false).map(Some)
}

/// By-version-id read with the §6.1 stale probe; `full` loads records.
pub(crate) fn materialize_checked(
    c: &Connection,
    version_id: &str,
    full: bool,
) -> Result<CheckedRecord, Error> {
    let series = require_series(c, version_id)?;
    checked_record(c, &series, full)
}

/// Per-year row metadata (first/last trading day, year_unfinished) by
/// re-aggregating the entity's CURRENT daily series clipped to the stored
/// `as_of_day` — the `YearlyMetaScan` equivalent for schema 21. Append-only
/// "behind" states reproduce the stored row exactly (all appended bars are
/// strictly after the recorded content); `Invalid` rows are gated upstream
/// and never reach here.
pub(crate) fn yearly_row_meta(
    c: &Connection,
    r: &CheckedRecord,
    date: NaiveDate,
) -> Result<crate::sqlite_supply::YearlyRowMeta, Error> {
    if r.version().yearly_derivation.is_none() {
        return Err(integrity("yearly record lacks derivation"));
    }
    let as_of: NaiveDate = r.raw()["compact_yearly_lineage_v1"]["as_of"]
        .as_str()
        .ok_or_else(|| integrity("yearly series lacks stored as_of"))?
        .parse()
        .map_err(|e: chrono::ParseError| integrity(e.to_string()))?;
    let daily = read_series(c, &r.version().symbol, Period::Day)?
        .ok_or_else(|| integrity("yearly series without daily base"))?;
    let year = date.year();
    let start = NaiveDate::from_ymd_opt(year, 1, 1).unwrap();
    let end = NaiveDate::from_ymd_opt(year, 12, 31).unwrap().min(as_of);
    if start > end {
        return Err(integrity("yearly record year after as_of"));
    }
    let records = bars_in_range(
        c,
        daily.series_key,
        compact_market::epoch_day(start),
        compact_market::epoch_day(end),
    )?
    .iter()
    .map(|b| b.to_bar(Period::Day))
    .collect::<Result<Vec<_>, _>>()
    .map_err(integrity)?;
    // `aggregate` requires confirmed unit/adjustment evidence — the daily
    // series' version evidence supplies it.
    let (kind, canonical) =
        crate::sqlite_market::evidence_store::read_evidence(c, &daily.evidence_digest)?;
    if kind != EvidenceKind::Version {
        return Err(integrity("version evidence kind mismatch"));
    }
    let evidence: VersionEvidence = serde_json::from_str(&canonical).map_err(integrity)?;
    let published_at = DateTime::parse_from_rfc3339(&daily.updated_at)
        .map_err(integrity)?
        .with_timezone(&Utc);
    let daily_version = EffectiveVersion {
        version_id: daily.version_id.clone(),
        symbol: daily.entity.clone(),
        period: Period::Day,
        adjustment: Adjustment::Forward,
        source: evidence.source,
        records,
        published_at,
        validation: ValidationSummary {
            record_count: 0,
            unique_record_count: 0,
            duplicate_count: 0,
        },
        unit_currency: evidence.unit_currency,
        adjustment_evidence: evidence.adjustment_evidence,
        yearly_derivation: None,
    };
    let years = std::collections::BTreeSet::from([year]);
    let annual = crate::yearly::aggregate(&daily_version, Some(&years), as_of)
        .map_err(|e| integrity(format!("yearly re-aggregation: {e}")))?;
    let record = annual
        .records
        .iter()
        .find(|rec| rec.bar.date == date)
        .ok_or_else(|| integrity("yearly record missing from re-aggregation"))?;
    Ok((
        Some(record.lineage.first_trading_date),
        Some(record.lineage.last_trading_date),
        Some(record.year_unfinished),
    ))
}

// ---------------------------------------------------------------------------
// Invariants (SPEC §3.4) and startup checks (§3.5 + RULINGS A3)
// ---------------------------------------------------------------------------

/// Run the committed-state closure checks. No-ops when `market_series` is
/// absent (non-v21 schemas). All probes are O(#series) or covering-index
/// scans — never O(events) joins (RULINGS A3). `pub` so `data_service` (bin
/// crate) can reach it through `MarketBackend::read`.
pub fn startup_check(c: &Connection) -> Result<(), Error> {
    if !super::is_latest_only_schema(c)? {
        return Ok(());
    }
    // 1. Tail closure per series: two INDEX-ONLY probes against the
    //    covering index market_events_entity_period
    //    (entity, period, seq DESC, new_content_digest, kind):
    //    the named event must exist with the stored digest, and no newer
    //    event may exist for the pair. No table-row fetches at all.
    let broken: bool = c
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM market_series s
                WHERE NOT EXISTS(
                        SELECT 1 FROM market_events e INDEXED BY market_events_entity_period
                        WHERE e.entity = s.entity AND e.period = s.period
                          AND e.seq = s.last_event_seq
                          AND e.new_content_digest = s.content_digest)
                   OR EXISTS(
                        SELECT 1 FROM market_events e2 INDEXED BY market_events_entity_period
                        WHERE e2.entity = s.entity AND e2.period = s.period
                          AND e2.seq > s.last_event_seq))",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if broken {
        return Err(integrity("series/event tail closure broken"));
    }
    // 2. Orphan yearly lineage / lineage pointing at a non-yearly row.
    let orphan: bool = c
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM yearly_series_lineage l
                LEFT JOIN market_series s ON s.series_key = l.series_key
                WHERE s.series_key IS NULL OR s.period <> 2)",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if orphan {
        return Err(integrity("orphan yearly lineage"));
    }
    // 3. Orphan events: every (entity, period) carrying events must own a
    //    series row. NO O(events) DISTINCT scan here — SQLite has no loose
    //    index scan, so emulate one: seek the next distinct (entity,period)
    //    pair in index order, O(#pairs) index seeks total.
    let mut cursor: Option<(String, i64)> = None;
    loop {
        let pair: Option<(String, i64)> = match &cursor {
            None => c
                .query_row(
                    "SELECT entity, period FROM market_events INDEXED BY market_events_entity_period ORDER BY entity, period LIMIT 1",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(storage)?,
            Some((entity, period)) => c
                .query_row(
                    "SELECT entity, period FROM market_events INDEXED BY market_events_entity_period WHERE (entity, period) > (?1, ?2) ORDER BY entity, period LIMIT 1",
                    params![entity, period],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(storage)?,
        };
        let Some((entity, period)) = pair else { break };
        let exists: bool = c
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM market_series WHERE entity=?1 AND period=?2 AND adjustment=1)",
                params![entity, period],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if !exists {
            return Err(integrity("market event without series row"));
        }
        cursor = Some((entity, period));
    }
    // 4. Series-driven first-publish: one bounded index seek per series —
    //    driven by market_series, not by an event-side DISTINCT scan.
    let bad_first: bool = c
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM market_series s
                WHERE COALESCE(
                    (SELECT e.kind FROM market_events e INDEXED BY market_events_entity_period
                     WHERE e.entity = s.entity AND e.period = s.period
                     ORDER BY e.seq ASC LIMIT 1), '') <> 'first_publish')",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if bad_first {
        return Err(integrity("first market event is not first_publish"));
    }
    Ok(())
}

/// Explicit deep pass for `task_sqlite check` (SPEC §3.5 open item / RULINGS
/// Q11): the startup closure probes plus a full fold recompute and
/// count/window reconciliation of every series. O(#bars) — never on the
/// service path.
pub fn deep_check(c: &Connection) -> Result<Vec<String>, Error> {
    if !super::is_latest_only_schema(c)? {
        return Ok(vec![]);
    }
    let mut findings = Vec::new();
    if let Err(e) = startup_check(c) {
        findings.push(format!("series closure: {e}"));
    }
    let rows = c
        .prepare(
            "SELECT series_key,entity,period,content_digest,record_count,first_day,last_day FROM market_series",
        )
        .map_err(storage)?
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    for (key, entity, period_code, digest_raw, record_count, first_day, last_day) in rows {
        let label = format!("series {entity}#{key}");
        let period = match decode_period(period_code) {
            Ok(p) => p,
            Err(e) => {
                findings.push(format!("{label}: {e}"));
                continue;
            }
        };
        let stored = match digest32(digest_raw) {
            Ok(d) => d,
            Err(e) => {
                findings.push(format!("{label}: {e}"));
                continue;
            }
        };
        match all_bars(c, key)
            .and_then(|bars| fold_bars(&entity, period, &bars).map(|digest| (digest, bars)))
        {
            Err(e) => findings.push(format!("{label}: fold failed: {e}")),
            Ok((computed, bars)) => {
                if computed != stored {
                    findings.push(format!("{label}: content digest mismatch"));
                }
                if bars.len() != record_count as usize {
                    findings.push(format!(
                        "{label}: record_count {record_count} but {} bars",
                        bars.len()
                    ));
                }
                if bars.first().is_some_and(|b| b.trade_day != first_day)
                    || bars.last().is_some_and(|b| b.trade_day != last_day)
                {
                    findings.push(format!("{label}: first_day/last_day mismatch"));
                }
            }
        }
    }
    // Yearly reconciliation: lineage rows name the derived-from daily base;
    // the first/last trade days must match the stored yearly bars, the
    // lineage must classify (forged bases fail closed), and the daily
    // counterpart must exist.
    let yearly_rows = c
        .prepare(
            "SELECT l.series_key,l.first_trade_day,l.last_trade_day FROM yearly_series_lineage l",
        )
        .map_err(storage)?
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    for (key, first_trade, last_trade) in yearly_rows {
        let label = format!("yearly lineage {key}");
        let Some(series) = read_series_by_key(c, key)? else {
            findings.push(format!("{label}: orphan lineage row"));
            continue;
        };
        if series.period != Period::Year {
            findings.push(format!("{label}: attached to non-yearly series"));
        }
        let bars = match all_bars(c, key) {
            Ok(bars) => bars,
            Err(e) => {
                findings.push(format!("{label}: bar read failed: {e}"));
                continue;
            }
        };
        if bars.first().is_some_and(|b| b.trade_day != first_trade)
            || bars.last().is_some_and(|b| b.trade_day != last_trade)
        {
            findings.push(format!(
                "{label}: first/last_trade_day disagrees with stored bars"
            ));
        }
        if let Err(e) = yearly_state(c, &series) {
            findings.push(format!("{label}: {e}"));
        }
    }
    Ok(findings)
}

/// Per-entity event-chain closure (SPEC §3.4, item 4): every event's
/// `old_content_digest` must equal the previous event's `new_content_digest`.
/// O(events of this pair) — used by verification paths, never at startup.
#[allow(dead_code)]
pub(crate) fn check_entity_chain(
    c: &Connection,
    entity: &str,
    period: Period,
) -> Result<(), Error> {
    let rows = c
        .prepare(
            "SELECT seq,kind,old_content_digest,new_content_digest FROM market_events WHERE entity=?1 AND period=?2 ORDER BY seq",
        )
        .map_err(storage)?
        .query_map(
            params![entity, super::compact_repo::period_code(period)],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    let mut previous: Option<Digest> = None;
    for (seq, kind, old, new) in &rows {
        match kind.as_str() {
            "first_publish" => {
                if previous.is_some() {
                    return Err(integrity("duplicate first_publish"));
                }
                if old.is_some() {
                    return Err(integrity("first_publish carries old digest"));
                }
            }
            _ => {
                let prev =
                    previous.ok_or_else(|| integrity("chain starts without first_publish"))?;
                let old_digest = digest32_opt(old.clone())?
                    .ok_or_else(|| integrity("chain event lacks old digest"))?;
                if old_digest != prev {
                    return Err(integrity(format!("event chain broken at seq {seq}")));
                }
            }
        }
        previous = Some(digest32(new.clone())?);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Write helpers — used by `#[cfg(test)]` seeding now; the S2 commit paths will
// call the same inserts inside their transactions (one event per item,
// RULINGS A4).
// ---------------------------------------------------------------------------

/// Next `market_events.seq` (the table is WITHOUT ROWID; writers assign the
/// value inside their transaction).
pub(crate) fn next_event_seq(c: &Connection) -> Result<i64, Error> {
    c.query_row(
        "SELECT COALESCE(MAX(seq),0)+1 FROM market_events",
        [],
        |r| r.get(0),
    )
    .map_err(storage)
}

/// Insert-shape for one `market_events` row; `seq` comes from
/// `next_event_seq` inside the writer's transaction.
#[derive(Debug, Clone)]
pub(crate) struct NewEvent {
    pub entity: String,
    pub period: Period,
    pub kind: &'static str,
    pub outcome: i64,
    pub metadata_changed: bool,
    pub continuity_mode: Option<i64>,
    pub prefix_bytes: Option<i64>,
    pub old_content_digest: Option<Digest>,
    pub new_content_digest: Digest,
    pub range_start_day: Option<i64>,
    pub range_end_day: Option<i64>,
    pub window_digest: Option<Digest>,
    /// `yearly_rederive` events: the daily base digest derived from.
    pub base_content_digest: Option<Digest>,
    pub algorithm_version: Option<String>,
    pub source_binding_digest: Option<Digest>,
    pub evidence_digest: Digest,
    pub task_id: String,
    pub item_id: String,
    pub task_revision: i64,
    pub result_id: String,
    pub result_digest: Digest,
    pub created_at: String,
    pub semantics_changed: i64,
}

pub(crate) fn insert_event(c: &Connection, seq: i64, e: &NewEvent) -> Result<(), Error> {
    c.execute(
        "INSERT INTO market_events(seq,entity,period,kind,outcome,metadata_changed,continuity_mode,prefix_bytes,old_content_digest,new_content_digest,range_start_day,range_end_day,window_digest,base_content_digest,algorithm_version,source_binding_digest,evidence_digest,task_id,item_id,task_revision,result_id,result_digest,created_at,semantics_changed) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)",
        params![
            seq,
            e.entity,
            super::compact_repo::period_code(e.period),
            e.kind,
            e.outcome,
            e.metadata_changed as i64,
            e.continuity_mode,
            e.prefix_bytes,
            e.old_content_digest.as_ref().map(|d| d.as_slice()),
            e.new_content_digest.as_slice(),
            e.range_start_day,
            e.range_end_day,
            e.window_digest.as_ref().map(|d| d.as_slice()),
            e.base_content_digest.as_ref().map(|d| d.as_slice()),
            e.algorithm_version,
            e.source_binding_digest.as_ref().map(|d| d.as_slice()),
            e.evidence_digest.as_slice(),
            e.task_id,
            e.item_id,
            e.task_revision,
            e.result_id,
            e.result_digest.as_slice(),
            e.created_at,
            e.semantics_changed,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

/// Insert-shape for one `market_series` row; `version_id` is derived from
/// `content_digest` (the CHECK would reject a mismatch anyway).
#[derive(Debug, Clone)]
pub(crate) struct NewSeries {
    pub entity: String,
    pub period: Period,
    pub content_digest: Digest,
    pub record_count: i64,
    pub first_day: i64,
    pub last_day: i64,
    pub coverage_status: CoverageStatus,
    pub source_binding_digest: Digest,
    pub evidence_digest: Digest,
    pub last_event_seq: i64,
    #[allow(dead_code)] // creation timestamp retained for audit surfaces
    pub created_at: String,
    pub updated_at: String,
}

/// Insert a series row; returns its `series_key`.
pub(crate) fn insert_series(c: &Connection, s: &NewSeries) -> Result<i64, Error> {
    c.execute(
        "INSERT INTO market_series(entity,period,adjustment,version_id,content_digest,digest_format,record_count,first_day,last_day,coverage_status,source_binding_digest,evidence_digest,last_event_seq,created_at,updated_at) VALUES(?1,?2,1,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            s.entity,
            super::compact_repo::period_code(s.period),
            version_id(&s.content_digest),
            s.content_digest.as_slice(),
            DIGEST_FORMAT,
            s.record_count,
            s.first_day,
            s.last_day,
            coverage_code(s.coverage_status),
            s.source_binding_digest.as_slice(),
            s.evidence_digest.as_slice(),
            s.last_event_seq,
            s.created_at,
            s.updated_at,
        ],
    )
    .map_err(storage)?;
    Ok(c.last_insert_rowid())
}

pub(crate) fn insert_bars(
    c: &Connection,
    series_key: i64,
    bars: &[CompactBar],
) -> Result<(), Error> {
    let mut statement = c
        .prepare(
            "INSERT INTO series_bars(series_key,trade_day,open_cents,high_cents,low_cents,close_cents,volume,amount,r7_raw) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )
        .map_err(storage)?;
    for bar in bars {
        statement
            .execute(params![
                series_key,
                bar.trade_day,
                bar.open_cents,
                bar.high_cents,
                bar.low_cents,
                bar.close_cents,
                bar.volume,
                bar.amount,
                bar.r7_raw,
            ])
            .map_err(storage)?;
    }
    Ok(())
}

#[allow(dead_code)] // yearly lineage writes land with S3
pub(crate) fn insert_lineage(c: &Connection, l: &SeriesLineage) -> Result<(), Error> {
    c.execute(
        "INSERT INTO yearly_series_lineage(series_key,base_daily_digest,base_event_seq,algorithm_version,first_trade_day,last_trade_day,as_of_day) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            l.series_key,
            l.base_daily_digest.as_slice(),
            l.base_event_seq,
            l.algorithm_version,
            l.first_trade_day,
            l.last_trade_day,
            l.as_of_day,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

#[cfg(test)]
mod tests;
