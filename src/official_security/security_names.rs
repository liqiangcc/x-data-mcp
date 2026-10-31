//! Security short-name history source adapter (issue-308 S-6; issue-446
//! PR-12, D-61).
//!
//! Layer position (separation point 1): this is a **source adapter**. It turns
//! the frozen SZSE short-name table into name intervals through the domain
//! ([`crate::security_master::names`]) and hands the set to the storage writer.
//! It never opens a writer on its own and never turns a current name into
//! history.
//!
//! ```text
//! plans() ──► exchange_source::Entry (PR-2) ──► frozen bytes (source store)
//!                                                     │
//!              parse (official_security::risk_warnings, one parser)
//!                                                     ▼
//!                        assemble_sz (domain rules, D-61)
//!                                                     │
//!                          NameSet ──► writer (PR-12)
//! ```
//!
//! ## Admission (D-61)
//!
//! - SZ: the dated `SSGSGMXX`/`tab2` short-name change table is
//!   `APPROVED_SOURCE`; its change date opens/closes the interval.
//! - SH and BJ are `NOT_AVAILABLE`: there is no dated name history. Their
//!   coverage is recorded as an unobserved window with
//!   `source_kind='unknown'` and zero rows — the `unbounded`/`unknown`
//!   vocabulary of §4.4 for absence, never a guessed interval. The SH status
//!   events belong to the historical-ST channel (PR-10, D-58); this adapter
//!   does not build a second semantics for them.
//!
//! ## Reuse, not a second parser
//!
//! The SZSE table, its pagination dialect and its row parser belong to PR-10
//! (`risk_warnings::acquire_sz_short_name_table` / `parse_sz_page`). This
//! adapter asks the same entry for the same plan and converts the resulting
//! [`SzNameChange`](crate::security_master::risk_warnings::SzNameChange) rows;
//! both derived items share the run-level download cache, so one run freezes
//! the table once.
//!
//! ## 0 rows
//!
//! An empty table is a source defect, not "no name": the table is a
//! whole-market snapshot, so the item fails closed
//! ([`EMPTY_TABLE_CODE`]) and the previous version stays readable (PR-2 already
//! reports a 0-row page set as `unverified_zero`). A `not_observed` coverage
//! window is recorded as a window, never as a row.
//!
//! ## Replay determinism (#446 §9.4.2)
//!
//! `source_store replay --scope security_names` rebuilds the set from the frozen
//! `szse_short_name` pages alone. Coverage is part of the recorded evidence, so
//! it must be a function of the bytes, not of the wall clock: one frozen table
//! produced different SH/BJ window ends when the live writer stamped `today`.
//! Both paths now call `assemble_table`, which bounds the `NOT_AVAILABLE`
//! windows with the table's own last change day; the acquisition day remains a
//! manifest fact (`acquired_at`) and is not re-derived on replay.
//!
//! ## Failure semantics
//!
//! Acquisition failures come from PR-2 (`source_unavailable`, `source_invalid`,
//! `source_not_admitted`, `partially_failed`); a page that is not the recorded
//! row shape is `source_invalid`. The caller turns any of them into a failed
//! item.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use serde_json::{Value, json};

use crate::exchange_source::{
    AcquireError, Client, Entry, RequestError, RunCache, SnapshotBinding, TransportError,
};
use crate::official_security::risk_warnings;
use crate::security_master::Exchange;
use crate::security_master::assembly::PendingSource;
use crate::security_master::coverage::CoverageWindow;
use crate::security_master::names::{self, NameSet};
use crate::security_master::risk_warnings::SzNameChange;
use crate::source_store::{SourceStore, StoreError};
use crate::sqlite_market::backend::MarketBackend;

/// Business-field parser version of this adapter. It is registered on the
/// derived item's outcome evidence, not in PR-2's envelope manifest.
pub const PARSER_VERSION: &str = "security-names-v1";
/// Reason code for a frozen table with no A-share change row: a whole-market
/// snapshot with zero change rows is a source defect, never an empty history.
pub const EMPTY_TABLE_CODE: &str = "empty_name_set";

/// Documented start of the SZSE short-name table (#391 §4.4: 1994-01-03). The
/// SH/BJ unobserved windows start here so their absence is bounded by the same
/// period the SZ source covers.
pub fn history_start() -> NaiveDate {
    NaiveDate::from_ymd_opt(1994, 1, 3).expect("1994-01-03 is valid")
}

