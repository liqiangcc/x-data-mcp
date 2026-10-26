//! Trading calendar derived from index daily-bar dates (issue-383 §5, M1 PR-6):
//! the `cal-union-v1` algorithm, its input registration and the coverage
//! self-check threshold.
//!
//! Layer position (separation points 1/2/6): this module knows **no source**.
//! The adapter hands over [`CalendarBatch`] — per registered input, the bar
//! dates of one index `.day` member plus that member's sha256 — and the union
//! is computed from those dates alone, so the calendar can always be recomputed
//! from the layer below (§11 separation point 2). Which indices feed it is a
//! registration row here, not an `if entity == …` branch, and the threshold is
//! a constant of the algorithm rather than a per-run option.
//!
//! Semantics (BR-M1-5) — the inference is deliberately one-way:
//!
//! - a date **without** a row, inside the covered span, is a non-trading day;
//! - a date **with** a row is *not* "tradable": a security can be suspended,
//!   not yet listed or already delisted on a day an index printed a level.
//!   [`TradingCalendar::day_status`] therefore never returns a "tradable"
//!   answer, and there is no `is_trading_day` accessor to misuse;
//! - a date after the last input bar is **unknown** (the calendar does not
//!   predict future trading days, §3.8), so it is neither of the above.
//!
//! The calendar cannot prove completeness either: there is no official
//! calendar file, so "an index had a bar" is the only evidence a date is a
//! trading day, and the derivation stays marked as derived data with its input
//! list, its member digests and its algorithm version (§3.10).

use chrono::NaiveDate;

use crate::market_data::CalendarBatch;

/// Derivation label stored in `derived_dataset_versions.algorithm_version`
/// (§3.10/§5.2): the union of the registered inputs' bar dates.
pub const ALGORITHM_VERSION: &str = "cal-union-v1";
/// Digest algorithm name stored in `digest_format` (§3.10):
/// `SHA256("trading-calendar-v1" ‖ n ‖ (trade_day, sources)…)`.
pub const DIGEST_FORMAT: &str = "trading-calendar-v1";
/// `derived_dataset_versions.dataset_kind` of the calendar (§3.7 DDL CHECK).
pub const DATASET_KIND: &str = "trading_calendar";
/// `derived_dataset_versions.dataset_key`: there is one calendar, the union of
/// both exchanges (§3.7).
pub const DATASET_KEY: &str = "exchange_union";

/// Coverage self-check limit (§5.2): bars outside the calendar must stay at or
/// below 0.01% of every 不复权 bar, expressed as an exact fraction so the
/// comparison never depends on a float.
pub const COVERAGE_LIMIT_NUMERATOR: i64 = 1;
pub const COVERAGE_LIMIT_DENOMINATOR: i64 = 10_000;
/// Bounds on the reported list (§5.2 "逐条列出"): the day list is the primary
/// 清单, the per-bar rows are the detail. At the 0.01% limit a passing run
/// cannot exceed these (0.01% of 16.7M bars is ~1,675 bars over ~90 days), so
/// truncation only happens on a failing run, where the ratio alone decides.
pub const COVERAGE_DAY_LIMIT: usize = 512;
pub const COVERAGE_BAR_LIMIT: usize = 2048;

/// One registered calendar input (§5.1).
#[derive(Debug, Clone, Copy)]
pub struct CalendarInput {
    /// The index entity whose `.day` member dates feed the union.
    pub entity: &'static str,
    /// `trading_days.sources` bit for this input (§3.8: bit0 = sh.000001,
    /// bit1 = sz.399001). A date only this input printed still proves the day.
    pub bit: i64,
    /// Why this input is in the list — the §5.1 evidence travels with the row
    /// instead of living in a document only.
    pub evidence_note: &'static str,
}

/// The registered inputs, in bit order. Adding an input is a row here plus its
/// evidence, not a new branch in the union.
pub const INPUTS: [CalendarInput; 2] = [
    CalendarInput {
        entity: "sh.000001",
        bit: 1 << 0,
        evidence_note: "上证综指: the longest SSE-published index history \
            (1990-12-19 onwards) and a whole-market level, so it has a bar on \
            every SSE trading day (issue-383 §5.1)",
    },
    CalendarInput {
        entity: "sz.399001",
        bit: 1 << 1,
        evidence_note: "深证成指: the longest SZSE-published index history \
            (1991-04-03 onwards). Required because the two exchanges kept \
            different holidays in the early 1990s: sh.000001 alone would mark \
            261 Shenzhen bars (the 1991 Saturday sessions) as non-trading \
            days (issue-383 §5.1)",
    },
];

