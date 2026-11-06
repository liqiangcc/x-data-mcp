//! ETF identity sources (M3-2 PR-A, issue-509 §3/§5/§12; D-70 Q1).
//!
//! Layer position: this is a **source adapter**. It turns the frozen bytes of
//! the two admitted exchange ETF lists into identity rows for
//! `security_master` and nothing else. It does not write the database, does not
//! assemble A-share intervals, and never infers identity from a code prefix.
//!
//! ```text
//! plans() ──► exchange_source::Entry (M3 §6) ──► frozen bytes (source store)
//!                                                        │
//!                                      parse (etf-list-v2)
//!                                                        ▼
//!              identity_rows() ──► security_master class=2 rows (writer)
//! ```
//!
//! ## Admitted sources (D-70 Q1)
//!
//! - SSE `FUND_LIST` (`commonSoaQuery.do`): `source_id=sse_etf_list`. Carries
//!   `fundCode`, `listingDate`, `subClass` and `INDEX_CODE`; the listing day is
//!   `listing_source=exchange_list`. `etf-list-v2` also reads `secNameFull`
//!   as the row's `name`.
//! - SZSE `1945` (`ShowReport?SHOWTYPE=xlsx`): `source_id=szse_etf_list`.
//!   Carries code/name/fitted index/size/manager but **no listing day**; the
//!   SZSE listing day comes from TDX `base.dbf SSDATE`
//!   (`listing_source=tdx_ssdate`, D-47). `etf-list-v2` reads 证券简称 as the
//!   row's `name`.
//! - SSE `ebs` is a quote snapshot, not an identity source (D-70 Q1). It is
//!   never registered here.
//!
//! ## Names (`etf-list-v2`, etf-name-search §1/§5/§6)
//!
//! Parsing only. Nothing here writes a name, and no identity row, dataset or
//! read surface depends on one.
//!
//! - `name` is SSE `secNameFull` and SZSE 证券简称: the same shape (theme +
//!   `ETF` + fund company), so one search field can hold both. SSE `fundAbbr`
//!   is a different, truncated on-exchange short name and never becomes
//!   `name`.
//! - The text is kept as published apart from whitespace: it is trimmed and
//!   every inner whitespace run (ASCII, no-break or ideographic) becomes one
//!   space. Full-width letters and digits are **not** folded: that is the
//!   search-side normalisation (NFKC, casefold), not a property of the
//!   published name.
//! - An empty, null or missing name is `None`. It is never filled from
//!   `fundAbbr`, the code or an earlier snapshot, and it does not fail the
//!   parse: identity does not depend on it.
//! - [`Admitted::names`] is the per-code chain of [`NameObservation`]s
//!   `(first observed day, name)`, built from the frozen manifest alone. The
//!   day is the first snapshot day that showed the name, not the day the
//!   exchange renamed the fund. Two facts about how the store freezes bytes
//!   decide what the chain can see:
//!   - an SZSE workbook embeds its generation time, so identical content
//!     still freezes as a new sha and every refresh is a snapshot;
//!   - an SSE response is deterministic JSON and the store dedupes by
//!     `(sha256, source, origin)`, so a return to earlier content (A→B→A)
//!     adds no manifest line and the chain ends at B.
//!
//!   The second case is **not repaired here**. The selected current list
//!   (`rows`) still says A, and [`Admitted::names_behind_current`] names the
//!   codes where it disagrees with the chain tail. No day is invented for the
//!   missing A segment: the only dated evidence is a refresh run's
//!   `verified_through`, which moves on every run, so using it would make the
//!   chain change with the clock.
//!
//! ## Identity rules (§3.3)
//!
//! - `instrument_class=2`, `issuer_key=fund code`, `issuer_resolution=issuer_key`.
//! - `delisting_date=NULL`: the current lists only prove current membership, so
//!   removal from a list is **not** a delisting day.
//! - `effective_from` and `known_at` are the acquisition date of the first
//!   frozen snapshot that contained the code (the snapshot's `fetched_at`).
//! - The 128 `hsjday` members that are not on the current lists are **not**
//!   written: only the exchange lists grant ETF identity.
//!
//! ## Failure semantics
//!
//! Empty list, duplicate code, a missing SSE `listingDate`, an SSE
//! `pageHelp.total` that disagrees with the rows, a malformed workbook and an
//! SZSE row count below the shape floor are all `source_invalid`-class failures
//! (or a named code with the same effect). A missing/overdue frozen set is the
//! shared read path's error (`source_snapshot_not_found` /
//! `source_refresh_overdue`). The main-table item treats any of them as an
//! ETF-only partial failure: it keeps the previous class=2 rows, writes the
//! A-share assembly, and reports `etf_source.status=failed` on the item outcome
//! (`success=false`), so the run ends `partially_failed` rather than silently
//! succeeding or blocking A-share derivation.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde_json::{Value, json};

use crate::exchange_source::select::{self, AsOf, Freshness, Reader};
use crate::exchange_source::{
    AcquisitionPlan, BasisStatus, BindError, PagingFormat, SnapshotBinding, SourceId,
};
use crate::security_master::assembly::{DATASET_KIND, MemberFact, PendingSource};
use crate::security_master::names::{self, EtfNameChain, EtfNameSet, EtfNameStep};
use crate::security_master::{Exchange, ListingSource, SecurityLifecycleFact};
use crate::source_store::{Policy, SourceStore, StoreError, StoreKind};

/// Business-field parser version of this adapter. The source-store manifest
/// records the *envelope* parser version (`exchange-sse-page-v2` /
/// `exchange-szse-xlsx-v1`), which is what replay re-derives row/page counts
/// with; this consumer version is registered on the item's evidence and is what
/// a re-derivation of the identity rows pins.
///
/// `v2` adds the row `name` and the name chain. It changes no identity row:
/// the only place the string is written is the item evidence of
/// `derived:security_master`.
pub const PARSER_VERSION: &str = "etf-list-v2";

