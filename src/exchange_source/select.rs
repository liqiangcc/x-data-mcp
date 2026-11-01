//! The read-only half of the exchange entry (issue-446 §5.1/§5.3/§5.4, R3).
//!
//! A daily item must not send a request: it derives from the bytes a
//! `refresh_exchange_sources` run already fetched, validated and froze. This
//! module answers, for one source, *which* frozen pages are that item's input:
//!
//! - a whole-table source takes the member page set of the newest complete
//!   successful run (§5.4): the index cannot say which page 1 belongs with
//!   which page 2, so the run record owns that identity. A table whose pages
//!   were split across runs fails closed instead of silently pairing a page 1
//!   with an unrelated page 2;
//! - a windowed source takes, per planned window, the newest *complete* page
//!   set registered under that window, so a later partial or failed fetch
//!   cannot replace a good set and a closed window whose bytes never changed
//!   keeps being read (the append case §5.3 relies on);
//! - without a run record (a store filled before R2, or a fresh init) the
//!   selection falls back to the newest page per page number and demands the
//!   same completeness. That fallback cannot flatter the age gate:
//!   `verified_through` then falls back to the oldest selected `as_of`, so a
//!   store nobody refreshed fails closed after `H`.
//!
//! Freshness is computed here, once, for every reader (§5.3): `as_of` is the
//! oldest selected page for a table and the newest for a window, never the run
//! day; `verified_through` is the refresh run's confirmation date, and
//! `today - verified_through > H` fails closed with `source_refresh_overdue`. The gate never compares the selected `as_of`: a
//! source that answers with unchanged bytes keeps its first acquisition date
//! forever, so an `as_of` gate would misjudge a verified source as overdue.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;

use super::request::{ExchangeRequest, PagingFormat, Segment};
use super::reuse::{self, ReuseClass, ReusePolicyTable};
use super::validate::{self, PageEnvelope};
use super::{AcquisitionPlan, SnapshotRow, SourceId};
use crate::market_storage::Error as StorageError;
use crate::source_store::{SourceStore, StoreError};
use crate::sqlite_market::backend::MarketBackend;
use crate::sqlite_market::refresh_runs::CompleteRun;

/// The parameters of one request: what a series key compares.
type Params = BTreeMap<String, String>;

/// Request parameters that select one page. They are stripped from a series
/// key: the pages of one acquisition share every other parameter. The list is
/// exactly what [`PagingFormat::apply_page`] writes — a parameter left here by
/// accident would split one acquisition into two series and the adapter would
/// fail closed on a set that is in fact complete.
const PAGE_PARAMS: [&str; 5] = [
    "PAGENO",
    "pageHelp.pageSize",
    "pageHelp.pageNo",
    "pageHelp.beginPage",
    "pageHelp.endPage",
];

#[derive(Debug, thiserror::Error)]
pub enum SelectError {
    /// The store holds no snapshot of the source at all: nothing was ever
    /// refreshed, so there is no input to derive from.
    #[error("source_snapshot_not_found: {detail}")]
    NotFound { detail: String },
    /// Snapshots exist but no complete set does (a missing page, a page set
    /// split across runs, pages the index cannot explain).
    #[error("snapshot_incomplete: {detail}")]
    Incomplete { detail: String },
    /// The newest complete refresh is older than `H`: the bytes may be fine,
    /// but nobody confirmed the fingerprint set recently (§5.1).
    #[error("source_refresh_overdue: {detail}")]
    Overdue { detail: String },
    /// The frozen pages carry a source defect the acquisition would have
    /// failed with (`unverified_zero`, `page_size_ignored`, `truncated`,
    /// `source_invalid`): the read path must reach the same verdict instead of
    /// relabelling it as a store gap (§5.1, "same inputs, same verdicts").
    #[error("{text}")]
    Verdict { code: &'static str, text: String },
    /// The frozen bytes themselves cannot be read back or re-parsed.
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Database(#[from] StorageError),
}

impl SelectError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "source_snapshot_not_found",
            Self::Incomplete { .. } => "snapshot_incomplete",
            Self::Overdue { .. } => "source_refresh_overdue",
            Self::Verdict { code, .. } => code,
            Self::Store(error) => error.code(),
            Self::Database(_) => "source_index_unavailable",
        }
    }

    /// A source-defect verdict with the acquisition's own reason code, taken
    /// over from the page checks the entry performs (§5.1).
    fn verdict(code: &'static str, detail: impl std::fmt::Display) -> Self {
        Self::Verdict {
            code,
            text: format!("{code}: {detail}"),
        }
    }
}