/// The registration row of one entity, `None` when it is not a calendar input.
pub fn lookup(entity: &str) -> Option<&'static CalendarInput> {
    INPUTS.iter().find(|input| input.entity == entity)
}

/// What the calendar says about one date. There is no `Tradable` variant on
/// purpose: see the module documentation (BR-M1-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayStatus {
    /// A `trading_days` row exists: at least one input index had a bar. This
    /// does **not** mean a given security could be traded that day.
    InCalendar,
    /// No row, and the date is inside the covered span: a non-trading day.
    NonTrading,
    /// After the last input bar: unknown. The calendar does not predict.
    BeyondCalendar,
}

/// One `trading_days` row (§3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradingDay {
    pub day: NaiveDate,
    /// Input bitmask; always non-zero (the DDL CHECK repeats this).
    pub sources: i64,
}

/// One input as it actually entered a derived calendar, for the evidence
/// bundle and the `source_bindings` digest (§5.2 "带输入成员 sha256").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarInputUsed {
    pub entity: &'static str,
    pub bit: i64,
    pub day_member_sha256: String,
    pub day_count: usize,
}

/// A derived trading calendar: strictly increasing days plus the inputs it was
/// derived from.
///
/// A calendar read back from `trading_days` carries no inputs (the table stores
/// rows, not provenance); [`Self::source_member_digest_hex`] fails closed on
/// that, so a stored calendar can be queried but never re-registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradingCalendar {
    days: Vec<TradingDay>,
    inputs: Vec<CalendarInputUsed>,
}

impl TradingCalendar {
    /// `cal-union-v1`: merge the registered inputs' bar dates into one
    /// ascending list, OR-ing the input mask per date.
    ///
    /// Fails closed on anything that would make the union mean less than it
    /// says: an unregistered input, a registered input missing from the batch
    /// (a silent degradation to "one index decided the calendar"), a
    /// duplicate input, an input without dates, a member digest that is not a
    /// sha256, or dates that are not strictly increasing.
    pub fn union(batch: &CalendarBatch) -> Result<Self, String> {
        let mut present = vec![false; INPUTS.len()];
        let mut used = Vec::with_capacity(INPUTS.len());
        let mut contributions: Vec<TradingDay> = Vec::new();
        for input in &batch.inputs {
            let registration = lookup(&input.entity).ok_or_else(|| {
                format!("{} is not a registered calendar input (§5.1)", input.entity)
            })?;
            let slot = INPUTS
                .iter()
                .position(|candidate| candidate.entity == registration.entity)
                .ok_or_else(|| format!("{} has no registration slot", input.entity))?;
            if std::mem::replace(&mut present[slot], true) {
                return Err(format!("calendar input {} appears twice", input.entity));
            }
            if input.days.is_empty() {
                return Err(format!(
                    "calendar input {} carries no bar dates; an absent input would \
                     silently shrink the calendar",
                    input.entity
                ));
            }
            if !crate::hash::is_sha256(&input.day_member_sha256) {
                return Err(format!(
                    "calendar input {} does not carry a sha256 member digest",
                    input.entity
                ));
            }
            let mut previous: Option<NaiveDate> = None;
            for day in &input.days {
                if previous.is_some_and(|last| last >= *day) {
                    return Err(format!(
                        "calendar input {} dates are not strictly increasing",
                        input.entity
                    ));
                }
                previous = Some(*day);
                contributions.push(TradingDay {
                    day: *day,
                    sources: registration.bit,
                });
            }
            used.push(CalendarInputUsed {
                entity: registration.entity,
                bit: registration.bit,
                day_member_sha256: input.day_member_sha256.clone(),
                day_count: input.days.len(),
            });
        }
        for (slot, seen) in present.iter().enumerate() {
            if !seen {
                return Err(format!(
                    "calendar input {} is missing from the batch",
                    INPUTS[slot].entity
                ));
            }
        }
        contributions.sort_by_key(|day| day.day);
        let mut days: Vec<TradingDay> = Vec::with_capacity(contributions.len());
        for day in contributions {
            match days.last_mut() {
                Some(last) if last.day == day.day => last.sources |= day.sources,
                _ => days.push(day),
            }
        }
        Self::validated(days, used)
    }

