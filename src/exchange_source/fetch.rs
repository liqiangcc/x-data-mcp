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

use chrono::{NaiveDate, Utc};

use super::bind::{self, SnapshotBinding};
use super::freeze::{FrozenBytes, FrozenSnapshot, freeze_bytes, record_snapshots};
use super::request::{ExchangeRequest, PagingFormat, Segment};
use super::transport::{Client, HttpRequest};
use super::validate::{self, EmptyPolicy, FailureClass, PageEnvelope, PageSet};
use super::{BasisStatus, SourceId, default_headers};
use crate::source_store::SourceStore;
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
            segment.apply(&mut params, from_param, to_param);
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
}

/// One succeeded window of a multi-window run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowResult {
    pub window: Segment,
    pub snapshots: Vec<FrozenSnapshot>,
    pub row_count: usize,
    pub page_count: u32,
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
}

#[derive(Debug, thiserror::Error)]
pub enum AcquireError {
    #[error("source_unavailable: {detail}")]
    Unavailable { detail: String },
    #[error("{code}: {detail}")]
    Invalid { code: &'static str, detail: String },
    #[error("source_not_admitted: {detail}")]
    NotAdmitted { detail: String },
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
}

impl AcquireError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable { .. } => "source_unavailable",
            Self::Invalid { code, .. } => code,
            Self::NotAdmitted { .. } => "source_not_admitted",
            Self::PartiallyFailed { .. } => "partially_failed",
            Self::Freeze(error) => error.code(),
            Self::Bind(error) => error.code(),
        }
    }

    pub fn class(&self) -> FailureClass {
        match self {
            Self::Unavailable { .. } => FailureClass::SourceUnavailable,
            Self::PartiallyFailed { .. } => FailureClass::PartiallyFailed,
            Self::Invalid { .. } | Self::NotAdmitted { .. } => FailureClass::SourceInvalid,
            Self::Freeze(_) | Self::Bind(_) => FailureClass::SourceInvalid,
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
}

impl<'a> Entry<'a> {
    pub fn new(store: &'a SourceStore, backend: &'a MarketBackend, client: &'a Client) -> Self {
        Self {
            store,
            backend,
            client,
            today: Utc::now().date_naive(),
            cache: None,
        }
    }

    /// Share one run-level download cache (§10.1). Pages another item of the
    /// same run already fetched and froze are not downloaded twice.
    pub fn with_cache(mut self, cache: Arc<RunCache>) -> Self {
        self.cache = Some(cache);
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
        let (frozen, pages) = self.acquire_pages(plan, None, "", "")?;
        self.finish(plan, frozen, pages)
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
        let (frozen, pages) = self.acquire_pages(plan, Some(window), from_param, to_param)?;
        self.finish(plan, frozen, pages)
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
        Ok(WindowOutcome {
            source_id: plan.source_id.as_str(),
            results: succeeded,
            snapshot_count,
            row_count,
            binding,
            reused,
        })
    }

    fn acquire_pages(
        &self,
        plan: &AcquisitionPlan,
        window: Option<Segment>,
        from_param: &str,
        to_param: &str,
    ) -> Result<(Vec<FrozenBytes>, PageSet), AcquireError> {
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
        let policy = EmptyPolicy::for_window(window, self.today);
        let mut frozen = Vec::new();
        let mut envelopes = Vec::new();
        let mut page = plan.format.first_page();
        let mut page_count: Option<u32> = None;
        loop {
            let request = plan.request(page, window, from_param, to_param);
            let fingerprint = request.fingerprint();
            let cached = self
                .cache
                .as_ref()
                .and_then(|cache| cache.get(&fingerprint));
            let from_cache = cached.is_some();
            let page_state = match cached {
                Some(hit) => hit,
                None => {
                    let http = HttpRequest {
                        url: request.display_url(),
                        headers: default_headers(plan.source_id),
                    };
                    let response =
                        self.client
                            .get(&http)
                            .map_err(|error| AcquireError::Unavailable {
                                detail: format!("{}: {error}", error.code()),
                            })?;
                    validate::require_status(response.status).map_err(|error| {
                        AcquireError::Invalid {
                            code: error.code(),
                            detail: error.to_string(),
                        }
                    })?;
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
                    let envelope =
                        validate::parse_page(plan.format, &response.body).map_err(|error| {
                            AcquireError::Invalid {
                                code: error.code(),
                                detail: error.to_string(),
                            }
                        })?;
                    let page_state = Arc::new(CachedPage {
                        frozen: bytes,
                        envelope,
                    });
                    if let Some(cache) = &self.cache {
                        cache.insert(fingerprint, page_state.clone());
                    }
                    page_state
                }
            };
            let envelope = page_state.envelope.clone();
            if envelope.page != page {
                return Err(AcquireError::Invalid {
                    code: "source_invalid",
                    detail: format!(
                        "requested page {page} but the envelope reports page {}",
                        envelope.page
                    ),
                });
            }
            match page_count {
                None => {
                    if envelope.page_count > plan.max_pages {
                        return Err(AcquireError::Invalid {
                            code: "page_count_exceeds_bound",
                            detail: format!(
                                "pageCount {} exceeds the bound {}",
                                envelope.page_count, plan.max_pages
                            ),
                        });
                    }
                    page_count = Some(envelope.page_count);
                }
                Some(expected) if expected != envelope.page_count => {
                    return Err(AcquireError::Invalid {
                        code: "truncated",
                        detail: format!(
                            "page {page} reports pageCount {} but the window started with {expected}",
                            envelope.page_count
                        ),
                    });
                }
                Some(_) => {}
            }
            envelopes.push(envelope);
            let mut frozen_page = page_state.frozen.clone();
            if from_cache {
                // This call appended nothing: the line came from the call that
                // populated the cache, so the snapshot is a re-use.
                frozen_page.appended_manifest_line = false;
            }
            frozen.push(frozen_page);
            if page >= page_count.unwrap_or(0) {
                break;
            }
            page += 1;
        }
        let pages = validate::validate_pages(&envelopes, policy).map_err(|error| {
            AcquireError::Invalid {
                code: error.code(),
                detail: error.to_string(),
            }
        })?;
        Ok((frozen, pages))
    }

    fn finish(
        &self,
        plan: &AcquisitionPlan,
        frozen: Vec<FrozenBytes>,
        pages: PageSet,
    ) -> Result<AcquisitionOutcome, AcquireError> {
        let pairs: Vec<(FrozenBytes, PageEnvelope)> = frozen
            .into_iter()
            .zip(pages.pages.iter().cloned())
            .collect();
        let snapshots = record_snapshots(self.backend, &pairs)?;
        // The binding is the source's, not this window's: `bind` reads every
        // frozen snapshot of `source_id` from the index (§6.3 "bind"), so two
        // consumers acquiring different windows of one source agree on it.
        let binding = bind::bind(self.backend, plan.source_id, plan.required_basis)?;
        let reused = snapshots
            .iter()
            .filter(|snapshot| snapshot.reused())
            .count();
        Ok(AcquisitionOutcome {
            source_id: plan.source_id.as_str(),
            snapshots,
            row_count: pages.row_count,
            page_count: pages.page_count,
            binding,
            reused,
        })
    }
}
