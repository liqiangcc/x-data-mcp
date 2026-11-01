//! The shared entry the five consumers call (design §6.2/§6.3).
//!
//! A consumer supplies a [`AcquisitionPlan`] (source id, URL template, the
//! parameters that are not pagination/segment bounds, the paging dialect and
//! the evidence tier) and gets back frozen snapshots plus a
//! [`SnapshotBinding`]. Everything the design forbids duplicating — retries,
//! Referer/UA, pagination quirks, the 0-row rule, segment bounds, the frozen
//! write order, the binding digest — happens here once.
//!
//! The sequence per window is exactly §6.3's:
//!
//! ```text
//! fetch (bounded) → require 2xx → freeze bytes → parse envelope → validate
//!   counts/zero → index rows (one transaction) → bind
//! ```
//!
//! Bytes are frozen before the envelope is parsed, so `source_invalid` keeps
//! its evidence. Index rows are written only after the whole window validates,
//! so a truncated window leaves no rows and no half-written set.
//!
//! A multi-window run (the SSE bulletin index loops quarters, §6.4) collects
//! failures per window and returns [`AcquireError::PartiallyFailed`] with the
//! succeeded windows: frozen snapshots are kept, uncovered windows produce no
//! rows.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;

use super::bind::{self, SnapshotBinding};
use super::freeze::{FrozenBytes, FrozenSnapshot, freeze_bytes, record_snapshots};
use super::index::SnapshotRow;
use super::request::{DateFormat, ExchangeRequest, PagingFormat, Segment};
use super::reuse::{self, ReuseEvidence, ReusePolicyTable, SelectedBy, SelectedPage};
use super::transport::{Client, HttpRequest};
use super::validate::{self, EmptyPolicy, FailureClass, PageEnvelope, PageSet, ZeroPolicy};
use super::{BasisStatus, SourceId, default_headers};
use crate::market_storage::Error as StorageError;
use crate::source_store::{SourceStore, StoreError};
use crate::sqlite_market::backend::MarketBackend;

/// Hard bound on pages per window (§6.3 "bounded"): a runaway `pageCount` is a
/// source defect, not something to loop on.
pub const MAX_PAGES: u32 = 500;

/// One downloaded-and-frozen page, kept for the rest of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedPage {
    frozen: FrozenBytes,
    envelope: PageEnvelope,
}

/// Where one page of a window came from (design §5.2).
///
/// A fetched page still has to be indexed; a reused page is already in the
/// index and the store, so it is never written again.
#[derive(Debug, Clone)]
enum PageState {
    Fetched {
        bytes: FrozenBytes,
        /// The page came from this run's cache, not from this call's request.
        cached: bool,
    },
    /// Same request fingerprint, bytes already frozen by an earlier run.
    Reused { row: SnapshotRow },
}

/// One selected page of a window: its state plus the envelope the validation
/// rules see. A reused page's envelope is parsed again from the verified bytes.
#[derive(Debug, Clone)]
struct PageEntry {
    state: PageState,
    envelope: PageEnvelope,
}

/// Run-level download cache (§10.1: several items of one run that need the same
/// request download it once).
///
/// The key is the canonical request fingerprint, so a hit means byte-identical
/// request identity — the same thing the source store de-duplicates on. A hit
/// neither fetches nor freezes again; the snapshot it names is already in the
/// store and the index, so the second window reports it as `reused`.
///
/// Create one per run and share it with [`Entry::with_cache`]; without it an
/// `Entry` caches nothing across calls.
#[derive(Debug, Default)]
pub struct RunCache {
    pages: Mutex<BTreeMap<String, Arc<CachedPage>>>,
}

