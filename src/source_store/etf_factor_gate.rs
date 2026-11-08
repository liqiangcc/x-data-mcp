//! The ETF forward-adjustment blocking decision (`#607` PR-B, D-92 Q1 (a),
//! Q10, design §3).
//!
//! Layer position: this module is a **pure function over facts**, with no
//! database, clock or I/O. Its inputs are
//!
//! 1. the entity's current stored factor facts (each with the digest of the
//!    evidence bundle it was written with, `market_data::AdjustmentEvent` +
//!    `evidence_digest`), and
//! 2. the check records read out of the source store
//!    ([`super::etf_factor_checks::CheckSet`]).
//!
//! and its output is either "admit the whole entity" or "block the whole
//! entity with one reason code". Option (a) of design §3.3 — whole-entity
//! fail-closed — is the ruling: a truncated sequence whose start day moves with
//! every factor revision is the kind of silent drift D-89 §6 exists to stop.
//!
//! Rules (design §3.1/§3.2, D-92):
//!
//! - **Missing record blocks.** There is no allow-list: an event with no check
//!   record is `etf_factor_unverified`. This is what makes a factor event
//!   arriving from a daily run block the entity until it is checked (Q10),
//!   because the record is keyed by the event's `evidence_digest`.
//! - **Any `unverified` blocks the whole entity** (Q1 (a)).
//! - **Events before the first bar and `no_effect` events do not take part.**
//!   They cannot change the sequence (D-91 §3), so they neither admit nor
//!   block.
//! - **A changed `evidence_digest` stops matching** the old record: the key is
//!   `(entity, ordinal, evidence_digest)`, so a re-derived event needs a new
//!   check.
//! - **Category outside 1/11 blocks** (`etf_event_category_unexpected`): the DDL
//!   already forbids it, so this is a defence in depth.
//! - **An unparsable factor blocks** (`etf_factor_unparsable`): a category-11
//!   `c3` of zero, a non-finite or negative factor.
//! - **Two events on one effective day X** (D-91 §2 "先 cat11 后 cat1") need
//!   both records verified; one missing record is `etf_factor_unverified`, and a
//!   group a check explicitly marked as order-unverified is
//!   `etf_event_order_unverified` (design §3.1 B4).

use std::collections::BTreeSet;

use chrono::NaiveDate;

use super::etf_factor_checks::EtfFactorCheck;

/// Stable reason code of a blocked entity (design §3.2 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BlockReason {
    /// B1/B2/B6: an event that matters for the sequence has no verified check.
    FactorUnverified,
    /// B4: several events on one effective day whose order/composition is not
    /// anchored by a verified check of the group.
    EventOrderUnverified,
    /// B3: a category outside 1/11.
    EventCategoryUnexpected,
    /// B5: a factor that cannot be turned into an exact fraction.
    FactorUnparsable,
    /// The implementation has not built a forward series for this entity yet.
    QfqNotBuilt,
}

impl BlockReason {
    /// The wire value (`details.reason` of `adjustment_unavailable`, design
    /// §3.2; PR-D maps it into the envelope, this PR only names it).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FactorUnverified => "etf_factor_unverified",
            Self::EventOrderUnverified => "etf_event_order_unverified",
            Self::EventCategoryUnexpected => "etf_event_category_unexpected",
            Self::FactorUnparsable => "etf_factor_unparsable",
            Self::QfqNotBuilt => "etf_qfq_not_built",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "etf_factor_unverified" => Self::FactorUnverified,
            "etf_event_order_unverified" => Self::EventOrderUnverified,
            "etf_event_category_unexpected" => Self::EventCategoryUnexpected,
            "etf_factor_unparsable" => Self::FactorUnparsable,
            "etf_qfq_not_built" => Self::QfqNotBuilt,
            _ => return None,
        })
    }

    /// Precedence when more than one condition holds: the first one names the
    /// verdict, so two runs over the same inputs report the same reason.
    fn rank(self) -> u8 {
        match self {
            Self::EventCategoryUnexpected => 0,
            Self::FactorUnparsable => 1,
            Self::EventOrderUnverified => 2,
            Self::FactorUnverified => 3,
            Self::QfqNotBuilt => 4,
        }
    }
}

