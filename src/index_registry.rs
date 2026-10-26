//! M1 index benchmark registry (issue-383 §6, §12 scenario 1).
//!
//! This is the **one** place the benchmark's member list lives: nine
//! exchange-system wide-base series (Q1/D-31 includes the four CSI-published
//! broad indices). Adding a benchmark later is one row here plus one
//! `index_series_meta` row — no new table, no new source adapter, no
//! `if entity == ...` branch anywhere (separation points 1 and 6).
//!
//! The registry is neutral data about *which* series are benchmarks and *how
//! their volume/amount were aggregated*; it never names a source file, a URL
//! or a vendor. Unit and aggregation scope are data (BR-M1-6): the storage
//! layer writes them into `index_series_meta`, so a reader can answer
//! "is this volume in 手 or 股?" without grepping code.
//!
//! Deliberately not registered: `sh.880xxx`/`sh.887xxx` (TDX-invented board
//! indices, not exchange/CSI published) and `sz.399005`/`sz.399303` (optional
//! later additions, one row each when needed).

/// `index_series_meta.price_unit` of an index level: 0.01 点 (`price_scale=2`).
pub const PRICE_UNIT_INDEX_POINT: &str = "index_point";
/// `index_series_meta.volume_unit` of an index: 手 (100 shares), **not** 股.
pub const VOLUME_UNIT_HAND: &str = "hand";
/// `index_series_meta.amount_unit` of an index: 元 (f32-derived decimal).
pub const AMOUNT_UNIT_YUAN: &str = "yuan";

/// How an index's volume/amount are aggregated (§6.2). The value is stored
/// per index; the code only enumerates the vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateScope {
    /// 成分股合计 — the index is computed over a fixed constituent list.
    Constituents,
    /// 全市场合计 — e.g. the Shanghai composite covers the whole exchange.
    WholeMarket,
    /// 全板块合计 — e.g. the ChiNext index covers one board.
    WholeBoard,
}

impl AggregateScope {
    /// Stored text (`index_series_meta.aggregate_scope`, §3.9).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Constituents => "constituents",
            Self::WholeMarket => "whole_market",
            Self::WholeBoard => "whole_board",
        }
    }

    /// Exact inverse of [`Self::as_str`]; `None` for values the DDL does not
    /// define, so a reader fails closed instead of coercing.
    pub fn from_str_exact(value: &str) -> Option<Self> {
        match value {
            "constituents" => Some(Self::Constituents),
            "whole_market" => Some(Self::WholeMarket),
            "whole_board" => Some(Self::WholeBoard),
            _ => None,
        }
    }
}

/// One registered benchmark series: identity, alternate codes, and the
/// unit/scope claims that must be provable before the index may be stored
/// (AC-M1-6: "无证据的指数不收录").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexRegistration {
    /// Canonical `entity` of the stored series (`sh.000300`).
    pub entity: &'static str,
    /// Alternate codes of the *same* series (e.g. `sz.399300`); the other
    /// code is not stored twice, so one series keeps one `version_id`.
    pub aliases: &'static [&'static str],
    pub aggregate_scope: AggregateScope,
    /// Constituent count where the index is constituent-based; `None` for
    /// whole-market/board aggregates (§6.1).
    pub member_count: Option<i64>,
    pub price_unit: &'static str,
    pub volume_unit: &'static str,
    pub amount_unit: &'static str,
    /// How the unit/scope claim was proven — the same text lands in
    /// `index_series_meta.evidence_note`. Empty = not collectible.
    pub evidence_note: &'static str,
}

impl IndexRegistration {
    /// `aliases` as stored: comma-separated, no spaces, registration order.
    /// Empty string when the series has no alternate code.
    pub fn aliases_text(&self) -> String {
        self.aliases.join(",")
    }

    /// Parse a stored `aliases` value back to codes (empty entries dropped).
    /// Exact inverse of [`Self::aliases_text`] for canonical input.
    pub fn parse_aliases(text: &str) -> Vec<&str> {
        text.split(',').filter(|code| !code.is_empty()).collect()
    }