/// An admitted name set plus everything the derived item must register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    pub set: NameSet,
    pub binding: SnapshotBinding,
    /// `label → snapshot ids` of the frozen SZSE pages, in page order.
    pub snapshot_ids: BTreeMap<&'static str, Vec<String>>,
    /// Rows read from the frozen SZSE pages (A-share rows plus skipped ones).
    pub sz_rows: usize,
    pub sz_skipped_codes: usize,
}

impl Admitted {
    pub fn evidence(&self) -> Value {
        json!({
            "adapter": "security name history (S-6, D-61)",
            "parser_version": PARSER_VERSION,
            "source_binding": self.binding,
            "snapshot_ids": self.snapshot_ids,
            "sz_change_rows": self.sz_rows,
            "sz_skipped_codes": self.sz_skipped_codes,
            "name_rows": self.set.rows.len(),
            "observed_windows": self.set.observed_windows(),
            "not_observed_windows": self.set.not_observed_windows(),
            "coverage": self.set.coverage.iter().map(|window| json!({
                "source_kind": window.source_kind,
                "exchange": window.exchange.as_str(),
                "from": window.from.to_string(),
                "to": window.to.to_string(),
                "observed": window.observed,
                "rows": window.rows,
                "reason": window.reason,
            })).collect::<Vec<_>>(),
            "market_tiers": {
                "sz": "APPROVED_SOURCE: SSGSGMXX/tab2 change dates (D-61)",
                "sh": "NOT_AVAILABLE: no dated name history; status events live in the risk_warnings channel",
                "bj": "NOT_AVAILABLE: no canonical dated name source",
            },
            "normalisation": "XD/XR/DR marker prefixes are stripped before a name lands; \
                a row whose names differ only by a marker opens no interval",
            "known_at_policy": "SZ rows carry known_at = NULL: the table dates the change \
                but carries no publication time",
            "absence_policy": "a not_observed window is never a name; an empty table fails \
                closed instead of replacing the dataset with zero rows",
        })
    }
}

/// The daily item's answer: an admitted set, or the reason the source is
/// pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunSource {
    Admitted(Admitted),
    Pending(PendingSource),
}

/// Why the adapter could not produce a settled row set.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Acquire(#[from] AcquireError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Request(#[from] RequestError),
    #[error(transparent)]
    Domain(#[from] names::Error),
    /// A failure inside PR-10's frozen SZSE acquisition/parse entry.
    #[error(transparent)]
    Szse(#[from] risk_warnings::Error),
    #[error("{code}: {detail}")]
    Invalid { code: &'static str, detail: String },
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Acquire(error) => error.code(),
            Self::Store(error) => error.code(),
            Self::Transport(error) => error.code(),
            Self::Request(error) => error.code(),
            Self::Domain(_) => "source_invalid",
            Self::Szse(error) => error.code(),
            Self::Invalid { code, .. } => code,
        }
    }
}

/// Resolve the source store and fetch client, then acquire.
///
/// Under `Policy::Optional` not configured is not a fetch failure: the item
/// records `pending_source` and writes zero rows. Under `Policy::Required` (ops
/// entry on a compact backend) a missing or unreadable store fails the item with
/// `source_store_unconfigured` instead of a silent zero-row success. A
/// configured store whose fetch fails is a real failure. `cache` is the run-level download
/// cache (§10.1): `derived:risk_warnings` asks for the same SZSE table.
pub fn run_source(
    market: &MarketBackend,
    cache: Arc<RunCache>,
    policy: crate::source_store::Policy,
) -> Result<RunSource, Error> {
    let Some(store) = crate::source_store::configured(policy).map_err(|error| Error::Invalid {
        code: error.code(),
        detail: error.detail().to_string(),
    })?
    else {
        return Ok(RunSource::Pending(PendingSource {
            dataset: names::DATASET_KIND,
            source: "exchange_name_history",
            reason: "the M3 source store is not configured for this process \
                     (X_DATA_MCP_SOURCE_STORE); name history stays pending_source",
        }));
    };
    let client = Client::helper()?;
    Ok(RunSource::Admitted(acquire_today(
        &store,
        market,
        &client,
        Utc::now().date_naive(),
        cache,
    )?))
}

fn acquire_today(
    store: &SourceStore,
    market: &MarketBackend,
    client: &Client,
    today: NaiveDate,
    cache: Arc<RunCache>,
) -> Result<Admitted, Error> {
    let entry = Entry::new(store, market, client)
        .with_cache(cache)
        .with_today(today);
    let table = risk_warnings::acquire_sz_short_name_table(&entry, store)?;
    let set = assemble_table(&table.changes)?;
    let mut snapshot_ids = BTreeMap::new();
    snapshot_ids.insert("szse_short_name", table.snapshot_ids);
    Ok(Admitted {
        set,
        binding: table.binding,
        snapshot_ids,
        sz_rows: table.rows,
        sz_skipped_codes: table.skipped_codes,
    })
}

