//! `price_limit_rules` / `min_tick_rules` read/write (issue-391 §16 PR-5).
//!
//! Layer position (separation point 6): the tables are data, so this module
//! is the storage adapter. It never interprets a band and never decides which
//! row applies — that is [`crate::trading_rules::RuleBook`] (selection) and
//! [`crate::research::price_limit`] (derivation). The writer replaces both
//! tables atomically and registers one `price_limit_rules` dataset version,
//! because §3.2 defines the band rules and the minimum tick as one dataset.
//!
//! Whole-dataset replace matches the other M2 writers (`security_master`,
//! `capital_events`): a rule removed from the admitted set disappears from the
//! table instead of surviving as stale data. An empty set removes both the
//! rows and the registration, so "no rules" is distinguishable from "never
//! written" without a sentinel.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::derived::{self, DerivedVersionRow};
use super::{evidence_store, integrity, series_commit, storage};
use crate::compact_market::{self, Digest, EvidenceKind};
use crate::market_storage::Error;
use crate::trading_rules::{
    self, ALGORITHM_VERSION, BandSide, BasisStatus, DATASET_KIND, DIGEST_FORMAT, ListingPhase,
    MinTickRule, PriceLimitRule, Rounding, RuleBoard, RuleBook, RuleError, RuleMarket, StatusClass,
};

fn rule_error(error: RuleError) -> Error {
    match error {
        RuleError::Invalid(message) | RuleError::Json(message) => integrity(message),
    }
}

