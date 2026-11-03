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
//!                                      parse (etf-list-v1)
//!                                                        ▼
//!              identity_rows() ──► security_master class=2 rows (writer)
//! ```
//!
//! ## Admitted sources (D-70 Q1)
//!
//! - SSE `FUND_LIST` (`commonSoaQuery.do`): `source_id=sse_etf_list`. Carries
//!   `fundCode`, `listingDate`, `subClass` and `INDEX_CODE`; the listing day is
//!   `listing_source=exchange_list`.
//! - SZSE `1945` (`ShowReport?SHOWTYPE=xlsx`): `source_id=szse_etf_list`.
//!   Carries code/name/fitted index/size/manager but **no listing day**; the
//!   SZSE listing day comes from TDX `base.dbf SSDATE`
//!   (`listing_source=tdx_ssdate`, D-47).
//! - SSE `ebs` is a quote snapshot, not an identity source (D-70 Q1). It is
//!   never registered here.
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
//! `source_refresh_overdue`). The caller turns any of them into a failed item,
//! so the previous main-table version stays readable.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde_json::{Value, json};

use crate::exchange_source::select::{self, AsOf, Freshness, Reader};
use crate::exchange_source::{
    AcquisitionPlan, BasisStatus, BindError, PagingFormat, SnapshotBinding, SourceId,
};
use crate::security_master::assembly::{DATASET_KIND, MemberFact, PendingSource};
use crate::security_master::{Exchange, ListingSource, SecurityLifecycleFact};
use crate::source_store::{Policy, SourceStore, StoreError, StoreKind};

/// Business-field parser version of this adapter. The source-store manifest
/// records the *envelope* parser version (`exchange-sse-page-v2` /
/// `exchange-szse-xlsx-v1`), which is what replay re-derives row/page counts
/// with; this consumer version is registered on the item's evidence and is what
/// a re-derivation of the identity rows pins.
pub const PARSER_VERSION: &str = "etf-list-v1";

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
    pub rows: Vec<EtfListRow>,
    /// `(exchange, code) → acquisition date of the first frozen snapshot that
    /// contained the code`.
    pub first_seen: BTreeMap<(Exchange, String), NaiveDate>,
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
    Admitted(Admitted),
    Pending(PendingSource),
}

/// Resolve the deployment's source store, then read the frozen ETF lists.
///
/// Under [`Policy::Optional`] (stdio, tests, non-ops) an unconfigured store is
/// not a read failure: the main-table item keeps the TDX/A-share result and
/// records `pending_source`. Under [`Policy::Required`] (the ops entry) a
/// missing or unreadable store fails with `source_store_unconfigured` instead of
/// writing a main table with no ETF identity.
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
        Ok(admitted) => Ok(RunSource::Admitted(admitted)),
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
    let first_seen = first_seen(store, &rows)?;
    Ok(Admitted {
        rows,
        first_seen,
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

/// The acquisition date of the first frozen snapshot that contained each code.
///
/// It scans every frozen response of the two sources, not only the selected
/// page set: `effective_from` is "first appeared in a frozen snapshot", and a
/// code that was already present in an older snapshot must keep that older
/// date. The scan is a pure function of the store (bytes + `acquired_at`), so a
/// replay reproduces it without HTTP.
fn first_seen(
    store: &SourceStore,
    rows: &[EtfListRow],
) -> Result<BTreeMap<(Exchange, String), NaiveDate>, EtfError> {
    let wanted: BTreeSet<(Exchange, String)> = rows
        .iter()
        .map(|row| (row.exchange, row.code.clone()))
        .collect();
    let mut seen: BTreeMap<(Exchange, String), NaiveDate> = BTreeMap::new();
    let mut parsed: BTreeSet<String> = BTreeSet::new();
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
        let bytes = store.read_verified(&record.sha256)?;
        for row in parse(kind, &bytes)? {
            let key = (row.exchange, row.code);
            if !wanted.contains(&key) {
                continue;
            }
            seen.entry(key)
                .and_modify(|current| {
                    if day < *current {
                        *current = day;
                    }
                })
                .or_insert(day);
        }
    }
    Ok(seen)
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
    let mut facts = Vec::with_capacity(admitted.rows.len());
    for row in &admitted.rows {
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