/// SSE `commonSoaQuery.do` endpoint (D-70 Q1).
pub const SSE_URL: &str = "https://query.sse.com.cn/commonSoaQuery.do";
/// SSE ETF list `sqlId`.
pub const SSE_SQL_ID: &str = "FUND_LIST";
/// SSE `fundType` of ETFs.
pub const SSE_FUND_TYPE: &str = "00";
/// SSE `subClass` set of the ETF list (the page's own parameter).
pub const SSE_SUBCLASS: &str = "01,02,03,04,05,06,07,08,09,30,31,32,33,34,35,36,37,38";
/// SZSE `ShowReport` endpoint.
pub const SZSE_URL: &str = "https://www.szse.cn/api/report/ShowReport";
/// SZSE ETF list catalog id.
pub const SZSE_CATALOG: &str = "1945";
/// Lower shape bound of the SZSE list. The 2026-11-03 list has 770 rows; the
/// floor is a truncation/empty-shape guard, not a business count. It is
/// deliberately below the observed count so an ordinary market change does not
/// fail the item, while a broken workbook (header only, one page, a renamed
/// sheet) still does.
pub const SZSE_MIN_ROWS: usize = 700;

/// `base.dbf` `SSDATE` keyed by `(market, code)`; the SZSE listing-day input.
pub type TdxListingDates = BTreeMap<(u8, String), NaiveDate>;

/// The two admitted identity acquisitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ListKind {
    Sse,
    Szse,
}

impl ListKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sse => "sse_etf_list",
            Self::Szse => "szse_etf_list",
        }
    }

    pub fn source(self) -> SourceId {
        match self {
            Self::Sse => SourceId::SseEtfList,
            Self::Szse => SourceId::SzseEtfList,
        }
    }

    pub fn from_source(source: SourceId) -> Option<Self> {
        match source {
            SourceId::SseEtfList => Some(Self::Sse),
            SourceId::SzseEtfList => Some(Self::Szse),
            _ => None,
        }
    }
}

/// The two acquisitions of this adapter: one request each, one page each.
pub fn plans() -> Vec<(ListKind, AcquisitionPlan)> {
    let sse = AcquisitionPlan::new(SourceId::SseEtfList, SSE_URL, PagingFormat::SsePageHelp)
        .param("isPagination", "true")
        .param("sqlId", SSE_SQL_ID)
        .param("fundType", SSE_FUND_TYPE)
        .param("subClass", SSE_SUBCLASS)
        // The 2026-11-03 response is one page of 947 rows with
        // `pageHelp.pageCount=1`; a larger count means the endpoint ignored the
        // requested page size and the first page cannot stand for the list.
        .single_page_only();
    let szse = AcquisitionPlan::new(SourceId::SzseEtfList, SZSE_URL, PagingFormat::SzseXlsx)
        .param("SHOWTYPE", "xlsx")
        .param("CATALOGID", SZSE_CATALOG)
        .single_page_only();
    vec![(ListKind::Sse, sse), (ListKind::Szse, szse)]
}

/// One ETF list row as an identity fact. `listing_date` is present only for
/// SSE (SZSE takes it from `base.dbf SSDATE` at assembly time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EtfListRow {
    pub exchange: Exchange,
    pub code: String,
    pub listing_date: Option<NaiveDate>,
    /// SSE `subClass`; kept as evidence only (Q11 does not build a dataset).
    pub sub_class: Option<String>,
    /// SSE `INDEX_CODE`; kept as evidence only.
    pub index_code: Option<String>,
    /// SSE `secNameFull` / SZSE 证券简称, whitespace-normalised (see the module
    /// doc). `None` when the source gives no usable text; never invented.
    pub name: Option<String>,
}

/// A name a code carried, with the first snapshot day that showed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameObservation {
    /// The first observation day (UTC day of `acquired_at`, the same basis as
    /// the identity rows' `effective_from`).
    pub observed_from: NaiveDate,
    /// The **Shanghai** day the snapshot that decided `observed_from` entered
    /// the store (never earlier than `observed_from`).
    pub known_at: NaiveDate,
    pub name: String,
    /// sha256 of the snapshot that decided `observed_from`; the name row's
    /// `source_record_id` cites it.
    pub snapshot_sha256: String,
}

/// Why the adapter could not produce identity rows.
#[derive(Debug, thiserror::Error)]
pub enum EtfError {
    #[error(transparent)]
    Select(#[from] select::SelectError),
    #[error(transparent)]
    Bind(#[from] BindError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{code}: {detail}")]
    Invalid { code: &'static str, detail: String },
}

impl EtfError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Select(error) => error.code(),
            Self::Bind(error) => error.code(),
            Self::Store(error) => error.code(),
            Self::Invalid { code, .. } => code,
        }
    }

    fn invalid(code: &'static str, detail: impl Into<String>) -> Self {
        Self::Invalid {
            code,
            detail: detail.into(),
        }
    }
}