impl std::fmt::Display for BlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One factor fact as the decision sees it: the stored event plus the digest
/// of the evidence bundle it was written with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactorFact {
    pub ordinal: i64,
    /// The gbbq event day (D-91: the registration day `R`).
    pub effective_day: NaiveDate,
    pub category: u8,
    /// `c1..c4` as the stored business decimals (ETF: f32 shortest round-trip).
    pub factors: [rust_decimal::Decimal; 4],
    /// Hex of the 32-byte `adjustment_events.evidence_digest`.
    pub evidence_digest: String,
}

/// The current factor facts of one entity plus the two inputs the "before the
/// first bar" rule needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityFacts {
    pub entity: String,
    /// First trade day of the entity's raw (不复权) series; `None` when the
    /// entity has no series at all — then no event can be "before the first
    /// bar" and every event takes part.
    pub first_bar_day: Option<NaiveDate>,
    pub facts: Vec<FactorFact>,
}

impl EntityFacts {
    /// Every fact's evidence digest, for the per-entity cache key.
    pub fn evidence_digests(&self) -> Vec<String> {
        self.facts
            .iter()
            .map(|fact| fact.evidence_digest.clone())
            .collect()
    }
}

/// The decision result: admit, or block the whole entity with one reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Every event that can affect the sequence is verified.
    Admitted {
        /// Events that took part in the decision.
        judged: usize,
        /// Events excluded because they precede the first bar or are
        /// `no_effect`.
        excluded: usize,
    },
    /// Whole-entity fail-closed (design §3.3 option (a)).
    Blocked {
        /// The stable reason code (§3.2).
        reason: BlockReason,
        /// How many events the reason applies to (never the event list itself:
        /// a variable-length array does not belong in an error envelope).
        blocking_events: usize,
        /// The categories of those events, ascending, deduplicated.
        categories: Vec<u8>,
    },
}

impl Verdict {
    pub fn blocked(reason: BlockReason, blocking_events: usize, categories: Vec<u8>) -> Self {
        Self::Blocked {
            reason,
            blocking_events,
            categories,
        }
    }

    pub fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted { .. })
    }

    /// The reason code when blocked (the wire `details.reason`).
    pub fn reason(&self) -> Option<BlockReason> {
        match self {
            Self::Admitted { .. } => None,
            Self::Blocked { reason, .. } => Some(*reason),
        }
    }

    /// Blocking event count when blocked (the wire `details.blocking_events`).
    pub fn blocking_events(&self) -> usize {
        match self {
            Self::Admitted { .. } => 0,
            Self::Blocked {
                blocking_events, ..
            } => *blocking_events,
        }
    }
}

/// Why a single factor cannot enter an exact chain. Mirrors the requirement of
/// the PR-A transform: a category-11 factor must be a positive rational and a
/// category-1 dividend must be finite; anything else must not be guessed.
pub fn unparsable_reason(fact: &FactorFact) -> Option<&'static str> {
    match fact.category {
        1 => {
            // A dividend is applied as `p - c1/10`; it only needs to be finite
            // and non-negative (a zero dividend with a different category would
            // be `no_effect`, but the record decides that, not this check).
            if fact.factors[0].is_sign_negative() && !fact.factors[0].is_zero() {
                return Some("category-1 c1 is negative");
            }
            None
        }
        11 => {
            let c3 = fact.factors[2];
            if c3.is_zero() {
                return Some("category-11 c3 is zero");
            }
            if c3.is_sign_negative() {
                return Some("category-11 c3 is negative");
            }
            None
        }
        _ => None,
    }
}

