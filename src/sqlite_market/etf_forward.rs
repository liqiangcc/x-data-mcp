//! ETF 前复权派生写入 (#607 PR-C, D-89/D-91/D-93).
//!
//! The ETF 前复权 series is a **pure derivation** of two facts the same item
//! already writes: the 不复权 bars and the category-1/11 factor facts. There is
//! no separate source binding and no separate plan item (design §5.2), so the
//! build hangs off the existing `update_data` item:
//!
//! * [`plan_etf_forward`] decides what the derived series needs — nothing, a
//!   tail append, or a whole-series rewrite;
//! * [`commit_etf_forward_tx`] commits that decision through the shared
//!   [`super::series_commit::commit_series_inner`] inside the caller's
//!   transaction.
//!
//! Both run **inside the 不复权 commit's transaction**, so a 前复权 build
//! failure rolls the 不复权 rows back too: the store never holds a 不复权 head
//! whose 前复权 series is behind it (PR-C requirement 2).
//!
//! Decision table (design §5.2):
//!
//! | 不复权 change | factor digest | action |
//! |---|---|---|
//! | `Append` | unchanged | append the new bars' 前复权 values only |
//! | `Append` | changed | rewrite the whole series |
//! | `NoChange` / `Metadata` | changed | rewrite the whole series |
//! | `NoChange` | unchanged | leave the series untouched |
//! | any, no 前复权 series yet | any | first publish of the whole series |
//!
//! Appending only the tail is sound for the same reason the A-share path
//! records (`src/official_market.rs` "a proven-identical prefix never needs
//! re-adjustment"): a bar's 前复权 value depends only on the events **strictly
//! after** its own day (`qfq.rs`), so new bars cannot change older values.
//!
//! **No check-record filtering here.** Every ETF that has a 不复权 series gets
//! a 前复权 series; whether a reader may see it is PR-D's gate over the
//! stored check records. Design §4.2's "通过核对" wording is a delivery gate,
//! not a write-time predicate: the evidence lives in the source store and can
//! arrive after the market row (the same day's 公告 may be published later),
//! so making the write path depend on it would either block 不复权 updates or
//! let a late-arriving check silently need a rewrite of already-published
//! bytes.

use rusqlite::Connection;

use super::{integrity, series, series_commit};
use crate::compact_market::{self, CompactBar};
use crate::market_data::{Adjustment, Period};
use crate::market_storage::Error;
use crate::qfq::QfqBar;

/// What the derived 前复权 series must do for this item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ForwardPlan {
    /// 不复权 and factors both stand still: the derived series is already right.
    Unchanged,
    /// Extend the existing series with these bars (aliased from the tail of
    /// the freshly derived value list).
    Append(Vec<CompactBar>),
    /// Rebuild the whole series from every 不复权 bar and every event.
    Replace(Vec<CompactBar>),
}

/// Derive the 前复权 bars of `entity` from its stored 不复权 series and stored
/// factor facts, and decide what the derived series needs.
///
/// `raw_changed` says whether this item wrote new 不复权 rows: `Append` when it
/// did, `NoChange`/`Metadata` when it did not. `factors_changed` says whether
/// the factor digest moved (see [`factor_digest`]).
///
/// The derived bars are computed over the **whole** 不复权 series, because an
/// event only ever moves the prefix before it; the plan then keeps only the
/// part the change needs. That keeps one code path for correctness while
/// letting the append case write one bar worth of bytes per new 不复权 bar.
pub(crate) fn plan_etf_forward(
    c: &Connection,
    entity: &str,
    raw_changed: bool,
    factors_changed: bool,
) -> Result<ForwardPlan, Error> {
    let raw_head = series::read_series_adjusted(c, entity, Period::Day, Adjustment::Raw)?
        .ok_or_else(|| integrity("ETF forward derivation lost its 不复权 head"))?;
    let forward_head = series::read_series_adjusted(c, entity, Period::Day, Adjustment::Forward)?;
    // Nothing moved anywhere: the stored derived series is already the answer.
    if !raw_changed && !factors_changed && forward_head.is_some() {
        return Ok(ForwardPlan::Unchanged);
    }
    let raw_bars = series::all_bars(c, raw_head.series_key)?;
    if raw_bars.is_empty() {
        return Err(integrity("ETF 不复权 series carries no bars"));
    }
    let events = read_etf_events(c, entity)?;
    let derived = derive_forward_bars(entity, &raw_bars, &events)?;
    match (raw_changed, factors_changed, forward_head) {
        // A factor change moves every earlier bar, so the series is rebuilt.
        (_, true, Some(_)) => Ok(ForwardPlan::Replace(derived)),
        // First derivation for this entity.
        (_, _, None) => Ok(ForwardPlan::Replace(derived)),
        // New 不复权 bars only: the derived tail follows them, the prefix the
        // append cannot have changed stays byte-identical.
        (true, false, Some(head)) => {
            let tail: Vec<CompactBar> = derived
                .into_iter()
                .filter(|bar| bar.trade_day > head.last_day)
                .collect();
            if tail.is_empty() {
                return Err(integrity(
                    "ETF 不复权 append produced no new 前复权 bar",
                ));
            }
            Ok(ForwardPlan::Append(tail))
        }
        (false, false, Some(_)) => Ok(ForwardPlan::Unchanged),
    }
}

