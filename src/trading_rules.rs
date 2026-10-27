//! Trading-rule data (issue-391 §8.1, separation point 6).
//!
//! The band, side, rounding and minimum tick are **data**, keyed by the
//! effective date and scoped by market × board × status class × listing phase.
//! This module owns the vocabulary, the wildcard matching and the invariants;
//! it contains no derivation and no amplitude constants. The derivation lives
//! in [`crate::research::price_limit`] and only ever reads a rule row.
//!
//! Two properties are load-bearing:
//!
//! - **Exactly one verified row** may match `(market, board, status, phase,
//!   day)`. Zero hits is `rule_missing`, two or more is `rule_ambiguous`;
//!   `basis_status='pending'` rows are recorded but never participate. There
//!   is no "take the latest" fallback (§8.1, §8.5).
//! - **A row's `basis_ref` is its provenance**: URL + clause + fetch time +
//!   sha256, and the owner-risk tier when the tier is not plain
//!   `APPROVED_SOURCE`. `basis_status` only distinguishes "admitted" from
//!   "known but not admitted yet"; the finer tier stays in the text so it
//!   cannot be mistaken for an approval.
//!
//! `price_limit_rules` and `min_tick_rules` are one dataset
//! (`market.price_limit_rules`, §3.2) even though they are two tables.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// `derived_dataset_versions.dataset_kind` / dataset name (§3.2).
pub const DATASET_KIND: &str = "price_limit_rules";
/// The single key of the rule dataset: rules are whole-market, not per entity.
pub const DATASET_KEY: &str = "price_limit_rules";
/// Label carried by the derivation (`algorithm_version`, §8.3).
pub const ALGORITHM_VERSION: &str = "limit-v1";
/// Digest algorithm of the rule dataset.
pub const DIGEST_FORMAT: &str = "trading-rules-v1";

/// The `market` column. `All` is the wildcard row the design allows; it is
/// matched explicitly, never treated as "the default".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleMarket {
    Sh,
    Sz,
    Bj,
    All,
}

impl RuleMarket {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sh => "sh",
            Self::Sz => "sz",
            Self::Bj => "bj",
            Self::All => "all",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "sh" => Self::Sh,
            "sz" => Self::Sz,
            "bj" => Self::Bj,
            "all" => Self::All,
            _ => return None,
        })
    }

    fn matches(self, query: Self) -> bool {
        self == Self::All || self == query
    }
}

/// The `board` column (BSE is reserved by the table but out of M2 scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleBoard {
    Main,
    Gem,
    Star,
    Bse,
    All,
}

impl RuleBoard {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Gem => "gem",
            Self::Star => "star",
            Self::Bse => "bse",
            Self::All => "all",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "main" => Self::Main,
            "gem" => Self::Gem,
            "star" => Self::Star,
            "bse" => Self::Bse,
            "all" => Self::All,
            _ => return None,
        })
    }

    fn matches(self, query: Self) -> bool {
        self == Self::All || self == query
    }
}

/// The `status_class` column (§8.1). `All` matches any risk state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusClass {
    Normal,
    St,
    StarSt,
    DelistingArrangement,
    All,
}

impl StatusClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::St => "st",
            Self::StarSt => "star_st",
            Self::DelistingArrangement => "delisting_arrangement",
            Self::All => "all",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "normal" => Self::Normal,
            "st" => Self::St,
            "star_st" => Self::StarSt,
            "delisting_arrangement" => Self::DelistingArrangement,
            "all" => Self::All,
            _ => return None,
        })
    }

    fn matches(self, query: Self) -> bool {
        self == Self::All || self == query
    }
}

/// The `listing_phase` column (§8.4). `All` matches any phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingPhase {
    FirstDay,
    First5Days,
    Seasoned,
    All,
}

impl ListingPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstDay => "first_day",
            Self::First5Days => "first_5_days",
            Self::Seasoned => "seasoned",
            Self::All => "all",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "first_day" => Self::FirstDay,
            "first_5_days" => Self::First5Days,
            "seasoned" => Self::Seasoned,
            "all" => Self::All,
            _ => return None,
        })
    }

    fn matches(self, query: Self) -> bool {
        self == Self::All || self == query
    }
}

/// `band_side` (§8.1). `None` is an explicit "no price limit", which is a
/// *known* answer; a missing rule is a different state (§8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandSide {
    Both,
    UpOnly,
    None,
}

impl BandSide {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::UpOnly => "up_only",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "both" => Self::Both,
            "up_only" => Self::UpOnly,
            "none" => Self::None,
            _ => return None,
        })
    }
}

/// How a computed limit price is rounded to the minimum tick (§8.3, R9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rounding {
    HalfUp,
    Floor,
    Ceil,
}