impl RunCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, fingerprint: &str) -> Option<Arc<CachedPage>> {
        self.pages
            .lock()
            .expect("the run cache lock is never held across a panic")
            .get(fingerprint)
            .cloned()
    }

    fn insert(&self, fingerprint: String, page: Arc<CachedPage>) {
        self.pages
            .lock()
            .expect("the run cache lock is never held across a panic")
            .insert(fingerprint, page);
    }

    /// Requests this cache answered without a download.
    pub fn len(&self) -> usize {
        self.pages
            .lock()
            .expect("the run cache lock is never held across a panic")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What one consumer asks the entry for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquisitionPlan {
    pub source_id: SourceId,
    pub url: String,
    /// Parameters that are neither pagination nor segment bounds (filters such
    /// as `CATALOGID`); pagination is added by [`PagingFormat`], the window by
    /// [`Entry::acquire_window`].
    pub params: BTreeMap<String, String>,
    pub format: PagingFormat,
    /// Tier recorded on every snapshot of this plan.
    pub basis_status: BasisStatus,
    /// Tier the consumer's writer requires; below it the entry refuses with
    /// `source_not_admitted` before any request is sent (§6.3).
    pub required_basis: BasisStatus,
    pub max_pages: u32,
    /// This plan must be satisfied by one page: a first envelope reporting
    /// more than one page means the source ignored the request's page size, and
    /// the entry fails with `page_size_ignored` instead of paging (§2).
    pub single_page_only: bool,
    /// What a 0-row window means to this consumer (BR-M3-10). Default
    /// [`ZeroPolicy::FailClosed`]; A-3 asks for [`ZeroPolicy::NotObserved`]
    /// because it only derives positive facts.
    pub zero_policy: ZeroPolicy,
    /// Date shape of the window bounds; [`DateFormat::Iso`] unless the source
    /// documents the compact shape.
    pub date_format: DateFormat,
}

impl AcquisitionPlan {
    pub fn new(source_id: SourceId, url: impl Into<String>, format: PagingFormat) -> Self {
        Self {
            source_id,
            url: url.into(),
            params: BTreeMap::new(),
            format,
            basis_status: source_id.default_basis(),
            required_basis: BasisStatus::ApprovedSource,
            max_pages: MAX_PAGES,
            single_page_only: false,
            zero_policy: ZeroPolicy::FailClosed,
            date_format: DateFormat::Iso,
        }
    }

    pub fn param(mut self, name: &str, value: impl Into<String>) -> Self {
        self.params.insert(name.to_string(), value.into());
        self
    }

    pub fn basis(mut self, basis_status: BasisStatus) -> Self {
        self.basis_status = basis_status;
        self
    }

    pub fn required_basis(mut self, required_basis: BasisStatus) -> Self {
        self.required_basis = required_basis;
        self
    }

    pub fn max_pages(mut self, max_pages: u32) -> Self {
        self.max_pages = max_pages;
        self
    }

    /// Require the whole window in one page (the SSE list dialect); more than
    /// one page means the source ignored the request's page size.
    pub fn single_page_only(mut self) -> Self {
        self.single_page_only = true;
        self
    }

    /// Record 0-row windows as `not_observed` instead of failing them
    /// (BR-M3-10; only for consumers that store positive facts only).
    pub fn zero_rows_not_observed(mut self) -> Self {
        self.zero_policy = ZeroPolicy::NotObserved;
        self
    }

    /// Write the window bounds as `YYYYMMDD` (the `commonSoaQuery` sources
    /// ignore requests with the dashed shape).
    pub fn compact_dates(mut self) -> Self {
        self.date_format = DateFormat::Compact;
        self
    }

    /// Canonical request of one page, including the window bounds when the
    /// source takes them as request parameters.
    pub fn request(
        &self,
        page: u32,
        window: Option<Segment>,
        from_param: &str,
        to_param: &str,
    ) -> ExchangeRequest {
        let mut request = ExchangeRequest::new(self.source_id, self.url.clone())
            .with_params(&self.params)
            .paged(self.format, page);
        if let Some(segment) = window {
            let mut params = BTreeMap::new();
            segment.apply_with(&mut params, from_param, to_param, self.date_format);
            request = request.with_params(&params);
        }
        request
    }
}

/// One window's frozen snapshots plus the validated counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquisitionOutcome {
    pub source_id: &'static str,
    pub snapshots: Vec<FrozenSnapshot>,
    pub row_count: usize,
    pub page_count: u32,
    pub binding: SnapshotBinding,
    /// Snapshots this run re-used instead of adding (same fingerprint + bytes).
    pub reused: usize,
    /// Reuse evidence of this window (design §5.2/§5.3): the rule in force, the
    /// reused/fetched counts, the selected snapshots and the dates they imply.
    pub evidence: ReuseEvidence,
}