fn push_text(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn push_opt_i64(out: &mut Vec<u8>, value: Option<i64>) {
    match value {
        Some(value) => {
            out.push(1);
            out.extend_from_slice(&value.to_be_bytes());
        }
        None => out.push(0),
    }
}

fn push_day(out: &mut Vec<u8>, day: NaiveDate) {
    out.extend_from_slice(&compact_market::epoch_day(day).to_be_bytes());
}

fn push_opt_day(out: &mut Vec<u8>, day: Option<NaiveDate>) {
    match day {
        Some(day) => {
            out.push(1);
            push_day(out, day);
        }
        None => out.push(0),
    }
}

/// Content identity of the rule dataset (§3.10). The digest covers every
/// column that changes an answer: scope, band, the `min_band_ticks` floor,
/// side, rounding, interval and basis. `basis_status` is included because a
/// pending row becoming verified changes which rule the derivation may use.
fn rules_digest(book: &RuleBook) -> Digest {
    let mut payload = Vec::with_capacity(128 + book.price_limits.len() * 96);
    payload.extend_from_slice(b"trading-rules-v1\0");
    payload.extend_from_slice(
        &u64::try_from(book.price_limits.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    let mut limits: Vec<&PriceLimitRule> = book.price_limits.iter().collect();
    limits.sort_by(|left, right| left.rule_id.cmp(&right.rule_id));
    for rule in limits {
        push_text(&mut payload, &rule.rule_id);
        push_text(&mut payload, rule.market.as_str());
        push_text(&mut payload, rule.board.as_str());
        push_text(&mut payload, rule.status_class.as_str());
        push_text(&mut payload, rule.listing_phase.as_str());
        push_opt_i64(&mut payload, rule.band_bp);
        push_opt_i64(&mut payload, rule.min_band_ticks);
        push_text(&mut payload, rule.band_side.as_str());
        push_text(&mut payload, rule.rounding.as_str());
        push_day(&mut payload, rule.effective_from);
        push_opt_day(&mut payload, rule.effective_to);
        push_text(&mut payload, &rule.basis_ref);
        push_text(&mut payload, rule.basis_status.as_str());
    }
    payload.extend_from_slice(
        &u64::try_from(book.min_ticks.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    let mut ticks: Vec<&MinTickRule> = book.min_ticks.iter().collect();
    ticks.sort_by(|left, right| left.rule_id.cmp(&right.rule_id));
    for rule in ticks {
        push_text(&mut payload, &rule.rule_id);
        push_text(&mut payload, rule.market.as_str());
        push_text(&mut payload, rule.board.as_str());
        push_day(&mut payload, rule.effective_from);
        push_opt_day(&mut payload, rule.effective_to);
        payload.extend_from_slice(&rule.tick_units.to_be_bytes());
        payload.extend_from_slice(&rule.price_scale.to_be_bytes());
        push_text(&mut payload, &rule.basis_ref);
        push_text(&mut payload, rule.basis_status.as_str());
    }
    Sha256::digest(&payload).into()
}

/// Read the whole rule dataset. The read model calls this once per query so a
/// single answer cannot see the price rules from one write and the tick rules
/// from another.
pub(crate) fn read_trading_rules(c: &Connection) -> Result<RuleBook, Error> {
    let mut price_statement = c
        .prepare(
            "SELECT rule_id,market,board,status_class,listing_phase,band_bp,min_band_ticks,\
             band_side,rounding,effective_from,effective_to,basis_ref,basis_status \
             FROM price_limit_rules ORDER BY rule_id",
        )
        .map_err(storage)?;
    let price_rows = price_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, String>(12)?,
            ))
        })
        .map_err(storage)?;
    let mut price_limits = Vec::new();
    for row in price_rows {
        let (
            rule_id,
            market,
            board,
            status_class,
            listing_phase,
            band_bp,
            min_band_ticks,
            band_side,
            rounding,
            effective_from,
            effective_to,
            basis_ref,
            basis_status,
        ) = row.map_err(storage)?;
        price_limits.push(PriceLimitRule {
            rule_id,
            market: RuleMarket::parse(&market)
                .ok_or_else(|| integrity("price_limit_rules market is unregistered"))?,
            board: RuleBoard::parse(&board)
                .ok_or_else(|| integrity("price_limit_rules board is unregistered"))?,
            status_class: StatusClass::parse(&status_class)
                .ok_or_else(|| integrity("price_limit_rules status_class is unregistered"))?,
            listing_phase: ListingPhase::parse(&listing_phase)
                .ok_or_else(|| integrity("price_limit_rules listing_phase is unregistered"))?,
            band_bp,
            min_band_ticks,
            band_side: BandSide::parse(&band_side)
                .ok_or_else(|| integrity("price_limit_rules band_side is unregistered"))?,
            rounding: Rounding::parse(&rounding)
                .ok_or_else(|| integrity("price_limit_rules rounding is unregistered"))?,
            effective_from: read_day(effective_from)?,
            effective_to: read_opt_day(effective_to)?,
            basis_ref,
            basis_status: BasisStatus::parse(&basis_status)
                .ok_or_else(|| integrity("price_limit_rules basis_status is unregistered"))?,
        });
    }

    let mut tick_statement = c
        .prepare(
            "SELECT rule_id,market,board,effective_from,effective_to,tick_units,\
             price_scale,basis_ref,basis_status FROM min_tick_rules ORDER BY rule_id",
        )
        .map_err(storage)?;
    let tick_rows = tick_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })
        .map_err(storage)?;
    let mut min_ticks = Vec::new();
    for row in tick_rows {
        let (
            rule_id,
            market,
            board,
            effective_from,
            effective_to,
            tick_units,
            price_scale,
            basis_ref,
            basis_status,
        ) = row.map_err(storage)?;
        min_ticks.push(MinTickRule {
            rule_id,
            market: RuleMarket::parse(&market)
                .ok_or_else(|| integrity("min_tick_rules market is unregistered"))?,
            board: RuleBoard::parse(&board)
                .ok_or_else(|| integrity("min_tick_rules board is unregistered"))?,
            effective_from: read_day(effective_from)?,
            effective_to: read_opt_day(effective_to)?,
            tick_units,
            price_scale: u32::try_from(price_scale)
                .map_err(|_| integrity("min_tick_rules price_scale is negative"))?,
            basis_ref,
            basis_status: BasisStatus::parse(&basis_status)
                .ok_or_else(|| integrity("min_tick_rules basis_status is unregistered"))?,
        });
    }
    Ok(RuleBook {
        price_limits,
        min_ticks,
    })
}

