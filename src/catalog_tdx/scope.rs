//! Frozen scope rule S and the explicit exclusion partition (design:
//! docs/tasks/issue-6-catalog-impl.md §3).
//!
//! S = { f0 ∈ {0,1} ∧ f2 ≠ ε ∧ f5 ≠ ε }. Every non-S row is classified into
//! exactly one exclusion class; the partition is disjoint and exhaustive by
//! construction, and `reconciliation` exposes the row-count identity the
//! publisher must report.

use super::tdxhy::TdxhyRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    /// f0 = 2 (Beijing market digit; "BJ" is the anchored enum value, the
    /// business meaning stays UNANCHORED).
    BjMarket,
    /// f0 ∈ {0,1} with empty T or X code and a B-share-segment code prefix
    /// (200xxx / 900xxx). Segment membership is a mechanical prefix fact.
    BShare,
    /// f0 ∈ {0,1} with empty T or X code outside the B-share prefixes.
    Anomalous,
}

/// An in-scope member: market digit, six-digit code, and both industry codes
/// present (non-optional because S requires them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedMember {
    pub line: usize,
    pub market: u8,
    pub code: String,
    pub t_code: String,
    pub x_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedRow {
    pub line: usize,
    pub market: u8,
    pub code: String,
    pub class: Exclusion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeOutcome {
    pub members: Vec<ScopedMember>,
    pub excluded: Vec<ExcludedRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reconciliation {
    pub total: usize,
    pub scope: usize,
    pub bj_market: usize,
    pub b_share: usize,
    pub anomalous: usize,
}

/// Partition parsed rows into S members and explicitly classified
/// exclusions. The identity `total == scope + bj + b_share + anomalous`
/// holds by construction and is re-asserted in `reconciliation`.
pub fn apply(rows: &[TdxhyRow]) -> ScopeOutcome {
    let mut members = Vec::new();
    let mut excluded = Vec::new();
    for row in rows {
        if row.market == 2 {
            excluded.push(ExcludedRow {
                line: row.line,
                market: row.market,
                code: row.code.clone(),
                class: Exclusion::BjMarket,
            });
            continue;
        }
        match (&row.t_code, &row.x_code) {
            (Some(t_code), Some(x_code)) => members.push(ScopedMember {
                line: row.line,
                market: row.market,
                code: row.code.clone(),
                t_code: t_code.clone(),
                x_code: x_code.clone(),
            }),
            _ => excluded.push(ExcludedRow {
                line: row.line,
                market: row.market,
                code: row.code.clone(),
                class: if row.code.starts_with("200") || row.code.starts_with("900") {
                    Exclusion::BShare
                } else {
                    Exclusion::Anomalous
                },
            }),
        }
    }
    ScopeOutcome { members, excluded }
}

impl ScopeOutcome {
    /// Row-count identity the refresh diagnostics must report; a violation
    /// means the partition itself is broken and must not silently publish.
    pub fn reconciliation(&self, total: usize) -> Reconciliation {
        let counts = Reconciliation {
            total,
            scope: self.members.len(),
            bj_market: self
                .excluded
                .iter()
                .filter(|row| row.class == Exclusion::BjMarket)
                .count(),
            b_share: self
                .excluded
                .iter()
                .filter(|row| row.class == Exclusion::BShare)
                .count(),
            anomalous: self
                .excluded
                .iter()
                .filter(|row| row.class == Exclusion::Anomalous)
                .count(),
        };
        debug_assert_eq!(
            counts.total,
            counts.scope + counts.bj_market + counts.b_share + counts.anomalous
        );
        counts
    }
}