/// One succeeded window of a multi-window run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowResult {
    pub window: Segment,
    pub snapshots: Vec<FrozenSnapshot>,
    pub row_count: usize,
    pub page_count: u32,
    /// Whether this window's pages were fetched or read through, and the date
    /// rule of §5.3 over them.
    pub evidence: ReuseEvidence,
}

/// One failed window of a multi-window run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFailure {
    pub window: Segment,
    pub class: FailureClass,
    pub code: &'static str,
    pub detail: String,
}

/// A fully successful multi-window run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowOutcome {
    pub source_id: &'static str,
    pub results: Vec<WindowResult>,
    pub snapshot_count: usize,
    pub row_count: usize,
    pub binding: SnapshotBinding,
    pub reused: usize,
    /// The merged evidence of every succeeded window, in window/page order.
    pub evidence: ReuseEvidence,
}

#[derive(Debug, thiserror::Error)]
pub enum AcquireError {
    #[error("source_unavailable: {detail}")]
    Unavailable { detail: String },
    #[error("{code}: {detail}")]
    Invalid { code: &'static str, detail: String },
    #[error("source_not_admitted: {detail}")]
    NotAdmitted { detail: String },
    /// The hard age bound `H` passed and no refresh run has confirmed the
    /// bytes since: fail closed instead of publishing stale source facts
    /// (design §5.1).
    #[error("source_refresh_overdue: {detail}")]
    RefreshOverdue { detail: String },
    #[error(
        "partially_failed: {failed} of {windows} window(s) failed; frozen snapshots of the \
         other windows are kept and the failed windows produce no rows"
    )]
    PartiallyFailed {
        windows: usize,
        failed: usize,
        failures: Vec<WindowFailure>,
        succeeded: Vec<WindowResult>,
    },
    #[error(transparent)]
    Freeze(#[from] super::freeze::FreezeError),
    #[error(transparent)]
    Bind(#[from] bind::BindError),
    /// Reading the bytes of a reused snapshot back through the source store:
    /// a missing or tampered blob is `source_store_corrupt`, never a silent
    /// fallback (design §5.2).
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The index lookup a reuse decision needs failed.
    #[error(transparent)]
    Index(#[from] StorageError),
}

impl AcquireError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable { .. } => "source_unavailable",
            Self::Invalid { code, .. } => code,
            Self::NotAdmitted { .. } => "source_not_admitted",
            Self::RefreshOverdue { .. } => "source_refresh_overdue",
            Self::PartiallyFailed { .. } => "partially_failed",
            Self::Freeze(error) => error.code(),
            Self::Bind(error) => error.code(),
            Self::Store(error) => error.code(),
            Self::Index(_) => "source_index_unavailable",
        }
    }

    pub fn class(&self) -> FailureClass {
        match self {
            Self::Unavailable { .. } => FailureClass::SourceUnavailable,
            Self::PartiallyFailed { .. } => FailureClass::PartiallyFailed,
            Self::Invalid { .. } | Self::NotAdmitted { .. } | Self::RefreshOverdue { .. } => {
                FailureClass::SourceInvalid
            }
            Self::Freeze(_) | Self::Bind(_) => FailureClass::SourceInvalid,
            // The bytes could not be read back locally: nothing usable arrived
            // for this window, the same class `bind` gives a store fault.
            Self::Store(_) | Self::Index(_) => FailureClass::SourceUnavailable,
        }
    }
}