/// Replace the whole rule dataset and register its version. `None` means the
/// set was empty and both the rows and the registration were removed.
pub(crate) fn replace_trading_rules(
    c: &Connection,
    book: &RuleBook,
    source_binding_digest: Digest,
    task_id: &str,
    item_id: &str,
) -> Result<Option<DerivedVersionRow>, Error> {
    book.validate().map_err(rule_error)?;
    if book.price_limits.is_empty() != book.min_ticks.is_empty() {
        return Err(integrity(
            "the rule dataset must carry both price-limit and min-tick rows",
        ));
    }
    c.execute("DELETE FROM price_limit_rules", [])
        .map_err(storage)?;
    c.execute("DELETE FROM min_tick_rules", [])
        .map_err(storage)?;
    if book.price_limits.is_empty() {
        c.execute(
            "DELETE FROM derived_dataset_versions WHERE dataset_kind=?1 AND dataset_key=?2",
            params![DATASET_KIND, trading_rules::DATASET_KEY],
        )
        .map_err(storage)?;
        return Ok(None);
    }

    let digest = rules_digest(book);
    let evidence = json!({
        "dataset": DATASET_KIND,
        "dataset_key": trading_rules::DATASET_KEY,
        "digest_format": DIGEST_FORMAT,
        "algorithm_version": ALGORITHM_VERSION,
        "content_digest": compact_market::digest_hex(&digest),
        "price_limit_rule_count": book.price_limits.len(),
        "min_tick_rule_count": book.min_ticks.len(),
        "price_limit_rule_ids": book.price_limits.iter().map(|rule| rule.rule_id.as_str()).collect::<Vec<_>>(),
        "min_tick_rule_ids": book.min_ticks.iter().map(|rule| rule.rule_id.as_str()).collect::<Vec<_>>(),
        "pending_price_limit_rule_ids": book.price_limits.iter()
            .filter(|rule| rule.basis_status == BasisStatus::Pending)
            .map(|rule| rule.rule_id.as_str())
            .collect::<Vec<_>>(),
        // Rules are facts keyed by effective date; the tables have no
        // `known_at` column (§8.1), so a point-in-time `known_at` filter cannot
        // bound this dataset. Stated here so the limitation travels with the
        // data instead of being implied by the read-model defaults.
        "known_at": "not_applicable",
        "known_at_reason": "price_limit_rules/min_tick_rules carry an effective date and \
            basis evidence only; the DDL has no known_at column, so a query-time known_at \
            filter cannot exclude a rule that became knowable later",
    });
    let evidence_digest = evidence_store::put_evidence(c, EvidenceKind::Derived, &evidence)?;
    let now = chrono::Utc::now().to_rfc3339();

    let mut ids = BTreeSet::new();
    for rule in &book.price_limits {
        if !ids.insert(rule.rule_id.as_str()) {
            return Err(integrity("duplicate price-limit rule_id after validation"));
        }
        c.execute(
            "INSERT INTO price_limit_rules(rule_id,market,board,status_class,listing_phase,\
             band_bp,min_band_ticks,band_side,rounding,effective_from,effective_to,basis_ref,\
             basis_status,evidence_digest) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                rule.rule_id,
                rule.market.as_str(),
                rule.board.as_str(),
                rule.status_class.as_str(),
                rule.listing_phase.as_str(),
                rule.band_bp,
                rule.min_band_ticks,
                rule.band_side.as_str(),
                rule.rounding.as_str(),
                compact_market::epoch_day(rule.effective_from),
                rule.effective_to.map(compact_market::epoch_day),
                rule.basis_ref,
                rule.basis_status.as_str(),
                evidence_digest.as_slice(),
            ],
        )
        .map_err(storage)?;
    }
    let mut tick_ids = BTreeSet::new();
    for rule in &book.min_ticks {
        if !tick_ids.insert(rule.rule_id.as_str()) {
            return Err(integrity("duplicate min-tick rule_id after validation"));
        }
        c.execute(
            "INSERT INTO min_tick_rules(rule_id,market,board,effective_from,effective_to,\
             tick_units,price_scale,basis_ref,basis_status,evidence_digest) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                rule.rule_id,
                rule.market.as_str(),
                rule.board.as_str(),
                compact_market::epoch_day(rule.effective_from),
                rule.effective_to.map(compact_market::epoch_day),
                rule.tick_units,
                i64::from(rule.price_scale),
                rule.basis_ref,
                rule.basis_status.as_str(),
                evidence_digest.as_slice(),
            ],
        )
        .map_err(storage)?;
    }

    let first_day = book
        .price_limits
        .iter()
        .map(|rule| rule.effective_from)
        .chain(book.min_ticks.iter().map(|rule| rule.effective_from))
        .min()
        .ok_or_else(|| integrity("non-empty rule set checked above"))?;
    let last_day = book
        .price_limits
        .iter()
        .map(|rule| rule.effective_to.unwrap_or(rule.effective_from))
        .chain(
            book.min_ticks
                .iter()
                .map(|rule| rule.effective_to.unwrap_or(rule.effective_from)),
        )
        .max()
        .ok_or_else(|| integrity("non-empty rule set checked above"))?
        .max(first_day);
    let record_count = i64::try_from(book.price_limits.len() + book.min_ticks.len())
        .map_err(|_| integrity("rule row count overflow"))?;
    let version_id = compact_market::digest_hex(&digest);
    c.execute(
        "INSERT INTO derived_dataset_versions(dataset_kind,dataset_key,version_id,\
         content_digest,digest_format,record_count,first_day,last_day,algorithm_version,\
         source_binding_digest,evidence_digest,task_id,item_id,created_at,updated_at) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14) \
         ON CONFLICT(dataset_kind,dataset_key) DO UPDATE SET \
         version_id=excluded.version_id,content_digest=excluded.content_digest,\
         digest_format=excluded.digest_format,record_count=excluded.record_count,\
         first_day=excluded.first_day,last_day=excluded.last_day,\
         algorithm_version=excluded.algorithm_version,\
         source_binding_digest=excluded.source_binding_digest,\
         evidence_digest=excluded.evidence_digest,task_id=excluded.task_id,\
         item_id=excluded.item_id,updated_at=excluded.updated_at",
        params![
            DATASET_KIND,
            trading_rules::DATASET_KEY,
            version_id,
            digest.as_slice(),
            DIGEST_FORMAT,
            record_count,
            compact_market::epoch_day(first_day),
            compact_market::epoch_day(last_day),
            ALGORITHM_VERSION,
            source_binding_digest.as_slice(),
            evidence_digest.as_slice(),
            task_id,
            item_id,
            now,
        ],
    )
    .map_err(storage)?;
    derived::read_version(c, DATASET_KIND, trading_rules::DATASET_KEY)?
        .map(Some)
        .ok_or_else(|| integrity("rule registration vanished after its insert"))
}

