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
/// What the 不复权 commit did, in the three shapes the derivation cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawMutation {
    /// Tail extension.
    Append,
    /// Non-prefix rewrite or first publication: the whole series is new.
    Rewrite,
    /// `NoChange` / `Metadata`: no bar moved.
    Still,
}

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
/// `raw_change` is the 不复权 commit's own kind, because "wrote new bars" is not
/// a boolean (#607 PR-C review r1 item 2): an `Append` may extend the derived
/// series, while a `Replace` is a non-prefix rewrite of the 不复权 series and
/// must rewrite the derived one too — the stored prefix is no longer the
/// 不复权 prefix, so keeping it would strand stale bytes. `factors_changed`
/// says whether the factor digest moved (see [`factor_digest`]).
///
/// The derived bars are computed over the **whole** 不复权 series, because an
/// event only ever moves the prefix before it; the plan then keeps only the
/// part the change needs. That keeps one code path for correctness while
/// letting the append case write one bar worth of bytes per new 不复权 bar.
pub(crate) fn plan_etf_forward(
    c: &Connection,
    entity: &str,
    raw_change: &series_commit::SeriesChange<'_>,
    factors_changed: bool,
) -> Result<ForwardPlan, Error> {
    let kind = match raw_change {
        series_commit::SeriesChange::Append { .. } => RawMutation::Append,
        series_commit::SeriesChange::Replace { .. }
        | series_commit::SeriesChange::FirstPublish { .. } => RawMutation::Rewrite,
        series_commit::SeriesChange::NoChange | series_commit::SeriesChange::Metadata => {
            RawMutation::Still
        }
        // Yearly is rejected by the ETF commit path before this point; naming
        // it keeps the match exhaustive instead of silently treating a yearly
        // change as "still".
        series_commit::SeriesChange::Yearly { .. } => {
            return Err(integrity("a yearly change cannot reach the ETF daily path"));
        }
    };
    let (appends, still) = match kind {
        RawMutation::Append => (true, false),
        RawMutation::Rewrite => (false, false),
        RawMutation::Still => (false, true),
    };
    let raw_head = series::read_series_adjusted(c, entity, Period::Day, Adjustment::Raw)?
        .ok_or_else(|| integrity("ETF forward derivation lost its 不复权 head"))?;
    let forward_head = series::read_series_adjusted(c, entity, Period::Day, Adjustment::Forward)?;
    // Nothing moved anywhere: the stored derived series is already the answer.
    if still && !factors_changed && forward_head.is_some() {
        return Ok(ForwardPlan::Unchanged);
    }
    // An append can only move the tail, and the derivation is a per-bar fold
    // over the factor barriers, so only the bars at or before the stored head
    // ever need reading again — they cannot have changed. A rewrite or a first
    // derivation has to see the whole series (#607 PR-C review r1 item 3).
    let raw_bars = match (appends, &forward_head) {
        (true, Some(head)) => {
            series::bars_in_range(c, raw_head.series_key, head.last_day + 1, i64::MAX)?
        }
        _ => series::all_bars(c, raw_head.series_key)?,
    };
    if raw_bars.is_empty() {
        return Err(integrity("ETF 不复权 series carries no bars"));
    }
    let events = read_etf_events(c, entity)?;
    let derived = derive_forward_bars(entity, &raw_bars, &events)?;
    match plan_kind(appends, factors_changed, forward_head.is_some()) {
        ForwardPlanKind::Append => {
            // The bars read for the append arm are already the tail, so the
            // derived set here is exactly what the append writes.
            if derived.is_empty() {
                return Err(integrity("ETF 不复权 append produced no new 前复权 bar"));
            }
            Ok(ForwardPlan::Append(derived))
        }
        ForwardPlanKind::Replace => Ok(ForwardPlan::Replace(derived)),
    }
}

