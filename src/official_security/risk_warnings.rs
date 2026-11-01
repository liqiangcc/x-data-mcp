//! Historical ST/*ST source adapter (issue-308, issue-446 PR-10, D-58).
//!
//! Layer position (separation point 1): this is a **source adapter**. It turns
//! frozen exchange responses into the domain values
//! [`SzNameChange`](crate::security_master::risk_warnings::SzNameChange) and
//! [`RiskWarningStatusEvent`](crate::security_master::risk_warnings::RiskWarningStatusEvent)
//! and hands them to the domain assembly. It never opens a writer and never
//! invents an interval.
//!
//! ```text
//! plans() ──► exchange_source::Entry (PR-2) ──► frozen bytes (source store)
//!                                                     │
//!                                  parse (this module, PARSER_VERSION)
//!                                                     ▼
//!             assemble_sz / assemble_sh (domain rules, D-58 tiers)
//!                                                     │
//!                               RiskWarningSet ──► writer (PR-10)
//! ```
//!
//! ## Admission tiers (D-58)
//!
//! - SZ: the dated `SSGSGMXX`/`tab2` short-name change table is
//!   `APPROVED_SOURCE`; its change date opens/closes the interval.
//! - SH: the announcement index is `APPROVED_SOURCE` at the event level
//!   (implementation/revocation + announcement day). The index archive starts
//!   2000-01, but a title-filtered query only starts matching in **2004Q1**:
//!   every 2000–2003 quarter answers `pageCount=0`, so those years stay
//!   `not_observed` permanently instead of being claimed as covered
//!   (XDM-151 §7). The effective day is owner-risk: an explicit date in the
//!   title when present, otherwise the announcement day. Real data uses the
//!   announcement day for all events (no title carries an explicit date); it is
//!   recorded as `OWNER_RISK_ACCEPTED` in the item evidence, never claimed as a
//!   dated name history.
//! - SH before 2000-01 and BJ are not requested and not inferred.
//!
//! ## 0 rows
//!
//! A filtered quarter with no rows is `unverified_zero` from PR-2, not proof
//! that nothing happened. The adapter records it as a `not_observed` coverage
//! window; the domain cuts every open SH interval at that gap. No row ever
//! says `normal` because evidence is absent. The source states an empty
//! quarter as `pageCount=0` with `data: []` (2000Q1 is always one, #446 §5);
//! PR-2 reports that with the same `unverified_zero` as a `pageCount=1` empty
//! page, so one branch covers both shapes.
//!
//! ## Failure semantics
//!
//! Acquisition failures come from PR-2 (`source_unavailable`,
//! `source_invalid`, `source_not_admitted`, `partially_failed`); a page that
//! is not the recorded row shape is `source_invalid`. The caller turns any of
//! them into a failed item, so the previous version remains readable.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use serde_json::{Value, json};

use crate::exchange_source::{
    AcquireError, AcquisitionPlan, Client, Entry, PagingFormat, RequestError, RunCache,
    SnapshotBinding, SourceId, TransportError, quarter_segments, validate,
};
use crate::security_master::assembly::PendingSource;
use crate::security_master::risk_warnings::{
    self, CoverageWindow, RiskWarningAction, RiskWarningSet, RiskWarningStatusEvent, SzNameChange,
};
use crate::security_master::{Exchange, RiskStatus};
use crate::source_store::{Policy, SourceStore, StoreError};
use crate::sqlite_market::backend::MarketBackend;

/// Business-field parser version of this adapter. It is registered on the
/// derived item's outcome evidence, not in PR-2's envelope manifest. v1 → v2:
/// the SZSE rows come from the block the request declared (`TABKEY=tab2`)
/// instead of the first block with a `data` array, so a page with an empty
/// `tab1` no longer parses as an empty table; old evidence stays
/// distinguishable and is not replayed under the new meaning. v2 → v3: the SSE
/// bulletin `pageHelp.data` elements are announcement *groups* (`[[doc,…],…]`)
/// as well as the old flat rows, so a page is read document by document and
/// events are de-duplicated by `(bulletin id, event)` instead of the first
/// document of a bulletin winning (XDM-151 §4/§7). Flat v2 snapshots still
/// parse: one object is one group.
pub const PARSER_VERSION: &str = "risk-warnings-v3";
/// Environment variable naming the deployment's source store, shared with
/// A-1/A-2 (`X_DATA_MCP_SOURCE_STORE`).
pub const SOURCE_STORE_ENV: &str = crate::source_store::CONFIG_ENV;
/// SZSE `ShowReport` data endpoint.
pub const SZSE_URL: &str = "https://www.szse.cn/api/report/ShowReport/data";
/// SZSE short-name change catalog (`SSGSGMXX`, tab 2).
pub const SZSE_SHORT_NAME_CATALOG: &str = "SSGSGMXX";
/// `TABKEY` of the SZSE short-name change table (简称变更): the plan declares
/// it and the parser selects exactly that block.
pub const SZSE_SHORT_NAME_TABKEY: &str = "tab2";
/// SSE company-bulletin index endpoint.
pub const SSE_BULLETIN_URL: &str =
    "https://query.sse.com.cn/security/stock/queryCompanyBulletinNew.do";
/// Title filters that cover the pre- and post-2012 warning vocabulary.
pub const SSE_TITLE_FILTERS: [&str; 2] = ["风险警示", "特别处理"];

const SZSE_A_PREFIXES: [&str; 7] = ["000", "001", "002", "003", "300", "301", "302"];
const SZSE_B_PREFIX: &str = "200";

/// One acquisition of this adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    SzseShortName,
    SseRiskWarning,
    SseSpecialTreatment,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::SzseShortName => "szse_short_name",
            Self::SseRiskWarning => "sse_bulletin_risk_warning",
            Self::SseSpecialTreatment => "sse_bulletin_special_treatment",
        }
    }

    pub fn source_id(self) -> SourceId {
        match self {
            Self::SzseShortName => SourceId::SzseShortName,
            Self::SseRiskWarning | Self::SseSpecialTreatment => SourceId::SseBulletin,
        }
    }

    fn sse(self) -> bool {
        !matches!(self, Self::SzseShortName)
    }
}

/// The three acquisitions: one SZSE table (paged), plus two SSE bulletin title
/// filters (quarterly windows). The list is static so a rerun requests the
/// same fingerprints in the same order.
pub fn plans() -> Vec<(Kind, AcquisitionPlan)> {
    let szse = AcquisitionPlan::new(
        SourceId::SzseShortName,
        SZSE_URL,
        PagingFormat::SzseMetadata,
    )
    .param("SHOWTYPE", "JSON")
    .param("CATALOGID", SZSE_SHORT_NAME_CATALOG)
    .param("TABKEY", SZSE_SHORT_NAME_TABKEY);
    let bulletin = |kind: Kind, title: &str| {
        // The bulletin index is the one SSE endpoint that pages by
        // `pageHelp.beginPage`/`endPage` and only accepts `pageSize<=100`
        // (XDM-151 §3); the page size is written by the dialect, once.
        let plan = AcquisitionPlan::new(
            SourceId::SseBulletin,
            SSE_BULLETIN_URL,
            PagingFormat::SsePageHelpBeginPage,
        )
        .param("isPagination", "true")
        .param("TITLE", title);
        (kind, plan)
    };
    vec![
        (Kind::SzseShortName, szse),
        bulletin(Kind::SseRiskWarning, SSE_TITLE_FILTERS[0]),
        bulletin(Kind::SseSpecialTreatment, SSE_TITLE_FILTERS[1]),
    ]
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
    Domain(#[from] risk_warnings::Error),
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
            Self::Invalid { code, .. } => code,
        }
    }

    fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid {
            code: "source_invalid",
            detail: detail.into(),
        }
    }
}

