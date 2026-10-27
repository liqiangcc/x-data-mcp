//! 生效时间 ↔ 知晓时间 (separation point 3, issue-391 §12.2).
//!
//! Every M2 fact row carries two times: the interval it describes
//! (`effective_from`/`effective_to`) and the earliest day the consumer could
//! have known it (`known_at`). A point-in-time query first restricts the
//! *information set* by `known_at`, then selects the *record set* by `as_of` —
//! in that order, because reversing it would let a later-announced event
//! rewrite an earlier state (§12.2).
//!
//! A NULL `known_at` is "the 知晓时间 is unknown", never a sentinel and never
//! filled from the effective day (BR-M2-2). Without a `known_at` filter such a
//! row is usable; with a filter it is excluded **and counted**, so a caller can
//! see how much of the answer rests on rows of unknown provenance.
//!
//! This module owns the vocabulary and the counting only. Each read-model
//! module feeds its rows through [`TemporalReport::observe`], which is what
//! keeps "exclude NULL and count" from being re-implemented per dataset.

use std::collections::BTreeMap;

use chrono::NaiveDate;

/// `as_of` (record set) plus an optional `known_at` (information set).
///
/// `as_of` is explicit on purpose: the read model never reads the clock, so
/// the same request over the same database always answers the same question.
/// An entry adapter that wants "today" resolves the last trading day first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Temporal {
    pub as_of: NaiveDate,
    /// `None` = no information-set filter (the Q4 `effective_only` default).
    pub known_at: Option<NaiveDate>,
}

impl Temporal {
    /// `as_of` only: select intervals by their effective day (Q4 default).
    pub fn effective_only(as_of: NaiveDate) -> Self {
        Self {
            as_of,
            known_at: None,
        }
    }

    /// Restrict the information set to rows known by `known_at`, then select by
    /// `as_of`. NULL `known_at` rows are excluded and counted.
    pub fn known_by(as_of: NaiveDate, known_at: NaiveDate) -> Self {
        Self {
            as_of,
            known_at: Some(known_at),
        }
    }
}

/// How much of one dataset carries a 知晓时间. Mirrors the per-dataset
/// `temporal` declaration v8 will publish (§12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownAtCoverage {
    /// No row carries a `known_at` (the gbbq capital facts).
    None,
    /// Some rows do, some do not.
    Partial,
    /// Every row does.
    Complete,
}

impl KnownAtCoverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Partial => "partial",
            Self::Complete => "complete",
        }
    }
}

/// Per-dataset counters behind [`TemporalReport`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DatasetTemporal {
    /// Rows the query looked at (after the entity filter, before `known_at`).
    pub rows: usize,
    pub rows_with_known_at: usize,
    /// Rows whose `known_at` is NULL (all of them are excluded once a filter
    /// is present; the count is reported either way).
    pub unknown_known_at_rows: usize,
    /// Rows dropped because their `known_at` is later than the filter.
    pub excluded_known_at_rows: usize,
}

impl DatasetTemporal {
    pub fn coverage(&self) -> KnownAtCoverage {
        if self.rows == 0 || self.rows_with_known_at == 0 {
            KnownAtCoverage::None
        } else if self.rows_with_known_at == self.rows {
            KnownAtCoverage::Complete
        } else {
            KnownAtCoverage::Partial
        }
    }
}

/// The `temporal` block every M2 read-model answer carries (§12.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporalReport {
    pub as_of: NaiveDate,
    pub known_at: Option<NaiveDate>,
    pub datasets: BTreeMap<&'static str, DatasetTemporal>,
}

impl TemporalReport {
    pub fn new(temporal: Temporal) -> Self {
        Self {
            as_of: temporal.as_of,
            known_at: temporal.known_at,
            datasets: BTreeMap::new(),
        }
    }

    /// Record one row and answer whether the information-set filter admits it.
    ///
    /// Callers must only observe rows that belong to the query (the entity
    /// filter is applied first), otherwise the counts describe the table
    /// instead of the answer.
    pub fn observe(&mut self, dataset: &'static str, known_at: Option<NaiveDate>) -> bool {
        let entry = self.datasets.entry(dataset).or_default();
        entry.rows += 1;
        match known_at {
            Some(_) => entry.rows_with_known_at += 1,
            None => entry.unknown_known_at_rows += 1,
        }
        let Some(cutoff) = self.known_at else {
            return true;
        };
        match known_at {
            Some(day) if day <= cutoff => true,
            _ => {
                entry.excluded_known_at_rows += 1;
                false
            }
        }
    }

    pub fn dataset(&self, dataset: &str) -> Option<&DatasetTemporal> {
        self.datasets.get(dataset)
    }

    pub fn coverage(&self, dataset: &str) -> KnownAtCoverage {
        self.dataset(dataset)
            .map(DatasetTemporal::coverage)
            .unwrap_or(KnownAtCoverage::None)
    }

    /// Total rows the filter excluded because the 知晓时间 is unknown.
    pub fn unknown_known_at_rows(&self) -> usize {
        self.datasets
            .values()
            .map(|entry| entry.unknown_known_at_rows)
            .sum()
    }

    /// Datasets that were read and carry no 知晓时间 at all (§12.2: the
    /// response must say so instead of silently returning every row).
    pub fn datasets_without_known_at(&self) -> Vec<&'static str> {
        self.datasets
            .iter()
            .filter(|(_, entry)| entry.rows > 0 && entry.coverage() == KnownAtCoverage::None)
            .map(|(name, _)| *name)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn a_null_known_at_is_excluded_and_counted_only_under_a_filter() {
        let temporal = Temporal::known_by(day(2026, 1, 10), day(2026, 1, 5));
        let mut report = TemporalReport::new(temporal);
        assert!(report.observe("a", Some(day(2026, 1, 4))));
        assert!(!report.observe("a", Some(day(2026, 1, 6))));
        assert!(!report.observe("a", None));
        let entry = report.dataset("a").unwrap();
        assert_eq!(entry.rows, 3);
        assert_eq!(entry.rows_with_known_at, 2);
        assert_eq!(entry.unknown_known_at_rows, 1);
        assert_eq!(entry.excluded_known_at_rows, 2);
        assert_eq!(report.unknown_known_at_rows(), 1);
        assert_eq!(report.coverage("a"), KnownAtCoverage::Partial);
    }

    #[test]
    fn without_a_filter_every_row_is_usable() {
        let mut report = TemporalReport::new(Temporal::effective_only(day(2026, 1, 10)));
        assert!(report.observe("a", None));
        assert!(report.observe("a", Some(day(2030, 1, 1))));
        assert_eq!(report.coverage("a"), KnownAtCoverage::Partial);
        assert_eq!(report.unknown_known_at_rows(), 1);
        assert_eq!(report.dataset("a").unwrap().excluded_known_at_rows, 0);
    }

    #[test]
    fn a_dataset_with_no_known_at_is_reported_by_name() {
        let mut report = TemporalReport::new(Temporal::known_by(day(2026, 1, 10), day(2026, 1, 5)));
        report.observe("capital_events", None);
        assert_eq!(report.datasets_without_known_at(), vec!["capital_events"]);
        assert_eq!(report.coverage("capital_events"), KnownAtCoverage::None);
    }
}
