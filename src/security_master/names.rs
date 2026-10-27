//! Name-history adapter boundary (issue-391 §12.1, §10.1).
//!
//! The `security_names` write set belongs to the daily
//! `derived:security_master` item ("主表 + 简称 + 上市/退市 + 迁移"), but its
//! source — the exchange name history (S-6) — is blocked on the #308 Final
//! Gate. This module therefore owns only the *shape* of the answer: an
//! admitted, generation-frozen name snapshot, or a [`PendingSource`] saying why
//! the item writes no name rows. It deliberately holds no writer, so an
//! unadmitted source cannot leak into `security_name_history` (§10.1:
//! "简称历史 → 只有当前快照 + GP29 事件").
//!
//! The current name is a different fact (the TDX snapshot M1 already serves);
//! this dataset is the *interval* history with `known_at`, which only the
//! announcement source can date. `certainty='unbounded'` marks a row whose end
//! is unknown, and `source_kind='unknown'` keeps a name whose provenance is
//! unresolved — neither is rendered as "renamed today" (§4.2).

use super::Exchange;
use super::assembly::PendingSource;
use chrono::NaiveDate;

/// Dataset name the daily item reports (`derived_dataset_versions.dataset_kind`,
/// §3.2) and the read model consumes.
pub const DATASET_KIND: &str = "security_names";
/// One whole-market dataset, like the main table.
pub const DATASET_KEY: &str = "a_share";

/// One `security_name_history` interval (§4.2). The item writes the whole set
/// or none of it: a partial copy could pair today's name with yesterday's
/// `known_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameInterval {
    pub exchange: Exchange,
    pub code: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub name: String,
    pub certainty: NameCertainty,
    /// The announcement day the source proves; `None` stays unknown rather
    /// than being filled with `effective_from` (separation point 3).
    pub known_at: Option<NaiveDate>,
    pub source_kind: NameSourceKind,
    pub source_record_id: Option<String>,
}

/// Whether the row's end is proven. `Unbounded` is not "still current".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameCertainty {
    Interval,
    Unbounded,
}

impl NameCertainty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interval => "interval",
            Self::Unbounded => "unbounded",
        }
    }
}

/// Which announcement family proved the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameSourceKind {
    SzseNameHistory,
    Gp29Marker,
    ExchangeSnapshot,
    Unknown,
}

impl NameSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SzseNameHistory => "szse_name_history",
            Self::Gp29Marker => "gp29_marker",
            Self::ExchangeSnapshot => "exchange_snapshot",
            Self::Unknown => "unknown",
        }
    }
}

/// The name-history adapter's answer for one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameSource {
    Admitted(Vec<NameInterval>),
    Pending(PendingSource),
}

/// The adapter boundary (§16 PR-8): one function, no trait.
///
/// `Some(rows)` is an admitted snapshot; `None` is not "no name changes": it
/// is the `pending_source` state, and the item then writes zero name rows.
pub fn name_history_source(rows: Option<Vec<NameInterval>>) -> NameSource {
    match rows {
        Some(rows) => NameSource::Admitted(rows),
        None => NameSource::Pending(PendingSource {
            dataset: DATASET_KIND,
            source: "exchange_name_history",
            reason: "no admitted name-history snapshot is frozen for this generation \
                     (#308 S-6); the item writes zero security_name_history rows and the \
                     current name stays the TDX snapshot M1 already serves",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_source_is_pending_not_an_empty_answer() {
        match name_history_source(None) {
            NameSource::Pending(pending) => {
                assert_eq!(pending.dataset, DATASET_KIND);
                assert!(!pending.reason.is_empty());
            }
            NameSource::Admitted(_) => panic!("None must not become an empty row set"),
        }
    }

    #[test]
    fn the_interval_vocabulary_matches_the_table_check() {
        assert_eq!(NameCertainty::Interval.as_str(), "interval");
        assert_eq!(NameCertainty::Unbounded.as_str(), "unbounded");
        assert_eq!(
            NameSourceKind::SzseNameHistory.as_str(),
            "szse_name_history"
        );
        assert_eq!(NameSourceKind::Unknown.as_str(), "unknown");
    }
}