    /// The evidence gate: a registration without a unit/scope proof may not
    /// be materialized, and a constituent-based index must name its
    /// constituent count (§6.1, AC-M1-6).
    pub fn require_evidence(&self) -> Result<(), String> {
        if self.evidence_note.trim().is_empty() {
            return Err(format!(
                "index {} has no unit/scope evidence; not collectible",
                self.entity
            ));
        }
        if self.aggregate_scope == AggregateScope::Constituents && self.member_count.is_none() {
            return Err(format!(
                "constituent index {} does not declare its member count",
                self.entity
            ));
        }
        if self.member_count.is_some_and(|count| count <= 0) {
            return Err(format!(
                "index {} has a non-positive member count",
                self.entity
            ));
        }
        Ok(())
    }
}

const CONSTITUENT_NOTE_PREFIX: &str = "成分股合计口径：官方指数接口 consNumber 与成分数一致；\
     OHLC 按 0.01 点与官方接口逐点精确匹配（本次取证含中证 96 个 OHLC 点）；\
     volume 为手：官方 tradingVol(股) ÷ .day vol = 100.0000（24/24）；\
     amount 为元：官方 tradingValue(亿元)×1e8 ÷ .day amount ≈ 0.999999–1.000006。";
const WHOLE_MARKET_NOTE: &str = "全市场合计口径：上证/深证官方接口的成交量本身以手发布，\
     .day vol 比值 1.0000000；OHLC 按 0.01 点与官方接口逐点精确匹配（本次取证含上证 12 个 OHLC 点）；\
     amount 为元：官方 tradingValue(亿元)×1e8 ÷ .day amount ≈ 0.999999–1.000006。";

/// The nine M1 benchmark series (§6.1). Order is the registration order and
/// is not semantically meaningful; `lookup` is by `entity`.
pub const INDEX_SERIES: [IndexRegistration; 9] = [
    IndexRegistration {
        entity: "sh.000001",
        aliases: &[],
        aggregate_scope: AggregateScope::WholeMarket,
        member_count: None,
        price_unit: PRICE_UNIT_INDEX_POINT,
        volume_unit: VOLUME_UNIT_HAND,
        amount_unit: AMOUNT_UNIT_YUAN,
        // 上证综指 covers the whole Shanghai market (B shares included).
        evidence_note: WHOLE_MARKET_NOTE,
    },
    IndexRegistration {
        entity: "sz.399001",
        aliases: &[],
        aggregate_scope: AggregateScope::WholeMarket,
        member_count: None,
        price_unit: PRICE_UNIT_INDEX_POINT,
        volume_unit: VOLUME_UNIT_HAND,
        amount_unit: AMOUNT_UNIT_YUAN,
        evidence_note: WHOLE_MARKET_NOTE,
    },
    IndexRegistration {
        entity: "sz.399006",
        aliases: &[],
        aggregate_scope: AggregateScope::WholeBoard,
        member_count: None,
        price_unit: PRICE_UNIT_INDEX_POINT,
        volume_unit: VOLUME_UNIT_HAND,
        amount_unit: AMOUNT_UNIT_YUAN,
        // 创业板指 covers the whole ChiNext board.
        evidence_note: WHOLE_MARKET_NOTE,
    },
    IndexRegistration {
        entity: "sh.000016",
        aliases: &[],
        aggregate_scope: AggregateScope::Constituents,
        member_count: Some(50),
        price_unit: PRICE_UNIT_INDEX_POINT,
        volume_unit: VOLUME_UNIT_HAND,
        amount_unit: AMOUNT_UNIT_YUAN,
        evidence_note: CONSTITUENT_NOTE_PREFIX,
    },
    IndexRegistration {
        entity: "sh.000300",
        aliases: &["sz.399300"],
        aggregate_scope: AggregateScope::Constituents,
        member_count: Some(300),
        price_unit: PRICE_UNIT_INDEX_POINT,
        volume_unit: VOLUME_UNIT_HAND,
        amount_unit: AMOUNT_UNIT_YUAN,
        evidence_note: CONSTITUENT_NOTE_PREFIX,
    },
    IndexRegistration {
        entity: "sh.000905",
        aliases: &["sz.399905"],
        aggregate_scope: AggregateScope::Constituents,
        member_count: Some(500),
        price_unit: PRICE_UNIT_INDEX_POINT,
        volume_unit: VOLUME_UNIT_HAND,
        amount_unit: AMOUNT_UNIT_YUAN,
        evidence_note: CONSTITUENT_NOTE_PREFIX,
    },
    IndexRegistration {
        entity: "sh.000852",
        aliases: &[],
        aggregate_scope: AggregateScope::Constituents,
        member_count: Some(1000),
        price_unit: PRICE_UNIT_INDEX_POINT,
        volume_unit: VOLUME_UNIT_HAND,
        amount_unit: AMOUNT_UNIT_YUAN,
        evidence_note: CONSTITUENT_NOTE_PREFIX,
    },
    IndexRegistration {
        entity: "sh.000906",
        aliases: &[],
        aggregate_scope: AggregateScope::Constituents,
        member_count: Some(800),
        price_unit: PRICE_UNIT_INDEX_POINT,
        volume_unit: VOLUME_UNIT_HAND,
        amount_unit: AMOUNT_UNIT_YUAN,
        evidence_note: CONSTITUENT_NOTE_PREFIX,
    },
    IndexRegistration {
        entity: "sh.000688",
        aliases: &[],
        aggregate_scope: AggregateScope::Constituents,
        member_count: Some(50),
        price_unit: PRICE_UNIT_INDEX_POINT,
        volume_unit: VOLUME_UNIT_HAND,
        amount_unit: AMOUNT_UNIT_YUAN,
        evidence_note: CONSTITUENT_NOTE_PREFIX,
    },
];