/// An admitted identity set plus everything a re-derivation must pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    /// Newest complete snapshot set: the **current** member list.
    pub rows: Vec<EtfListRow>,
    /// Union of every frozen list snapshot. A code that disappeared from a
    /// newer snapshot stays here (monotonic identity, PR-A2); `effective_to`
    /// says from when the list no longer proved it. Each row is the first
    /// snapshot's, so its `name` is cleared: a name from that day says nothing
    /// about now. Names are in [`Admitted::names`].
    pub history: Vec<EtfListRow>,
    /// `(exchange, code) → acquisition date of the first frozen snapshot that
    /// contained the code`.
    pub first_seen: BTreeMap<(Exchange, String), NaiveDate>,
    /// `(exchange, code) → first observation after which the newest list no
    /// longer contained the code`. Only historical members carry an entry.
    pub effective_to: BTreeMap<(Exchange, String), NaiveDate>,
    /// `(exchange, code) → names in observation order`, adjacent repeats merged.
    /// A code that never showed a usable name has no entry. Evidence input for
    /// a later writer; no row or dataset uses it yet.
    pub names: BTreeMap<(Exchange, String), Vec<NameObservation>>,
    /// `(exchange, code) → first observation after which the **newest frozen
    /// snapshot of its source** no longer contained the code`. This is the
    /// manifest's own view of membership, the one a replay has; it can differ
    /// from the selected current list only when the exchange went back to
    /// earlier content (see the module doc), and
    /// [`Admitted::membership_behind_current`] names those codes.
    pub name_ends: BTreeMap<(Exchange, String), NaiveDate>,
    /// One binding per `source_id`, sorted.
    pub bindings: Vec<SnapshotBinding>,
    /// `label → snapshot ids`, in acquisition order.
    pub snapshot_ids: BTreeMap<&'static str, Vec<String>>,
    /// §5.3 freshness of the selected snapshot set.
    pub freshness: Freshness,
}

impl Admitted {
    pub fn as_of(&self) -> Option<NaiveDate> {
        self.freshness.as_of.as_deref().and_then(parse_day)
    }

    /// Codes whose name in the selected current list differs from the last
    /// segment of their chain. This is exactly the content the manifest scan
    /// cannot place in time (SSE A→B→A, see the module doc); a writer must
    /// treat these as unresolved, not as the chain's tail.
    pub fn names_behind_current(&self) -> Vec<(Exchange, String)> {
        self.rows
            .iter()
            .filter_map(|row| {
                let key = (row.exchange, row.code.clone());
                let tail = self
                    .names
                    .get(&key)
                    .and_then(|chain| chain.last())
                    .map(|last| last.name.as_str());
                (row.name.is_some() && row.name.as_deref() != tail).then_some(key)
            })
            .collect()
    }

    /// Codes on the selected current list that the manifest's own scan has
    /// already closed (absent from the newest frozen snapshot of their source).
    /// Same cause as [`Admitted::names_behind_current`], seen from membership:
    /// the name rows end at the first missing observation, the main table does
    /// not. Reported, not repaired.
    pub fn membership_behind_current(&self) -> Vec<(Exchange, String)> {
        self.rows
            .iter()
            .map(|row| (row.exchange, row.code.clone()))
            .filter(|key| self.name_ends.contains_key(key))
            .collect()
    }

    /// Codes on the selected current list for which no snapshot ever showed a
    /// usable name. They get no name row; this is how that is made visible.
    pub fn codes_without_name(&self) -> Vec<(Exchange, String)> {
        self.rows
            .iter()
            .map(|row| (row.exchange, row.code.clone()))
            .filter(|key| !self.names.contains_key(key))
            .collect()
    }

    pub fn sse_rows(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.exchange == Exchange::Sh)
            .count()
    }

    pub fn szse_rows(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.exchange == Exchange::Sz)
            .count()
    }

    /// Everything a re-derivation needs to pin this acquisition (§6.3 "bind").
    pub fn evidence(&self) -> Value {
        let first_seen_oldest = self.first_seen.values().min().map(|day| day.to_string());
        let first_seen_newest = self.first_seen.values().max().map(|day| day.to_string());
        json!({
            "adapter": "M3-2 ETF identity",
            "parser_version": PARSER_VERSION,
            "as_of": self.freshness.as_of,
            "freshness": self.freshness.json(),
            "source_bindings": self.bindings,
            "snapshot_ids": self.snapshot_ids,
            "member_count": self.rows.len(),
            "history_member_count": self.history.len(),
            "removed_member_count": self.effective_to.len(),
            "sse_rows": self.sse_rows(),
            "szse_rows": self.szse_rows(),
            "first_seen_oldest": first_seen_oldest,
            "first_seen_newest": first_seen_newest,
        })
    }
}

/// The daily item's answer: an admitted identity set, or the reason the branch
/// is still `pending_source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunSource {
    Admitted(Box<Admitted>),
    Pending(PendingSource),
}

/// Resolve the deployment's source store, then read the frozen ETF lists.
///
/// Under [`Policy::Optional`] (stdio, tests, non-ops) an unconfigured store is
/// not a read failure: the main-table item keeps the TDX/A-share result and
/// records `pending_source`. Under [`Policy::Required`] (the ops entry) a
/// missing or unreadable store is an ETF-source error; the main-table item
/// keeps the previous class=2 rows, writes the A-share assembly, and reports
/// the ETF part as a partial failure instead of blocking A-share derivation.
pub fn run_source(
    market: &crate::sqlite_market::backend::MarketBackend,
    today: NaiveDate,
    policy: Policy,
) -> Result<RunSource, EtfError> {
    let Some(store) =
        crate::source_store::configured(policy).map_err(|error| EtfError::Invalid {
            code: error.code(),
            detail: error.detail().to_string(),
        })?
    else {
        return Ok(RunSource::Pending(PendingSource {
            dataset: DATASET_KIND,
            source: "etf_list",
            reason: "the M3 source store is not configured for this process \
                     (X_DATA_MCP_SOURCE_STORE); ETF identity stays pending_source",
        }));
    };
    match read(&store, market, today) {
        Ok(admitted) => Ok(RunSource::Admitted(Box::new(admitted))),
        // A store that has never frozen an ETF list is the pre-M3-2 state, not
        // a broken source: the branch stays `pending_source` and the A-share
        // main table keeps its previous behaviour. Once a list is frozen, an
        // incomplete/overdue/invalid set is a real failure (R3's stable codes).
        Err(EtfError::Select(select::SelectError::NotFound { .. })) => {
            Ok(RunSource::Pending(PendingSource {
                dataset: DATASET_KIND,
                source: "etf_list",
                reason: "the M3 source store has no frozen ETF list; \
                         run refresh_exchange_sources full",
            }))
        }
        Err(error) => Err(error),
    }
}