impl From<validate::ValidateError> for SelectError {
    fn from(error: validate::ValidateError) -> Self {
        // The acquisition's verdict, verbatim: a frozen page that does not
        // parse or does not satisfy its own counts is the same defect whether
        // it is seen now or at fetch time.
        Self::Verdict {
            code: error.code(),
            text: error.to_string(),
        }
    }
}

/// How several sources' dataset dates combine into one item's `as_of`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsOf {
    /// A whole-table item is as old as its oldest table.
    Oldest,
    /// A windowed item is as new as its newest window.
    Newest,
}

/// §5.3's freshness evidence for one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freshness {
    /// The dataset date the selected bytes imply (never the run day).
    pub as_of: Option<String>,
    /// The date the selected fingerprint set was last confirmed unchanged.
    pub verified_through: Option<String>,
    /// `today - verified_through`, the number the `H` gate compares.
    pub staleness_days: Option<i64>,
    /// The refresh runs the selection relied on, oldest id first. Empty when
    /// the index fallback was used.
    pub run_ids: Vec<String>,
    /// `refresh_run` (member pages of a complete run) or `index_latest`.
    pub selected_by: &'static str,
}

impl Freshness {
    /// The freshness of a first acquisition: the bytes were just fetched, so
    /// the acquisition day is both the dataset date and the confirmation date.
    /// The daily items never use this (they read); the refresh and replay paths
    /// do, so their evidence keeps the same shape.
    pub fn fetched(as_of: NaiveDate) -> Self {
        let date = as_of.format("%Y-%m-%d").to_string();
        Self {
            as_of: Some(date.clone()),
            verified_through: Some(date),
            staleness_days: Some(0),
            run_ids: Vec::new(),
            selected_by: "fetch",
        }
    }

    /// The item-level answer for something that reads several sources: the
    /// oldest confirmation and the largest staleness (an item is only as fresh
    /// as its stalest input), the union of the runs, and §5.3's rule over the
    /// sources' own dataset dates.
    pub fn combined(parts: &[&Freshness], as_of: AsOf) -> Self {
        let mut run_ids: BTreeSet<String> = BTreeSet::new();
        for part in parts {
            run_ids.extend(part.run_ids.iter().cloned());
        }
        let selected_by = match parts.first() {
            Some(first)
                if parts
                    .iter()
                    .all(|part| part.selected_by == first.selected_by) =>
            {
                first.selected_by
            }
            _ => "mixed",
        };
        Self {
            as_of: match as_of {
                AsOf::Oldest => parts
                    .iter()
                    .filter_map(|part| part.as_of.as_deref())
                    .min()
                    .map(str::to_string),
                AsOf::Newest => parts
                    .iter()
                    .filter_map(|part| part.as_of.as_deref())
                    .max()
                    .map(str::to_string),
            },
            verified_through: parts
                .iter()
                .filter_map(|part| part.verified_through.as_deref())
                .min()
                .map(str::to_string),
            staleness_days: parts.iter().filter_map(|part| part.staleness_days).max(),
            run_ids: run_ids.into_iter().collect(),
            selected_by,
        }
    }

    /// The evidence fragment every reader writes (design §5.3).
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "as_of": self.as_of,
            "verified_through": self.verified_through,
            "staleness_days": self.staleness_days,
            "refresh_run_ids": self.run_ids,
            "selected_by": self.selected_by,
        })
    }
}

/// One frozen page a daily item derives from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenPage {
    /// Blob identity of the frozen bytes.
    pub snapshot_id: String,
    pub request_fingerprint: String,
    pub parser_version: String,
    /// When these exact bytes were first observed.
    pub fetched_at: String,
    /// The snapshot's own `as_of` (acquisition date of these exact bytes).
    pub as_of: String,
    /// The request's URL and its non-pagination, non-window parameters: one
    /// planned acquisition.
    pub series: String,
    /// The window the request declared, for a windowed source.
    pub window: Option<(NaiveDate, NaiveDate)>,
    pub envelope: PageEnvelope,
}