/// Forward-adjust the stored 不复权 bars over the stored factor facts.
///
/// The arithmetic is [`crate::etf_qfq::etf_forward_adjust_exact`] — PR-A's
/// pure layer — and is not reimplemented here. `volume`/`amount`/`r7` are
/// carried through unchanged (they are not adjusted, `qfq.rs` file header),
/// so the derived bar differs from the 不复权 bar exactly in the four price
/// fields and in the owning series' `price_scale`.
fn derive_forward_bars(
    entity: &str,
    raw_bars: &[CompactBar],
    events: &[crate::etf_qfq::EtfEvent],
) -> Result<Vec<CompactBar>, Error> {
    let records: Vec<crate::day_file::RawDayRecord> = raw_bars
        .iter()
        .map(raw_record_of)
        .collect::<Result<_, Error>>()?;
    let adjusted = crate::etf_qfq::etf_forward_adjust_exact(
        &records,
        events,
        compact_market::PRICE_SCALE_QFQ,
    )
    .map_err(|e| integrity(format!("ETF forward chain failed for {entity}: {e}")))?;
    if adjusted.len() != raw_bars.len() {
        return Err(integrity("ETF forward chain changed the bar count"));
    }
    raw_bars
        .iter()
        .zip(adjusted.iter())
        .map(|(raw, adj)| forward_bar(raw, adj))
        .collect()
}

/// The stored 不复权 bar as the `.day` record the pure layer consumes: the
/// stored integers are already the `.day` 厘 values at `price_scale=3`, so the
/// mapping is a widening cast, not a rescale.
fn raw_record_of(bar: &CompactBar) -> Result<crate::day_file::RawDayRecord, Error> {
    if bar.open_units < 0 || bar.high_units < 0 || bar.low_units < 0 || bar.close_units < 0 {
        return Err(integrity("ETF 不复权 bar carries a negative price"));
    }
    Ok(crate::day_file::RawDayRecord {
        date: compact_market::epoch_date(bar.trade_day).map_err(integrity)?,
        ohlc_raw: [
            u32::try_from(bar.open_units).map_err(|_| integrity("ETF open out of range"))?,
            u32::try_from(bar.high_units).map_err(|_| integrity("ETF high out of range"))?,
            u32::try_from(bar.low_units).map_err(|_| integrity("ETF low out of range"))?,
            u32::try_from(bar.close_units).map_err(|_| integrity("ETF close out of range"))?,
        ],
        // The .day word order is (amount f32 bits, volume, r7); the stored
        // row keeps the same three facts, so they pass through bit for bit.
        amount_float32_bits: bar
            .amount
            .as_deref()
            .map(parse_amount_bits)
            .transpose()?
            .unwrap_or(0),
        volume_raw: bar
            .volume
            .map(|v| u32::try_from(v).map_err(|_| integrity("ETF volume out of range")))
            .transpose()?
            .unwrap_or(0),
        r7_raw: bar.r7_raw.unwrap_or(0),
    })
}

/// The stored `amount` column is the decimal rendering of the `.day` f32
/// bits; parse it back to the same bits.
fn parse_amount_bits(text: &str) -> Result<u32, Error> {
    text.parse::<f32>()
        .map(f32::to_bits)
        .map_err(|_| integrity("stored ETF amount is not an f32"))
}

/// One derived bar at the 前复权 `price_scale`, with the 不复权 bar's
/// volume/amount/`r7` copied verbatim.
fn forward_bar(raw: &CompactBar, adj: &QfqBar) -> Result<CompactBar, Error> {
    use num_traits::ToPrimitive as _;
    use rust_decimal::Decimal;
    let scale = compact_market::PRICE_SCALE_QFQ;
    let factor = Decimal::from(10u32.pow(scale));
    let units = |value: Decimal| -> Result<i64, Error> {
        let scaled = value
            .checked_mul(factor)
            .ok_or_else(|| integrity("ETF forward price out of range"))?;
        scaled
            .to_i64()
            .ok_or_else(|| integrity("ETF forward price out of range"))
    };
    Ok(CompactBar {
        trade_day: raw.trade_day,
        open_units: units(adj.open)?,
        high_units: units(adj.high)?,
        low_units: units(adj.low)?,
        close_units: units(adj.close)?,
        volume: raw.volume,
        amount: raw.amount.clone(),
        r7_raw: raw.r7_raw,
    })
}