    /// A calendar read back from `trading_days`: rows only, no inputs.
    pub fn from_stored(days: Vec<TradingDay>) -> Result<Self, String> {
        Self::validated(days, Vec::new())
    }

    fn validated(days: Vec<TradingDay>, inputs: Vec<CalendarInputUsed>) -> Result<Self, String> {
        if days.is_empty() {
            return Err("a trading calendar without a day is not a calendar".into());
        }
        let mut previous: Option<NaiveDate> = None;
        for day in &days {
            if day.sources <= 0 {
                return Err(format!("{} carries no input source", day.day));
            }
            if previous.is_some_and(|last| last >= day.day) {
                return Err("trading calendar days are not strictly increasing".into());
            }
            previous = Some(day.day);
        }
        Ok(Self { days, inputs })
    }

    /// The rows, strictly increasing by date.
    pub fn days(&self) -> &[TradingDay] {
        &self.days
    }

    /// The inputs this calendar was derived from; empty for a stored calendar.
    pub fn inputs(&self) -> &[CalendarInputUsed] {
        &self.inputs
    }

    pub fn len(&self) -> usize {
        self.days.len()
    }

    pub fn is_empty(&self) -> bool {
        self.days.is_empty()
    }

    pub fn first_day(&self) -> Option<NaiveDate> {
        self.days.first().map(|day| day.day)
    }

    /// The last day the calendar covers (§3.8: later days are unknown).
    pub fn last_day(&self) -> Option<NaiveDate> {
        self.days.last().map(|day| day.day)
    }

    /// Whether a row exists. **Not** an "is tradable" answer: see
    /// [`Self::day_status`].
    pub fn contains(&self, day: NaiveDate) -> bool {
        self.days.binary_search_by_key(&day, |row| row.day).is_ok()
    }

    /// The input mask of one date, `None` when the calendar has no row.
    pub fn sources_on(&self, day: NaiveDate) -> Option<i64> {
        self.days
            .binary_search_by_key(&day, |row| row.day)
            .ok()
            .map(|index| self.days[index].sources)
    }

    /// The three-valued answer of BR-M1-5. `None` only for an empty calendar,
    /// which [`Self::union`] and [`Self::from_stored`] never produce.
    pub fn day_status(&self, day: NaiveDate) -> Option<DayStatus> {
        let last = self.last_day()?;
        if self.contains(day) {
            Some(DayStatus::InCalendar)
        } else if day > last {
            Some(DayStatus::BeyondCalendar)
        } else {
            Some(DayStatus::NonTrading)
        }
    }

    /// `true` only when the date is *provably* a non-trading day: inside the
    /// covered span and absent from the table. A future date is unknown, not
    /// non-trading.
    pub fn is_non_trading_day(&self, day: NaiveDate) -> bool {
        self.day_status(day) == Some(DayStatus::NonTrading)
    }

    /// Content digest of §3.10 in hex:
    /// `SHA256("trading-calendar-v1" ‖ n ‖ (trade_day, sources)…)`.
    ///
    /// Values and order only: the inputs, their digests, the coverage numbers
    /// and the task ids never enter it, so re-deriving the same dates from the
    /// same members is the same version.
    pub fn digest_hex(&self) -> String {
        let mut payload = Vec::with_capacity(32 + self.days.len() * 16);
        payload.extend_from_slice(b"trading-calendar-v1\0");
        payload.extend_from_slice(&(self.days.len() as u64).to_be_bytes());
        for day in &self.days {
            payload.extend_from_slice(&crate::compact_market::epoch_day(day.day).to_be_bytes());
            payload.extend_from_slice(&(day.sources as u64).to_be_bytes());
        }
        crate::hash::sha256(&payload)
    }

