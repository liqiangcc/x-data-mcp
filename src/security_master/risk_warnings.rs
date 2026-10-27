//! Risk-warning (ST/*ST) adapter boundary (issue-391 §6.3, §10.1).
//!
//! The dataset's rows come from a publisher announcement source (S-3) that is
//! still blocked on the #308 admission decision. This module therefore owns
//! only the *shape* of the answer: an admitted row set, or a
//! [`PendingSource`] that says why there are none. It deliberately holds no
//! writer: while the source is pending the daily item writes **zero rows** and
//! reports `pending_source` (AC-M2-13). An empty row set is never rendered as
//! "normal" — the read model treats a missing interval as `unknown`, not
//! `normal` (§10.1).
//!
//! Once #308 admits the source, the parser lands next to `official_security`
//! and this function returns `Admitted(rows)`; the daily item then writes the
//! rows through the same seam without this module learning a file name.

use super::RiskWarningInterval;
use super::assembly::PendingSource;

/// Dataset name the daily item reports and the read model consumes
/// (`derived_dataset_versions.dataset_kind`, §3.2).
pub const DATASET_KIND: &str = "risk_warnings";
/// One whole-market dataset, like the main table and the suspensions.
pub const DATASET_KEY: &str = "a_share";

/// The risk-warning adapter's answer for one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskWarningSource {
    Admitted(Vec<RiskWarningInterval>),
    Pending(PendingSource),
}

/// The adapter boundary (§16 PR-3/PR-8): one function, no trait.
///
/// `Some(rows)` is an admitted, generation-frozen announcement snapshot.
/// `None` is not "no warnings": it is the `pending_source` state, and the
/// dataset then carries zero rows.
pub fn risk_warning_source(rows: Option<Vec<RiskWarningInterval>>) -> RiskWarningSource {
    match rows {
        Some(rows) => RiskWarningSource::Admitted(rows),
        None => RiskWarningSource::Pending(PendingSource {
            dataset: DATASET_KIND,
            source: "exchange_risk_warning_announcement",
            reason: "no admitted ST/risk-warning announcement snapshot is frozen for this \
                     generation (#308 S-3/S-6); zero rows are written and no 'normal' state \
                     is inferred",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_source_is_pending_not_an_empty_answer() {
        match risk_warning_source(None) {
            RiskWarningSource::Pending(pending) => {
                assert_eq!(pending.dataset, DATASET_KIND);
                assert!(!pending.reason.is_empty());
            }
            RiskWarningSource::Admitted(_) => panic!("None must not become an empty row set"),
        }
        let admitted = risk_warning_source(Some(Vec::new()));
        assert_eq!(admitted, RiskWarningSource::Admitted(Vec::new()));
    }
}