impl Rounding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HalfUp => "half_up",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "half_up" => Self::HalfUp,
            "floor" => Self::Floor,
            "ceil" => Self::Ceil,
            _ => return None,
        })
    }
}

/// Whether the rule's text is admitted. `pending` rows are provenance notes;
/// they never select a rule (§8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasisStatus {
    Verified,
    Pending,
}

impl BasisStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Pending => "pending",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "verified" => Self::Verified,
            "pending" => Self::Pending,
            _ => return None,
        })
    }
}

/// One `price_limit_rules` row (§8.1). `effective_to` is exclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceLimitRule {
    pub rule_id: String,
    pub market: RuleMarket,
    pub board: RuleBoard,
    pub status_class: StatusClass,
    pub listing_phase: ListingPhase,
    pub band_bp: Option<i64>,
    pub band_side: BandSide,
    pub rounding: Rounding,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub basis_ref: String,
    pub basis_status: BasisStatus,
}

impl PriceLimitRule {
    pub fn covers(&self, day: NaiveDate) -> bool {
        self.effective_from <= day && self.effective_to.is_none_or(|to| day < to)
    }

    fn matches(
        &self,
        market: RuleMarket,
        board: RuleBoard,
        status: StatusClass,
        phase: ListingPhase,
        day: NaiveDate,
    ) -> bool {
        self.market.matches(market)
            && self.board.matches(board)
            && self.status_class.matches(status)
            && self.listing_phase.matches(phase)
            && self.covers(day)
    }

    fn validate(&self) -> Result<(), RuleError> {
        if self.rule_id.trim().is_empty() {
            return Err(RuleError::Invalid("rule_id is empty".into()));
        }
        if self.basis_ref.trim().is_empty() {
            return Err(RuleError::Invalid(format!(
                "{} has an empty basis_ref",
                self.rule_id
            )));
        }
        if self
            .effective_to
            .is_some_and(|to| to <= self.effective_from)
        {
            return Err(RuleError::Invalid(format!(
                "{} has a non-positive effective interval",
                self.rule_id
            )));
        }
        match (self.band_side, self.band_bp) {
            (BandSide::None, None) => {}
            (BandSide::None, Some(_)) => {
                return Err(RuleError::Invalid(format!(
                    "{} has band_side=none and a band",
                    self.rule_id
                )));
            }
            (_, None) => {
                return Err(RuleError::Invalid(format!(
                    "{} is bounded but has no band_bp",
                    self.rule_id
                )));
            }
            (_, Some(bp)) if bp < 0 => {
                return Err(RuleError::Invalid(format!(
                    "{} has a negative band_bp",
                    self.rule_id
                )));
            }
            (_, Some(_)) => {}
        }
        Ok(())
    }

    fn scope_key(&self) -> (RuleMarket, RuleBoard, StatusClass, ListingPhase) {
        (
            self.market,
            self.board,
            self.status_class,
            self.listing_phase,
        )
    }
}

/// One `min_tick_rules` row (§8.1). `tick_units` is expressed in
/// `10^-price_scale` units, so the row itself says what one tick is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinTickRule {
    pub rule_id: String,
    pub market: RuleMarket,
    pub board: RuleBoard,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub tick_units: i64,
    pub price_scale: u32,
    pub basis_ref: String,
    pub basis_status: BasisStatus,
}

impl MinTickRule {
    pub fn covers(&self, day: NaiveDate) -> bool {
        self.effective_from <= day && self.effective_to.is_none_or(|to| day < to)
    }

    fn matches(&self, market: RuleMarket, board: RuleBoard, day: NaiveDate) -> bool {
        self.market.matches(market) && self.board.matches(board) && self.covers(day)
    }

    fn validate(&self) -> Result<(), RuleError> {
        if self.rule_id.trim().is_empty() {
            return Err(RuleError::Invalid("rule_id is empty".into()));
        }
        if self.basis_ref.trim().is_empty() {
            return Err(RuleError::Invalid(format!(
                "{} has an empty basis_ref",
                self.rule_id
            )));
        }
        if self.tick_units <= 0 {
            return Err(RuleError::Invalid(format!(
                "{} has a non-positive tick",
                self.rule_id
            )));
        }
        if self.price_scale > 6 {
            return Err(RuleError::Invalid(format!(
                "{} has an unsupported price_scale",
                self.rule_id
            )));
        }
        if self
            .effective_to
            .is_some_and(|to| to <= self.effective_from)
        {
            return Err(RuleError::Invalid(format!(
                "{} has a non-positive effective interval",
                self.rule_id
            )));
        }
        Ok(())
    }
}

/// The whole rule dataset. Read and written as one unit so a partial update
/// cannot pair today's band with yesterday's tick.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleBook {
    pub price_limits: Vec<PriceLimitRule>,
    pub min_ticks: Vec<MinTickRule>,
}