impl FrozenPage {
    /// The page's window as a [`Segment`], when it has one.
    pub fn segment(&self, source: SourceId) -> Result<Option<Segment>, SelectError> {
        match self.window {
            Some((start, end)) => Ok(Some(Segment::new(start, end).map_err(|error| {
                SelectError::Incomplete {
                    detail: format!(
                        "{}: frozen window {start}..{end} is invalid: {error}",
                        source.as_str()
                    ),
                }
            })?)),
            None => Ok(None),
        }
    }
}

/// The complete snapshot set one source contributes to an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRead {
    pub source_id: SourceId,
    pub class: ReuseClass,
    pub pages: Vec<FrozenPage>,
    pub freshness: Freshness,
}

impl SourceRead {
    /// The pages of one planned acquisition (a whole table, or one SSE list
    /// plan, is exactly one series). `from_param`/`to_param` are the window
    /// bound parameters the adapter declared, and are what the series key
    /// leaves out: the same plan at a later window is the same series.
    pub fn series_for(
        &self,
        plan: &AcquisitionPlan,
        from_param: &str,
        to_param: &str,
    ) -> Vec<&FrozenPage> {
        let series = plan_series(plan, from_param, to_param);
        self.pages
            .iter()
            .filter(|page| page.series == series)
            .collect()
    }
}

/// (plan series, window start) → window end → the pages frozen for that
/// request. The end varies for an open window (it grows every day), so the
/// newest *complete* set of each start wins (§5.2).
type WindowCandidates<'r> =
    BTreeMap<(String, NaiveDate), BTreeMap<NaiveDate, Vec<&'r SnapshotRow>>>;

/// Reading rules of one planned acquisition, looked up by series key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlanRule {
    single_page_only: bool,
    max_pages: u32,
}

/// Reader of the frozen source store for daily items.
#[derive(Clone)]
pub struct Reader<'a> {
    store: &'a SourceStore,
    backend: &'a MarketBackend,
    today: NaiveDate,
}

impl<'a> Reader<'a> {
    /// The default `today` is the market's own date (XDM-156 ruling A): the
    /// refresh run writes `verified_through` in Shanghai time, so a UTC date
    /// would open the `H` gate a day early between 00:00 and 08:00 Beijing
    /// time. Every production caller passes the run's single `today` with
    /// [`Reader::with_today`]; this default is the same date, so a forgotten
    /// argument cannot silently change the verdict.
    pub fn new(store: &'a SourceStore, backend: &'a MarketBackend) -> Self {
        Self {
            store,
            backend,
            today: crate::calendar::shanghai_today(),
        }
    }

    /// Fix "today" for the `H` gate (tests, and the same evidence twice).
    pub fn with_today(mut self, today: NaiveDate) -> Self {
        self.today = today;
        self
    }

    /// Select a whole-table source: the member set of the newest complete run,
    /// or the newest page per page number when no run recorded one.
    pub fn table(
        &self,
        source: SourceId,
        plans: &[AcquisitionPlan],
    ) -> Result<SourceRead, SelectError> {
        let rows = self.rows(source)?;
        let complete = self.complete_run(source)?;
        let rules = plan_rules(plans, "", "");
        let mut by_series: BTreeMap<String, Vec<&SnapshotRow>> = BTreeMap::new();
        match &complete {
            Some(run) => {
                let index: BTreeMap<(&str, &str), &SnapshotRow> = rows
                    .iter()
                    .map(|row| {
                        (
                            (row.snapshot_id.as_str(), row.request_fingerprint.as_str()),
                            row,
                        )
                    })
                    .collect();
                for member in &run.members {
                    let Some(row) = index.get(&(
                        member.snapshot_id.as_str(),
                        member.request_fingerprint.as_str(),
                    )) else {
                        return Err(SelectError::Incomplete {
                            detail: format!(
                                "refresh run {} selected snapshot {} for request {} but the \
                                 exchange index has no such row; run `source_store replay \
                                 --scope exchange`",
                                run.run_id, member.snapshot_id, member.request_fingerprint
                            ),
                        });
                    };
                    by_series
                        .entry(series_of(&row.request_fingerprint, "", "")?.plan)
                        .or_default()
                        .push(row);
                }
                if by_series.is_empty() {
                    return Err(SelectError::Incomplete {
                        detail: format!(
                            "refresh run {} claims a complete {} refresh but recorded no page; \
                             rerun refresh_exchange_sources full",
                            run.run_id,
                            source.as_str()
                        ),
                    });
                }
            }
            None => {
                for row in &rows {
                    let key = series_of(&row.request_fingerprint, "", "")?.plan;
                    by_series.entry(key).or_default().push(row);
                }
            }
        }
        let mut pages = Vec::new();
        for (series, rows) in &by_series {
            let rule = rules.get(series).copied().unwrap_or(UNKNOWN_PLAN_RULE);
            pages.extend(self.complete_set(source, series, rows, rule, "", "")?);
        }
        self.finish(source, pages, complete.as_ref())
    }