/// Decide one entity (design §3, D-92 Q1 (a)/Q10).
///
/// `checks` is the check set read from the source store; the function only
/// looks at records of `facts.entity`.
pub fn decide(facts: &EntityFacts, checks: &[EtfFactorCheck]) -> Verdict {
    let latest = latest_records(checks, &facts.entity);
    let mut blocking: Vec<(BlockReason, Vec<u8>)> = Vec::new();
    let mut judged = 0usize;
    let mut excluded = 0usize;

    // Group the events that can affect the sequence by effective day, because
    // the order rule (D-91 §2) is about one day's composition.
    let mut by_day: std::collections::BTreeMap<NaiveDate, Vec<&FactorFact>> =
        std::collections::BTreeMap::new();

    for fact in &facts.facts {
        if !matches!(fact.category, 1 | 11) {
            blocking.push((BlockReason::EventCategoryUnexpected, vec![fact.category]));
            continue;
        }
        if unparsable_reason(fact).is_some() {
            blocking.push((BlockReason::FactorUnparsable, vec![fact.category]));
            continue;
        }
        let record = latest.get(&(
            facts.entity.clone(),
            fact.ordinal,
            fact.evidence_digest.clone(),
        ));
        // "Before the first bar" excludes the event from judging only when the
        // record itself does not claim a verdict: D-91 §3 says such an event is
        // not part of the check set. A recorded `no_effect` excludes it too.
        let before_first_bar = facts
            .first_bar_day
            .is_some_and(|first| fact.effective_day < first);
        let no_effect = record.is_some_and(|record| record.check_status.is_no_effect());
        if before_first_bar || no_effect {
            excluded += 1;
            continue;
        }
        judged += 1;
        match record {
            None => blocking.push((BlockReason::FactorUnverified, vec![fact.category])),
            Some(record) if !record.check_status.admits() => {
                blocking.push((BlockReason::FactorUnverified, vec![fact.category]));
            }
            Some(_) => {}
        }
        by_day.entry(fact.effective_day).or_default().push(fact);
    }

    // One effective day with more than one event: D-91 §2 fixes "cat11 then
    // cat1", so every event of the group must be verified on its own, and a
    // record that exists but cannot anchor the composition blocks the entity.
    for (_day, group) in by_day {
        if group.len() < 2 {
            continue;
        }
        let mut verified = 0usize;
        let mut categories = Vec::new();
        let mut order_unverified = false;
        for fact in &group {
            categories.push(fact.category);
            match latest.get(&(
                facts.entity.clone(),
                fact.ordinal,
                fact.evidence_digest.clone(),
            )) {
                Some(record) if record.check_status.admits() => {
                    verified += 1;
                    if record.within_one_tick == Some(false) {
                        // The composition was compared and did not match the
                        // chain: this group's order is not anchored (B4).
                        order_unverified = true;
                    }
                }
                // A missing or non-admitting record already produced one
                // `FactorUnverified` in the per-event pass above; counting it
                // again here would report the same blocking event twice. Both
                // events of a day still have to be verified, which that pass
                // already enforces.
                _ => {}
            }
        }
        if order_unverified {
            blocking.push((BlockReason::EventOrderUnverified, categories));
        } else if verified < group.len() && verified == 0 {
            // No record of the group admits anything at all: the group is
            // unverified as a group too (design §3.1 B4, "合并结果无法锚定").
            blocking.push((BlockReason::FactorUnverified, categories));
        }
    }

    if blocking.is_empty() {
        return Verdict::Admitted { judged, excluded };
    }
    blocking.sort_by_key(|(reason, categories)| (reason.rank(), categories.clone()));
    let reason = blocking[0].0;
    let blocking_events = blocking.len();
    let mut categories: BTreeSet<u8> = BTreeSet::new();
    for (_, cats) in &blocking {
        categories.extend(cats.iter().copied());
    }
    Verdict::blocked(reason, blocking_events, categories.into_iter().collect())
}

/// Newest record per `(entity, ordinal, evidence_digest)`; the same
/// determinism rule the store reader uses (`checked_at`, then record sha).
fn latest_records<'a>(
    checks: &'a [EtfFactorCheck],
    entity: &str,
) -> std::collections::BTreeMap<(String, i64, String), &'a EtfFactorCheck> {
    let mut out: std::collections::BTreeMap<(String, i64, String), &'a EtfFactorCheck> =
        std::collections::BTreeMap::new();
    for record in checks.iter().filter(|record| record.entity == entity) {
        let key = record.key();
        match out.get(&key) {
            Some(existing) if existing.checked_at > record.checked_at => {}
            Some(existing)
                if existing.checked_at == record.checked_at
                    && existing.check_status <= record.check_status => {}
            _ => {
                out.insert(key, record);
            }
        }
    }
    out
}

/// `etf_qfq_not_built` for an entity the implementation has not built a forward
/// series for. Kept here, not in the caller, so the reason code has exactly one
/// home.
pub fn not_built() -> Verdict {
    Verdict::blocked(BlockReason::QfqNotBuilt, 0, Vec::new())
}

/// Whether a record set admits every fact of `facts`. Convenience wrapper for
/// the loader and the tests.
pub fn admits(facts: &EntityFacts, checks: &[EtfFactorCheck]) -> bool {
    decide(facts, checks).is_admitted()
}

#[cfg(test)]
mod tests;