/// The stored category-1/11 facts as the pure layer's events.
///
/// `effective_day` is carried through as the epoch day encoded to
/// `yyyymmdd`, which is the form [`crate::etf_qfq::EtfEvent`] keys on and the
/// same 生效日 the A-share chain uses.
fn read_etf_events(
    c: &Connection,
    entity: &str,
) -> Result<Vec<crate::etf_qfq::EtfEvent>, Error> {
    use chrono::Datelike as _;
    let stored = super::derived::read_adjustment_events(c, entity)?;
    stored
        .into_iter()
        .map(|event| {
            let day = event.effective_day;
            Ok(crate::etf_qfq::EtfEvent {
                effective_day: day.year() as u32 * 10000
                    + day.month() * 100
                    + day.day(),
                category: event.category,
                factors: event.factors,
            })
        })
        .collect()
}

/// Digest of the entity's stored factor facts, used to decide whether a 前复权
/// rewrite is due. It is [`super::derived::events_digest`] over the stored set
/// — the same digest the factor set was written with, so no second hashing
/// scheme exists.
pub(crate) fn factor_digest(c: &Connection, entity: &str) -> Result<compact_market::Digest, Error> {
    let events = super::derived::read_adjustment_events(c, entity)?;
    super::derived::events_digest(entity, &events, 0)
}

/// Commit the derived 前复权 series for the item described by `raw`.
///
/// Runs inside the caller's `BEGIN IMMEDIATE` transaction, after the 不复权
/// commit and its factor facts: the derivation reads the just-written bars and
/// events, so it must observe them, and a failure here must undo them
/// (requirement 2: no state where 不复权 committed and 前复权 did not follow).
///
/// Bookkeeping stays with the 不复权 half (`ItemFinish::None`): the item owns
/// one result row and one task revision, and the derived series is an event on
/// top of it, not a second item.
pub(crate) fn commit_etf_forward_tx(
    c: &Connection,
    raw: &series_commit::Commit<'_>,
    raw_changed: bool,
    factors_changed: bool,
) -> Result<(), Error> {
    let plan = plan_etf_forward(c, raw.entity, raw_changed, factors_changed)?;
    let forward_head =
        series::read_series_adjusted(c, raw.entity, Period::Day, Adjustment::Forward)?;
    let mut bars: Vec<CompactBar> = Vec::new();
    let mut range_start = 0_i64;
    let mut range_end = 0_i64;
    let change = match plan {
        ForwardPlan::Unchanged => return Ok(()),
        ForwardPlan::Append(tail) => {
            let start = tail[0].trade_day;
            let end = tail[tail.len() - 1].trade_day;
            range_start = start;
            range_end = end;
            bars = tail;
            series_commit::SeriesChange::Append { upserts: &bars }
        }
        ForwardPlan::Replace(all) => {
            let start = all[0].trade_day;
            let end = all[all.len() - 1].trade_day;
            range_start = start;
            range_end = end;
            bars = all;
            // No stored series yet: insert it (`Replace` would demand an
            // existing head and fail `NotFound`).
            if forward_head.is_some() {
                series_commit::SeriesChange::Replace { bars: &bars }
            } else {
                series_commit::SeriesChange::FirstPublish { bars: &bars }
            }
        }
    };
    let window_digest = Some(series_commit::window_fold(&bars)?);
    // The derived series inherits the 不复权 head's binding and evidence: the
    // 前复权 bytes are a function of those inputs, so they name the same
    // provenance instead of inventing a second one.
    let raw_head = series::read_series_adjusted(c, raw.entity, Period::Day, Adjustment::Raw)?
        .ok_or_else(|| integrity("ETF forward commit lost its 不复权 head"))?;
    let base_version_id = forward_head.as_ref().map(|row| row.version_id.clone());
    let result_id = format!("{}:forward", raw.result_id);
    let commit = series_commit::Commit {
        result_id: &result_id,
        task_id: raw.task_id,
        item_id: raw.item_id,
        run_id: raw.run_id,
        occupancy_key: raw.occupancy_key,
        expected_task_revision: raw.expected_task_revision,
        entity: raw.entity,
        period: Period::Day,
        adjustment: Adjustment::Forward,
        base_version_id: base_version_id.as_deref(),
        change,
        range_start_day: range_start,
        range_end_day: range_end,
        algorithm_version: series_commit::UPDATE_ALGO,
        window_digest,
        source_binding_digest: Some(raw_head.source_binding_digest),
        evidence_digest: forward_head
            .as_ref()
            .map(|row| row.evidence_digest)
            .or(Some(raw_head.evidence_digest)),
        coverage_status: Some(raw_head.coverage_status),
        metadata_changed: true,
        event_evidence_digest: raw.event_evidence_digest,
        published_at: raw.published_at,
        staged_version_id: None,
        defer_task_row: true,
    };
    series_commit::commit_series_inner(
        c,
        &commit,
        series_commit::ItemFinish::None,
        series::SeriesShape::etf(Adjustment::Forward),
    )?;
    Ok(())
}