/// Read the newest complete frozen set of each list and parse it. No request,
/// no new snapshot, no manifest line.
pub fn read(
    store: &SourceStore,
    market: &crate::sqlite_market::backend::MarketBackend,
    today: NaiveDate,
) -> Result<Admitted, EtfError> {
    let reader = Reader::new(store, market).with_today(today);
    let planned = plans();
    let mut rows = Vec::new();
    let mut bindings = Vec::new();
    let mut snapshot_ids: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut freshness = Vec::new();
    for source in [SourceId::SseEtfList, SourceId::SzseEtfList] {
        let source_plans: Vec<AcquisitionPlan> = planned
            .iter()
            .filter(|(_, plan)| plan.source_id == source)
            .map(|(_, plan)| plan.clone())
            .collect();
        let read = reader.table(source, &source_plans)?;
        bindings.push(crate::exchange_source::bind::bind(
            market,
            source,
            BasisStatus::ApprovedSource,
        )?);
        freshness.push(read.freshness.clone());
        for (kind, plan) in planned.iter().filter(|(_, plan)| plan.source_id == source) {
            let selected = read.series_for(plan, "", "");
            if selected.is_empty() {
                return Err(EtfError::Select(select::SelectError::Incomplete {
                    detail: format!(
                        "{}: the frozen set has no page for the ETF list acquisition",
                        source.as_str()
                    ),
                }));
            }
            for page in selected {
                let bytes = store.read_verified(&page.snapshot_id)?;
                let mut parsed = parse(*kind, &bytes)?;
                snapshot_ids
                    .entry(kind.label())
                    .or_default()
                    .push(page.snapshot_id.clone());
                rows.append(&mut parsed);
            }
        }
    }
    let freshness = Freshness::combined(&freshness.iter().collect::<Vec<_>>(), AsOf::Oldest);
    validate_set(&rows)?;
    let scanned = scan_history(store)?;
    let mut first_seen = BTreeMap::new();
    let mut history = Vec::new();
    for ((exchange, code), entry) in &scanned.rows {
        first_seen.insert((*exchange, code.clone()), entry.first_seen);
        history.push(entry.row.clone());
    }
    let names = scanned.names;
    let name_ends = scanned.name_ends;
    // Defensive: the selected current rows are always part of the union even
    // if an older manifest line used a parser version this binary skips.
    for row in &rows {
        let key = (row.exchange, row.code.clone());
        if !history
            .iter()
            .any(|existing| existing.exchange == row.exchange && existing.code == row.code)
        {
            history.push(row.clone());
            first_seen.entry(key).or_insert(
                freshness
                    .as_of
                    .as_deref()
                    .and_then(parse_day)
                    .unwrap_or_else(|| {
                        NaiveDate::from_ymd_opt(2000, 1, 1).expect("2000-01-01 is valid")
                    }),
            );
        }
    }
    // A history row is an identity fact from the first snapshot that showed the
    // code; its name would be that snapshot's, which says nothing about now.
    // The names live in `names`.
    for row in &mut history {
        row.name = None;
    }
    history.sort_by(|left, right| {
        (left.exchange, left.code.as_str()).cmp(&(right.exchange, right.code.as_str()))
    });
    let current: BTreeSet<(Exchange, String)> = rows
        .iter()
        .map(|row| (row.exchange, row.code.clone()))
        .collect();
    let mut effective_to = BTreeMap::new();
    for ((exchange, code), entry) in &scanned.rows {
        if current.contains(&(*exchange, code.clone())) {
            continue;
        }
        let end = entry
            .first_missing
            .unwrap_or_else(|| entry.last_seen.succ_opt().unwrap_or(entry.last_seen));
        if end > entry.first_seen {
            effective_to.insert((*exchange, code.clone()), end);
        }
    }
    Ok(Admitted {
        rows,
        history,
        first_seen,
        effective_to,
        names,
        name_ends,
        bindings,
        snapshot_ids,
        freshness,
    })
}

/// Parse one frozen response. Pure function of `(kind, bytes)`.
pub fn parse(kind: ListKind, bytes: &[u8]) -> Result<Vec<EtfListRow>, EtfError> {
    match kind {
        ListKind::Sse => parse_sse(bytes),
        ListKind::Szse => parse_szse(bytes),
    }
}

fn parse_sse(bytes: &[u8]) -> Result<Vec<EtfListRow>, EtfError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        EtfError::invalid("source_invalid", format!("SSE ETF list: not JSON: {error}"))
    })?;
    let help = value
        .get("pageHelp")
        .and_then(Value::as_object)
        .ok_or_else(|| EtfError::invalid("source_invalid", "SSE ETF list: missing pageHelp"))?;
    let data = help.get("data").and_then(Value::as_array).ok_or_else(|| {
        EtfError::invalid(
            "source_invalid",
            "SSE ETF list: pageHelp.data is not an array",
        )
    })?;
    // §3.2: `pageHelp.total` is the publisher's own count; a disagreement is a
    // truncated/mis-paged answer, not "some rows are enough".
    if let Some(total) = help.get("total").and_then(number)
        && total != data.len() as u64
    {
        return Err(EtfError::invalid(
            "etf_total_mismatch",
            format!(
                "SSE ETF list: pageHelp.total={total} but the page carries {} row(s)",
                data.len()
            ),
        ));
    }
    let mut rows = Vec::with_capacity(data.len());
    let mut seen = BTreeSet::new();
    for row in data {
        let code = text(row.get("fundCode")).ok_or_else(|| {
            EtfError::invalid("source_invalid", "SSE ETF list: a row has no fundCode")
        })?;
        check_code("SSE ETF list", &code)?;
        if !seen.insert(code.clone()) {
            return Err(EtfError::invalid(
                "etf_code_duplicate",
                format!("SSE ETF list: code {code} appears twice"),
            ));
        }
        let listing = text(row.get("listingDate")).ok_or_else(|| {
            EtfError::invalid(
                "etf_listing_date_missing",
                format!("SSE ETF list: {code} has no listingDate"),
            )
        })?;
        let listing_date = parse_day(&listing).ok_or_else(|| {
            EtfError::invalid(
                "source_invalid",
                format!("SSE ETF list: {code} listingDate {listing:?} is not a day"),
            )
        })?;
        rows.push(EtfListRow {
            exchange: Exchange::Sh,
            code,
            listing_date: Some(listing_date),
            sub_class: text(row.get("subClass")),
            index_code: text(row.get("INDEX_CODE")),
            // `secNameFull`, not `fundAbbr`: the latter is a truncated
            // on-exchange short name of a different kind (module doc).
            name: row
                .get("secNameFull")
                .and_then(Value::as_str)
                .and_then(clean_name),
        });
    }
    if rows.is_empty() {
        return Err(EtfError::invalid(
            "etf_list_empty",
            "SSE ETF list is empty; 0 rows is not evidence",
        ));
    }
    Ok(rows)
}

