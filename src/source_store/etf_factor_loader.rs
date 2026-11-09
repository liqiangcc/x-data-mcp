//! Read-only loader and per-entity verdict cache for the ETF factor-check
//! blocking decision (`#607` PR-B, design §3.2.2 "读取" and "缓存").
//!
//! This PR does **not** wire the decision into the read surface: PR-C/PR-D own
//! `resolve_read_shape` and the `details.reason` envelope. What lands here is
//! the API they will call and its tests:
//!
//! ```text
//! Loader::open(store, backend)        // one check-set read, shared lock
//!   .verdict(&facts)                  // -> Verdict, cached
//!   .checks_digest()                  // -> etf_checks_digest of that read
//! ```
//!
//! Two deliberate design points:
//!
//! - **The check set is read once per loader**, not per entity: the store is
//!   append-only evidence, and a run that judged two entities must judge them
//!   against the same set (the `etf_checks_digest` is what names it).
//! - **The cache key contains the evidence digests**, not a version id, so a
//!   changed factor fact cannot hit a stale verdict and no invalidation notice
//!   is needed (design §3.2.2: "key 里含摘要，所以因子一变缓存自然失效").
//!   [`Loader::refresh`] additionally re-reads the store for a long-lived
//!   process; it is not required for correctness.

use std::collections::BTreeMap;
use std::sync::Mutex;

use chrono::NaiveDate;

use super::etf_factor_checks::{self, CheckSet};
use super::etf_factor_gate::{self, EntityFacts, FactorFact, Verdict};
use super::{SourceStore, StoreError};
use crate::sqlite_market::backend::MarketBackend;

/// Cache key of one decision: the entity plus the digests of exactly the facts
/// that entered the judgement. Order is part of the entity's own fact order, so
/// the same facts always give the same key.
pub type CacheKey = (String, Vec<String>);

/// One entity's facts as read from the database, ready for [`Loader::verdict`].
///
/// `first_bar_day` comes from the entity's raw series; when the database has no
/// series for it the value is `None`, which means "no event precedes a first
/// bar" and therefore every event takes part in the decision.
pub fn facts_from_database(
    backend: &MarketBackend,
    entity: &str,
    first_bar_day: Option<NaiveDate>,
) -> Result<EntityFacts, String> {
    let stored = backend
        .stored_factor_events(entity)
        .map_err(|error| format!("factor facts of {entity}: {error}"))?;
    Ok(EntityFacts {
        entity: entity.to_string(),
        first_bar_day,
        facts: stored
            .into_iter()
            .map(|row| FactorFact {
                ordinal: row.event.ordinal,
                effective_day: row.event.effective_day,
                category: row.event.category,
                factors: row.event.factors,
                evidence_digest: hex(&row.evidence_digest),
            })
            .collect(),
    })
}

fn hex(digest: &crate::compact_market::Digest) -> String {
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

/// A read-only view over one store's check records plus the verdict cache.
pub struct Loader {
    checks: CheckSet,
    cache: Mutex<BTreeMap<CacheKey, Verdict>>,
    hits: Mutex<usize>,
    misses: Mutex<usize>,
}

impl Loader {
    /// Read every check record once. The read takes the store's shared lock
    /// only: it never creates the store and never writes it.
    pub fn open(store: &SourceStore) -> Result<Self, StoreError> {
        Ok(Self::from_set(etf_factor_checks::read(store)?))
    }

    /// Build a loader over an already-read set (the replay/test path).
    pub fn from_set(checks: CheckSet) -> Self {
        Self {
            checks,
            cache: Mutex::new(BTreeMap::new()),
            hits: Mutex::new(0),
            misses: Mutex::new(0),
        }
    }

    /// `etf_checks_digest` of the set this loader judges with: the reproduction
    /// handle a response must carry (design §3.2.2, D-92 Q2 (c)).
    pub fn checks_digest(&self) -> String {
        self.checks.digest()
    }

    /// How many records the set holds (all entities).
    pub fn record_count(&self) -> usize {
        self.checks.records.len()
    }

    /// The whole set this loader judges with (`#607` PR-E: the research export
    /// reports each event's verdict as `basis_status`).
    pub fn checks(&self) -> CheckSet {
        self.checks.clone()
    }

    /// Re-read the store's check records (a long-lived process picks up an
    /// import without a restart). The cache is emptied because the digest that
    /// names the set changed.
    pub fn refresh(&mut self, store: &SourceStore) -> Result<(), StoreError> {
        self.checks = etf_factor_checks::read(store)?;
        self.cache.lock().expect("cache lock").clear();
        Ok(())
    }

    /// Decide one entity, memoized by `(entity, evidence digests)`.
    pub fn verdict(&self, facts: &EntityFacts) -> Verdict {
        let key: CacheKey = (facts.entity.clone(), facts.evidence_digests());
        if let Some(verdict) = self.cache.lock().expect("cache lock").get(&key) {
            *self.hits.lock().expect("hits lock") += 1;
            return verdict.clone();
        }
        let verdict = etf_factor_gate::decide(facts, &self.checks.records);
        self.cache
            .lock()
            .expect("cache lock")
            .insert(key, verdict.clone());
        *self.misses.lock().expect("misses lock") += 1;
        verdict
    }

    /// `(cache hits, cache misses)` since the loader was built. Only tests and
    /// diagnostics read it; production callers just call [`Self::verdict`].
    pub fn cache_stats(&self) -> (usize, usize) {
        (
            *self.hits.lock().expect("hits lock"),
            *self.misses.lock().expect("misses lock"),
        )
    }
}

#[cfg(test)]
mod tests;