/// The shared entry: store + index handle + transport.
pub struct Entry<'a> {
    store: &'a SourceStore,
    backend: &'a MarketBackend,
    client: &'a Client,
    today: NaiveDate,
    cache: Option<Arc<RunCache>>,
    /// The declarative refresh rules (§5.1). The default table has every rule
    /// registered and every rule off, so an `Entry` that is not told otherwise
    /// fetches exactly as before.
    reuse: ReusePolicyTable,
}

impl<'a> Entry<'a> {
    pub fn new(store: &'a SourceStore, backend: &'a MarketBackend, client: &'a Client) -> Self {
        Self {
            store,
            backend,
            client,
            // `source_refresh` passes its own `today` with `with_today`; the
            // default is the market's date for the same reason as
            // `select::Reader` (XDM-156 ruling A).
            today: crate::calendar::shanghai_today(),
            cache: None,
            reuse: ReusePolicyTable::declared(),
        }
    }

    /// Share one run-level download cache (§10.1). Pages another item of the
    /// same run already fetched and froze are not downloaded twice.
    pub fn with_cache(mut self, cache: Arc<RunCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Use a table other than the shipped one. R2/R3 pass a table with the
    /// sources they refresh enabled; the R1 acceptance tests use it to exercise
    /// read-through reuse without changing the default.
    pub fn with_reuse_policies(mut self, reuse: ReusePolicyTable) -> Self {
        self.reuse = reuse;
        self
    }

    /// Replace the "today" the empty-window rule compares against (tests and
    /// deterministic acceptance runs).
    pub fn with_today(mut self, today: NaiveDate) -> Self {
        self.today = today;
        self
    }

    /// Acquire one unpaginated window (or the source's whole current page set).
    pub fn acquire(&self, plan: &AcquisitionPlan) -> Result<AcquisitionOutcome, AcquireError> {
        let (pages, snapshots, selected) = self.acquire_pages(plan, None, "", "", None)?;
        self.finish(plan, pages, snapshots, selected)
    }

    /// Acquire one date window; `from_param`/`to_param` are the source's
    /// parameter names for the inclusive bounds.
    pub fn acquire_window(
        &self,
        plan: &AcquisitionPlan,
        window: Segment,
        from_param: &str,
        to_param: &str,
    ) -> Result<AcquisitionOutcome, AcquireError> {
        let (pages, snapshots, selected) =
            self.acquire_pages(plan, Some(window), from_param, to_param, None)?;
        self.finish(plan, pages, snapshots, selected)
    }

    /// Fetch exactly page 1 of a whole-table plan (the daily change probe,
    /// §5.1 table class). It freezes and indexes the page like any other
    /// acquisition but does not require the rest of the table to arrive: the
    /// caller decides whether the probe changed and a full acquisition follows.
    pub fn probe_first_page(
        &self,
        plan: &AcquisitionPlan,
    ) -> Result<AcquisitionOutcome, AcquireError> {
        let (pages, snapshots, selected) = self.acquire_pages(plan, None, "", "", Some(1))?;
        self.finish(plan, pages, snapshots, selected)
    }

    /// Acquire a set of windows (e.g. `quarter_segments`) with per-window
    /// isolation. Any failed window turns the run into `partially_failed`.
    pub fn acquire_windows(
        &self,
        plan: &AcquisitionPlan,
        windows: &[Segment],
        from_param: &str,
        to_param: &str,
    ) -> Result<WindowOutcome, AcquireError> {
        if windows.is_empty() {
            return Err(AcquireError::Invalid {
                code: "no_window",
                detail: "the window set is empty".to_string(),
            });
        }
        let mut succeeded = Vec::new();
        let mut failures = Vec::new();
        for window in windows {
            match self.acquire_window(plan, *window, from_param, to_param) {
                Ok(outcome) => succeeded.push(WindowResult {
                    window: *window,
                    snapshots: outcome.snapshots,
                    row_count: outcome.row_count,
                    page_count: outcome.page_count,
                    evidence: outcome.evidence,
                }),
                Err(error) => failures.push(WindowFailure {
                    window: *window,
                    class: error.class(),
                    code: error.code(),
                    detail: error.to_string(),
                }),
            }
        }
        if !failures.is_empty() {
            let failed = failures.len();
            return Err(AcquireError::PartiallyFailed {
                windows: windows.len(),
                failed,
                failures,
                succeeded,
            });
        }
        let snapshot_count: usize = succeeded.iter().map(|result| result.snapshots.len()).sum();
        let row_count: usize = succeeded.iter().map(|result| result.row_count).sum();
        let reused: usize = succeeded
            .iter()
            .flat_map(|result| result.snapshots.iter())
            .filter(|snapshot| snapshot.reused())
            .count();
        let binding = bind::bind(self.backend, plan.source_id, plan.required_basis)?;
        let mut evidence = ReuseEvidence::merged(
            self.reuse.for_source(plan.source_id),
            &succeeded
                .iter()
                .map(|result| result.evidence.clone())
                .collect::<Vec<_>>(),
        );
        if let Some(verified) = self.complete_refresh_date(plan.source_id)? {
            evidence.verified_through = Some(verified);
        }
        Ok(WindowOutcome {
            source_id: plan.source_id.as_str(),
            results: succeeded,
            snapshot_count,
            row_count,
            binding,
            reused,
            evidence,
        })
    }

    /// Fetch, freeze and index one window.
    ///
    /// Index rows are written for **every** frozen page whose envelope parsed —
    /// including pages of a window that then fails validation. The index is a
    /// rebuildable accelerator over the store (§9.4.2), so it must be a
    /// function of the store alone; admission is decided by `basis_status` plus
    /// this call's result, not by whether a row exists. A page whose envelope
    /// does not parse has no counts to index and is reported instead.
    fn acquire_pages(
        &self,
        plan: &AcquisitionPlan,
        window: Option<Segment>,
        from_param: &str,
        to_param: &str,
        only_page: Option<u32>,
    ) -> Result<(PageSet, Vec<FrozenSnapshot>, Vec<SelectedPage>), AcquireError> {
        if !plan.basis_status.admits(plan.required_basis) {
            return Err(AcquireError::NotAdmitted {
                detail: format!(
                    "{} basis {} is below the required {}",
                    plan.source_id.as_str(),
                    plan.basis_status.as_str(),
                    plan.required_basis.as_str()
                ),
            });
        }
        let fetched_at = super::freeze::now();
        let empty_policy = EmptyPolicy::for_window(window, self.today);
        let mut entries: Vec<PageEntry> = Vec::new();
        let mut page = plan.format.first_page();
        let mut page_count: Option<u32> = None;
        let mut failure: Option<AcquireError> = None;
        loop {
            let request = plan.request(page, window, from_param, to_param);
            let fingerprint = request.fingerprint();
            let cached = self
                .cache
                .as_ref()
                .and_then(|cache| cache.get(&fingerprint));
            let (state, envelope) = match cached {
                Some(hit) => (
                    PageState::Fetched {
                        bytes: hit.frozen.clone(),
                        cached: true,
                    },
                    hit.envelope.clone(),
                ),
                None => match self.reuse_page(plan, &fingerprint)? {
                    Some((state, envelope)) => (state, envelope),
                    None => {
                        let http = HttpRequest {
                            url: request.display_url(),
                            headers: default_headers(plan.source_id),
                        };
                        let response = match self.client.get(&http) {
                            Ok(response) => response,
                            Err(error) => {
                                failure = Some(AcquireError::Unavailable {
                                    detail: format!("{}: {error}", error.code()),
                                });
                                break;
                            }
                        };
                        if let Err(error) = validate::require_status(response.status) {
                            failure = Some(AcquireError::Invalid {
                                code: error.code(),
                                detail: error.to_string(),
                            });
                            break;
                        }
                        // Frozen before parsing: a parse failure keeps the bytes
                        // (§6.3). A failed parse never reaches the cache.
                        let bytes = freeze_bytes(
                            self.store,
                            &request,
                            &response,
                            plan.format.parser_version(),
                            plan.basis_status,
                            &fetched_at,
                        )?;
                        let envelope = match validate::parse_page(
                            plan.format,
                            validate::declared_tabkey(&plan.params),
                            &response.body,
                        ) {
                            Ok(envelope) => envelope,
                            Err(error) => {
                                failure = Some(AcquireError::Invalid {
                                    code: error.code(),
                                    detail: error.to_string(),
                                });
                                break;
                            }
                        };
                        let page_state = Arc::new(CachedPage {
                            frozen: bytes.clone(),
                            envelope: envelope.clone(),
                        });
                        if let Some(cache) = &self.cache {
                            cache.insert(fingerprint, page_state);
                        }
                        (
                            PageState::Fetched {
                                bytes,
                                cached: false,
                            },
                            envelope,
                        )
                    }
                },
            };
            if envelope.page != page {
                failure = Some(AcquireError::Invalid {
                    code: "source_invalid",
                    detail: format!(
                        "requested page {page} but the envelope reports page {}",
                        envelope.page
                    ),
                });
                break;
            }
            // A list service that stopped honouring `pageHelp.pageSize` would
            // answer page 1 only; taking the first page as the whole window
            // would silently truncate it, so such a plan fails closed instead
            // of paging. The frozen page is still indexed (§3: envelope
            // failures are window verdicts, not index state).
            if plan.single_page_only && envelope.page_count > 1 {
                let declared = match envelope.total {
                    Some(total) => total.to_string(),
                    None => "unknown".to_string(),
                };
                let detail = format!(
                    "declared total {declared} but collected {} row(s): the source reports pageCount \
                     {} for a request that sends pageHelp.pageSize; the single-page dialect no \
                     longer returns the whole list",
                    envelope.rows, envelope.page_count,
                );
                entries.push(PageEntry { state, envelope });
                failure = Some(AcquireError::Invalid {
                    code: "page_size_ignored",
                    detail,
                });
                break;
            }
            match page_count {
                None => {
                    if envelope.page_count > plan.max_pages {
                        failure = Some(AcquireError::Invalid {
                            code: "page_count_exceeds_bound",
                            detail: format!(
                                "pageCount {} exceeds the bound {}",
                                envelope.page_count, plan.max_pages
                            ),
                        });
                        break;
                    }
                    page_count = Some(envelope.page_count);
                }
                Some(expected) if expected != envelope.page_count => {
                    failure = Some(AcquireError::Invalid {
                        code: "truncated",
                        detail: format!(
                            "page {page} reports pageCount {} but the window started with {expected}",
                            envelope.page_count
                        ),
                    });
                    break;
                }
                Some(_) => {}
            }
            entries.push(PageEntry { state, envelope });
            if only_page.is_some() || page >= page_count.unwrap_or(0) {
                break;
            }
            page += 1;
        }
        // Index first, decide second: the rows are facts about the store, and
        // replay must reproduce exactly these pages even when the window fails
        // (`empty data` and `truncated` are window verdicts, not index state).
        let (snapshots, selected) = self.index_pages(&entries)?;
        if let Some(failure) = failure {
            return Err(failure);
        }
        let envelopes: Vec<PageEnvelope> =
            entries.into_iter().map(|entry| entry.envelope).collect();
        if only_page.is_some() {
            // A probe sees only page 1. The page's envelope still names the
            // table's page count; validating it as a complete set would reject
            // every multi-page table, which is exactly what a probe must not
            // do. Row/page coherence is checked by the caller against the
            // frozen bytes it selected.
            let first = envelopes
                .first()
                .cloned()
                .ok_or_else(|| AcquireError::Invalid {
                    code: "source_invalid",
                    detail: "the probe produced no page".to_string(),
                })?;
            return Ok((
                PageSet {
                    pages: envelopes,
                    row_count: first.rows,
                    page_count: first.page_count,
                },
                snapshots,
                selected,
            ));
        }
        let set = validate::validate_pages(&envelopes, empty_policy, plan.zero_policy).map_err(
            |error| AcquireError::Invalid {
                code: error.code(),
                detail: error.to_string(),
            },
        )?;
        Ok((set, snapshots, selected))
    }

    /// Write the index rows of the pages this call downloaded and describe every
    /// selected page for the evidence (`selected_by`, `as_of`).
    ///
    /// A reused page is already indexed: it contributes no write, and its row
    /// keeps the fetch date it was first observed with (reuse never advances
    /// `fetched_at`/`as_of`, design §5.3).
    fn index_pages(
        &self,
        entries: &[PageEntry],
    ) -> Result<(Vec<FrozenSnapshot>, Vec<SelectedPage>), AcquireError> {
        let fetched: Vec<(FrozenBytes, PageEnvelope)> = entries
            .iter()
            .filter_map(|entry| match &entry.state {
                PageState::Fetched { bytes, .. } => Some((bytes.clone(), entry.envelope.clone())),
                PageState::Reused { .. } => None,
            })
            .collect();
        let recorded = if fetched.is_empty() {
            Vec::new()
        } else {
            record_snapshots(self.backend, &fetched)?
        };
        let mut snapshots = Vec::with_capacity(entries.len());
        let mut selected = Vec::with_capacity(entries.len());
        let mut next_recorded = recorded.into_iter();
        for entry in entries {
            let (snapshot, selected_by) = match &entry.state {
                PageState::Reused { row } => (
                    FrozenSnapshot {
                        row: row.clone(),
                        appended_manifest_line: false,
                        inserted_index_row: false,
                    },
                    SelectedBy::Reuse,
                ),
                PageState::Fetched { cached, .. } => {
                    let mut snapshot = next_recorded
                        .next()
                        .expect("one indexed row per fetched page");
                    // A cache hit appended nothing: the line came from the call
                    // that populated the cache, so the snapshot is a re-use.
                    if *cached {
                        snapshot.appended_manifest_line = false;
                    }
                    let selected_by = if *cached {
                        SelectedBy::RunCache
                    } else {
                        SelectedBy::Fetch
                    };
                    (snapshot, selected_by)
                }
            };
            selected.push(SelectedPage {
                snapshot_id: snapshot.row.snapshot_id.clone(),
                as_of: snapshot.row.as_of.clone(),
                selected_by,
            });
            snapshots.push(snapshot);
        }
        Ok((snapshots, selected))
    }

    /// Read-through reuse of one page (design §5.2).
    ///
    /// Returns `None` when the rule is off or the fingerprint was never seen:
    /// the caller then fetches. When the index holds a snapshot for this exact
    /// request, its bytes are read back with `read_verified` (a missing or
    /// tampered blob is `source_store_corrupt`) and parsed again — the index row
    /// is never used as the parsed result. A selected snapshot older than the
    /// rule's `H` fails closed with `source_refresh_overdue` instead of serving
    /// increasingly stale bytes (§5.1).
    fn reuse_page(
        &self,
        plan: &AcquisitionPlan,
        fingerprint: &str,
    ) -> Result<Option<(PageState, PageEnvelope)>, AcquireError> {
        let policy = self.reuse.for_source(plan.source_id);
        if !policy.enabled.is_enabled() {
            return Ok(None);
        }
        let rows = self
            .backend
            .read_exchange_snapshots_for_fingerprint(plan.source_id.as_str(), fingerprint)?;
        let Some(row) = reuse::latest_candidate(&rows, plan.source_id, fingerprint) else {
            return Ok(None);
        };
        // A complete refresh of the source is the only thing that confirms the
        // fingerprint set has not changed; the selected page's own `as_of` is
        // only the fallback when no run recorded one.
        let confirmed = match self.complete_refresh_date(plan.source_id)? {
            Some(verified_through) => verified_through,
            None => row.as_of.clone(),
        };
        let Some(age) = reuse::age_days(&confirmed, self.today) else {
            return Err(AcquireError::Invalid {
                code: "source_store_corrupt",
                detail: format!(
                    "{} request fingerprint {} is confirmed through {:?}, which is not a date",
                    plan.source_id.as_str(),
                    fingerprint,
                    confirmed
                ),
            });
        };
        if age > i64::from(policy.max_age_days) {
            return Err(AcquireError::RefreshOverdue {
                detail: format!(
                    "{} was last completely verified on {confirmed} ({age} day(s) ago), beyond \
                     H={} days; run refresh_exchange_sources full before deriving from it",
                    plan.source_id.as_str(),
                    policy.max_age_days
                ),
            });
        }
        let bytes = self.store.read_verified(&row.snapshot_id)?;
        let Some(format) = PagingFormat::from_parser_version(&row.parser_version) else {
            return Err(AcquireError::Invalid {
                code: "unknown_parser",
                detail: format!(
                    "indexed snapshot {} records parser version {:?}",
                    row.snapshot_id, row.parser_version
                ),
            });
        };
        let envelope =
            validate::parse_page(format, validate::declared_tabkey(&plan.params), &bytes).map_err(
                |error| AcquireError::Invalid {
                    code: error.code(),
                    detail: format!(
                        "indexed snapshot {} no longer parses: {error}",
                        row.snapshot_id
                    ),
                },
            )?;
        // The row's counts are a rebuild of these bytes; a disagreement means
        // the index (or the bytes) was altered behind the store's back.
        let expected_pages = row.page_count.unwrap_or(envelope.page_count);
        if envelope.rows != row.row_count || envelope.page_count != expected_pages {
            return Err(AcquireError::Invalid {
                code: "source_store_corrupt",
                detail: format!(
                    "indexed snapshot {} reports {} row(s) over {} page(s) but its verified bytes \
                     parse to {} row(s) over {}",
                    row.snapshot_id,
                    row.row_count,
                    expected_pages,
                    envelope.rows,
                    envelope.page_count
                ),
            });
        }
        Ok(Some((PageState::Reused { row: row.clone() }, envelope)))
    }

    /// Latest complete successful refresh date for `plan`'s source, if reuse
    /// is on for it. With reuse off the R1 fallback (`oldest as_of`) stays.
    fn complete_refresh_date(
        &self,
        source_id: crate::exchange_source::SourceId,
    ) -> Result<Option<String>, AcquireError> {
        if !self.reuse.for_source(source_id).enabled.is_enabled() {
            return Ok(None);
        }
        Ok(self
            .backend
            .latest_complete_verified_through(source_id.as_str())?)
    }

    fn finish(
        &self,
        plan: &AcquisitionPlan,
        pages: PageSet,
        snapshots: Vec<FrozenSnapshot>,
        selected: Vec<SelectedPage>,
    ) -> Result<AcquisitionOutcome, AcquireError> {
        // The binding is the source's, not this window's: `bind` reads every
        // frozen snapshot of `source_id` from the index (§6.3 "bind"), so two
        // consumers acquiring different windows of one source agree on it.
        let binding = bind::bind(self.backend, plan.source_id, plan.required_basis)?;
        let reused = snapshots
            .iter()
            .filter(|snapshot| snapshot.reused())
            .count();
        let mut evidence =
            ReuseEvidence::from_pages(self.reuse.for_source(plan.source_id), &selected);
        // The evidence records the date the `H` gate measured: the newest
        // complete run's `verified_through` when one exists, and the pages'
        // own `as_of` fallback otherwise. A page keeps its first observation
        // date forever, so its date alone cannot say the set was re-confirmed
        // (XDM-156 ③).
        if let Some(verified) = self.complete_refresh_date(plan.source_id)? {
            evidence.verified_through = Some(verified);
        }
        Ok(AcquisitionOutcome {
            source_id: plan.source_id.as_str(),
            snapshots,
            row_count: pages.row_count,
            page_count: pages.page_count,
            binding,
            reused,
            evidence,
        })
    }
}