fn parse_szse(bytes: &[u8]) -> Result<Vec<EtfListRow>, EtfError> {
    let sheet = crate::exchange_source::xlsx::read_first_sheet(bytes)
        .map_err(|error| EtfError::invalid("source_invalid", format!("SZSE ETF list: {error}")))?;
    let header = sheet
        .rows
        .first()
        .ok_or_else(|| EtfError::invalid("etf_list_empty", "SZSE ETF list: empty workbook"))?;
    let code_column = header
        .iter()
        .position(|cell| cell.trim() == "证券代码")
        .ok_or_else(|| {
            EtfError::invalid(
                "source_invalid",
                format!("SZSE ETF list: header has no 证券代码 column: {header:?}"),
            )
        })?;
    // A workbook without the name column still gives identity; every name is
    // then `None` rather than a failure or a guess.
    let name_column = header.iter().position(|cell| cell.trim() == "证券简称");
    let mut rows = Vec::with_capacity(sheet.data_rows());
    let mut seen = BTreeSet::new();
    for (index, row) in sheet.rows.iter().enumerate().skip(1) {
        let code = row
            .get(code_column)
            .map(|cell| cell.trim().to_string())
            .filter(|code| !code.is_empty())
            .ok_or_else(|| {
                EtfError::invalid(
                    "source_invalid",
                    format!("SZSE ETF list: row {} has no code", index + 1),
                )
            })?;
        check_code("SZSE ETF list", &code)?;
        if !seen.insert(code.clone()) {
            return Err(EtfError::invalid(
                "etf_code_duplicate",
                format!("SZSE ETF list: code {code} appears twice"),
            ));
        }
        rows.push(EtfListRow {
            exchange: Exchange::Sz,
            code,
            listing_date: None,
            sub_class: None,
            index_code: None,
            name: name_column
                .and_then(|column| row.get(column))
                .and_then(|cell| clean_name(cell)),
        });
    }
    if rows.is_empty() {
        return Err(EtfError::invalid(
            "etf_list_empty",
            "SZSE ETF list is empty; 0 rows is not evidence",
        ));
    }
    Ok(rows)
}