/// Assemble one frozen SZSE change chain into the admitted name set.
///
/// This is the **only** place the coverage windows are built, and it reads no
/// clock: the live writer ([`run_source`]) and `source_store replay
/// --scope security_names` call it with the same frozen table and therefore
/// record bit-identical evidence on any day (`#446` §9.4.2 determinism).
///
/// - The SZ window spans the table's own change days.
/// - The SH/BJ `NOT_AVAILABLE` windows end on the table's last change day, not
///   on the acquisition day. A `not_observed` window records that no dated
///   source exists at all; its end is not an observation, and a wall-clock
///   bound made the same frozen bytes produce new evidence on every replay.
///   The acquisition day stays a recorded fact of the store
///   (`manifest.jsonl.acquired_at`), never of the dataset.
///
/// An empty chain is [`EMPTY_TABLE_CODE`], fail-closed: the caller must not
/// replace the history with zero rows.
fn assemble_table(changes: &[SzNameChange]) -> Result<NameSet, Error> {
    let (Some(from), Some(to)) = (
        changes.iter().map(|change| change.day).min(),
        changes.iter().map(|change| change.day).max(),
    ) else {
        return Err(Error::Invalid {
            code: EMPTY_TABLE_CODE,
            detail: "the frozen SZSE short-name table has no A-share change row; \
                     refusing to replace the name history with zero rows"
                .into(),
        });
    };
    let mut coverage = vec![CoverageWindow {
        source_kind: names::NameSourceKind::SzseNameHistory.as_str().into(),
        exchange: Exchange::Sz,
        from,
        to,
        observed: true,
        rows: changes.len(),
        reason: "the frozen short-name table is non-empty".into(),
    }];
    coverage.extend(unavailable_markets(to));
    Ok(names::assemble_sz(changes, coverage)?)
}

/// The windows of the markets with no dated name source (D-61). They carry no
/// row: `source_kind='unknown'` and `observed=false` say "not available", not
/// "no rename".
///
/// `last_change_day` bounds them: see `assemble_table` for why it is the
/// admitted table's own end instead of the day the bytes were acquired.
fn unavailable_markets(last_change_day: NaiveDate) -> Vec<CoverageWindow> {
    [Exchange::Sh, Exchange::Bj]
        .into_iter()
        .map(|exchange| CoverageWindow {
            source_kind: names::NameSourceKind::Unknown.as_str().into(),
            exchange,
            from: history_start(),
            to: last_change_day,
            observed: false,
            rows: 0,
            reason: "NOT_AVAILABLE (D-61): no dated name history for this market; \
                     no interval is inferred from a current name"
                .into(),
        })
        .collect()
}

