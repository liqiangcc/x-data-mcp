//! The read-face ETF 前复权 gate (`#607` PR-D, design §3.2.2).
//!
//! PR-B landed the decision ([`super::etf_factor_gate::decide`]) and the
//! check-record loader ([`super::etf_factor_loader::Loader`]) as pure pieces.
//! This module is the object the read surface injects into
//! [`crate::sqlite_market::series::resolve_read_shape`]: it owns the store, the
//! loader and the per-entity verdict cache, and it answers one question —
//! "may this entity's stored 前复权 series be served right now?".
//!
//! Three rules from design §3.2.2 shape the implementation:
//!
//! * **Facts are read per request.** The caller passes the entity's *current*
//!   stored facts (`adjustment_events.evidence_digest` plus the 不复权 first
//!   bar). A factor row that arrived with the last daily run therefore blocks
//!   the next read with no offline sync step: its digest matches no record.
//! * **The cache key carries the digests.** `(entity, evidence digests)` means
//!   a changed factor set cannot hit a stale verdict, and no invalidation
//!   notice exists to forget.
//! * **A changed check set is picked up without a restart.** Every call runs a
//!   cheap probe — `(len, mtime)` of `manifest.jsonl` — and only when that
//!   moved is the manifest scanned for the check-record digest
//!   ([`super::etf_factor_checks::manifest_digest`], no blob reads). If that
//!   digest differs from the loader's, the loader is refreshed (one full
//!   verified read). Cost: one `stat` per ETF `qfq` request, plus one manifest
//!   scan when the store changed, plus one full read when the *check records*
//!   changed. An import of anything else in the store costs the manifest scan
//!   and nothing more.
//! * **No store means no admission.** An unconfigured or unreadable source
//!   store fails closed for every ETF `qfq`, with the cause in the message and
//!   `etf_factor_unverified` as the reason code (the events cannot be shown to
//!   be verified). There is no path where a missing store reads as "verified".

use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use super::etf_factor_checks;
use super::etf_factor_gate::{self, BlockReason, EntityFacts, FactorFact};
use super::etf_factor_loader::Loader;
use super::{CONFIG_ENV, Policy, SourceStore, StoreError, configured};
use crate::sqlite_market::series::{
    EtfForwardAdmission, EtfForwardFact, EtfForwardFacts, EtfForwardGate,
};

/// Where the gate's check records come from.
enum Source {
    /// `X_DATA_MCP_SOURCE_STORE`, resolved lazily so a store mounted after
    /// start-up is picked up without a restart and an unset variable is a
    /// per-request fail-closed answer rather than a start-up failure.
    FromEnv,
    /// An explicitly opened store (tests and any caller that already has one).
    Store(SourceStore),
    /// Explicitly unavailable, with the cause to report.
    Unavailable(String),
}

/// The loader plus the probe value it was built from.
struct Open {
    loader: Loader,
    probe: Probe,
}

/// Cheap change detector for the store's manifest: `(len, mtime)`. The
/// manifest is append-only (`SourceStore::register` appends one line and never
/// rewrites), so a change always moves at least one of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Probe {
    len: u64,
    mtime: Option<(u64, u32)>,
}

impl Probe {
    fn of(store: &SourceStore) -> Result<Self, StoreError> {
        match std::fs::metadata(store.manifest_path()) {
            Ok(meta) => Ok(Self {
                len: meta.len(),
                mtime: meta.modified().ok().and_then(|time| {
                    time.duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|d| (d.as_secs(), d.subsec_nanos()))
                }),
            }),
            // No manifest yet is the empty store, not an error.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                len: 0,
                mtime: None,
            }),
            Err(error) => Err(error.into()),
        }
    }
}

/// The injected ETF 前复权 gate over one source store.
pub struct ForwardGate {
    source: Source,
    open: Mutex<Option<Open>>,
}

impl ForwardGate {
    /// Resolve the store from `X_DATA_MCP_SOURCE_STORE` on first use.
    pub fn from_env() -> Self {
        Self {
            source: Source::FromEnv,
            open: Mutex::new(None),
        }
    }

    /// Judge against an explicitly opened store.
    pub fn store(store: SourceStore) -> Self {
        Self {
            source: Source::Store(store),
            open: Mutex::new(None),
        }
    }