/// The outcome of one rule lookup. `Pending` is not `Missing`: the second says
/// "no rule exists", the first says "a rule exists but its text is not
/// admitted", and the design requires different reason codes (§8.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleLookup<'a, T> {
    Found(&'a T),
    Pending,
    Missing,
    Ambiguous(usize),
}

impl RuleBook {
    /// Structural validation used by the writer and by the admitted dataset
    /// loader. Overlap between *verified* rows of the same exact scope is a
    /// configuration error; wildcard overlap is left to the read model, which
    /// must fail closed with `rule_ambiguous` anyway (§8.1).
    pub fn validate(&self) -> Result<(), RuleError> {
        let mut ids = BTreeSet::new();
        for rule in &self.price_limits {
            rule.validate()?;
            if !ids.insert(rule.rule_id.as_str()) {
                return Err(RuleError::Invalid(format!(
                    "duplicate price-limit rule_id {}",
                    rule.rule_id
                )));
            }
        }
        let mut tick_ids = BTreeSet::new();
        for rule in &self.min_ticks {
            rule.validate()?;
            if !tick_ids.insert(rule.rule_id.as_str()) {
                return Err(RuleError::Invalid(format!(
                    "duplicate min-tick rule_id {}",
                    rule.rule_id
                )));
            }
        }
        self.check_verified_overlaps()?;
        Ok(())
    }

    fn check_verified_overlaps(&self) -> Result<(), RuleError> {
        type Scope = (RuleMarket, RuleBoard, StatusClass, ListingPhase);
        let mut by_scope: std::collections::BTreeMap<Scope, Vec<&PriceLimitRule>> =
            std::collections::BTreeMap::new();
        for rule in &self.price_limits {
            if rule.basis_status == BasisStatus::Verified {
                by_scope.entry(rule.scope_key()).or_default().push(rule);
            }
        }
        for (_, mut rules) in by_scope {
            rules.sort_by_key(|rule| rule.effective_from);
            for pair in rules.windows(2) {
                let previous = pair[0];
                let next = pair[1];
                if previous
                    .effective_to
                    .is_none_or(|to| next.effective_from < to)
                {
                    return Err(RuleError::Invalid(format!(
                        "verified rules {} and {} overlap for the same scope",
                        previous.rule_id, next.rule_id
                    )));
                }
            }
        }
        Ok(())
    }

    /// `price_limit_rules` lookup (§8.1). Wildcards (`all`) are alternatives,
    /// not defaults: a specific row and a wildcard row both matching is
    /// `Ambiguous`.
    pub fn price_rule(
        &self,
        market: RuleMarket,
        board: RuleBoard,
        status: StatusClass,
        phase: ListingPhase,
        day: NaiveDate,
    ) -> RuleLookup<'_, PriceLimitRule> {
        let mut verified = Vec::new();
        let mut pending = 0usize;
        for rule in &self.price_limits {
            if !rule.matches(market, board, status, phase, day) {
                continue;
            }
            match rule.basis_status {
                BasisStatus::Verified => verified.push(rule),
                BasisStatus::Pending => pending += 1,
            }
        }
        match (verified.len(), pending) {
            (1, _) => RuleLookup::Found(verified[0]),
            (0, 0) => RuleLookup::Missing,
            (0, _) => RuleLookup::Pending,
            (count, _) => RuleLookup::Ambiguous(count),
        }
    }

    /// `min_tick_rules` lookup. The market dimension is part of the key
    /// because B-share ticks differ between exchanges (#395 §3 R8).
    pub fn min_tick(
        &self,
        market: RuleMarket,
        board: RuleBoard,
        day: NaiveDate,
    ) -> RuleLookup<'_, MinTickRule> {
        let mut verified = Vec::new();
        let mut pending = 0usize;
        for rule in &self.min_ticks {
            if !rule.matches(market, board, day) {
                continue;
            }
            match rule.basis_status {
                BasisStatus::Verified => verified.push(rule),
                BasisStatus::Pending => pending += 1,
            }
        }
        match (verified.len(), pending) {
            (1, _) => RuleLookup::Found(verified[0]),
            (0, 0) => RuleLookup::Missing,
            (0, _) => RuleLookup::Pending,
            (count, _) => RuleLookup::Ambiguous(count),
        }
    }
}

/// Rule-data failures. They are distinct from the read model's `unknown`
/// answers: these reject the dataset, not one query.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuleError {
    #[error("RULE_INVALID: {0}")]
    Invalid(String),
    #[error("RULE_JSON: {0}")]
    Json(String),
}

impl RuleError {
    pub fn json(error: impl std::fmt::Display) -> Self {
        Self::Json(error.to_string())
    }
}

#[cfg(test)]
mod tests;