    /// Select a windowed source: for every planned window, the newest complete
    /// page set registered under it. Windows without a frozen page are simply
    /// absent here; the adapter decides whether its consumer can live with that
    /// (`a3` cannot, the risk-warning bulletin can).
    pub fn windows(
        &self,
        source: SourceId,
        plans: &[AcquisitionPlan],
        windows: &[Segment],
        from_param: &str,
        to_param: &str,
    ) -> Result<SourceRead, SelectError> {
        let rows = self.rows(source)?;
        let complete = self.complete_run(source)?;
        let rules = plan_rules(plans, from_param, to_param);
        let mut candidates: WindowCandidates<'_> = BTreeMap::new();
        for row in &rows {
            let key = series_of(&row.request_fingerprint, from_param, to_param)?;
            let Some((start, end)) = key.window else {
                continue;
            };
            candidates
                .entry((key.plan, start))
                .or_default()
                .entry(end)
                .or_default()
                .push(row);
        }
        let mut pages = Vec::new();
        for plan in plans {
            let series = plan_series(plan, from_param, to_param);
            let rule = rules.get(&series).copied().unwrap_or(UNKNOWN_PLAN_RULE);
            for window in windows {
                let Some(by_end) = candidates.get(&(series.clone(), window.start())) else {
                    continue;
                };
                // Newest complete page set of this window wins; an incomplete
                // newer attempt never replaces a complete older one.
                let mut ends: Vec<&NaiveDate> = by_end.keys().collect();
                ends.sort_by_key(|end| std::cmp::Reverse(**end));
                let mut chosen = None;
                let mut first_error = None;
                for end in ends {
                    match self.complete_set(
                        source,
                        &series,
                        &by_end[end],
                        rule,
                        from_param,
                        to_param,
                    ) {
                        Ok(set) => {
                            chosen = Some(set);
                            break;
                        }
                        Err(error) => {
                            first_error.get_or_insert(error);
                        }
                    }
                }
                match chosen {
                    Some(set) => pages.extend(set),
                    None => {
                        return Err(first_error.unwrap_or_else(|| SelectError::Incomplete {
                            detail: format!(
                                "{}: window {} has no page",
                                source.as_str(),
                                window.start_text()
                            ),
                        }));
                    }
                }
            }
        }
        if pages.is_empty() {
            return Err(SelectError::NotFound {
                detail: format!(
                    "the source store holds no frozen {} page for the windows this item needs; \
                     run refresh_exchange_sources full first",
                    source.as_str()
                ),
            });
        }
        self.finish(source, pages, complete.as_ref())
    }

    /// Index rows of one source, oldest observation first so that "newest wins"
    /// is a plain comparison on the last row.
    fn rows(&self, source: SourceId) -> Result<Vec<SnapshotRow>, SelectError> {
        let mut rows: Vec<SnapshotRow> = self
            .backend
            .read_exchange_snapshots()?
            .into_iter()
            .filter(|row| row.source_id == source.as_str())
            .collect();
        if rows.is_empty() {
            return Err(SelectError::NotFound {
                detail: format!(
                    "the source store holds no frozen {} snapshot; run \
                     refresh_exchange_sources full before deriving from it",
                    source.as_str()
                ),
            });
        }
        rows.sort_by(|a, b| (&a.fetched_at, &a.snapshot_id).cmp(&(&b.fetched_at, &b.snapshot_id)));
        Ok(rows)
    }