    /// A gate that admits nothing, with `detail` as the reported cause.
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            source: Source::Unavailable(detail.into()),
            open: Mutex::new(None),
        }
    }

    /// The `etf_checks_digest` of the set the gate currently judges with, once
    /// a store has been read. `None` before the first decision.
    pub fn checks_digest(&self) -> Option<String> {
        self.open
            .lock()
            .expect("gate lock")
            .as_ref()
            .map(|open| open.loader.checks_digest())
    }

    fn resolve_store(&self) -> Result<SourceStore, String> {
        match &self.source {
            Source::Store(store) => Ok(store.clone()),
            Source::Unavailable(detail) => Err(detail.clone()),
            Source::FromEnv => match configured(Policy::Optional) {
                Ok(Some(store)) => Ok(store),
                Ok(None) => Err(format!(
                    "{CONFIG_ENV} is not configured; the ETF factor-check records \
                     live in the source store"
                )),
                Err(error) => Err(error.to_string()),
            },
        }
    }

    /// Read the store once and build the loader, failing closed on any error.
    fn opened(&self, store: &SourceStore) -> Result<Open, String> {
        let loader = Loader::open(store)
            .map_err(|error| format!("source store check records are unreadable: {error}"))?;
        let probe =
            Probe::of(store).map_err(|error| format!("source store unreadable: {error}"))?;
        Ok(Open { loader, probe })
    }

    /// Pick up an imported check set without a restart (see the module docs for
    /// the probe's cost).
    fn refresh_if_changed(&self, open: &mut Open, store: &SourceStore) -> Result<(), String> {
        let probe =
            Probe::of(store).map_err(|error| format!("source store unreadable: {error}"))?;
        if probe == open.probe {
            return Ok(());
        }
        // The manifest moved. Scan it (no blobs) and compare the set digest
        // before paying for a verified re-read.
        let digest = etf_factor_checks::manifest_digest(store)
            .map_err(|error| format!("source store unreadable: {error}"))?;
        if digest != open.loader.checks_digest() {
            open.loader
                .refresh(store)
                .map_err(|error| format!("source store check records are unreadable: {error}"))?;
        }
        open.probe = probe;
        Ok(())
    }

    /// The decision for one entity, against the facts the caller just read.
    fn decide(&self, facts: &EtfForwardFacts) -> EtfForwardAdmission {
        if !facts.forward_built {
            // Nothing is stored, so nothing can be verified: the reason code
            // comes from the gate module, which owns every wire value.
            return EtfForwardAdmission::Blocked {
                reason: BlockReason::QfqNotBuilt.as_str(),
                blocking_events: 0,
                categories: Vec::new(),
                detail: None,
            };
        }
        let store = match self.resolve_store() {
            Ok(store) => store,
            Err(detail) => return blocked_unavailable(detail),
        };
        let mut open = self.open.lock().expect("gate lock");
        if open.is_none() {
            match self.opened(&store) {
                Ok(opened) => *open = Some(opened),
                Err(detail) => return blocked_unavailable(detail),
            }
        }
        let state = open.as_mut().expect("opened above");
        if let Err(detail) = self.refresh_if_changed(state, &store) {
            // The store went away under us: drop the loader so the next
            // request retries, and fail closed for this one.
            *open = None;
            return blocked_unavailable(detail);
        }
        let verdict = state.loader.verdict(&entity_facts(facts));
        admission(verdict, state.loader.checks_digest())
    }
}

impl EtfForwardGate for ForwardGate {
    fn admit(&self, facts: &EtfForwardFacts) -> EtfForwardAdmission {
        self.decide(facts)
    }

    fn check_set(&self) -> Option<etf_factor_checks::CheckSet> {
        // Open the store if this gate has not been asked a decision yet (a
        // `--adjustment raw` export with `--dataset factors` judges nothing but
        // still reports each event's verdict). A failure here is not an error:
        // the column then says `unverified`, which matches the gate's own
        // fail-closed answer for every ETF.
        let store = self.resolve_store().ok()?;
        let mut open = self.open.lock().expect("gate lock");
        if open.is_none() {
            *open = self.opened(&store).ok();
        }
        let state = open.as_mut()?;
        self.refresh_if_changed(state, &store).ok()?;
        Some(state.loader.checks())
    }
}

/// The market-side facts in the shape PR-B's decision consumes.
fn entity_facts(facts: &EtfForwardFacts) -> EntityFacts {
    EntityFacts {
        entity: facts.entity.clone(),
        first_bar_day: facts.first_bar_day,
        facts: facts.facts.iter().map(factor_fact).collect(),
    }
}

fn factor_fact(fact: &EtfForwardFact) -> FactorFact {
    FactorFact {
        ordinal: fact.ordinal,
        effective_day: fact.effective_day,
        category: fact.category,
        factors: fact.factors,
        evidence_digest: fact.evidence_digest.clone(),
    }
}

fn admission(verdict: etf_factor_gate::Verdict, checks_digest: String) -> EtfForwardAdmission {
    match verdict {
        etf_factor_gate::Verdict::Admitted { .. } => {
            EtfForwardAdmission::Admitted { checks_digest }
        }
        etf_factor_gate::Verdict::Blocked {
            reason,
            blocking_events,
            categories,
        } => EtfForwardAdmission::Blocked {
            reason: reason.as_str(),
            blocking_events,
            categories,
            detail: None,
        },
    }
}

/// Fail-closed answer for "the gate cannot judge": the events cannot be shown
/// to be verified, and `detail` says why.
fn blocked_unavailable(detail: String) -> EtfForwardAdmission {
    EtfForwardAdmission::Blocked {
        reason: BlockReason::FactorUnverified.as_str(),
        blocking_events: 0,
        categories: Vec::new(),
        detail: Some(detail),
    }
}

#[cfg(test)]
mod tests;
