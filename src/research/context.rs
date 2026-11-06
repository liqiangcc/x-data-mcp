//! Shared read-only inputs for the per-entity read models (issue-433 §5.2).
//!
//! `tradability` and `price_limit` answer one entity at a time, but the tables
//! behind them are whole-table reads: the main table, the code migrations, the
//! raw-series spans, the risk-warning intervals, the suspension intervals, the
//! calendar and the rule book. Asking about N entities used to decode those
//! tables N times. A [`ResearchContext`] decodes each of them at most once, on
//! first use, and every later entity only does in-memory lookups.
//!
//! Nothing here changes a verdict. The single-entity entry points
//! ([`super::tradability::tradability`], [`super::price_limit::price_limit`])
//! build a context for one call, so they read exactly the tables they read
//! before and nothing more. The per-entity bar lookups stay indexed SQL
//! queries; they are not whole-table reads.
//!
//! A context is only as consistent as its connection: build it inside the
//! caller's read transaction and drop it with the transaction, so every
//! entity sees the same snapshot.

use std::cell::OnceCell;
use std::collections::BTreeMap;

use rusqlite::Connection;

use super::Error;
use super::temporal::DatasetTemporal;
use super::universe::SelectionInputs;
use crate::security_master::SuspensionInterval;
use crate::sqlite_market::{calendar, derived, rules, security};
use crate::trading_calendar::TradingCalendar;
use crate::trading_rules::{DATASET_KEY, DATASET_KIND, RuleBook};

/// The suspension intervals, indexed by entity. Within one entity the rows keep
/// the table order, which the overlap check and the `temporal` counts rely on.
pub(crate) struct SuspensionIndex {
    rows: Vec<SuspensionInterval>,
    by_entity: BTreeMap<String, Vec<usize>>,
}

impl SuspensionIndex {
    fn new(rows: Vec<SuspensionInterval>) -> Self {
        let mut by_entity: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, row) in rows.iter().enumerate() {
            by_entity.entry(row.entity()).or_default().push(index);
        }
        Self { rows, by_entity }
    }

    pub(crate) fn of<'a>(
        &'a self,
        entity: &str,
    ) -> impl Iterator<Item = &'a SuspensionInterval> + 'a {
        self.by_entity
            .get(entity)
            .into_iter()
            .flatten()
            .map(|index| &self.rows[*index])
    }
}

/// The rule book plus the registered version it was read under, and the
/// `temporal` counters `price_limit` reports for it.
pub(crate) struct RulesInput {
    pub book: RuleBook,
    pub version: Option<String>,
    pub temporal: DatasetTemporal,
}

/// Lazily loaded, read-only inputs shared by many entity lookups.
pub struct ResearchContext<'c> {
    c: &'c Connection,
    master: OnceCell<SelectionInputs>,
    calendar: OnceCell<Option<TradingCalendar>>,
    suspensions: OnceCell<SuspensionIndex>,
    rules: OnceCell<RulesInput>,
}

impl<'c> ResearchContext<'c> {
    pub fn new(c: &'c Connection) -> Self {
        Self {
            c,
            master: OnceCell::new(),
            calendar: OnceCell::new(),
            suspensions: OnceCell::new(),
            rules: OnceCell::new(),
        }
    }

    pub(crate) fn connection(&self) -> &'c Connection {
        self.c
    }

    pub(crate) fn master(&self) -> Result<&SelectionInputs, Error> {
        if self.master.get().is_none() {
            let _ = self.master.set(SelectionInputs::load(self.c)?);
        }
        Ok(self.master.get().expect("just initialised"))
    }

    /// `None` = the calendar was never derived.
    pub(crate) fn calendar(&self) -> Result<Option<&TradingCalendar>, Error> {
        if self.calendar.get().is_none() {
            let _ = self
                .calendar
                .set(calendar::read_calendar(self.c).map_err(Error::from)?);
        }
        Ok(self.calendar.get().expect("just initialised").as_ref())
    }

    pub(crate) fn suspensions(&self) -> Result<&SuspensionIndex, Error> {
        if self.suspensions.get().is_none() {
            let rows = security::read_suspension_intervals(self.c).map_err(Error::from)?;
            let _ = self.suspensions.set(SuspensionIndex::new(rows));
        }
        Ok(self.suspensions.get().expect("just initialised"))
    }

    pub(crate) fn rules(&self) -> Result<&RulesInput, Error> {
        if self.rules.get().is_none() {
            let book = rules::read_trading_rules(self.c)?;
            let rows = book.price_limits.len() + book.min_ticks.len();
            // The dataset has no `known_at` column, so it is reported with
            // coverage `none` instead of pretending the filter bounded it.
            let temporal = DatasetTemporal {
                rows,
                rows_with_known_at: 0,
                unknown_known_at_rows: rows,
                excluded_known_at_rows: 0,
            };
            let version =
                derived::read_version(self.c, DATASET_KIND, DATASET_KEY)?.map(|row| row.version_id);
            let _ = self.rules.set(RulesInput {
                book,
                version,
                temporal,
            });
        }
        Ok(self.rules.get().expect("just initialised"))
    }
}
