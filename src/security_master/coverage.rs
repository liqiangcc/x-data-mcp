//! Coverage windows shared by the derived whole-market datasets
//! (issue-446 §10.1, D-58/D-61).
//!
//! One acquisition window and whether it produced rows. `observed=false` is a
//! `not_observed` window: no row is derived from it, and an open interval is
//! cut at its start instead of being claimed across the gap. The type lives
//! here instead of in one dataset module because risk warnings (PR-10) and
//! name history (PR-12) both record acquisition windows with the same shape
//! and the same "absence is not evidence" rule.

use chrono::NaiveDate;

use super::Exchange;

/// One acquisition window and whether it produced rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageWindow {
    pub source_kind: String,
    pub exchange: Exchange,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub observed: bool,
    pub rows: usize,
    pub reason: String,
}