/// `--scope security_names` replay support: parse the frozen SZSE short-name
/// pages and rebuild the domain set without any HTTP request and without
/// reading the clock.
///
/// `pages` are `(source_id, snapshot_id, bytes)` in manifest order; only
/// `szse_short_name` pages matter (the SSE bulletin pages belong to PR-10).
/// Coverage is derived from the bytes by `assemble_table`, the same function
/// the live writer uses, so a fresh init replays the windows the first write
/// recorded instead of re-stamping them with the current day. A table with no
/// A-share change row fails with [`EMPTY_TABLE_CODE`].
pub fn replay(pages: &[(&str, String, Vec<u8>)]) -> Result<NameSet, Error> {
    let mut changes = Vec::new();
    for (source_id, snapshot_id, bytes) in pages {
        if *source_id != "szse_short_name" {
            continue;
        }
        let (mut parsed, _, _) = risk_warnings::parse_sz_page(bytes, snapshot_id)?;
        changes.append(&mut parsed);
    }
    assemble_table(&changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("tests/fixtures/exchange-security-names/{name}"))
            .expect("security-name fixture exists")
    }

    fn page(name: &str) -> Vec<(&'static str, String, Vec<u8>)> {
        vec![("szse_short_name", format!("{name}-sha"), fixture(name))]
    }

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn a_chain_fixture_replays_into_intervals() {
        let set = replay(&page("szse-chain.json")).expect("chain replays");
        // Three dated changes for one code: two closed intervals and one open.
        assert_eq!(set.rows.len(), 3);
        assert_eq!(set.rows[0].name, "ST深安达");
        assert_eq!(set.rows[0].effective_from, day(1998, 4, 28));
        assert_eq!(set.rows[0].effective_to, Some(day(1999, 4, 27)));
        // The last interval holds the name the last change moved *to*.
        assert_eq!(set.rows[2].name, "深安达");
        assert_eq!(set.rows[2].certainty, names::NameCertainty::Unbounded);
        // The B-share row of the fixture is skipped, not turned into a name.
        assert!(set.rows.iter().all(|row| row.code != "200001"));
        assert_eq!(set.observed_windows(), 1);
        assert_eq!(set.not_observed_windows(), 2);
    }

    #[test]
    fn xd_xr_marker_rows_do_not_become_intervals() {
        let set = replay(&page("szse-xd-xr.json")).expect("marker fixture replays");
        assert_eq!(set.rows.len(), 1);
        assert_eq!(set.rows[0].name, "万科企业");
        assert!(set.rows.iter().all(|row| !row.name.contains("XD")));
        assert!(set.rows.iter().all(|row| !row.name.contains("XR")));
    }

    #[test]
    fn same_day_changes_fail_closed() {
        let error = replay(&page("szse-same-day.json")).expect_err("same-day changes are refused");
        assert_eq!(error.code(), "source_invalid");
    }

    #[test]
    fn a_zero_row_table_is_not_an_empty_history() {
        // The page has no A-share change row. Assembling it would be zero rows
        // and only `not_observed` windows, so the adapter refuses it: the live
        // item must not write that as the name history, and the replay must
        // not write it as "the empty table was rebuilt".
        let error = replay(&page("szse-empty.json")).expect_err("an empty table is refused");
        assert_eq!(error.code(), EMPTY_TABLE_CODE);
    }

    #[test]
    fn unavailable_markets_use_the_unknown_vocabulary() {
        let windows = unavailable_markets(day(2001, 3, 7));
        assert_eq!(windows.len(), 2);
        for window in &windows {
            assert_eq!(window.source_kind, "unknown");
            assert!(!window.observed);
            assert_eq!(window.rows, 0);
            assert_eq!(window.from, history_start());
            // Bounded by the admitted table, not by a clock reading.
            assert_eq!(window.to, day(2001, 3, 7));
        }
        let exchanges: Vec<Exchange> = windows.iter().map(|window| window.exchange).collect();
        assert_eq!(exchanges, vec![Exchange::Sh, Exchange::Bj]);
    }

    /// The writer's assembly and the replay entry are the same function: the
    /// same frozen bytes produce the same rows **and** the same coverage
    /// windows, on any day (no `Utc::now()` anywhere on this path).
    #[test]
    fn the_live_writer_and_the_replay_record_the_same_coverage() {
        let pages = page("szse-chain.json");
        let replayed = replay(&pages).expect("chain replays");
        let (changes, _, _) =
            risk_warnings::parse_sz_page(&pages[0].2, &pages[0].1).expect("the fixture parses");
        let written = assemble_table(&changes).expect("the live writer assembles the same chain");
        assert_eq!(replayed, written);

        // The as-of bound the writer now records is the table's own last
        // change day; the acquisition day is not part of the dataset evidence.
        let sz = replayed
            .coverage
            .iter()
            .find(|window| window.exchange == Exchange::Sz)
            .expect("the SZ window exists");
        assert_eq!((sz.from, sz.to), (day(1998, 4, 28), day(2001, 3, 7)));
        assert!(sz.observed);
        for window in replayed
            .coverage
            .iter()
            .filter(|window| window.exchange != Exchange::Sz)
        {
            assert_eq!(window.to, day(2001, 3, 7));
        }
    }

    #[test]
    fn a_strict_run_without_a_source_store_fails_the_item() {
        // `silent-path #446` §14.5: this consumer reads the same
        // `X_DATA_MCP_SOURCE_STORE`; under a Required policy its absence is a
        // configuration failure, not "0 rows succeeded".
        let _lock = crate::source_store::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env = crate::source_store::PolicyEnv::set(None, false);
        let (_dir, market) = crate::official_security::compact_market_fixture();
        let error = run_source(
            &market,
            std::sync::Arc::new(RunCache::new()),
            crate::source_store::Policy::Required,
        )
        .unwrap_err();
        assert_eq!(error.code(), "source_store_unconfigured");
        assert!(
            error.to_string().starts_with("source_store_unconfigured: "),
            "the stable code leads the item error text: {error}"
        );
        assert!(market.read_exchange_snapshots().unwrap().is_empty());
    }
}