/// An admitted row set plus everything the derived item must register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    pub set: RiskWarningSet,
    pub bindings: Vec<SnapshotBinding>,
    /// `label → snapshot ids`, in acquisition order.
    pub snapshot_ids: BTreeMap<&'static str, Vec<String>>,
    pub sz_rows: usize,
    pub sz_skipped_codes: usize,
    /// Bulletin documents read (attachments included).
    pub sh_rows: usize,
    pub sh_events: usize,
    pub sh_ignored_rows: usize,
    /// Distinct announcement ids among the documents read.
    pub sh_bulletins: usize,
    /// Announcement ids that carry more than one document (body + attachment).
    pub sh_multi_doc_bulletins: usize,
    /// Largest document count of one announcement.
    pub sh_max_docs_per_bulletin: usize,
}

impl Admitted {
    pub fn evidence(&self) -> Value {
        json!({
            "adapter": "historical ST/risk warnings (D-58)",
            "parser_version": PARSER_VERSION,
            "source_bindings": self.bindings,
            "snapshot_ids": self.snapshot_ids,
            "sz_name_change_rows": self.sz_rows,
            "sz_skipped_codes": self.sz_skipped_codes,
            "sh_bulletin_rows": self.sh_rows,
            "sh_status_events": self.sh_events,
            "sh_ignored_rows": self.sh_ignored_rows,
            // XDM-151 §7: one announcement is one index entry that may carry
            // its body plus attachments. Recording the shape lets an operator
            // tell "the endpoint stopped grouping" from "the attachment titles
            // changed" when the event count moves.
            "sh_docs_per_bulletin": {
                "documents_read": self.sh_rows,
                "distinct_bulletins": self.sh_bulletins,
                "multi_document_bulletins": self.sh_multi_doc_bulletins,
                "max": self.sh_max_docs_per_bulletin,
            },
            "interval_rows": self.set.rows.len(),
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
            // D-58 says the SH effective day is not in the announcement source;
            // this is the owner-risk anchor the writer records.
            "effective_date_basis": {
                "sz": "source_change_date (APPROVED_SOURCE)",
                "sh": "explicit title date when present, otherwise announcement day (OWNER_RISK_ACCEPTED)",
                "sh_status": "owner_risk_accepted",
            },
            "absence_policy": "not_observed is a coverage window, never a `normal` row; \
                a missing interval remains unknown",
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

/// Resolve the source store and fetch client, then acquire.
///
/// Under [`Policy::Optional`] not configured is not a fetch failure: the item
/// records `pending_source` and writes zero rows. Under [`Policy::Required`]
/// (ops entry on a compact backend) a missing or unreadable store fails the item
/// with `source_store_unconfigured` instead of a silent zero-row success. A
/// configured store whose fetch fails is a real failure. `cache` is the run-level download
/// cache (§10.1): the SZSE table is shared with `derived:security_names`
/// (PR-12), which asks for the same request in the same run.
pub fn run_source(
    market: &MarketBackend,
    cache: Arc<RunCache>,
    policy: Policy,
) -> Result<RunSource, Error> {
    let Some(store) = crate::source_store::configured(policy).map_err(|error| Error::Invalid {
        code: error.code(),
        detail: error.detail().to_string(),
    })?
    else {
        return Ok(RunSource::Pending(PendingSource {
            dataset: risk_warnings::DATASET_KIND,
            source: "exchange_risk_warning_announcement",
            reason: "the M3 source store is not configured for this process \
                     (X_DATA_MCP_SOURCE_STORE); historical ST stays pending_source",
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

/// One frozen SZSE short-name table: the dated change rows both derived
/// datasets read (ST intervals, PR-10; name intervals, PR-12), the per-page
/// snapshot ids and the counters the item evidence reports.
#[derive(Debug, Clone)]
pub(crate) struct SzShortNameTable {
    pub changes: Vec<SzNameChange>,
    pub snapshot_ids: Vec<String>,
    pub rows: usize,
    pub skipped_codes: usize,
    pub binding: SnapshotBinding,
}

/// The `TABKEY` the static `SSGSGMXX` plan declares, read back from the plan
/// instead of hard-coded at a second site: the block a page is parsed from is
/// the block the request asked for, and the live acquisition and both replay
/// entries read the same value.
pub(crate) fn szse_declared_tabkey() -> Option<String> {
    plans()
        .into_iter()
        .find(|(kind, _)| *kind == Kind::SzseShortName)
        .and_then(|(_, plan)| validate::declared_tabkey(&plan.params).map(str::to_string))
}

/// Acquire, freeze and parse the SZSE `SSGSGMXX`/`tab2` table through PR-2's
/// entry. This is the only acquisition of the dated name source; a second
/// consumer must call it rather than open its own request or parser.
///
/// An empty A-share change set is not an answer: the table is a whole-market
/// snapshot, so the caller decides (both consumers fail closed).
pub(crate) fn acquire_sz_short_name_table(
    entry: &Entry<'_>,
    store: &SourceStore,
) -> Result<SzShortNameTable, Error> {
    let (_, plan) = plans()
        .into_iter()
        .find(|(kind, _)| *kind == Kind::SzseShortName)
        .expect("the SZSE plan is static");
    // The declared tab travels with the request into the parser: a page whose
    // `tab1` is an empty tab must not be read as an empty table.
    let declared_tabkey = validate::declared_tabkey(&plan.params).map(str::to_string);
    let outcome = entry.acquire(&plan)?;
    let mut changes = Vec::new();
    let mut rows = 0usize;
    let mut skipped_codes = 0usize;
    let mut snapshot_ids = Vec::new();
    for snapshot in &outcome.snapshots {
        let bytes = store.read_verified(snapshot.snapshot_id())?;
        let (mut parsed, page_rows, page_skipped) =
            parse_sz_page(&bytes, snapshot.snapshot_id(), declared_tabkey.as_deref())?;
        rows += page_rows;
        skipped_codes += page_skipped;
        changes.append(&mut parsed);
        snapshot_ids.push(snapshot.snapshot_id().to_string());
    }
    Ok(SzShortNameTable {
        changes,
        snapshot_ids,
        rows,
        skipped_codes,
        binding: outcome.binding,
    })
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
    let mut snapshot_ids: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let (sz_kind, _) = plans()
        .into_iter()
        .find(|(kind, _)| *kind == Kind::SzseShortName)
        .expect("the SZSE plan is static");
    let sz = acquire_sz_short_name_table(&entry, store)?;
    let sz_changes = sz.changes;
    let sz_skipped = sz.skipped_codes;
    let sz_snapshot_rows = sz.rows;
    snapshot_ids.insert(sz_kind.label(), sz.snapshot_ids);
    let sz_days: Vec<NaiveDate> = sz_changes.iter().map(|change| change.day).collect();
    let sz_coverage = match (sz_days.iter().min(), sz_days.iter().max()) {
        (Some(from), Some(to)) => vec![CoverageWindow {
            source_kind: "szse_name_history".into(),
            exchange: Exchange::Sz,
            from: *from,
            to: *to,
            observed: true,
            rows: sz_changes.len(),
            reason: "the frozen short-name table is non-empty".into(),
        }],
        _ => {
            return Err(Error::invalid(
                "the frozen SZSE short-name table has no A-share row",
            ));
        }
    };

    let mut sh_rows = 0usize;
    let mut sh_ignored = 0usize;
    let mut sh_events = Vec::new();
    // De-duplication is by `(bulletin id, event)`, not by bulletin id alone: a
    // bulletin may carry more than one event document, and the same bulletin
    // legitimately matches both title filters (XDM-151 §7).
    let mut seen_events: BTreeSet<ShEventKey> = BTreeSet::new();
    // `id → documents`, merged across pages and both title filters; the max of
    // one id is what says "body + attachments".
    let mut docs_per_bulletin: BTreeMap<String, usize> = BTreeMap::new();
    let mut quarter_rows: BTreeMap<(NaiveDate, NaiveDate), usize> = BTreeMap::new();
    let mut quarter_observed: BTreeSet<(NaiveDate, NaiveDate)> = BTreeSet::new();
    let mut quarter_failures: BTreeMap<(NaiveDate, NaiveDate), String> = BTreeMap::new();
    let archive_start = NaiveDate::from_ymd_opt(2000, 1, 1).expect("2000-01-01 is valid");
    let windows = quarter_segments(archive_start, today)?;
    for (kind, plan) in plans().into_iter().filter(|(kind, _)| kind.sse()) {
        for window in &windows {
            let key = (window.start(), window.end());
            // Bulletins already read in *this* window and title filter. The
            // bulletin index that ignored `beginPage` answered page 1 again
            // with the page number echoed; when that repeated page also made
            // `total = rows × pageCount` the count check cannot see it, so a
            // repeated announcement is the evidence that page N was not page N
            // (XDM-151 §2/§3).
            let mut window_bulletins = BTreeSet::new();
            match entry.acquire_window(&plan, *window, "START_DATE", "END_DATE") {
                Ok(outcome) => {
                    quarter_observed.insert(key);
                    for snapshot in &outcome.snapshots {
                        let bytes = store.read_verified(snapshot.snapshot_id())?;
                        let page = parse_sh_page(&bytes, snapshot.snapshot_id())?;
                        if let Some(repeated) = page
                            .docs_per_bulletin
                            .keys()
                            .find(|id| window_bulletins.contains(*id))
                        {
                            return Err(Error::Invalid {
                                code: "truncated",
                                detail: format!(
                                    "{} window {}..{}: snapshot {} repeats bulletin {repeated} \
                                     already read in this window; the endpoint answered a \
                                     repeated page instead of the requested page",
                                    kind.label(),
                                    window.start(),
                                    window.end(),
                                    snapshot.snapshot_id()
                                ),
                            });
                        }
                        window_bulletins.extend(page.docs_per_bulletin.keys().cloned());
                        sh_rows += page.documents;
                        sh_ignored += page.ignored;
                        for (id, documents) in &page.docs_per_bulletin {
                            let seen = docs_per_bulletin.entry(id.clone()).or_default();
                            *seen = (*seen).max(*documents);
                        }
                        for event in &page.events {
                            if seen_events.insert(sh_event_key(event)) {
                                sh_events.push(event.clone());
                            }
                        }
                        quarter_rows
                            .entry(key)
                            .and_modify(|count| *count += page.events.len())
                            .or_insert(page.events.len());
                        snapshot_ids
                            .entry(kind.label())
                            .or_default()
                            .push(snapshot.snapshot_id().to_string());
                    }
                }
                Err(AcquireError::Invalid {
                    code: "unverified_zero",
                    ..
                }) => {
                    // A title-filtered quarter may genuinely have no matching
                    // announcement. That is not "no warning": keep it as an
                    // unobserved window and cut intervals at its start. The
                    // source states it as `pageCount=0` with `data: []`
                    // (2000Q1, #446 §5), and PR-2 reports that with the same
                    // `unverified_zero` as a `pageCount=1` empty page.
                    quarter_failures.insert(key, "unverified_zero".into());
                }
                Err(error) => return Err(Error::Acquire(error)),
            }
        }
    }
    let mut sh_coverage = Vec::with_capacity(windows.len());
    let mut gaps = Vec::new();
    for window in &windows {
        let key = (window.start(), window.end());
        let observed = quarter_observed.contains(&key);
        let rows = quarter_rows.get(&key).copied().unwrap_or(0);
        if !observed {
            gaps.push(key);
        }
        sh_coverage.push(CoverageWindow {
            source_kind: "exchange_announcement".into(),
            exchange: Exchange::Sh,
            from: window.start(),
            to: window.end(),
            observed,
            rows,
            reason: if observed {
                "at least one title filter returned an indexed window".into()
            } else {
                format!(
                    "not_observed: {}; no row is read as 'no risk warning'",
                    quarter_failures
                        .get(&key)
                        .map_or("no indexed announcement", String::as_str)
                )
            },
        });
    }
    let mut set = risk_warnings::assemble_sz(&sz_changes, sz_coverage)?;
    let sh = risk_warnings::assemble_sh(&sh_events, &gaps, sh_coverage)?;
    set.rows.extend(sh.rows);
    set.coverage.extend(sh.coverage);
    set.pending_sources.extend(sh.pending_sources);

    let mut bindings = Vec::new();
    for source in [SourceId::SzseShortName, SourceId::SseBulletin] {
        // The SH source may have no frozen page only if every window was
        // unobserved; in that case there is no binding to register and the
        // item must not claim an admitted set.
        match crate::exchange_source::bind::bind(
            market,
            source,
            crate::exchange_source::BasisStatus::ApprovedSource,
        ) {
            Ok(binding) => bindings.push(binding),
            Err(crate::exchange_source::BindError::NotFound { .. })
                if source == SourceId::SseBulletin && sh_events.is_empty() =>
            {
                return Err(Error::invalid(
                    "the SSE bulletin source produced no frozen snapshot; the ST event set \
                     cannot be bound",
                ));
            }
            Err(error) => return Err(Error::Acquire(AcquireError::Bind(error))),
        }
    }
    bindings.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    let multi_document_bulletins = docs_per_bulletin
        .values()
        .filter(|documents| **documents > 1)
        .count();
    let max_docs_per_bulletin = docs_per_bulletin.values().copied().max().unwrap_or(0);
    Ok(Admitted {
        set,
        bindings,
        snapshot_ids,
        sz_rows: sz_snapshot_rows,
        sz_skipped_codes: sz_skipped,
        sh_rows,
        sh_events: sh_events.len(),
        sh_ignored_rows: sh_ignored,
        sh_bulletins: docs_per_bulletin.len(),
        sh_multi_doc_bulletins: multi_document_bulletins,
        sh_max_docs_per_bulletin: max_docs_per_bulletin,
    })
}

/// Parse one SZSE page: `(changes, rows_read, skipped_non_a_codes)`.
///
/// `declared_tabkey` is the `TABKEY` the request declared (`tab2` for the
/// short-name table); the rows come from that block only. Shared with the
/// name-history adapter (`official_security::security_names`, PR-12): one
/// source row shape must not get a second parser.
pub(crate) fn parse_sz_page(
    bytes: &[u8],
    snapshot_id: &str,
    declared_tabkey: Option<&str>,
) -> Result<(Vec<SzNameChange>, usize, usize), Error> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| Error::invalid(format!("SZSE short-name page is not JSON: {error}")))?;
    let rows = sz_rows(&value, declared_tabkey)?;
    let total = rows.len();
    let mut changes = Vec::new();
    let mut skipped = 0usize;
    for row in rows {
        let code = text_field(&row, "zqdm")?;
        if code.starts_with(SZSE_B_PREFIX) {
            skipped += 1;
            continue;
        }
        if !SZSE_A_PREFIXES
            .iter()
            .any(|prefix| code.starts_with(prefix))
        {
            skipped += 1;
            continue;
        }
        let day = parse_day(&text_field(&row, "bgrq")?, "bgrq")?;
        let before = text_field(&row, "bgqzqjc")?;
        let after = text_field(&row, "bghzqjc")?;
        if before.trim().is_empty() || after.trim().is_empty() {
            return Err(Error::invalid(format!(
                "SZSE short-name row {code} {day} has an empty before/after name"
            )));
        }
        changes.push(SzNameChange {
            source_record_id: format!("{day}|{code}|{after}"),
            source_document_sha256: snapshot_id.to_string(),
            code,
            day,
            before_name: before,
            after_name: after,
        });
    }
    Ok((changes, total, skipped))
}

/// Rows of the block the request declared.
///
/// The 2026-10-30 real `SSGSGMXX` response has two blocks: `tab1` with
/// `data: []` and `tab2` with the change rows. Reading "the first block with a
/// `data` array" therefore read the empty tab and made a whole-market table
/// look empty (zero `changes`, `empty_name_set`). The selection is the same
/// `validate::szse_block` A-1/A-2 use (#446 D1): the declared tab must be
/// present, and without a declaration only a unique non-empty block is
/// accepted.
fn sz_rows(value: &Value, declared_tabkey: Option<&str>) -> Result<Vec<Value>, Error> {
    let (block, _selection) = validate::szse_block(value, declared_tabkey).map_err(|error| {
        let detail = match error {
            validate::ValidateError::Format { detail } => detail,
            other => other.to_string(),
        };
        Error::invalid(format!("SZSE short-name page: {detail}"))
    })?;
    block
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| Error::invalid("SZSE short-name page has no data row array"))
}

/// What one frozen SSE bulletin page yielded.
#[derive(Debug, Default)]
struct ShPage {
    events: Vec<RiskWarningStatusEvent>,
    /// Documents read, the attachments of a bulletin included.
    documents: usize,
    /// Documents that are not an admitted event (no A-share code, or a title
    /// that is not an implementation/revocation).
    ignored: usize,
    /// Documents per announcement id: one index entry is one announcement and
    /// may carry its body plus attachments (XDM-151 §4). The keys are also the
    /// page's bulletin ids.
    docs_per_bulletin: BTreeMap<String, usize>,
}

/// `(bulletin id, event)` is one admitted event. The id alone is not: one
/// announcement may carry several event documents, and de-duplicating on the
/// id would keep the first one and drop the rest (XDM-151 §7).
type ShEventKey = (String, RiskWarningAction, Option<RiskStatus>);

fn sh_event_key(event: &RiskWarningStatusEvent) -> ShEventKey {
    (event.source_record_id.clone(), event.action, event.status)
}

/// Parse one SSE bulletin page document by document.
///
/// `pageHelp.data` holds announcement **groups** in the real response
/// (`[[doc, doc], [doc], …]`, one group per `ORG_BULLETIN_ID`) and held flat
/// row objects in the snapshots frozen before XDM-151; both shapes parse, one
/// object being one group. Every document is classified on its own before
/// anything is de-duplicated, so a body that follows its attachment still
/// yields its event and two different events of one bulletin both survive.
fn parse_sh_page(bytes: &[u8], snapshot_id: &str) -> Result<ShPage, Error> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| Error::invalid(format!("SSE bulletin page is not JSON: {error}")))?;
    let groups = sh_groups(&value)?;
    let mut page = ShPage::default();
    // Within a page the same `(id, event)` is read once: both title filters
    // match the same bulletin in some years, and a frozen page can repeat a row.
    let mut seen: BTreeSet<ShEventKey> = BTreeSet::new();
    for group in groups {
        let Some(first) = group.first() else {
            return Err(Error::invalid(
                "SSE bulletin page has an empty announcement group",
            ));
        };
        let bulletin = bulletin_id(first).ok_or_else(|| {
            Error::invalid("SSE bulletin announcement group has no ORG_BULLETIN_ID or URL")
        })?;
        // Stated separately so the map borrow ends before the events below are
        // pushed: both live in `page`.
        *page.docs_per_bulletin.entry(bulletin).or_insert(0) += group.len();
        for document in &group {
            page.documents += 1;
            let Some(event) = sh_document_event(document, snapshot_id)? else {
                page.ignored += 1;
                continue;
            };
            if seen.insert(sh_event_key(&event)) {
                page.events.push(event);
            }
        }
    }
    Ok(page)
}

/// One announcement document as an event, or `None` when it is not one.
fn sh_document_event(
    row: &Value,
    snapshot_id: &str,
) -> Result<Option<RiskWarningStatusEvent>, Error> {
    let code = text_field(row, "SECURITY_CODE")?;
    if !is_sh_a_code(&code) {
        return Ok(None);
    }
    let title = text_field(row, "TITLE")?;
    let Some((action, status)) = classify_title(&title) else {
        return Ok(None);
    };
    let announced = parse_day(&text_field(row, "SSEDATE")?, "SSEDATE")?;
    let effective = explicit_effective_day(&title).unwrap_or(announced);
    let record_id = bulletin_id(row)
        .ok_or_else(|| Error::invalid(format!("SSE bulletin row {code} {announced} has no id")))?;
    Ok(Some(RiskWarningStatusEvent {
        code,
        day: effective,
        action,
        status,
        announced_at: Some(announced.to_string()),
        known_at: Some(announced),
        raw_status_code: match (action, status) {
            (RiskWarningAction::Implement, Some(status)) => {
                format!("implement:{}", status.as_str())
            }
            (RiskWarningAction::Revoke, _) => "revoke".into(),
            _ => return Err(Error::invalid("an implementation event has no status")),
        },
        source_kind: "exchange_announcement".into(),
        source_record_id: record_id,
        source_document_sha256: snapshot_id.to_string(),
    }))
}

/// The stable id of one document's announcement: `ORG_BULLETIN_ID`, or the
/// `URL` when the id is absent (old frozen rows).
fn bulletin_id(row: &Value) -> Option<String> {
    row.get("ORG_BULLETIN_ID")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            row.get("URL")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

/// The announcement groups of one `pageHelp.data` page.
///
/// Missing `pageHelp.data` stays `source_invalid`: an absent row array is a
/// different answer from an empty one, and the entry's envelope parser has
/// already refused it before this function runs on the live path (replay can
/// reach a frozen page directly, so it is checked here too).
fn sh_groups(value: &Value) -> Result<Vec<Vec<Value>>, Error> {
    let data = value
        .get("pageHelp")
        .and_then(|help| help.get("data"))
        .and_then(Value::as_array)
        .or_else(|| value.get("data").and_then(Value::as_array));
    let data = data
        .cloned()
        .ok_or_else(|| Error::invalid("SSE bulletin page has no data row array"))?;
    let mut groups = Vec::with_capacity(data.len());
    for element in data {
        match element {
            // Real shape: one announcement with its body and attachments.
            Value::Array(documents) => groups.push(documents),
            // v2 flat shape: one row object is one announcement.
            object @ Value::Object(_) => groups.push(vec![object]),
            _ => {
                return Err(Error::invalid(
                    "SSE bulletin page has a data element that is neither a row object \
                     nor a document group",
                ));
            }
        }
    }
    Ok(groups)
}

fn classify_title(title: &str) -> Option<(RiskWarningAction, Option<RiskStatus>)> {
    let title = title.trim();
    if !title.contains("风险警示") && !title.contains("特别处理") {
        return None;
    }
    // An application, a proposal, or a prediction is not an event. D-58
    // admits implementation/revocation, not intent.
    if title.contains("申请")
        || title.contains("拟")
        || title.contains("可能")
        || title.contains("将被")
    {
        return None;
    }
    let status = if title.contains("退市风险警示") {
        Some(RiskStatus::DelistingRiskWarning)
    } else if title.contains("特别处理") || title.contains("风险警示") {
        Some(RiskStatus::OtherRiskWarning)
    } else {
        None
    };
    if title.contains("撤销") || title.contains("取消") {
        return Some((RiskWarningAction::Revoke, None));
    }
    if title.contains("实施") || title.contains("实行") {
        return status.map(|status| (RiskWarningAction::Implement, Some(status)));
    }
    None
}

fn is_sh_a_code(code: &str) -> bool {
    code.len() == 6 && code.starts_with('6') && code.bytes().all(|byte| byte.is_ascii_digit())
}

/// A title such as “自2024年5月6日起” carries an explicit effective day; use
/// it when present. This is still an owner-risk anchor (the announcement index
/// is not a dated name history), but it is the publisher's own date.
fn explicit_effective_day(title: &str) -> Option<NaiveDate> {
    let chars: Vec<char> = title.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] != '年' {
            index += 1;
            continue;
        }
        let year_start = index.saturating_sub(4);
        let year: String = chars[year_start..index].iter().collect();
        if year.len() != 4 || !year.bytes().all(|byte| byte.is_ascii_digit()) {
            index += 1;
            continue;
        }
        let Some(month_end) = chars[index + 1..].iter().position(|ch| *ch == '月') else {
            index += 1;
            continue;
        };
        let month: String = chars[index + 1..index + 1 + month_end].iter().collect();
        let Some(day_end) = chars[index + 1 + month_end + 1..]
            .iter()
            .position(|ch| *ch == '日')
        else {
            index += 1;
            continue;
        };
        let day: String = chars[index + 1 + month_end + 1..index + 1 + month_end + 1 + day_end]
            .iter()
            .collect();
        let (Ok(year), Ok(month), Ok(day)) = (year.parse(), month.parse(), day.parse()) else {
            index += 1;
            continue;
        };
        if let Some(date) = NaiveDate::from_ymd_opt(year, month, day) {
            // Only accept a date followed by “起”; a date in “截至…” is not an
            // effective start.
            let after = index + 1 + month_end + 1 + day_end + 1;
            if chars.get(after) == Some(&'起') {
                return Some(date);
            }
        }
        index += 1;
    }
    None
}

fn text_field(row: &Value, field: &str) -> Result<String, Error> {
    let Some(value) = row.get(field).and_then(Value::as_str) else {
        return Err(Error::invalid(format!("row has no string field {field}")));
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::invalid(format!("row has an empty field {field}")));
    }
    Ok(value.to_string())
}

fn parse_day(text: &str, field: &str) -> Result<NaiveDate, Error> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .map_err(|_| Error::invalid(format!("{field}={text:?} is not a YYYY-MM-DD day")))
}

/// `--scope risk_warnings` replay support: parse frozen pages from the source
/// store and rebuild the domain set without any HTTP request.
///
/// `pages` are `(source_id, snapshot_id, bytes)` in manifest order. The
/// caller chooses which SZ short-name table snapshot is current; SSE bulletin
/// pages are parsed document by document and de-duplicated by
/// `(announcement id, event)`, like the live path.
pub fn replay(
    pages: &[(&str, String, Vec<u8>)],
    gaps: &[(NaiveDate, NaiveDate)],
    coverage: Vec<CoverageWindow>,
) -> Result<RiskWarningSet, Error> {
    // The replay entry reads the same declared `TABKEY` as the live
    // acquisition, so both paths select the same block from frozen bytes.
    let declared_tabkey = szse_declared_tabkey();
    let mut sz_changes = Vec::new();
    let mut sh_events = Vec::new();
    // Same key as the live path: `(bulletin id, event)` (XDM-151 §7).
    let mut seen_events: BTreeSet<ShEventKey> = BTreeSet::new();
    for (source_id, snapshot_id, bytes) in pages {
        match *source_id {
            "szse_short_name" => {
                let (mut changes, _, _) =
                    parse_sz_page(bytes, snapshot_id, declared_tabkey.as_deref())?;
                sz_changes.append(&mut changes);
            }
            "sse_bulletin" => {
                let page = parse_sh_page(bytes, snapshot_id)?;
                for event in page.events {
                    if seen_events.insert(sh_event_key(&event)) {
                        sh_events.push(event);
                    }
                }
            }
            _ => {}
        }
    }
    let mut sz_coverage = Vec::new();
    let mut sh_coverage = Vec::new();
    for window in coverage {
        if window.exchange == Exchange::Sz {
            sz_coverage.push(window);
        } else {
            sh_coverage.push(window);
        }
    }
    let mut set = risk_warnings::assemble_sz(&sz_changes, sz_coverage)?;
    let sh = risk_warnings::assemble_sh(&sh_events, gaps, sh_coverage)?;
    set.rows.extend(sh.rows);
    set.coverage.extend(sh.coverage);
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::exchange_source::{HttpRequest, HttpResponse, RetryPolicy, Transport};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("tests/fixtures/exchange-risk-warnings/{name}"))
            .expect("risk-warning fixture exists")
    }

    #[test]
    fn sz_page_parses_a_change_chain() {
        let (changes, rows, skipped) = parse_sz_page(
            &fixture("szse-name-changes.json"),
            "a",
            Some(SZSE_SHORT_NAME_TABKEY),
        )
        .expect("fixture parses");
        assert_eq!(rows, 4);
        assert_eq!(skipped, 1, "a 200 B-share row is skipped");
        assert_eq!(changes.len(), 3);
    }

    /// The plan's `TABKEY` is what the parser is told to select; both are read
    /// from one place so the request and the parsed block cannot drift apart.
    #[test]
    fn the_short_name_plan_declares_tab2() {
        assert_eq!(
            szse_declared_tabkey().as_deref(),
            Some(SZSE_SHORT_NAME_TABKEY)
        );
    }

    /// XDM-144 (Refs #446): the SSE title filters are the only non-ASCII
    /// parameters in these plans, and the helper only accepts ASCII URLs, so
    /// every plan's outbound request must already be percent-encoded.
    #[test]
    fn every_plan_has_an_ascii_outbound_url() {
        let mut seen_titles = Vec::new();
        for (kind, plan) in plans() {
            let url = plan
                .request(plan.format.first_page(), None, "START_DATE", "END_DATE")
                .display_url();
            assert!(
                url.is_ascii(),
                "{}: the outbound URL must be ASCII: {url}",
                kind.label()
            );
            if kind.sse() {
                seen_titles.push(url.clone());
            }
        }
        assert_eq!(seen_titles.len(), 2);
        assert!(
            seen_titles[0].contains("TITLE=%E9%A3%8E%E9%99%A9%E8%AD%A6%E7%A4%BA"),
            "风险警示 is UTF-8 percent-encoded: {}",
            seen_titles[0]
        );
        assert!(
            seen_titles[1].contains("TITLE=%E7%89%B9%E5%88%AB%E5%A4%84%E7%90%86"),
            "特别处理 is UTF-8 percent-encoded: {}",
            seen_titles[1]
        );
    }

    /// 2026-10-30 real shape: `tab1` (暂停上市) is empty with `pagecount=0` and
    /// `tab2` carries the change rows. The old `find_map(first data array)` read
    /// the empty `tab1` and made the whole-market table look empty.
    #[test]
    fn sz_page_reads_the_declared_tab2_block_after_an_empty_tab1() {
        let body = fixture("szse-name-changes-tab1-empty.json");
        let (changes, rows, skipped) =
            parse_sz_page(&body, "a", Some(SZSE_SHORT_NAME_TABKEY)).expect("fixture parses");
        assert_eq!(rows, 4, "the rows come from tab2, not from the empty tab1");
        assert_eq!(skipped, 1);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].code, "000004");

        // Without a declaration the same page is still unambiguous: tab2 is the
        // only block with rows, so the unique-non-empty fallback selects it.
        let (changes, rows, _) =
            parse_sz_page(&body, "a", None).expect("the unique non-empty block is accepted");
        assert_eq!((rows, changes.len()), (4, 3));
    }

    /// A declared tab always wins over position: when both tabs have rows the
    /// parser reads the declared one, while an undeclared page with two
    /// non-empty blocks is refused instead of guessing.
    #[test]
    fn two_non_empty_tabs_without_a_declaration_fail_closed() {
        let body = r#"[
          {"metadata":{"tabkey":"tab1","pageno":1,"pagecount":1,"recordcount":1},
           "data":[{"bgrq":"2001-03-07","zqdm":"000004","bgqzqjc":"ST深安达","bghzqjc":"深安达A"}]},
          {"metadata":{"tabkey":"tab2","pageno":1,"pagecount":1,"recordcount":1},
           "data":[{"bgrq":"2001-03-07","zqdm":"000005","bgqzqjc":"ST世纪星源","bghzqjc":"世纪星源"}]}
        ]"#
        .as_bytes();
        let error = parse_sz_page(body, "a", None).expect_err("two candidates are refused");
        assert_eq!(error.code(), "source_invalid");
        assert!(
            error.to_string().contains("several blocks"),
            "the reason names the ambiguity: {error}"
        );

        // The declaration is the selector: tab2 is read even though tab1 has a
        // row, and the row of tab1 is not in the result.
        let (changes, rows, _) = parse_sz_page(body, "a", Some(SZSE_SHORT_NAME_TABKEY))
            .expect("the declared tab is selected");
        assert_eq!((rows, changes.len()), (1, 1));
        assert_eq!(changes[0].code, "000005");
    }

    /// A request that declared `tab2` against a response without that block is
    /// `source_invalid`: reading another tab would silently change the meaning.
    #[test]
    fn a_missing_declared_tab_fails_closed() {
        let body = r#"[
          {"metadata":{"tabkey":"tab1","pageno":1,"pagecount":0,"recordcount":0},"data":[]},
          {"metadata":{"tabkey":"tab3","pageno":1,"pagecount":1,"recordcount":1},
           "data":[{"bgrq":"2001-03-07","zqdm":"000004","bgqzqjc":"ST深安达","bghzqjc":"深安达A"}]}
        ]"#
        .as_bytes();
        let error = parse_sz_page(body, "a", Some(SZSE_SHORT_NAME_TABKEY))
            .expect_err("a missing declared tab is refused");
        assert_eq!(error.code(), "source_invalid");
        assert!(
            error.to_string().contains("tab2"),
            "the reason names the declared tab: {error}"
        );
    }

    /// (d) XDM-151 §7: snapshots frozen under `risk-warnings-v2` hold flat row
    /// objects, one per announcement. The v3 parser must replay them unchanged:
    /// one object is one group.
    #[test]
    fn a_v2_flat_snapshot_still_replays() {
        let page = parse_sh_page(&fixture("sse-bulletins.json"), "b").expect("fixture parses");
        assert_eq!(page.documents, 5);
        assert_eq!(page.documents, page.events.len() + page.ignored);
        assert_eq!(page.events.len(), 3);
        assert_eq!(
            page.ignored, 2,
            "a proposal and an unrelated bulletin are ignored"
        );
        assert_eq!(
            page.docs_per_bulletin.len(),
            5,
            "a flat row is its own announcement"
        );
        assert_eq!(page.docs_per_bulletin.values().copied().max(), Some(1));
    }

    #[test]
    fn explicit_title_date_wins_over_announcement_day() {
        let page = parse_sh_page(&fixture("sse-bulletins.json"), "b").expect("fixture parses");
        assert_eq!(
            page.events[0].day,
            NaiveDate::from_ymd_opt(2024, 5, 6).unwrap()
        );
        assert_eq!(page.events[0].announced_at.as_deref(), Some("2024-04-30"));
    }

    /// (a) The real shape: `pageHelp.data` is a list of announcement groups,
    /// each holding the body and its attachments. Every document is classified
    /// on its own, attachments fall out by their titles, and the group is one
    /// announcement for the per-bulletin evidence.
    #[test]
    fn a_grouped_page_is_parsed_document_by_document() {
        let page = parse_sh_page(&fixture("sse-bulletins-grouped.json"), "c")
            .expect("the grouped fixture parses");
        assert_eq!(page.documents, 6, "three groups: 2 + 1 + 3 documents");
        assert_eq!(
            page.events.len(),
            2,
            "the implementation and the revocation"
        );
        assert_eq!(
            page.ignored, 4,
            "an audit attachment, an application and its two attachments"
        );
        assert_eq!(page.docs_per_bulletin.len(), 3);
        assert_eq!(
            page.docs_per_bulletin.values().copied().max(),
            Some(3),
            "the application carries two attachments"
        );
        assert_eq!(page.events[0].code, "600242");
        assert_eq!(page.events[0].action, RiskWarningAction::Implement);
        assert_eq!(
            page.events[0].status,
            Some(RiskStatus::DelistingRiskWarning)
        );
        assert_eq!(page.events[1].code, "601113");
        assert_eq!(page.events[1].action, RiskWarningAction::Revoke);
    }

    /// (e) Classification happens before de-duplication: an attachment that
    /// precedes the body must not shadow the body's event, and two different
    /// events of one bulletin are two events (XDM-151 §7).
    #[test]
    fn a_body_after_its_attachment_keeps_its_event() {
        let body = r#"{"pageHelp":{"pageNo":1,"pageCount":1,"total":2,"data":[
          [
            {"SSEDATE":"2024-06-01","SECURITY_CODE":"600010",
             "TITLE":"示例十关于撤销公司股票其他风险警示的补充说明",
             "ORG_BULLETIN_ID":"attach-first-1"},
            {"SSEDATE":"2024-06-01","SECURITY_CODE":"600010",
             "TITLE":"示例十关于公司股票被实施其他风险警示的公告",
             "ORG_BULLETIN_ID":"attach-first-1"}
          ],
          [
            {"SSEDATE":"2024-06-02","SECURITY_CODE":"600011",
             "TITLE":"示例十一关于公司股票被实施退市风险警示的专项审核意见",
             "ORG_BULLETIN_ID":"attach-first-2"},
            {"SSEDATE":"2024-06-02","SECURITY_CODE":"600011",
             "TITLE":"示例十一关于公司股票被实施退市风险警示的公告",
             "ORG_BULLETIN_ID":"attach-first-2"}
          ]
        ]}}"#
            .as_bytes();
        let page = parse_sh_page(body, "d").expect("the page parses");
        assert_eq!(page.documents, 4);
        assert_eq!(
            page.events.len(),
            3,
            "the revoke and the implementation of the first bulletin, plus the \
             second bulletin's implementation"
        );
        let actions: Vec<_> = page.events.iter().map(|event| event.action).collect();
        assert_eq!(
            actions,
            vec![
                RiskWarningAction::Revoke,
                RiskWarningAction::Implement,
                RiskWarningAction::Implement
            ]
        );
        // The second group's attachment also classifies as an implementation;
        // de-duplication by `(id, event)` keeps one event for it.
        assert_eq!(page.events[1].code, "600010");
        assert_eq!(page.events[1].status, Some(RiskStatus::OtherRiskWarning));
        assert_eq!(page.events[2].code, "600011");
        assert_eq!(
            page.events[2].status,
            Some(RiskStatus::DelistingRiskWarning)
        );
    }

    /// A page without `pageHelp.data` is still `source_invalid`: an absent row
    /// array is not an empty announcement list.
    #[test]
    fn a_page_without_page_help_data_fails_closed() {
        let body = br#"{"pageHelp":{"pageNo":1,"pageCount":0,"total":0}}"#;
        let error = parse_sh_page(body, "e").expect_err("a missing data array is refused");
        assert_eq!(error.code(), "source_invalid");
        assert!(
            error.to_string().contains("no data row array"),
            "the reason names the missing array: {error}"
        );
    }

    /// B. The bulletin index takes `pageHelp.beginPage`/`endPage` with
    /// `pageSize=100`; `pageNo` must travel with them so the envelope's page
    /// number stays truthful. The page size is written by the dialect only —
    /// a plan-level `pageHelp.pageSize` would be overridden again.
    #[test]
    fn the_bulletin_plans_page_by_begin_page_with_a_page_size_of_100() {
        let mut sse_plans = 0;
        for (kind, plan) in plans().into_iter().filter(|(kind, _)| kind.sse()) {
            sse_plans += 1;
            assert_eq!(plan.format, PagingFormat::SsePageHelpBeginPage);
            assert!(
                !plan.params.contains_key("pageHelp.pageSize"),
                "{}: the page size belongs to the dialect, not the plan",
                kind.label()
            );
            let first = plan.request(1, None, "START_DATE", "END_DATE");
            for (name, value) in [
                ("pageHelp.pageSize", "100"),
                ("pageHelp.pageNo", "1"),
                ("pageHelp.beginPage", "1"),
                ("pageHelp.endPage", "1"),
            ] {
                assert_eq!(
                    first.params.get(name).map(String::as_str),
                    Some(value),
                    "{}: {name}",
                    kind.label()
                );
            }
            let third = plan.request(3, None, "START_DATE", "END_DATE");
            for name in ["pageHelp.pageNo", "pageHelp.beginPage", "pageHelp.endPage"] {
                assert_eq!(
                    third.params.get(name).map(String::as_str),
                    Some("3"),
                    "{}: {name}",
                    kind.label()
                );
            }
        }
        assert_eq!(sse_plans, 2, "both title filters use the bulletin dialect");
    }

    #[test]
    fn proposals_are_not_events() {
        assert!(classify_title("关于申请撤销公司股票其他风险警示的公告").is_none());
        assert!(classify_title("关于公司股票可能被实施退市风险警示的提示性公告").is_none());
        assert_eq!(
            classify_title("关于公司股票被实施其他风险警示的公告"),
            Some((
                RiskWarningAction::Implement,
                Some(RiskStatus::OtherRiskWarning)
            ))
        );
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
            Policy::Required,
        )
        .unwrap_err();
        assert_eq!(error.code(), "source_store_unconfigured");
        assert!(
            error.to_string().starts_with("source_store_unconfigured: "),
            "the stable code leads the item error text: {error}"
        );
        assert!(market.read_exchange_snapshots().unwrap().is_empty());
    }

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    /// The real empty-window shape of the SSE announcement index: 2000Q1
    /// answers `pageCount:0`, `data:[]`, `total:0` (XDM-139 §5, reproduced with
    /// curl against the live source).
    const EMPTY_QUARTER: &[u8] = br#"{"pageHelp":{"beginPage":1,"endPage":1,"pageNo":1,
      "pageSize":100,"pageCount":0,"total":0,"data":[]}}"#;
    /// One implementation announcement in 2000Q2, minimal row shape.
    const ONE_EVENT_QUARTER: &[u8] = r#"{"pageHelp":{"beginPage":1,"endPage":1,"pageNo":1,
      "pageSize":100,"pageCount":1,"total":1,
      "data":[{"SSEDATE":"2000-05-08","SECURITY_CODE":"600001",
      "SECURITY_NAME":"示例一","TITLE":"示例一关于公司股票被实施其他风险警示的公告",
      "BULLETIN_TYPE_DESC":"风险警示","ORG_BULLETIN_ID":"risk-1",
      "URL":"/disclosure/listedinfo/announcement/c/new/2000-05-08/600001_1.pdf"}]}}"#
        .as_bytes();
    /// The revocation of the same warning in 2000Q4.
    const ONE_REVOKE_QUARTER: &[u8] = r#"{"pageHelp":{"beginPage":1,"endPage":1,"pageNo":1,
      "pageSize":100,"pageCount":1,"total":1,
      "data":[{"SSEDATE":"2000-10-11","SECURITY_CODE":"600001",
      "SECURITY_NAME":"示例一","TITLE":"示例一关于撤销其他风险警示的公告",
      "BULLETIN_TYPE_DESC":"风险警示","ORG_BULLETIN_ID":"risk-1-revoke",
      "URL":"/disclosure/listedinfo/announcement/c/new/2000-10-11/600001_2.pdf"}]}}"#
        .as_bytes();
    /// The SZSE short-name table the same item reads, in the real two-block
    /// shape: `tab1` (暂停上市) empty, `tab2` with one A-share change.
    const SHORT_NAME_TABLE: &[u8] = r#"[
      {"metadata":{"tabkey":"tab1","pageno":1,"pagecount":0,"recordcount":0},"data":[]},
      {"metadata":{"tabkey":"tab2","pageno":1,"pagecount":1,"recordcount":1},
       "data":[{"bgrq":"2000-03-07","zqdm":"000004","bgqzqjc":"辽物资A","bghzqjc":"ST辽物资"}]}
    ]"#
    .as_bytes();

    /// Scripted transport for the whole item: the SZSE table, plus the SSE
    /// announcement index where 2000Q2 carries one implementation and 2000Q4
    /// its revocation, while every other quarter is the empty shape. Both
    /// title filters ask for the same windows, so the duplicate event is also
    /// a de-duplication fixture.
    struct BulletinPages {
        requests: Mutex<Vec<String>>,
    }

    impl Transport for BulletinPages {
        fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.requests.lock().unwrap().push(request.url.clone());
            let body = if request.url.contains("CATALOGID=") {
                SHORT_NAME_TABLE.to_vec()
            } else if request.url.contains("END_DATE=2000-06-30") {
                ONE_EVENT_QUARTER.to_vec()
            } else if request.url.contains("END_DATE=2000-12-31") {
                ONE_REVOKE_QUARTER.to_vec()
            } else {
                EMPTY_QUARTER.to_vec()
            };
            Ok(HttpResponse {
                status: 200,
                etag: None,
                last_modified: None,
                body,
            })
        }
    }

    struct Shared<T>(Arc<T>);

    impl<T: Transport> Transport for Shared<T> {
        fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.0.get(request)
        }
    }

    /// One item-level acquisition (`acquire_today`) from synthetic windows,
    /// through the real entry (freeze, index, validate) and the real domain
    /// assembly. `today` bounds the quarter loop, so a test stays at a handful
    /// of windows.
    fn acquire_quarters<T: Transport + 'static>(
        store: &SourceStore,
        market: &MarketBackend,
        pages: &Arc<T>,
        today: NaiveDate,
    ) -> Result<Admitted, Error> {
        let client = Client::with_transport(Shared(pages.clone()))
            .with_policy(RetryPolicy::single())
            .without_sleep();
        acquire_today(store, market, &client, today, Arc::new(RunCache::new()))
    }

    fn bulletin_fixture() -> (
        tempfile::TempDir,
        SourceStore,
        MarketBackend,
        Arc<BulletinPages>,
    ) {
        let (dir, market) = crate::official_security::compact_market_fixture();
        let store = SourceStore::new(dir.path().join("source-store"));
        let pages = Arc::new(BulletinPages {
            requests: Mutex::new(Vec::new()),
        });
        (dir, store, market, pages)
    }

    /// (a) XDM-139 §5: 2000Q1 is always an empty quarter and the source says
    /// so as `pageCount:0` with `data:[]`. The item must not fail: the quarter
    /// is an unobserved coverage window and no `normal` row is derived from it.
    #[test]
    fn an_empty_first_quarter_is_not_observed_instead_of_failing_the_item() {
        let (_dir, store, market, pages) = bulletin_fixture();
        let admitted = acquire_quarters(&store, &market, &pages, day(2000, 3, 31))
            .expect("an empty quarter is a coverage gap, not a failure");
        // The empty quarter is named in the coverage with the acquisition
        // verdict, and it produced no SH row and no SH event.
        let sh_windows: Vec<_> = admitted
            .set
            .coverage
            .iter()
            .filter(|window| window.source_kind == "exchange_announcement")
            .collect();
        assert_eq!(sh_windows.len(), 1);
        assert_eq!(
            (sh_windows[0].from, sh_windows[0].to),
            (day(2000, 1, 1), day(2000, 3, 31))
        );
        assert!(!sh_windows[0].observed);
        assert_eq!(sh_windows[0].rows, 0);
        assert!(
            sh_windows[0]
                .reason
                .starts_with("not_observed: unverified_zero"),
            "the reason carries the acquisition verdict: {}",
            sh_windows[0].reason
        );
        assert_eq!((admitted.sh_events, admitted.sh_rows), (0, 0));
        assert!(
            admitted
                .set
                .rows
                .iter()
                .all(|row| row.exchange == Exchange::Sz),
            "the only rows are the SZSE change intervals"
        );
        // Both sources are bound: the empty page was still frozen and indexed,
        // so the item can claim an admitted set.
        assert_eq!(admitted.bindings.len(), 2);
        assert_eq!(pages.requests.lock().unwrap().len(), 3);
    }

    /// (b) A mixed run: an empty quarter, an implementation, an empty quarter,
    /// the revocation. The derivation must still produce the interval and must
    /// cut it at the start of the unobserved quarter between the two events
    /// instead of carrying the warning across the gap.
    #[test]
    fn empty_quarters_around_an_observed_one_still_derive_intervals() {
        let (_dir, store, market, pages) = bulletin_fixture();
        let admitted = acquire_quarters(&store, &market, &pages, day(2000, 12, 31))
            .expect("the mixed run succeeds");
        // Two events, each seen through both title filters and de-duplicated by
        // its record id; four rows were read from the two observed quarters.
        assert_eq!((admitted.sh_events, admitted.sh_rows), (2, 4));
        let sh_rows: Vec<_> = admitted
            .set
            .rows
            .iter()
            .filter(|row| row.exchange == Exchange::Sh)
            .collect();
        assert_eq!(sh_rows.len(), 1);
        assert_eq!(sh_rows[0].code, "600001");
        assert_eq!(sh_rows[0].effective_from, day(2000, 5, 8));
        assert_eq!(
            sh_rows[0].effective_to,
            Some(day(2000, 7, 1)),
            "the interval stops at the 2000Q3 gap, it is not claimed past it"
        );
        let coverage: Vec<(bool, usize)> = admitted
            .set
            .coverage
            .iter()
            .filter(|window| window.source_kind == "exchange_announcement")
            .map(|window| (window.observed, window.rows))
            .collect();
        // 2000Q1 empty, 2000Q2 observed, 2000Q3 empty, 2000Q4 observed.
        assert_eq!(coverage, vec![(false, 0), (true, 2), (false, 0), (true, 2)]);
        assert_eq!(admitted.set.not_observed_windows(), 2);
        // One SZSE request plus two title filters over four quarters; every
        // page is frozen even when its window is empty.
        assert_eq!(pages.requests.lock().unwrap().len(), 9);
        assert_eq!(store.records().unwrap().len(), 9);
    }

    /// One non-empty window (`END_DATE=2000-03-31`) that answers two pages
    /// selected by `pageHelp.beginPage`, plus the empty shape everywhere else.
    struct TwoPageWindow {
        requests: Mutex<Vec<String>>,
        first: &'static [u8],
        second: &'static [u8],
    }

    impl Transport for TwoPageWindow {
        fn get(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.requests.lock().unwrap().push(request.url.clone());
            let body = if request.url.contains("CATALOGID=") {
                SHORT_NAME_TABLE.to_vec()
            } else if request.url.contains("END_DATE=2000-03-31") {
                if request.url.contains("pageHelp.beginPage=2") {
                    self.second.to_vec()
                } else {
                    self.first.to_vec()
                }
            } else {
                EMPTY_QUARTER.to_vec()
            };
            Ok(HttpResponse {
                status: 200,
                etag: None,
                last_modified: None,
                body,
            })
        }
    }

    /// 2000Q1 page 1 of 2: two announcements of one document each, `total=3`.
    const MULTI_PAGE_FIRST: &[u8] = r#"{"pageHelp":{"beginPage":1,"endPage":1,"pageNo":1,
      "pageSize":100,"pageCount":2,"total":3,"data":[
      [{"SSEDATE":"2000-02-01","SECURITY_CODE":"600001",
        "TITLE":"示例一关于公司股票被实施其他风险警示的公告",
        "ORG_BULLETIN_ID":"page-1-a"}],
      [{"SSEDATE":"2000-02-02","SECURITY_CODE":"600002",
        "TITLE":"示例二关于公司股票被实施退市风险警示的公告",
        "ORG_BULLETIN_ID":"page-1-b"}]]}}"#
        .as_bytes();
    /// 2000Q1 page 2 of 2: one further announcement; 2 + 1 = `total`.
    const MULTI_PAGE_SECOND: &[u8] = r#"{"pageHelp":{"beginPage":2,"endPage":2,"pageNo":2,
      "pageSize":100,"pageCount":2,"total":3,"data":[
      [{"SSEDATE":"2000-03-01","SECURITY_CODE":"600003",
        "TITLE":"示例三关于公司股票被实施其他风险警示的公告",
        "ORG_BULLETIN_ID":"page-2-c"}]]}}"#
        .as_bytes();
    /// A two-page window whose second page repeats the first one's announcement
    /// and echoes the requested page number; the counts still add up
    /// (`1 + 1 = total 2`), so only the repeated id can tell.
    const REPEATED_FIRST: &[u8] = r#"{"pageHelp":{"beginPage":1,"endPage":1,"pageNo":1,
      "pageSize":100,"pageCount":2,"total":2,"data":[
      [{"SSEDATE":"2000-02-01","SECURITY_CODE":"600001",
        "TITLE":"示例一关于公司股票被实施其他风险警示的公告",
        "ORG_BULLETIN_ID":"repeated-1"}]]}}"#
        .as_bytes();
    const REPEATED_SECOND: &[u8] = r#"{"pageHelp":{"beginPage":2,"endPage":2,"pageNo":2,
      "pageSize":100,"pageCount":2,"total":2,"data":[
      [{"SSEDATE":"2000-02-01","SECURITY_CODE":"600001",
        "TITLE":"示例一关于公司股票被实施其他风险警示的公告",
        "ORG_BULLETIN_ID":"repeated-1"}]]}}"#
        .as_bytes();

    fn two_page_fixture(
        first: &'static [u8],
        second: &'static [u8],
    ) -> (
        tempfile::TempDir,
        SourceStore,
        MarketBackend,
        Arc<TwoPageWindow>,
    ) {
        let (dir, market) = crate::official_security::compact_market_fixture();
        let store = SourceStore::new(dir.path().join("source-store"));
        let pages = Arc::new(TwoPageWindow {
            requests: Mutex::new(Vec::new()),
            first,
            second,
        });
        (dir, store, market, pages)
    }

    /// (b) A multi-page window: page 2 is fetched with
    /// `pageHelp.beginPage=2`/`endPage=2` (and `pageNo=2`, so the envelope
    /// agrees) and carries announcements page 1 does not; the pages together
    /// equal the declared `total` (XDM-151 §3).
    #[test]
    fn a_multi_page_window_collects_every_page_by_begin_page() {
        let (_dir, store, market, pages) = two_page_fixture(MULTI_PAGE_FIRST, MULTI_PAGE_SECOND);
        let admitted = acquire_quarters(&store, &market, &pages, day(2000, 3, 31))
            .expect("both pages are collected");
        assert_eq!(admitted.sh_events, 3, "one event per announcement");
        assert_eq!(
            admitted.sh_rows, 6,
            "three documents read by each of the two title filters"
        );
        assert_eq!(admitted.sh_bulletins, 3);
        assert_eq!(admitted.sh_max_docs_per_bulletin, 1);
        assert_eq!(admitted.sh_multi_doc_bulletins, 0);
        let evidence = admitted.evidence();
        assert_eq!(
            evidence["sh_docs_per_bulletin"]["documents_read"], 6,
            "the evidence records docs_per_bulletin: {evidence}"
        );
        assert_eq!(evidence["sh_docs_per_bulletin"]["distinct_bulletins"], 3);
        assert_eq!(evidence["sh_docs_per_bulletin"]["max"], 1);
        let requests = pages.requests.lock().unwrap().clone();
        assert_eq!(
            requests.len(),
            5,
            "one SZSE table plus two pages × two filters"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|url| url.contains("pageHelp.beginPage=2"))
                .count(),
            2,
            "each title filter asks for page 2 explicitly: {requests:?}"
        );
        assert!(requests.iter().all(|url| {
            url.contains("CATALOGID=")
                || (url.contains("pageHelp.pageSize=100")
                    && url.contains("pageHelp.beginPage=")
                    && url.contains("pageHelp.endPage="))
        }));
    }

    /// (c) If the endpoint ignores the page parameter and answers page 1 again,
    /// the item must fail instead of counting the repeated announcements twice.
    /// The counts alone cannot see it here (`1 + 1 = total 2`), so the repeated
    /// `ORG_BULLETIN_ID` is the verdict.
    #[test]
    fn a_repeated_page_fails_the_item_instead_of_inflating_the_row_count() {
        let (_dir, store, market, pages) = two_page_fixture(REPEATED_FIRST, REPEATED_SECOND);
        let error = acquire_quarters(&store, &market, &pages, day(2000, 3, 31))
            .expect_err("a repeated page is not a second page");
        assert_eq!(error.code(), "truncated");
        assert!(
            error.to_string().contains("repeats bulletin repeated-1"),
            "the reason names the repeated announcement: {error}"
        );
    }
}