    fn complete_run(&self, source: SourceId) -> Result<Option<CompleteRun>, SelectError> {
        Ok(self
            .backend
            .read_latest_complete_refresh_run(source.as_str())?)
    }

    /// One series' complete page set: the newest observation of each page
    /// number, pages `1..=page_count`, one declared page count, and a row sum
    /// matching the publisher's total when the envelope carries one.
    fn complete_set(
        &self,
        source: SourceId,
        series: &str,
        rows: &[&SnapshotRow],
        rule: PlanRule,
        from_param: &str,
        to_param: &str,
    ) -> Result<Vec<FrozenPage>, SelectError> {
        let mut by_page: BTreeMap<u32, FrozenPage> = BTreeMap::new();
        for row in rows {
            let page = self.page(source, row, from_param, to_param)?;
            let replace = match by_page.get(&page.envelope.page) {
                Some(existing) => {
                    (&page.fetched_at, &page.snapshot_id)
                        > (&existing.fetched_at, &existing.snapshot_id)
                }
                None => true,
            };
            if replace {
                by_page.insert(page.envelope.page, page);
            }
        }
        let Some(first) = by_page.values().next() else {
            return Err(SelectError::Incomplete {
                detail: format!("{}: series {series} has no page", source.as_str()),
            });
        };
        let page_count = first.envelope.page_count;
        let row_sum: u64 = by_page.values().map(|page| page.envelope.rows as u64).sum();
        // A whole-table source that declares its pages and carries no row is
        // `unverified_zero` in the acquisition (`ZeroPolicy::FailClosed`), and
        // the read reaches the same verdict for the `pageCount=0` shape too.
        // A windowed source is left to its adapter: A-3 and the bulletin index
        // read a 0-row window as `not_observed`, not as a format defect.
        if row_sum == 0
            && ReusePolicyTable::declared().for_source(source).class == ReuseClass::Table
        {
            return Err(SelectError::verdict(
                "unverified_zero",
                format!(
                    "{}: series {series} carries 0 rows over {} page(s), which is not evidence",
                    source.as_str(),
                    by_page.len()
                ),
            ));
        }
        if page_count == 0 {
            // A quiet window answers `pageCount=0` with no rows (A-3, and the
            // SSE announcement index for a quarter without a matching
            // bulletin). The acquisition accepts exactly one such page, and so
            // does the read: it is a complete (empty) answer, not a missing
            // page set. A response that declares zero pages and carries rows is
            // the acquisition's `source_invalid` ("page_count is 0").
            if by_page.len() != 1 || by_page.values().any(|page| page.envelope.rows > 0) {
                return Err(SelectError::verdict(
                    "source_invalid",
                    format!(
                        "{}: series {series} declares zero pages but froze {} page(s) with rows",
                        source.as_str(),
                        by_page.len()
                    ),
                ));
            }
            return Ok(by_page.into_values().collect());
        }
        // The checks below are the entry's own page checks (§5.1: the read path
        // reaches the same verdict as the acquisition, it does not relabel a
        // source defect as a store gap).
        if rule.single_page_only && page_count > 1 {
            let declared = match first.envelope.total {
                Some(total) => total.to_string(),
                None => "unknown".to_string(),
            };
            return Err(SelectError::verdict(
                "page_size_ignored",
                format!(
                    "{}: series {series} declares total {declared} but collected {} row(s): \
                     the source reports pageCount {page_count} for a request that sends \
                     pageHelp.pageSize",
                    source.as_str(),
                    first.envelope.rows
                ),
            ));
        }
        if page_count > rule.max_pages {
            return Err(SelectError::verdict(
                "page_count_exceeds_bound",
                format!(
                    "{}: series {series} declares {page_count} pages, more than the bound {}",
                    source.as_str(),
                    rule.max_pages
                ),
            ));
        }
        for (page_no, page) in &by_page {
            if *page_no == 0 || *page_no > page_count {
                return Err(SelectError::verdict(
                    "truncated",
                    format!(
                        "{}: series {series} page {page_no} is outside 1..={page_count}",
                        source.as_str()
                    ),
                ));
            }
            if page.envelope.page_count != page_count {
                return Err(SelectError::verdict(
                    "truncated",
                    format!(
                        "{}: series {series} page {page_no} says {} pages while page {} says \
                         {page_count}",
                        source.as_str(),
                        page.envelope.page_count,
                        first.envelope.page
                    ),
                ));
            }
        }
        let totals: BTreeSet<u64> = by_page
            .values()
            .filter_map(|page| page.envelope.total)
            .collect();
        if totals.len() > 1 {
            return Err(SelectError::verdict(
                "truncated",
                format!(
                    "{}: series {series} total changed between pages: {}",
                    source.as_str(),
                    totals
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(" then ")
                ),
            ));
        }
        // A page the store never fetched is a store gap, not a source defect:
        // the fingerprints the run recorded are simply not all here.
        for page_no in 1..=page_count {
            if !by_page.contains_key(&page_no) {
                return Err(SelectError::Incomplete {
                    detail: format!(
                        "{}: series {series} is missing page {page_no} of {page_count}; \
                         run refresh_exchange_sources full",
                        source.as_str()
                    ),
                });
            }
        }
        if let Some(total) = totals.first().copied()
            && row_sum != total
        {
            return Err(SelectError::verdict(
                "truncated",
                format!(
                    "{}: series {series} froze {row_sum} rows over {page_count} pages while the \
                     source declared {total}",
                    source.as_str()
                ),
            ));
        }
        Ok(by_page.into_values().collect())
    }