/// Business-shape checks over the combined set: non-empty, SZSE floor, and one
/// row per `(exchange, code)`.
fn validate_set(rows: &[EtfListRow]) -> Result<(), EtfError> {
    if rows.is_empty() {
        return Err(EtfError::invalid(
            "etf_list_empty",
            "both ETF lists are empty; 0 rows is not evidence",
        ));
    }
    let szse = rows
        .iter()
        .filter(|row| row.exchange == Exchange::Sz)
        .count();
    if szse < SZSE_MIN_ROWS {
        return Err(EtfError::invalid(
            "etf_row_count_below_floor",
            format!("SZSE ETF list has {szse} row(s), below the shape floor {SZSE_MIN_ROWS}"),
        ));
    }
    let mut seen = BTreeSet::new();
    for row in rows {
        if !seen.insert((row.exchange, row.code.clone())) {
            return Err(EtfError::invalid(
                "etf_code_duplicate",
                format!("{} appears twice", entity(row.exchange, &row.code)),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    row: EtfListRow,
    first_seen: NaiveDate,
    last_seen: NaiveDate,
    first_missing: Option<NaiveDate>,
}

#[derive(Debug, Default)]
struct HistoryScan {
    rows: BTreeMap<(Exchange, String), HistoryEntry>,
    /// Name chains from the manifest alone; see [`NameChains`].
    names: BTreeMap<(Exchange, String), Vec<NameObservation>>,
    /// `first_missing` of every code the manifest's newest snapshot of its
    /// source no longer contains. The selected current list plays no part.
    name_ends: BTreeMap<(Exchange, String), NaiveDate>,
    /// Members of the newest frozen snapshot of each list: what a replay can
    /// check without a refresh run.
    newest: BTreeMap<&'static str, BTreeSet<(Exchange, String)>>,
}

/// One frozen list response to scan.
struct Snapshot {
    kind: ListKind,
    day: NaiveDate,
    acquired_at: String,
    sha256: String,
}

type ObservationMap = BTreeMap<&'static str, BTreeMap<NaiveDate, BTreeSet<(Exchange, String)>>>;

/// Scan every frozen list response, not only the selected page set. This is
/// the monotonic identity input (PR-A2): a code that appeared in any snapshot
/// stays in the union, and its first appearance fixes `effective_from`.
///
/// `first_missing` is the first later observation of the same source that no
/// longer contains the code. It bounds the interval but is not called a
/// delisting date: a list snapshot only proves membership, never the exact day
/// a fund stopped being listed.
///
/// Snapshots are parsed in `(day, acquired_at, sha256)` order and the name
/// chains grow as they go ([`NameChains`]): no snapshot's per-row
/// `acquired_at` or `sha256` is kept, only each chain's own steps.
fn scan_history(store: &SourceStore) -> Result<HistoryScan, EtfError> {
    let mut scan = HistoryScan::default();
    let mut observations: ObservationMap = BTreeMap::new();
    let mut parsed: BTreeSet<String> = BTreeSet::new();
    let mut snapshots: Vec<Snapshot> = Vec::new();
    for record in store.records()? {
        if record.store_kind != StoreKind::HttpFetch {
            continue;
        }
        let Some(source) = SourceId::parse(&record.source_id) else {
            continue;
        };
        let Some(kind) = ListKind::from_source(source) else {
            continue;
        };
        let Some(parser_version) = record.parser_version.as_deref() else {
            continue;
        };
        if PagingFormat::from_parser_version(parser_version).is_none() {
            continue;
        }
        if !parsed.insert(record.sha256.clone()) {
            continue;
        }
        let Some(day) = observation_day(&record.acquired_at) else {
            continue;
        };
        snapshots.push(Snapshot {
            kind,
            day,
            acquired_at: record.acquired_at,
            sha256: record.sha256,
        });
    }
    snapshots.sort_by(|left, right| {
        (left.day, &left.acquired_at, &left.sha256).cmp(&(
            right.day,
            &right.acquired_at,
            &right.sha256,
        ))
    });
    let mut chains = NameChains::default();
    for snapshot in &snapshots {
        let bytes = store.read_verified(&snapshot.sha256)?;
        let rows = parse(snapshot.kind, &bytes)?;
        let members: BTreeSet<(Exchange, String)> = rows
            .iter()
            .map(|row| (row.exchange, row.code.clone()))
            .collect();
        observations
            .entry(snapshot.kind.label())
            .or_default()
            .insert(snapshot.day, members);
        let known_at = crate::calendar::shanghai_date_of(&snapshot.acquired_at)
            .unwrap_or(snapshot.day)
            .max(snapshot.day);
        for row in rows {
            let key = (row.exchange, row.code.clone());
            if let Some(name) = &row.name {
                chains.push(&key, snapshot, known_at, name);
            }
            scan.rows
                .entry(key)
                .and_modify(|entry| {
                    entry.first_seen = entry.first_seen.min(snapshot.day);
                    entry.last_seen = entry.last_seen.max(snapshot.day);
                    if entry.row.listing_date.is_none() && row.listing_date.is_some() {
                        entry.row = row.clone();
                    }
                })
                .or_insert(HistoryEntry {
                    row,
                    first_seen: snapshot.day,
                    last_seen: snapshot.day,
                    first_missing: None,
                });
        }
    }
    scan.names = chains.finish();
    for entries in scan.rows.values_mut() {
        let source = match entries.row.exchange {
            Exchange::Sh => ListKind::Sse.label(),
            Exchange::Sz => ListKind::Szse.label(),
            Exchange::Bj => continue,
        };
        if let Some(by_day) = observations.get(source) {
            entries.first_missing = by_day
                .range((
                    std::ops::Bound::Excluded(entries.last_seen),
                    std::ops::Bound::Unbounded,
                ))
                .find(|(_, members)| {
                    !members.contains(&(entries.row.exchange, entries.row.code.clone()))
                })
                .map(|(day, _)| *day);
        }
    }
    for (key, entry) in &scan.rows {
        if let Some(day) = entry.first_missing {
            scan.name_ends.insert(key.clone(), day);
        }
    }
    scan.newest = observations
        .into_iter()
        .filter_map(|(source, mut by_day)| by_day.pop_last().map(|(_, members)| (source, members)))
        .collect();
    Ok(scan)
}

/// Name chains built in snapshot order.
///
/// Feed samples in `(day, acquired_at, sha256)` order. Of several snapshots on
/// one day the last one decides that day's name, because a writer keyed by
/// `(code, first observed day)` can hold only one. A name equal to the previous
/// segment's extends it instead of starting a new one, so a chain has no
/// adjacent repeats. A snapshot with no usable name is never pushed: it
/// neither ends nor starts a segment.
///
/// A step cites the snapshot that created it: the first one that showed the
/// name on its first day, or the later same-day one that changed the day's
/// name back to it. Later snapshots with the same name do not touch it.
#[derive(Debug, Default)]
struct NameChains {
    by_key: BTreeMap<(Exchange, String), Vec<NameObservation>>,
}

impl NameChains {
    fn push(
        &mut self,
        key: &(Exchange, String),
        snapshot: &Snapshot,
        known_at: NaiveDate,
        name: &str,
    ) {
        let chain = self.by_key.entry(key.clone()).or_default();
        // A step born on this very day came from an earlier snapshot of the
        // day; this snapshot is later in `(acquired_at, sha256)` order, so its
        // name replaces that step's.
        if chain
            .last()
            .is_some_and(|last| last.observed_from == snapshot.day)
        {
            if chain.last().is_some_and(|last| last.name == name) {
                return;
            }
            chain.pop();
        }
        if chain.last().is_some_and(|last| last.name == name) {
            return;
        }
        chain.push(NameObservation {
            observed_from: snapshot.day,
            known_at,
            name: name.to_string(),
            snapshot_sha256: snapshot.sha256.clone(),
        });
    }

    fn finish(self) -> BTreeMap<(Exchange, String), Vec<NameObservation>> {
        self.by_key
            .into_iter()
            .filter(|(_, chain)| !chain.is_empty())
            .collect()
    }
}

/// `source_record_id` of a name step: the list and the snapshot that showed it.
fn name_record_id(exchange: Exchange, sha256: &str) -> String {
    let label = match exchange {
        Exchange::Sh => ListKind::Sse.label(),
        _ => ListKind::Szse.label(),
    };
    format!("{label}:{sha256}")
}

/// The one function both the daily item and `source_store replay` use to turn
/// a manifest scan into ETF name rows. It reads no clock and no selected list.
///
/// `entities` is every code any frozen list showed (the ETF universe the
/// writer validates against). Codes without a chain produce no row.
fn assemble_names(
    names: &BTreeMap<(Exchange, String), Vec<NameObservation>>,
    name_ends: &BTreeMap<(Exchange, String), NaiveDate>,
    entities: BTreeSet<(Exchange, String)>,
) -> Result<EtfNameSet, names::Error> {
    let chains: Vec<EtfNameChain> = names
        .iter()
        .map(|((exchange, code), steps)| EtfNameChain {
            exchange: *exchange,
            code: code.clone(),
            steps: steps
                .iter()
                .map(|step| EtfNameStep {
                    observed_from: step.observed_from,
                    known_at: step.known_at,
                    name: step.name.clone(),
                    source_record_id: name_record_id(*exchange, &step.snapshot_sha256),
                })
                .collect(),
            ended: name_ends.get(&(*exchange, code.clone())).copied(),
        })
        .collect();
    let rows = names::assemble_etf_names(&chains)?;
    names::validate_set_for(&rows, &entities)?;
    Ok(EtfNameSet { rows, entities })
}

/// Lists that name nobody (a workbook that lost its name column, say) must not
/// replace stored names with zero rows: 0 rows is not evidence.
fn check_names_not_empty(set: &EtfNameSet) -> Result<(), EtfError> {
    if set.rows.is_empty() {
        return Err(EtfError::invalid(
            "etf_names_empty",
            "the frozen ETF lists carry no usable name; refusing to replace stored ETF \
             names with zero rows",
        ));
    }
    Ok(())
}

/// Samples kept in evidence for an unresolved-code list.
pub const EVIDENCE_SAMPLE_LIMIT: usize = 10;

fn sample_keys(keys: &[(Exchange, String)]) -> Value {
    json!({
        "count": keys.len(),
        "samples": keys
            .iter()
            .take(EVIDENCE_SAMPLE_LIMIT)
            .map(|(exchange, code)| entity(*exchange, code))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
pub(crate) fn sample_keys_for_test(keys: &[(Exchange, String)]) -> Value {
    sample_keys(keys)
}

/// ETF name rows for the daily item plus the evidence of what they leave open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedNames {
    pub set: EtfNameSet,
    /// Item evidence: row counts and the unresolved codes. Includes what only
    /// the selected list can tell (`names_behind_current`), so it belongs to
    /// the item outcome and never to the stored registration.
    pub evidence: Value,
}

/// The names item's ETF answer: rows, or the reason the branch is pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamesRun {
    Admitted(Box<AdmittedNames>),
    Pending(PendingSource),
}

/// `derived:security_names`' ETF branch: read the frozen lists like the main
/// table item does, then assemble the observed-name rows.
pub fn run_names(
    market: &crate::sqlite_market::backend::MarketBackend,
    today: NaiveDate,
    policy: Policy,
) -> Result<NamesRun, EtfError> {
    Ok(match run_source(market, today, policy)? {
        RunSource::Pending(mut pending) => {
            pending.dataset = names::DATASET_KIND;
            NamesRun::Pending(pending)
        }
        RunSource::Admitted(admitted) => NamesRun::Admitted(Box::new(admitted.names_run()?)),
    })
}

impl Admitted {
    /// The ETF name rows of this read (D-76 Q1) with their evidence.
    pub fn names_run(&self) -> Result<AdmittedNames, EtfError> {
        let entities: BTreeSet<(Exchange, String)> = self
            .first_seen
            .keys()
            .cloned()
            .chain(self.rows.iter().map(|row| (row.exchange, row.code.clone())))
            .collect();
        let set = assemble_names(&self.names, &self.name_ends, entities)
            .map_err(|error| EtfError::invalid("source_invalid", error.to_string()))?;
        check_names_not_empty(&set)?;
        let behind = self.names_behind_current();
        let without = self.codes_without_name();
        let membership = self.membership_behind_current();
        let evidence = json!({
            "status": "ok",
            "algorithm_version": names::ETF_ALGORITHM_VERSION,
            "parser_version": PARSER_VERSION,
            "basis": "fetch_time",
            "rows": set.rows.len(),
            "codes_with_rows": self.names.len(),
            "codes_without_name": sample_keys(&without),
            // The selected list says one name, the manifest chain ends at
            // another (SSE A→B→A, see the module doc). The rows hold the
            // chain; these codes' current name is NOT what the last row says.
            "names_behind_current": sample_keys(&behind),
            "membership_behind_current": sample_keys(&membership),
            "source": self.evidence(),
            "limitations": [
                "effective_from is the first observation day of the name, not the rename day",
                "no row exists before a code's first observation; nothing is back-filled",
                "a return to earlier content on SSE adds no manifest line (A→B→A), so the \
                 chain can end at the earlier name",
            ],
        });
        Ok(AdmittedNames { set, evidence })
    }
}

/// What `source_store replay` can say about ETF names from the manifest alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayedNames {
    /// The store has no frozen ETF list at all: nothing to rebuild, and
    /// nothing is claimed.
    Pending,
    Built(EtfNameSet),
}

/// Rebuild the ETF name rows from the frozen manifest only: no clock, no
/// database, no refresh run. It is the same scan and the same
/// [`assemble_names`] the daily item runs, so the rows are the ones the item
/// writes for the same store.
///
/// Fail-closed: a store with only one of the two lists, a snapshot that does
/// not parse, an empty list or an SZSE list below the shape floor is an error,
/// never an empty or shorter set.
pub fn replay_names(store: &SourceStore) -> Result<ReplayedNames, EtfError> {
    let scan = scan_history(store)?;
    if scan.rows.is_empty() && scan.newest.is_empty() {
        return Ok(ReplayedNames::Pending);
    }
    for kind in [ListKind::Sse, ListKind::Szse] {
        if !scan.newest.contains_key(kind.label()) {
            return Err(EtfError::invalid(
                "snapshot_incomplete",
                format!("no frozen {} snapshot in the store", kind.label()),
            ));
        }
    }
    let szse = scan
        .newest
        .get(ListKind::Szse.label())
        .map_or(0, BTreeSet::len);
    if szse < SZSE_MIN_ROWS {
        return Err(EtfError::invalid(
            "etf_row_count_below_floor",
            format!(
                "newest SZSE ETF list has {szse} row(s), below the shape floor {SZSE_MIN_ROWS}"
            ),
        ));
    }
    let entities = scan.rows.keys().cloned().collect();
    let set = assemble_names(&scan.names, &scan.name_ends, entities)
        .map_err(|error| EtfError::invalid("source_invalid", error.to_string()))?;
    check_names_not_empty(&set)?;
    Ok(ReplayedNames::Built(set))
}

/// The source adapter's contribution to the main table: one source-agnostic
/// [`MemberFact`] per member (M3-2 §3.3). PR-A2 adds its delisted-ETF list by
/// producing the same facts; [`crate::security_master::assembly::member_rows`]
/// is the one writer.
pub fn member_facts(
    admitted: &Admitted,
    tdx_listing_dates: &TdxListingDates,
) -> Result<Vec<MemberFact>, EtfError> {
    let fallback = admitted.as_of().ok_or_else(|| {
        EtfError::invalid(
            "source_invalid",
            "the selected ETF list set has no as_of date",
        )
    })?;
    let source_rows = if admitted.history.is_empty() {
        &admitted.rows
    } else {
        &admitted.history
    };
    let mut facts = Vec::with_capacity(source_rows.len());
    for row in source_rows {
        let first = admitted
            .first_seen
            .get(&(row.exchange, row.code.clone()))
            .copied()
            .unwrap_or(fallback);
        let tdx_listing = tdx_listing_dates
            .get(&(market(row.exchange), row.code.clone()))
            .copied();
        let (listing_date, listing_source) = match row.exchange {
            Exchange::Sh => (row.listing_date, ListingSource::ExchangeList),
            Exchange::Sz => match tdx_listing {
                Some(day) => (Some(day), ListingSource::TdxSsdate),
                // The source carries no listing day and TDX has no `SSDATE`
                // for this code: the fact is still an identity, but the date
                // is `unknown`, never guessed.
                None => (None, ListingSource::Unknown),
            },
            Exchange::Bj => (None, ListingSource::Unknown),
        };
        facts.push(MemberFact {
            exchange: row.exchange,
            code: row.code.clone(),
            first_seen: first,
            known_at: first,
            listing_date,
            listing_source,
            listing_date_tdx: tdx_listing,
            effective_to: admitted
                .effective_to
                .get(&(row.exchange, row.code.clone()))
                .copied(),
            delisting_date: None,
            delisting_source: crate::security_master::DelistingSource::Unknown,
            delisting_reason_class: crate::security_master::DelistingReason::Unknown,
            announcement_date: None,
            announcement_url: None,
            extraction: None,
        });
    }
    Ok(facts)
}

/// The identity rows for the main table (§3.3). Pure function of the admitted
/// set plus the TDX `base.dbf` listing dates.
///
/// Kept as the PR-A convenience wrapper; the row rules live in
/// [`crate::security_master::assembly::member_rows`] so PR-A2 does not copy
/// them.
pub fn identity_rows(
    admitted: &Admitted,
    tdx_listing_dates: &TdxListingDates,
) -> Result<Vec<SecurityLifecycleFact>, EtfError> {
    let facts = member_facts(admitted, tdx_listing_dates)?;
    crate::security_master::assembly::member_rows(&facts).map_err(|error| EtfError::Invalid {
        code: error.code(),
        detail: error.to_string(),
    })
}

fn market(exchange: Exchange) -> u8 {
    match exchange {
        Exchange::Sz => 0,
        Exchange::Sh => 1,
        Exchange::Bj => 2,
    }
}

fn entity(exchange: Exchange, code: &str) -> String {
    format!("{}.{code}", exchange.as_str())
}

fn check_code(label: &str, code: &str) -> Result<(), EtfError> {
    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(EtfError::invalid(
            "source_invalid",
            format!("{label}: {code:?} is not a 6-digit code"),
        ));
    }
    Ok(())
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// A published name, whitespace-normalised: invisible characters dropped, the
/// rest trimmed with every inner whitespace run (ASCII, no-break, ideographic)
/// as one space. Case and character width are left as published. `None` when
/// nothing is left; an empty name is not a name.
fn clean_name(raw: &str) -> Option<String> {
    let visible: String = raw
        .chars()
        .filter(|ch| !ch.is_control() || ch.is_whitespace())
        .filter(|ch| !matches!(ch, '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}'))
        .collect();
    let joined = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    (!joined.is_empty()).then_some(joined)
}

fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// `YYYYMMDD` (SSE) or `YYYY-MM-DD`.
fn parse_day(text: &str) -> Option<NaiveDate> {
    if text.len() == 8 && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return NaiveDate::from_ymd_opt(
            text[..4].parse().ok()?,
            text[4..6].parse().ok()?,
            text[6..8].parse().ok()?,
        );
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

fn observation_day(acquired_at: &str) -> Option<NaiveDate> {
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(acquired_at) {
        return Some(timestamp.date_naive());
    }
    acquired_at.get(..10).and_then(parse_day)
}

#[cfg(test)]
mod tests;