/// The registration of `entity`, or `None` when it is not a benchmark.
pub fn lookup(entity: &str) -> Option<&'static IndexRegistration> {
    INDEX_SERIES.iter().find(|entry| entry.entity == entity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_nine_unique_canonical_series() {
        let mut seen = std::collections::BTreeSet::new();
        for entry in &INDEX_SERIES {
            assert!(
                crate::compact_market::canonical_entity(entry.entity).is_ok(),
                "{}",
                entry.entity
            );
            assert!(seen.insert(entry.entity), "duplicate {}", entry.entity);
            entry.require_evidence().unwrap();
            assert_eq!(entry.price_unit, PRICE_UNIT_INDEX_POINT);
            assert_eq!(entry.volume_unit, VOLUME_UNIT_HAND);
            assert_eq!(entry.amount_unit, AMOUNT_UNIT_YUAN);
            // The alternate code must not itself be a registered entity:
            // one series, one row, one version id.
            for alias in entry.aliases {
                assert!(crate::compact_market::canonical_entity(alias).is_ok());
                assert!(lookup(alias).is_none(), "{alias} registered twice");
            }
            // Aliases round-trip through the stored text form.
            assert_eq!(
                IndexRegistration::parse_aliases(&entry.aliases_text()),
                entry.aliases
            );
        }
        assert_eq!(INDEX_SERIES.len(), 9);
        // Q1/D-31: the four CSI-published broad indices stay in the benchmark.
        for entity in ["sh.000300", "sh.000905", "sh.000852", "sh.000906"] {
            assert_eq!(
                lookup(entity).unwrap().aggregate_scope,
                AggregateScope::Constituents
            );
        }
        // Explicitly excluded TDX-invented board indices (§6.1).
        for entity in ["sh.880001", "sh.887001", "sz.399005", "sz.399303"] {
            assert!(lookup(entity).is_none(), "{entity} must not be registered");
        }
    }

    #[test]
    fn missing_evidence_or_member_count_is_rejected() {
        let mut entry = INDEX_SERIES[0];
        entry.evidence_note = "  ";
        assert!(entry.require_evidence().is_err());
        let mut entry = INDEX_SERIES[4];
        entry.member_count = None;
        assert!(entry.require_evidence().is_err());
        let mut entry = INDEX_SERIES[4];
        entry.member_count = Some(0);
        assert!(entry.require_evidence().is_err());
    }

    #[test]
    fn aggregate_scope_text_round_trips() {
        for scope in [
            AggregateScope::Constituents,
            AggregateScope::WholeMarket,
            AggregateScope::WholeBoard,
        ] {
            assert_eq!(AggregateScope::from_str_exact(scope.as_str()), Some(scope));
        }
        assert_eq!(AggregateScope::from_str_exact("all_market"), None);
    }
}