    /// Folded digest of the input `.day` members (§5.2):
    /// `SHA256("calendar-inputs-v1" ‖ n ‖ (bit, member sha256)…)` in
    /// registration order. `source_bindings` has one member column, so the two
    /// members are folded into the one digest the column can hold; the member
    /// names and per-input digests stay readable in the evidence bundle.
    pub fn source_member_digest_hex(&self) -> Result<String, String> {
        if self.inputs.is_empty() {
            return Err(
                "a calendar read back from storage carries no input members; \
                 its source binding cannot be recomputed"
                    .into(),
            );
        }
        let mut payload = Vec::with_capacity(32 + self.inputs.len() * 40);
        payload.extend_from_slice(b"calendar-inputs-v1\0");
        payload.extend_from_slice(&(self.inputs.len() as u64).to_be_bytes());
        for input in &self.inputs {
            payload.extend_from_slice(&(input.bit as u64).to_be_bytes());
            let digest = decode_sha256(&input.day_member_sha256)?;
            payload.extend_from_slice(&digest);
        }
        Ok(crate::hash::sha256(&payload))
    }
}

fn decode_sha256(text: &str) -> Result<[u8; 32], String> {
    if !crate::hash::is_sha256(text) {
        return Err(format!("{text:?} is not a sha256 digest"));
    }
    let mut out = [0u8; 32];
    for (index, chunk) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let text = std::str::from_utf8(chunk).map_err(|_| "digest is not hex".to_string())?;
        out[index] = u8::from_str_radix(text, 16).map_err(|_| "digest is not hex".to_string())?;
    }
    Ok(out)
}

/// The coverage self-check of §5.2: how many 不复权 bars fall outside the
/// calendar, and which ones.
///
/// The check exists because the calendar cannot prove completeness (no
/// official calendar file): a bar outside it means the union missed a real
/// trading day. Over the limit the item fails instead of quietly publishing a
/// calendar that contradicts the price data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarCoverage {
    /// Bars checked (every stored 不复权 security bar).
    pub total_bars: i64,
    /// Bars whose `trade_day` has no calendar row.
    pub outside_bars: i64,
    /// The 清单: out-of-calendar days with their bar counts, ascending.
    pub outside_days: Vec<(NaiveDate, i64)>,
    /// Per-bar detail (`entity`, day) for the first bars outside, ascending.
    pub outside_samples: Vec<(String, NaiveDate)>,
    /// Set when the day list / bar detail hit their bounds, so a truncated
    /// 清单 is never mistaken for a complete one.
    pub days_truncated: bool,
    pub samples_truncated: bool,
    /// The calendar this measurement was taken against; the commit re-checks
    /// it so a coverage figure can never be paired with another calendar.
    pub calendar_digest: String,
}

impl CalendarCoverage {
    /// `outside / total` as the exact fraction and its decimal form, for the
    /// evidence and for error messages.
    pub fn ratio_text(&self) -> String {
        if self.total_bars == 0 {
            return "no bars checked".into();
        }
        format!(
            "{}/{} = {:.5}%",
            self.outside_bars,
            self.total_bars,
            (self.outside_bars as f64) * 100.0 / (self.total_bars as f64)
        )
    }

    /// §5.2: at most `COVERAGE_LIMIT_NUMERATOR / COVERAGE_LIMIT_DENOMINATOR`
    /// (0.01%) of the bars may fall outside the calendar.
    ///
    /// A run that checked no bar is **not** within the limit: it proved
    /// nothing, and accepting it would let an empty run register a calendar
    /// the data never confirmed.
    pub fn within_limit(&self) -> bool {
        self.total_bars > 0
            && self.outside_bars >= 0
            && self.outside_bars * COVERAGE_LIMIT_DENOMINATOR
                <= self.total_bars * COVERAGE_LIMIT_NUMERATOR
    }

    /// The gate the calendar item runs through.
    pub fn require_within_limit(&self) -> Result<(), String> {
        if self.total_bars == 0 {
            return Err(
                "the coverage self-check found no 不复权 bar to check; a calendar \
                 nobody confirmed is not published"
                    .into(),
            );
        }
        if !self.within_limit() {
            return Err(format!(
                "{} 不复权 bars fall outside the calendar ({}), over the 0.01% limit; \
                 first days outside: {:?}",
                self.outside_bars,
                self.ratio_text(),
                self.outside_days
                    .iter()
                    .take(8)
                    .map(|(day, bars)| format!("{day} ({bars} bars)"))
                    .collect::<Vec<_>>()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