    /// Read back one frozen page and re-derive its envelope, so the row and
    /// page counts the selection reasons about come from the same bytes the
    /// parser will see.
    fn page(
        &self,
        source: SourceId,
        row: &SnapshotRow,
        from_param: &str,
        to_param: &str,
    ) -> Result<FrozenPage, SelectError> {
        let Some(format) = PagingFormat::from_parser_version(&row.parser_version) else {
            return Err(SelectError::Incomplete {
                detail: format!(
                    "{}: snapshot {} was frozen by unknown parser version {:?}",
                    source.as_str(),
                    row.snapshot_id,
                    row.parser_version
                ),
            });
        };
        let request =
            ExchangeRequest::from_fingerprint(&row.request_fingerprint).map_err(|error| {
                SelectError::Incomplete {
                    detail: format!(
                        "{}: snapshot {} has an unreadable request fingerprint: {error}",
                        source.as_str(),
                        row.snapshot_id
                    ),
                }
            })?;
        let bytes = self.store.read_verified(&row.snapshot_id)?;
        let envelope =
            validate::parse_page(format, validate::declared_tabkey(&request.params), &bytes)?;
        let key = series_of(&row.request_fingerprint, from_param, to_param)?;
        Ok(FrozenPage {
            snapshot_id: row.snapshot_id.clone(),
            request_fingerprint: row.request_fingerprint.clone(),
            parser_version: row.parser_version.clone(),
            fetched_at: row.fetched_at.clone(),
            as_of: row.as_of.clone(),
            series: key.plan,
            window: key.window,
            envelope,
        })
    }