fn read_day(value: i64) -> Result<NaiveDate, Error> {
    compact_market::epoch_date(value).map_err(integrity)
}

fn read_opt_day(value: Option<i64>) -> Result<Option<NaiveDate>, Error> {
    value.map(read_day).transpose()
}

/// One whole-dataset rule commit driven by the daily `derived:trading_rules`
/// item (issue-391 §12.1). The book is repository data admitted by D-44, not
/// a generation input; the generation binding only records which run first
/// wrote this content, so a content-identical run leaves it alone.
pub(crate) struct TradingRulesWrite<'a> {
    pub book: &'a RuleBook,
    pub task_id: &'a str,
    pub item_id: &'a str,
    pub generation_id: &'a str,
    pub generation_manifest_sha256: &'a str,
    pub member_sha256: &'a str,
    pub security_event_sha256: &'a str,
    pub generation_evidence: &'a Value,
}

/// The item's outcome: the registration it wrote, plus whether the content was
/// already stored. `no_change` is what stops a daily run from minting a new
/// version (and a new evidence bundle) for an unchanged rule book.
pub(crate) struct TradingRulesCommit {
    pub registration: Option<DerivedVersionRow>,
    pub no_change: bool,
}

pub(crate) fn commit_trading_rules(
    c: &Connection,
    write: &TradingRulesWrite<'_>,
) -> Result<TradingRulesCommit, Error> {
    write.book.validate().map_err(rule_error)?;
    let digest = rules_digest(write.book);
    // The readback is ordered by table key rather than by the book's vector
    // order, so compare the order-independent digest, not the vec equality.
    if let Some(existing) = derived::read_version(c, DATASET_KIND, trading_rules::DATASET_KEY)?
        && existing.content_digest == digest
        && rules_digest(&read_trading_rules(c)?) == digest
    {
        // Same content: keep the existing registration, its evidence and its
        // binding untouched instead of re-stamping version metadata.
        return Ok(TradingRulesCommit {
            registration: Some(existing),
            no_change: true,
        });
    }
    let binding = series_commit::put_generation_binding(
        c,
        write.generation_id,
        write.generation_manifest_sha256,
        write.member_sha256,
        write.security_event_sha256,
        write.generation_evidence,
    )?;
    let registration = replace_trading_rules(c, write.book, binding, write.task_id, write.item_id)?;
    Ok(TradingRulesCommit {
        registration,
        no_change: false,
    })
}

#[cfg(test)]
mod tests;