/// Whether the derived series can be extended, must be rebuilt, or stands.
///
/// Only an `Append` onto a stored head with an unchanged factor digest can keep
/// the stored prefix. Everything else rebuilds it: a factor change moves the
/// prefix, a 不复权 `Replace` is no longer the prefix we stored, and a first
/// derivation has nothing stored yet. The caller returns `Unchanged` before
/// reaching here when neither the bars nor the factors moved.
fn plan_kind(appends: bool, factors_changed: bool, has_forward_head: bool) -> ForwardPlanKind {
    match (appends, factors_changed, has_forward_head) {
        (true, false, true) => ForwardPlanKind::Append,
        _ => ForwardPlanKind::Replace,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForwardPlanKind {
    Append,
    Replace,
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
    let adjusted =
        crate::etf_qfq::etf_forward_adjust_exact(&records, events, compact_market::PRICE_SCALE_QFQ)
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
fn read_etf_events(c: &Connection, entity: &str) -> Result<Vec<crate::etf_qfq::EtfEvent>, Error> {
    use chrono::Datelike as _;
    let stored = super::derived::read_adjustment_events(c, entity)?;
    stored
        .into_iter()
        .map(|event| {
            let day = event.effective_day;
            Ok(crate::etf_qfq::EtfEvent {
                effective_day: day.year() as u32 * 10000 + day.month() * 100 + day.day(),
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
    raw_change: &series_commit::SeriesChange<'_>,
    factors_changed: bool,
) -> Result<(), Error> {
    let plan = plan_etf_forward(c, raw.entity, raw_change, factors_changed)?;
    let forward_head =
        series::read_series_adjusted(c, raw.entity, Period::Day, Adjustment::Forward)?;
    // The plan's own kind decides the shared commit kind; the bars are moved
    // out here so the `SeriesChange` borrow has one owner.
    let (is_append, bars) = match plan {
        ForwardPlan::Unchanged => return Ok(()),
        ForwardPlan::Append(tail) => (true, tail),
        ForwardPlan::Replace(all) => (false, all),
    };
    let range_start = bars[0].trade_day;
    let range_end = bars[bars.len() - 1].trade_day;
    let change = if is_append {
        series_commit::SeriesChange::Append { upserts: &bars }
    } else if forward_head.is_some() {
        series_commit::SeriesChange::Replace { bars: &bars }
    } else {
        // No stored series yet: insert it (`Replace` would demand an existing
        // head and fail `NotFound`).
        series_commit::SeriesChange::FirstPublish { bars: &bars }
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn compact(date: NaiveDate, li: i64) -> CompactBar {
        CompactBar {
            trade_day: compact_market::epoch_day(date),
            open_units: li,
            high_units: li,
            low_units: li,
            close_units: li,
            volume: Some(1_000),
            amount: Some("4.05".into()),
            r7_raw: Some(0),
        }
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn event(day: u32, category: u8, c1: &str, c3: &str) -> crate::etf_qfq::EtfEvent {
        crate::etf_qfq::etf_event(
            day,
            category,
            crate::etf_qfq::etf_factors(c1, "0", c3, "0").unwrap(),
        )
    }

    /// The derived bar is `price × 10^6` at `price_scale=6`; `li` is the
    /// stored 厘 value.
    fn forward_units(bars: &[CompactBar]) -> Vec<(i64, i64, i64, i64, i64)> {
        bars.iter()
            .map(|b| {
                (
                    b.trade_day,
                    b.open_units,
                    b.high_units,
                    b.low_units,
                    b.close_units,
                )
            })
            .collect()
    }

    #[test]
    fn sz_159919_cash_event_lands_on_4044900_units_in_storage() {
        // 4.111 元 − 0.661/10 = 4.0449 元, stored as 4,044,900 units.
        let bars = [compact(day(2025, 3, 28), 4_111)];
        let events = [event(
            20250331,
            crate::etf_qfq::ETF_CATEGORY_CASH,
            "0.661",
            "0",
        )];
        let derived = derive_forward_bars("sz.159919", &bars, &events).unwrap();
        assert_eq!(derived[0].close_units, 4_044_900);
        // volume/amount/r7 pass through byte for byte.
        assert_eq!(derived[0].volume, bars[0].volume);
        assert_eq!(derived[0].amount, bars[0].amount);
        assert_eq!(derived[0].r7_raw, bars[0].r7_raw);
    }

    #[test]
    fn sz_159922_scale_then_cash_lands_on_2334400_units_in_storage() {
        // 6.159 / 2.5 − 1.292/10 = 2.3344 元, stored as 2,334,400 units.
        let bars = [compact(day(2024, 11, 29), 6_159)];
        let events = [
            event(20241201, crate::etf_qfq::ETF_CATEGORY_SCALE, "0", "2.5"),
            event(20241202, crate::etf_qfq::ETF_CATEGORY_CASH, "1.292", "0"),
        ];
        let derived = derive_forward_bars("sz.159922", &bars, &events).unwrap();
        assert_eq!(derived[0].close_units, 2_334_400);
    }

    #[test]
    fn an_event_only_moves_the_bars_strictly_before_its_day() {
        let bars = [
            compact(day(2025, 3, 27), 4_111),
            compact(day(2025, 3, 28), 4_111),
            compact(day(2025, 3, 31), 4_111),
            compact(day(2025, 4, 1), 4_111),
        ];
        let events = [event(
            20250331,
            crate::etf_qfq::ETF_CATEGORY_CASH,
            "0.661",
            "0",
        )];
        let derived = derive_forward_bars("sz.159919", &bars, &events).unwrap();
        assert_eq!(
            forward_units(&derived),
            vec![
                (
                    bars[0].trade_day,
                    4_044_900,
                    4_044_900,
                    4_044_900,
                    4_044_900
                ),
                (
                    bars[1].trade_day,
                    4_044_900,
                    4_044_900,
                    4_044_900,
                    4_044_900
                ),
                // E itself and everything after it are untouched.
                (
                    bars[2].trade_day,
                    4_111_000,
                    4_111_000,
                    4_111_000,
                    4_111_000
                ),
                (
                    bars[3].trade_day,
                    4_111_000,
                    4_111_000,
                    4_111_000,
                    4_111_000
                ),
            ]
        );
    }

    #[test]
    fn derivation_is_deterministic_so_a_rerun_keeps_the_version() {
        // `version_id` is the fold over the derived bars (series-fold-v2), so
        // identical input giving identical bars is what makes a rerun
        // idempotent. Nothing else in the path can move the id.
        let bars = [compact(day(2025, 3, 28), 4_111)];
        let events = [event(
            20250331,
            crate::etf_qfq::ETF_CATEGORY_CASH,
            "0.661",
            "0",
        )];
        let first = derive_forward_bars("sz.159919", &bars, &events).unwrap();
        let again = derive_forward_bars("sz.159919", &bars, &events).unwrap();
        assert_eq!(first, again);
        let digest = |bars: &[CompactBar]| {
            series::fold_bars(
                "sz.159919",
                Period::Day,
                series::SeriesShape::etf(Adjustment::Forward),
                bars,
            )
            .unwrap()
        };
        assert_eq!(digest(&first), digest(&again));
    }

    #[test]
    fn an_empty_event_set_is_the_identity_chain() {
        let bars = [compact(day(2025, 3, 28), 4_111)];
        let derived = derive_forward_bars("sz.159919", &bars, &[]).unwrap();
        assert_eq!(derived[0].close_units, 4_111_000);
    }

    /// `raw_changed` as a boolean cannot distinguish an append from a
    /// non-prefix rewrite (#607 PR-C review r1 item 2). The plan must rewrite
    /// the derived series for a rewrite, and only extend it for an append onto
    /// an unchanged stored head.
    #[test]
    fn only_a_tail_append_onto_a_stored_head_can_extend_the_series() {
        // Append, no factor change, stored head: the one extendable shape.
        assert_eq!(plan_kind(true, false, true), ForwardPlanKind::Append);
        // A rewrite looks like "changed" to a boolean, but it is not the prefix
        // we stored, so the derived series is rebuilt.
        assert_eq!(plan_kind(false, false, true), ForwardPlanKind::Replace);
        // A factor change moves every earlier bar.
        assert_eq!(plan_kind(true, true, true), ForwardPlanKind::Replace);
        assert_eq!(plan_kind(false, true, true), ForwardPlanKind::Replace);
        // First derivation: nothing stored to extend.
        assert_eq!(plan_kind(true, false, false), ForwardPlanKind::Replace);
        assert_eq!(plan_kind(false, false, false), ForwardPlanKind::Replace);
        assert_eq!(plan_kind(true, true, false), ForwardPlanKind::Replace);
        assert_eq!(plan_kind(false, true, false), ForwardPlanKind::Replace);
    }
}

#[cfg(test)]
mod perf {
    //! Backfill cost measurement for #607 PR-C requirement 6. Ignored by
    //! default; run with
    //! `cargo test --lib --release etf_forward_backfill_cost -- --ignored --nocapture`.
    use super::tests_support::*;
    use super::*;

    #[test]
    #[ignore = "measurement, not a gate"]
    fn etf_forward_backfill_cost() {
        // Two history lengths: ~10 trading years and ~20, so the linearity
        // of the per-ETF cost is visible rather than assumed.
        for bars_per_etf in [2_500usize, 5_000] {
            const ETFS: usize = 1_632;
            let bars = synthetic_bars(bars_per_etf);
            let events = synthetic_events(12);
            let start = std::time::Instant::now();
            let derived = derive_forward_bars("sz.159919", &bars, &events).unwrap();
            let per_etf = start.elapsed();
            assert_eq!(derived.len(), bars_per_etf);
            let total = per_etf * ETFS as u32;
            let one_bar = std::mem::size_of::<CompactBar>();
            let peak_bytes =
                bars_per_etf * (one_bar + std::mem::size_of::<crate::day_file::RawDayRecord>());
            println!(
                "ETF forward backfill: bars={bars_per_etf} per_etf={per_etf:?} \
                 extrapolated_{ETFS}_etfs={total:?} \
                 per_etf_bar_bytes={peak_bytes} (one ETF at a time)"
            );
        }
    }
}

#[cfg(test)]
mod tests_support {
    use super::*;
    use chrono::{Datelike as _, NaiveDate};

    pub(super) fn synthetic_bars(count: usize) -> Vec<CompactBar> {
        let first = NaiveDate::from_ymd_opt(2015, 1, 5).unwrap();
        (0..count)
            .map(|index| {
                // Weekdays only, so the fixture looks like a trading calendar.
                let mut day = first + chrono::Days::new(index as u64);
                while matches!(day.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                    day = day + chrono::Days::new(1);
                }
                let li = 40_000 + (index as i64 % 97) * 3;
                CompactBar {
                    trade_day: compact_market::epoch_day(day),
                    open_units: li,
                    high_units: li + 12,
                    low_units: li - 9,
                    close_units: li + 4,
                    volume: Some(1_000_000),
                    amount: Some("1.5".into()),
                    r7_raw: Some(0),
                }
            })
            .collect()
    }

    pub(super) fn synthetic_events(count: usize) -> Vec<crate::etf_qfq::EtfEvent> {
        (0..count)
            .map(|index| {
                // One event per ~400 trading days, alternating the two
                // categories — the shape of a real ETF's factor history.
                let year = 2016 + index as u32;
                let (category, c1, c3) = if index % 2 == 0 {
                    (crate::etf_qfq::ETF_CATEGORY_CASH, "0.05", "0")
                } else {
                    (crate::etf_qfq::ETF_CATEGORY_SCALE, "0", "1.02")
                };
                crate::etf_qfq::etf_event(
                    year * 10_000 + 630,
                    category,
                    crate::etf_qfq::etf_factors(c1, "0", c3, "0").unwrap(),
                )
            })
            .collect()
    }
}