    /// Compute §5.3's freshness, apply the `H` gate and order the pages.
    fn finish(
        &self,
        source: SourceId,
        mut pages: Vec<FrozenPage>,
        complete: Option<&CompleteRun>,
    ) -> Result<SourceRead, SelectError> {
        let policy = ReusePolicyTable::declared().for_source(source);
        let as_of = match policy.class {
            ReuseClass::Table => pages.iter().map(|page| page.as_of.as_str()).min(),
            ReuseClass::Window => pages.iter().map(|page| page.as_of.as_str()).max(),
        }
        .map(str::to_string);
        let verified_through = match complete {
            Some(run) => Some(run.verified_through.clone()),
            // Without a run record the oldest selected observation is the only
            // honest confirmation date (never today).
            None => pages
                .iter()
                .map(|page| page.as_of.as_str())
                .min()
                .map(str::to_string),
        };
        let Some(confirmed) = verified_through.clone() else {
            return Err(SelectError::Incomplete {
                detail: format!("{}: no selected page carries an as_of", source.as_str()),
            });
        };
        let staleness_days =
            reuse::age_days(&confirmed, self.today).ok_or_else(|| SelectError::Incomplete {
                detail: format!(
                    "{}: verified_through {confirmed:?} is not a date",
                    source.as_str()
                ),
            })?;
        if staleness_days > i64::from(policy.max_age_days) {
            return Err(SelectError::Overdue {
                detail: format!(
                    "{} was last completely verified on {confirmed}, {staleness_days} days ago \
                     (> H={} days); its frozen bytes stay readable, but they are not a current \
                     input. Run refresh_exchange_sources full.",
                    source.as_str(),
                    policy.max_age_days
                ),
            });
        }
        pages.sort_by(|a, b| (&a.series, a.envelope.page).cmp(&(&b.series, b.envelope.page)));
        Ok(SourceRead {
            source_id: source,
            class: policy.class,
            pages,
            freshness: Freshness {
                as_of,
                verified_through,
                staleness_days: Some(staleness_days),
                run_ids: complete
                    .map(|run| vec![run.run_id.clone()])
                    .unwrap_or_default(),
                selected_by: if complete.is_some() {
                    "refresh_run"
                } else {
                    "index_latest"
                },
            },
        })
    }
}

fn plan_rules(
    plans: &[AcquisitionPlan],
    from_param: &str,
    to_param: &str,
) -> BTreeMap<String, PlanRule> {
    plans
        .iter()
        .map(|plan| {
            (
                plan_series(plan, from_param, to_param),
                PlanRule {
                    single_page_only: plan.single_page_only,
                    max_pages: plan.max_pages,
                },
            )
        })
        .collect()
}

/// The rule of a series no current plan declares: the page bound still applies,
/// because a page set that large is a source defect whatever plan asked for it.
const UNKNOWN_PLAN_RULE: PlanRule = PlanRule {
    single_page_only: false,
    max_pages: super::fetch::MAX_PAGES,
};

/// One registered acquisition: URL plus every parameter except pagination and
/// the window bounds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SeriesKey {
    plan: String,
    window: Option<(NaiveDate, NaiveDate)>,
}

fn plan_series(plan: &AcquisitionPlan, from_param: &str, to_param: &str) -> String {
    let mut params = plan.params.clone();
    for name in PAGE_PARAMS {
        params.remove(name);
    }
    if !from_param.is_empty() {
        params.remove(from_param);
        params.remove(to_param);
    }
    series_text(&plan.url, &params)
}

fn series_of(
    fingerprint: &str,
    from_param: &str,
    to_param: &str,
) -> Result<SeriesKey, SelectError> {
    let request = ExchangeRequest::from_fingerprint(fingerprint).map_err(|error| {
        SelectError::Incomplete {
            detail: format!("request fingerprint is unreadable: {error}"),
        }
    })?;
    let mut params = request.params.clone();
    for name in PAGE_PARAMS {
        params.remove(name);
    }
    let window = window_of(&params, from_param, to_param);
    if window.is_some() {
        params.remove(from_param);
        params.remove(to_param);
    }
    Ok(SeriesKey {
        plan: series_text(&request.url, &params),
        window,
    })
}

fn series_text(url: &str, params: &Params) -> String {
    format!(
        "{url}\u{0}{}",
        serde_json::to_string(params).expect("parameters serialize")
    )
}

/// The window a request declared, from the two bound parameters the adapter
/// named. Both the dashed and the compact date shape are accepted: SSE windows
/// are `YYYY-MM-DD`, the SZSE and suspension archives are compact.
fn window_of(params: &Params, from_param: &str, to_param: &str) -> Option<(NaiveDate, NaiveDate)> {
    if from_param.is_empty() {
        return None;
    }
    let start = parse_bound(params.get(from_param)?)?;
    let end = parse_bound(params.get(to_param)?)?;
    (start <= end).then_some((start, end))
}

fn parse_bound(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(text, "%Y%m%d"))
        .ok()
}
