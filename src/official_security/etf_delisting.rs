//! Delisted-ETF identity (M3-2 PR-A2, issue-509 §4.1; D-71/D-72).
//!
//! Layer position: this is a **source adapter**, not a second identity rule.
//! It turns two admitted announcement indexes and one operator-imported
//! extraction batch into the same [`MemberFact`] the current ETF lists produce;
//! [`crate::security_master::assembly::member_rows`] remains the only writer.
//!
//! ```text
//! hsjday ETF-segment member ──┐
//! current ETF list member ────┴─► candidate (hsjday-only, never identity alone)
//!                                      │
//! announcement index hit ──────────────┼─► class=2 identity (D-71)
//! frozen PDF extraction ───────────────┘─► delisting_date (D-72)
//! ```
//!
//! ## Sources (D-71, owner 2026-11-03)
//!
//! - SSE `query.sse.com.cn/commonQuery.do?sqlId=COMMON_PL_JJXX_JJGG_L`
//!   (`source_id=sse_fund_delist_ann`): `TITLE=终止上市`, segmented by
//!   `START_DATE`/`END_DATE`. The endpoint ignores paging and answers `result`
//!   as a row array, so [`PagingFormat::SsePageHelpUnpaged`] is its envelope
//!   dialect.
//! - SZSE `www.szse.cn/api/disc/announcement/annList`
//!   (`source_id=szse_etf_notice_ann`): POST JSON, channel
//!   `etfNotice_disc`, `pageSize` capped at 50 and `announceCount` as the
//!   total. `publishTime` is the announcement day; the `attachPath` date is not
//!   used because it is often one day earlier.
//!
//! These are **announcement indexes**, not structured terminated-fund lists.
//! The announcement day is evidence, never the delisting-effective day. The
//! exact day comes only from the D-72 PDF extraction batch; until one exists the
//! row keeps `delisting_date=NULL` and is bounded (not dated) by the first list
//! observation that no longer contained it.
//!
//! ## Identity rule (§4.1)
//!
//! A class=2 row is produced only when a code is **both** a candidate
//! (`hsjday` ETF segment, not in the current list) **and** an announcement hit.
//! A code prefix alone, or "in hsjday but not in the list" alone, is not
//! identity. The prefix table is a discovery filter, never a class source; the
//! 519/550 counterexamples in §3.5 stay outside.
//!
//! ## Extraction batch (D-72)
//!
//! The operator imports announcement PDFs plus the CSV extraction through the
//! existing `source_store import` batch/sha path. Each extraction row names
//! `doc_sha256`; the PDF must already be in the source store. Replay validates
//! the same rules as the live item and never parses a PDF in production code:
//!
//! - `announcement_date <= delisting_date <= announcement_date + 9 days`;
//! - `delisting_date` is after the code's last `.day` bar when that bar is
//!   available, and always after the extracted `last_operating_day` when given.
//!
//! A violation is fail-closed (`etf_delisting_invalid`), never a guessed date.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::exchange_source::request::PagingFormat;
use crate::exchange_source::select::{self, Reader};
use crate::exchange_source::{AcquisitionPlan, Segment, SourceId, quarter_segments};
use crate::security_master::assembly::MemberFact;
use crate::security_master::{DelistingSource, Exchange, ListingSource};
use crate::source_store::{SourceStore, StoreKind};

use super::etf_list::TdxListingDates;

/// Business parser version of the announcement rows.
pub const PARSER_VERSION: &str = "etf-delist-ann-v1";
/// Parser version of the operator-imported extraction CSV.
pub const EXTRACT_PARSER_VERSION: &str = "etf-delist-extract-v1";
/// Source id of a frozen announcement PDF (one blob per PDF).
pub const PDF_SOURCE_ID: &str = "etf_delist_pdf";
/// Source id of one imported extraction CSV batch.
pub const EXTRACT_SOURCE_ID: &str = "etf_delist_extract";

pub const SSE_URL: &str = "https://query.sse.com.cn/commonQuery.do";
pub const SSE_SQL_ID: &str = "COMMON_PL_JJXX_JJGG_L";
pub const SSE_TITLE: &str = "终止上市";
pub const SSE_START: &str = "2006-01-01";

pub const SZSE_URL: &str = "https://www.szse.cn/api/disc/announcement/annList";
pub const SZSE_CHANNEL: &str = "etfNotice_disc";
pub const SZSE_START: &str = "2004-01-01";
pub const SZSE_PAGE_SIZE: usize = 50;

/// Maximum number of days between an announcement and its effective day.
pub const MAX_ANNOUNCEMENT_LEAD_DAYS: i64 = 9;

/// The row values both the live item and `replay --scope exchange` write for a
/// D-72 extraction. One function owns them so the two paths cannot drift.
///
/// `effective_to` is the A-share convention (exclusive upper bound = the
/// delisting-effective day). `effective_from` is normally the announcement day;
/// when the announcement and the delisting day are the same (zero lead), the
/// interval would be empty, so the documented rule takes the day before the
/// delisting day instead of moving the upper bound with `+1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelistedRowValues {
    pub effective_from: NaiveDate,
    pub effective_to: NaiveDate,
    pub delisting_date: NaiveDate,
    pub delisting_source: DelistingSource,
    pub delisting_reason_class: crate::security_master::DelistingReason,
}

/// Shared D-72 row rule (see [`DelistedRowValues`]).
pub fn delisted_row_values(
    announcement_date: NaiveDate,
    delisting_date: NaiveDate,
) -> DelistedRowValues {
    let floor = delisting_date.pred_opt().unwrap_or(delisting_date);
    DelistedRowValues {
        effective_from: announcement_date.min(floor),
        effective_to: delisting_date,
        delisting_date,
        // The PDF is an exchange disclosure. We keep the existing
        // `exchange_list` vocabulary instead of adding a migration; the
        // evidence bundle names the PDF sha and extraction record.
        delisting_source: DelistingSource::ExchangeList,
        delisting_reason_class: crate::security_master::DelistingReason::Other,
    }
}

/// The two admitted announcement indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnnouncementKind {
    Sse,
    Szse,
}

impl AnnouncementKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sse => "sse_fund_delist_ann",
            Self::Szse => "szse_etf_notice_ann",
        }
    }

    pub fn source(self) -> SourceId {
        match self {
            Self::Sse => SourceId::SseFundDelistAnn,
            Self::Szse => SourceId::SzseEtfNoticeAnn,
        }
    }

    pub fn from_source(source: SourceId) -> Option<Self> {
        match source {
            SourceId::SseFundDelistAnn => Some(Self::Sse),
            SourceId::SzseEtfNoticeAnn => Some(Self::Szse),
            _ => None,
        }
    }

    /// Manifest-source-id variant of [`Self::from_source`] for the names
    /// reader, which walks raw manifest lines instead of typed plans.
    pub fn from_source_id(source: &str) -> Option<Self> {
        match source {
            "sse_fund_delist_ann" => Some(Self::Sse),
            "szse_etf_notice_ann" => Some(Self::Szse),
            _ => None,
        }
    }

    fn window_start(self) -> NaiveDate {
        parse_day(match self {
            Self::Sse => SSE_START,
            Self::Szse => SZSE_START,
        })
        .expect("the archive start constants are valid dates")
    }

    fn window_from(self) -> &'static str {
        match self {
            Self::Sse => "START_DATE",
            Self::Szse => "seDateStart",
        }
    }

    fn window_to(self) -> &'static str {
        match self {
            Self::Sse => "END_DATE",
            Self::Szse => "seDateEnd",
        }
    }
}

/// One request plan per announcement index. Both are window sources (the SSE
/// paging parameters select nothing, and the SZSE body carries the window).
pub fn plans() -> Vec<(AnnouncementKind, AcquisitionPlan)> {
    let sse = AcquisitionPlan::new(
        SourceId::SseFundDelistAnn,
        SSE_URL,
        PagingFormat::SsePageHelpUnpaged,
    )
    .param("sqlId", SSE_SQL_ID)
    .param("type", "inParams")
    .param("TITLE", SSE_TITLE)
    .param("SECURITY_CODE", "")
    .param("ORG_BULLETIN_TYPE", "")
    .param("OTHER_TYPE", "")
    .compact_dates()
    .single_page_only()
    .zero_rows_not_observed();
    let szse = AcquisitionPlan::new(
        SourceId::SzseEtfNoticeAnn,
        SZSE_URL,
        PagingFormat::SzseAnnouncement,
    )
    .param("channelCode", SZSE_CHANNEL)
    // `seDate` takes `YYYY-MM-DD`: the endpoint answers HTTP 200 with an
    // empty `{}` body for the compact `YYYYMMDD` shape, which the envelope
    // parser refuses (`missing data object/array`). Verified live
    // 2026-11-05; the 2026-11-03 forensics used the dashed shape.
    .zero_rows_not_observed();
    vec![(AnnouncementKind::Sse, sse), (AnnouncementKind::Szse, szse)]
}

/// The full quarter windows a complete read needs for both sources.
pub fn windows(kind: AnnouncementKind, today: NaiveDate) -> Result<Vec<Segment>, String> {
    // Close the window before "today", the same rule as A-3: a new quarter's
    // first day then still has the previous quarter as its open/closed tail,
    // so a daily item that runs before that day's `tail` refresh cannot demand
    // a quarter the refresh has not frozen yet. An announcement made today is
    // picked up by the next run.
    let Some(end) = today.pred_opt() else {
        return Err("today has no previous day".to_string());
    };
    quarter_segments(kind.window_start(), end).map_err(|error| error.to_string())
}

/// Main-announcement index keyed by entity.
pub type MainAnnouncementMap = BTreeMap<(Exchange, String), MainAnnouncement>;
/// Frozen snapshot ids keyed by source label.
pub type SnapshotIdMap = BTreeMap<&'static str, Vec<String>>;
/// Extraction records keyed by entity.
pub type ExtractionMap = BTreeMap<(Exchange, String), ExtractionRecord>;
/// Last `.day` bar day keyed by `exchange.code`.
pub type LastBarMap = BTreeMap<String, NaiveDate>;

/// One announcement index row. `url` is the announcement/PDF locator used for
/// deduplication and PDF lookup, not a second fetch. `sec_name` is the SZSE
/// `secName` field (PR-4 delisted ETF names); SSE rows have `None`.
/// `snapshot_sha256` is stamped by the reader with the frozen page that carried
/// the row, so a name's `known_at` can be resolved to that page's first arrival
/// day; [`parse`] leaves it empty for pure-parser tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    pub exchange: Exchange,
    pub code: String,
    pub announcement_date: NaiveDate,
    pub title: String,
    pub url: String,
    pub source_id: &'static str,
    pub source_label: &'static str,
    pub sec_name: Option<String>,
    pub snapshot_sha256: String,
}

/// One announcement hit after the main-announcement rule (D-72 §2.2):
/// dedupe by URL, drop `提示性` when a formal notice exists, choose the newest.
/// `sec_name` is the SZSE `secName` field (PR-4 delisted ETF names).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainAnnouncement {
    pub exchange: Exchange,
    pub code: String,
    pub announcement_date: NaiveDate,
    pub title: String,
    pub url: String,
    pub source_id: &'static str,
    pub source_label: &'static str,
    pub only_prompt: bool,
    pub sec_name: Option<String>,
    pub snapshot_sha256: String,
}

/// One row of the D-72 extraction CSV. `short_name` (PR-4) is the场内简称
/// extracted from the termination PDF by the operator's offline script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionRecord {
    pub entity: String,
    pub delisting_date: NaiveDate,
    pub announcement_date: NaiveDate,
    pub source_id: String,
    pub doc_url: String,
    pub doc_sha256: String,
    pub page_no: u32,
    pub snippet: String,
    pub extractor: String,
    pub extracted_at: String,
    pub confidence: String,
    #[serde(default)]
    pub last_operating_day: Option<NaiveDate>,
    #[serde(default)]
    pub registration_date: Option<NaiveDate>,
    #[serde(default)]
    pub short_name: Option<String>,
}

impl ExtractionRecord {
    pub fn exchange(&self) -> Option<Exchange> {
        self.entity
            .split_once('.')
            .and_then(|(exchange, _)| Exchange::parse(exchange))
    }

    pub fn code(&self) -> Option<&str> {
        self.entity
            .split_once('.')
            .map(|(_, code)| code)
            .filter(|code| code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()))
    }

    /// D-72's extraction validation. `last_bar` is the code's last `.day` bar
    /// when the caller has the generation bytes; replay may pass `None` and
    /// still checks the extracted `last_operating_day`.
    pub fn validate(&self, last_bar: Option<NaiveDate>) -> Result<(), EtfDelistingError> {
        let exchange = self.exchange().ok_or_else(|| {
            EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!("{}: entity has no sh/sz exchange", self.entity),
            )
        })?;
        if self.code().is_none() {
            return Err(EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!("{}: entity code is not six digits", self.entity),
            ));
        }
        let expected_source = match exchange {
            Exchange::Sh => AnnouncementKind::Sse.label(),
            Exchange::Sz => AnnouncementKind::Szse.label(),
            Exchange::Bj => "",
        };
        if self.source_id != expected_source {
            return Err(EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!(
                    "{}: source_id {} does not match the {} announcement index",
                    self.entity, self.source_id, expected_source
                ),
            ));
        }
        if self.confidence != "high" && self.confidence != "medium" {
            return Err(EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!(
                    "{}: confidence {:?} is not high/medium",
                    self.entity, self.confidence
                ),
            ));
        }
        let lead = (self.delisting_date - self.announcement_date).num_days();
        if !(0..=MAX_ANNOUNCEMENT_LEAD_DAYS).contains(&lead) {
            return Err(EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!(
                    "{}: announcement {} is not within 0..={} days before delisting {}",
                    self.entity,
                    self.announcement_date,
                    MAX_ANNOUNCEMENT_LEAD_DAYS,
                    self.delisting_date
                ),
            ));
        }
        if let Some(day) = self.last_operating_day
            && self.delisting_date <= day
        {
            return Err(EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!(
                    "{}: delisting {} is not after the extracted last operating day {}",
                    self.entity, self.delisting_date, day
                ),
            ));
        }
        if let Some(day) = last_bar
            && self.delisting_date <= day
        {
            return Err(EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!(
                    "{}: delisting {} is not after the .day last bar {}",
                    self.entity, self.delisting_date, day
                ),
            ));
        }
        if let Some(day) = self.registration_date
            && day > self.delisting_date
        {
            return Err(EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!(
                    "{}: registration date {} is after delisting {}",
                    self.entity, day, self.delisting_date
                ),
            ));
        }
        Ok(())
    }
}

/// The announcement set read from the frozen store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnouncementSet {
    pub main: MainAnnouncementMap,
    pub rows: Vec<Announcement>,
    pub snapshot_ids: SnapshotIdMap,
}

impl AnnouncementSet {
    pub fn evidence(&self) -> Value {
        json!({
            "adapter": "M3-2 delisted ETF announcement index",
            "parser_version": PARSER_VERSION,
            "announcement_rows": self.rows.len(),
            "main_announcements": self.main.len(),
            "sse_main": self.main.values().filter(|a| a.exchange == Exchange::Sh).count(),
            "szse_main": self.main.values().filter(|a| a.exchange == Exchange::Sz).count(),
            "only_prompt": self.main.values().filter(|a| a.only_prompt).count(),
            "snapshot_ids": self.snapshot_ids,
        })
    }
}

/// The extraction batch read from the frozen store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractionSet {
    pub records: ExtractionMap,
    pub extraction_shas: Vec<String>,
}

impl ExtractionSet {
    pub fn evidence(&self) -> Value {
        json!({
            "parser_version": EXTRACT_PARSER_VERSION,
            "extraction_rows": self.records.len(),
            "extraction_documents": self.extraction_shas,
            "documents": self.records.values().map(|record| json!({
                "entity": record.entity,
                "delisting_date": record.delisting_date,
                "announcement_date": record.announcement_date,
                "doc_url": record.doc_url,
                "doc_sha256": record.doc_sha256,
                "page_no": record.page_no,
                "snippet": record.snippet,
                "extractor": record.extractor,
                "extracted_at": record.extracted_at,
                "confidence": record.confidence,
                "last_operating_day": record.last_operating_day,
                "registration_date": record.registration_date,
            })).collect::<Vec<_>>(),
        })
    }
}

/// The daily item's answer for the delisted branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunSource {
    Admitted {
        announcements: AnnouncementSet,
        extractions: ExtractionSet,
    },
    Pending {
        reason: &'static str,
    },
}

/// Read both announcement indexes and the extraction batch from the frozen
/// store. No request, no snapshot, no manifest line.
pub fn read(
    store: &SourceStore,
    market: &crate::sqlite_market::backend::MarketBackend,
    today: NaiveDate,
) -> Result<(AnnouncementSet, ExtractionSet), EtfDelistingError> {
    let reader = Reader::new(store, market).with_today(today);
    let planned = plans();
    let mut rows = Vec::new();
    let mut snapshot_ids: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for (kind, plans) in [AnnouncementKind::Sse, AnnouncementKind::Szse]
        .into_iter()
        .map(|kind| {
            (
                kind,
                planned
                    .iter()
                    .filter(|(planned, _)| *planned == kind)
                    .map(|(_, plan)| plan.clone())
                    .collect::<Vec<_>>(),
            )
        })
    {
        let windows = windows(kind, today)
            .map_err(|error| EtfDelistingError::invalid("source_invalid", error))?;
        let read = reader.windows(
            kind.source(),
            &plans,
            &windows,
            kind.window_from(),
            kind.window_to(),
        )?;
        let mut planned_windows = BTreeSet::new();
        for page in read.pages {
            if let Some((start, _)) = page.window {
                planned_windows.insert(start);
            }
            let bytes = store.read_verified(&page.snapshot_id)?;
            let mut parsed = parse(kind, &bytes)?;
            for row in &mut parsed {
                row.snapshot_sha256 = page.snapshot_id.clone();
            }
            snapshot_ids
                .entry(kind.label())
                .or_default()
                .push(page.snapshot_id.clone());
            rows.append(&mut parsed);
        }
        let expected: BTreeSet<NaiveDate> = windows.iter().map(|window| window.start()).collect();
        if planned_windows != expected {
            let missing: Vec<String> = expected
                .difference(&planned_windows)
                .map(|day| day.to_string())
                .collect();
            return Err(EtfDelistingError::invalid(
                "source_snapshot_not_found",
                format!(
                    "{}: no frozen page for {} window(s): {}; run refresh_exchange_sources full",
                    kind.label(),
                    missing.len(),
                    missing.join(",")
                ),
            ));
        }
    }
    let main = main_announcements(&rows);
    let extractions = read_extractions(store, &BTreeMap::new())?;
    Ok((
        AnnouncementSet {
            main,
            rows,
            snapshot_ids,
        },
        extractions,
    ))
}

/// The daily-item entry: a missing store or no frozen announcement pages means
/// the branch stays `pending_source`; a broken page set is a real failure.
pub fn run_source(
    market: &crate::sqlite_market::backend::MarketBackend,
    today: NaiveDate,
    policy: crate::source_store::Policy,
) -> Result<RunSource, EtfDelistingError> {
    let Some(store) = crate::source_store::configured(policy)
        .map_err(|error| EtfDelistingError::invalid(error.code(), error.detail().to_string()))?
    else {
        return Ok(RunSource::Pending {
            reason: "the M3 source store is not configured for this process \
                     (X_DATA_MCP_SOURCE_STORE); delisted-ETF identity stays pending_source",
        });
    };
    match read(&store, market, today) {
        Ok((announcements, extractions)) => Ok(RunSource::Admitted {
            announcements,
            extractions,
        }),
        Err(EtfDelistingError::Select(select::SelectError::NotFound { .. })) => {
            Ok(RunSource::Pending {
                reason: "the M3 source store has no frozen ETF delisting announcement; \
                         run refresh_exchange_sources full",
            })
        }
        Err(error) => Err(error),
    }
}

/// Parse one frozen announcement response.
pub fn parse(kind: AnnouncementKind, bytes: &[u8]) -> Result<Vec<Announcement>, EtfDelistingError> {
    match kind {
        AnnouncementKind::Sse => parse_sse(bytes),
        AnnouncementKind::Szse => parse_szse(bytes),
    }
}

fn parse_sse(bytes: &[u8]) -> Result<Vec<Announcement>, EtfDelistingError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        EtfDelistingError::invalid(
            "source_invalid",
            format!("SSE delisting announcement: not JSON: {error}"),
        )
    })?;
    let rows = value
        .get("result")
        .or_else(|| value.get("pageHelp").and_then(|help| help.get("data")))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            EtfDelistingError::invalid(
                "source_invalid",
                "SSE delisting announcement: missing result array",
            )
        })?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let title = text(row.get("TITLE")).unwrap_or_default();
        // The index mixes all on-exchange funds. Only a terminated ETF is an
        // ETF identity fact; 500/501/502/505 rows stay out even if they share
        // the title filter.
        if !title.contains("终止上市") || !title.contains("交易型") {
            continue;
        }
        let code = text(row.get("SECURITY_CODE")).ok_or_else(|| {
            EtfDelistingError::invalid(
                "source_invalid",
                "SSE delisting announcement: a row has no SECURITY_CODE",
            )
        })?;
        check_code("SSE delisting announcement", &code)?;
        let date = text(row.get("SSEDATE"))
            .and_then(|text| parse_day(&text))
            .ok_or_else(|| {
                EtfDelistingError::invalid(
                    "source_invalid",
                    format!("SSE delisting announcement: {code} has no SSEDATE"),
                )
            })?;
        let url = text(row.get("URL")).unwrap_or_default();
        let key = (code.clone(), url.clone());
        if !seen.insert(key) {
            continue;
        }
        out.push(Announcement {
            exchange: Exchange::Sh,
            code,
            announcement_date: date,
            title,
            url,
            source_id: AnnouncementKind::Sse.label(),
            source_label: "sse_fund_delist_ann",
            sec_name: None,
            snapshot_sha256: String::new(),
        });
    }
    Ok(out)
}

fn parse_szse(bytes: &[u8]) -> Result<Vec<Announcement>, EtfDelistingError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        EtfDelistingError::invalid(
            "source_invalid",
            format!("SZSE ETF notice: not JSON: {error}"),
        )
    })?;
    let data = value
        .get("data")
        .and_then(|data| match data {
            Value::Array(rows) => Some(rows),
            Value::Object(object) => object.get("data").and_then(Value::as_array),
            _ => None,
        })
        .ok_or_else(|| {
            EtfDelistingError::invalid("source_invalid", "SZSE ETF notice: missing data array")
        })?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for row in data {
        let title = text(row.get("title")).unwrap_or_default();
        let date = text(row.get("publishTime"))
            .and_then(|text| parse_day(&text))
            .ok_or_else(|| {
                EtfDelistingError::invalid(
                    "source_invalid",
                    "SZSE ETF notice: a row has no publishTime",
                )
            })?;
        let url = text(row.get("attachPath")).unwrap_or_default();
        let sec_names = row.get("secName").and_then(Value::as_array);
        let codes = row
            .get("secCode")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                EtfDelistingError::invalid(
                    "source_invalid",
                    "SZSE ETF notice: secCode is not an array",
                )
            })?;
        for (index, code) in codes.iter().enumerate() {
            let Some(code) = code.as_str().map(str::trim).filter(|code| !code.is_empty()) else {
                continue;
            };
            check_code("SZSE ETF notice", code)?;
            let key = (code.to_string(), url.clone());
            if !seen.insert(key) {
                continue;
            }
            let sec_name = sec_names
                .and_then(|names| names.get(index))
                .and_then(|name| name.as_str())
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty());
            out.push(Announcement {
                exchange: Exchange::Sz,
                code: code.to_string(),
                announcement_date: date,
                title: title.clone(),
                url: url.clone(),
                source_id: AnnouncementKind::Szse.label(),
                source_label: "szse_etf_notice_ann",
                sec_name,
                snapshot_sha256: String::new(),
            });
        }
    }
    Ok(out)
}

/// The main-announcement rule (XDM-182 §2.2): dedupe by URL, prefer a formal
/// notice over one whose title contains `提示性`, then take the newest day.
/// If only prompt notices exist, use the newest one and record `only_prompt`.
pub fn main_announcements(rows: &[Announcement]) -> BTreeMap<(Exchange, String), MainAnnouncement> {
    let mut by_url: BTreeMap<(Exchange, String, String), &Announcement> = BTreeMap::new();
    for row in rows {
        let key = (row.exchange, row.code.clone(), row.url.clone());
        match by_url.get(&key) {
            Some(previous)
                if (previous.announcement_date, previous.title.as_str())
                    >= (row.announcement_date, row.title.as_str()) => {}
            _ => {
                by_url.insert(key, row);
            }
        }
    }
    let mut grouped: BTreeMap<(Exchange, String), Vec<&Announcement>> = BTreeMap::new();
    for row in by_url.values() {
        grouped
            .entry((row.exchange, row.code.clone()))
            .or_default()
            .push(row);
    }
    let mut out = BTreeMap::new();
    for (key, rows) in grouped {
        let formal: Vec<&&Announcement> = rows
            .iter()
            .filter(|row| !row.title.contains("提示性"))
            .collect();
        let only_prompt = formal.is_empty();
        let chosen = if only_prompt {
            rows.iter().max_by_key(|row| row.announcement_date)
        } else {
            formal.into_iter().max_by_key(|row| row.announcement_date)
        };
        let Some(chosen) = chosen else {
            continue;
        };
        out.insert(
            key,
            MainAnnouncement {
                exchange: chosen.exchange,
                code: chosen.code.clone(),
                announcement_date: chosen.announcement_date,
                title: chosen.title.clone(),
                url: chosen.url.clone(),
                source_id: chosen.source_id,
                source_label: chosen.source_label,
                only_prompt,
                sec_name: chosen.sec_name.clone(),
                snapshot_sha256: chosen.snapshot_sha256.clone(),
            },
        );
    }
    out
}

/// Read the latest imported extraction batch, verify every named PDF blob and
/// apply the D-72 date rules.
pub fn read_extractions(
    store: &SourceStore,
    last_bars: &LastBarMap,
) -> Result<ExtractionSet, EtfDelistingError> {
    let mut records: BTreeMap<(Exchange, String), ExtractionRecord> = BTreeMap::new();
    let mut extraction_shas = Vec::new();
    let mut parsed = BTreeSet::new();
    let manifest = store.records()?;
    for record in &manifest {
        if record.source_id != EXTRACT_SOURCE_ID
            || record.parser_version.as_deref() != Some(EXTRACT_PARSER_VERSION)
            || record.store_kind != StoreKind::ArchiveImport
        {
            continue;
        }
        if !parsed.insert(record.sha256.clone()) {
            continue;
        }
        let bytes = store.read_verified(&record.sha256)?;
        let rows = parse_extraction_csv(&bytes)?;
        extraction_shas.push(record.sha256.clone());
        for row in rows {
            let exchange = row.exchange().ok_or_else(|| {
                EtfDelistingError::invalid(
                    "etf_delisting_invalid",
                    format!("{}: entity has no sh/sz exchange", row.entity),
                )
            })?;
            let code = row.code().ok_or_else(|| {
                EtfDelistingError::invalid(
                    "etf_delisting_invalid",
                    format!("{}: entity code is not six digits", row.entity),
                )
            })?;
            row.validate(last_bars.get(&row.entity).copied())?;
            let pdf_registered = manifest.iter().any(|pdf| {
                pdf.source_id == PDF_SOURCE_ID
                    && pdf.store_kind == StoreKind::ArchiveImport
                    && pdf.sha256 == row.doc_sha256
            });
            if !pdf_registered {
                return Err(EtfDelistingError::invalid(
                    "etf_delisting_invalid",
                    format!(
                        "{}: extraction names doc_sha256 {} but no frozen PDF is registered",
                        row.entity, row.doc_sha256
                    ),
                ));
            }
            // Read the PDF back once; a missing or tampered blob is corruption,
            // not an extraction we may ignore.
            let pdf = store.read_verified(&row.doc_sha256)?;
            if pdf.is_empty() {
                return Err(EtfDelistingError::invalid(
                    "etf_delisting_invalid",
                    format!("{}: frozen PDF {} is empty", row.entity, row.doc_sha256),
                ));
            }
            let replace = records
                .get(&(exchange, code.to_string()))
                .map(|previous| {
                    (row.extracted_at.as_str(), row.doc_sha256.as_str())
                        >= (previous.extracted_at.as_str(), previous.doc_sha256.as_str())
                })
                .unwrap_or(true);
            if replace {
                records.insert((exchange, code.to_string()), row);
            }
        }
    }
    extraction_shas.sort();
    extraction_shas.dedup();
    Ok(ExtractionSet {
        records,
        extraction_shas,
    })
}

/// Parse the D-72 CSV. The snippet field is quoted when it contains a comma or
/// quote; a header row is required. Unknown extra columns are ignored by name.
pub fn parse_extraction_csv(bytes: &[u8]) -> Result<Vec<ExtractionRecord>, EtfDelistingError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        EtfDelistingError::invalid(
            "etf_delisting_invalid",
            format!("extraction CSV is not UTF-8: {error}"),
        )
    })?;
    let mut lines = csv_rows(text).into_iter();
    let header = lines.next().ok_or_else(|| {
        EtfDelistingError::invalid("etf_delisting_invalid", "extraction CSV is empty")
    })?;
    let index: BTreeMap<String, usize> = header
        .iter()
        .enumerate()
        .map(|(index, name)| (name.trim().to_string(), index))
        .collect();
    let required = [
        "entity",
        "delisting_date",
        "announcement_date",
        "source_id",
        "doc_url",
        "doc_sha256",
        "page_no",
        "snippet",
        "extractor",
        "extracted_at",
        "confidence",
    ];
    for name in required {
        if !index.contains_key(name) {
            return Err(EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!("extraction CSV header has no {name} column"),
            ));
        }
    }
    let field = |row: &[String], name: &str| -> String {
        index
            .get(name)
            .and_then(|index| row.get(*index))
            .map(|value| value.trim().to_string())
            .unwrap_or_default()
    };
    let optional_day =
        |row: &[String], name: &str| -> Result<Option<NaiveDate>, EtfDelistingError> {
            let text = field(row, name);
            if text.is_empty() {
                return Ok(None);
            }
            parse_day(&text).map(Some).ok_or_else(|| {
                EtfDelistingError::invalid(
                    "etf_delisting_invalid",
                    format!("extraction CSV {name} {text:?} is not a date"),
                )
            })
        };
    let mut out = Vec::new();
    for (line, row) in lines.enumerate() {
        if row.iter().all(|value| value.trim().is_empty()) {
            continue;
        }
        if row.len() != header.len() {
            return Err(EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!(
                    "extraction CSV line {} has {} column(s), header has {}",
                    line + 2,
                    row.len(),
                    header.len()
                ),
            ));
        }
        let parse_required_day = |name: &str| -> Result<NaiveDate, EtfDelistingError> {
            let text = field(&row, name);
            parse_day(&text).ok_or_else(|| {
                EtfDelistingError::invalid(
                    "etf_delisting_invalid",
                    format!(
                        "extraction CSV line {} {name} {text:?} is not a date",
                        line + 2
                    ),
                )
            })
        };
        let page_no = field(&row, "page_no").parse::<u32>().map_err(|_| {
            EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!("extraction CSV line {} page_no is not a number", line + 2),
            )
        })?;
        let doc_sha256 = field(&row, "doc_sha256");
        if doc_sha256.len() != 64 || !doc_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(EtfDelistingError::invalid(
                "etf_delisting_invalid",
                format!(
                    "extraction CSV line {} doc_sha256 is not a sha256",
                    line + 2
                ),
            ));
        }
        out.push(ExtractionRecord {
            entity: field(&row, "entity"),
            delisting_date: parse_required_day("delisting_date")?,
            announcement_date: parse_required_day("announcement_date")?,
            source_id: field(&row, "source_id"),
            doc_url: field(&row, "doc_url"),
            doc_sha256,
            page_no,
            snippet: field(&row, "snippet"),
            extractor: field(&row, "extractor"),
            extracted_at: field(&row, "extracted_at"),
            confidence: field(&row, "confidence"),
            last_operating_day: optional_day(&row, "last_operating_day")?,
            registration_date: optional_day(&row, "registration_date")?,
            short_name: {
                let text = field(&row, "short_name");
                if text.is_empty() { None } else { Some(text) }
            },
        });
    }
    Ok(out)
}

/// The ETF code segments used only to *discover* candidates. Membership is
/// still decided by the exchange lists plus announcements (§3.5).
pub fn is_etf_candidate_code(exchange: Exchange, code: &str) -> bool {
    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let prefix3 = &code[..3];
    match exchange {
        Exchange::Sh => matches!(
            prefix3,
            "510"
                | "511"
                | "512"
                | "513"
                | "515"
                | "516"
                | "517"
                | "518"
                | "520"
                | "526"
                | "530"
                | "551"
                | "560"
                | "561"
                | "562"
                | "563"
                | "588"
                | "589"
        ),
        Exchange::Sz => matches!(prefix3, "158" | "159"),
        Exchange::Bj => false,
    }
}

/// Turn candidate + announcement + extraction into the source-agnostic member
/// facts. Codes that are not both a candidate and an announcement hit are
/// skipped (D-71).
pub fn member_facts(
    candidates: &BTreeSet<String>,
    announcements: &AnnouncementSet,
    extractions: &ExtractionSet,
    tdx_listing_dates: &TdxListingDates,
    last_bars: &LastBarMap,
) -> Result<Vec<MemberFact>, EtfDelistingError> {
    let mut facts = Vec::new();
    for entity in candidates {
        let Some((exchange, code)) = split_entity(entity) else {
            continue;
        };
        if !is_etf_candidate_code(exchange, &code) {
            continue;
        }
        let Some(announcement) = announcements.main.get(&(exchange, code.clone())) else {
            continue;
        };
        let extraction = extractions.records.get(&(exchange, code.clone()));
        if let Some(extraction) = extraction {
            // The live path has the generation's `.day` archive, so it checks
            // the strict last-bar bound here; replay passes the same map when
            // it has ETF series and otherwise checked `last_operating_day`.
            extraction.validate(last_bars.get(entity).copied())?;
        }
        let listing = tdx_listing_dates
            .get(&(market(exchange), code.clone()))
            .copied();
        let (first_seen, delisting_date, delisting_source, effective_to, reason) = match extraction
        {
            Some(record) => {
                let values = delisted_row_values(record.announcement_date, record.delisting_date);
                (
                    values.effective_from,
                    Some(values.delisting_date),
                    values.delisting_source,
                    Some(values.effective_to),
                    values.delisting_reason_class,
                )
            }
            None => (
                announcement.announcement_date,
                None,
                DelistingSource::Unknown,
                announcement.announcement_date.succ_opt(),
                crate::security_master::DelistingReason::Unknown,
            ),
        };
        facts.push(MemberFact {
            exchange,
            code: code.clone(),
            first_seen,
            known_at: first_seen,
            listing_date: listing,
            listing_source: if listing.is_some() {
                ListingSource::TdxSsdate
            } else {
                ListingSource::Unknown
            },
            listing_date_tdx: listing,
            effective_to,
            delisting_date,
            delisting_source,
            delisting_reason_class: reason,
            announcement_date: Some(announcement.announcement_date),
            announcement_url: Some(announcement.url.clone()),
            extraction: extraction.map(|record| record.doc_sha256.clone()),
        });
    }
    Ok(facts)
}

/// Candidate entities from the generation's `.day` members minus the current
/// ETF list. The filter is deliberately narrow (see [`is_etf_candidate_code`]).
pub fn candidates<'a>(
    day_members: impl IntoIterator<Item = &'a String>,
    current: &BTreeSet<String>,
) -> BTreeSet<String> {
    day_members
        .into_iter()
        .filter(|entity| !current.contains(*entity))
        .filter(|entity| {
            split_entity(entity)
                .map(|(exchange, code)| is_etf_candidate_code(exchange, &code))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// PR-4: names of delisted ETFs (D-76 Q4/Q5, etf-name-search §3/§6/§9).
//
// Layer position: these are **source facts**, not an identity rule. The
// caller says which class-2 entities the main table closed (targets); this
// module only picks a name for each from the two admitted announcement
// indexes and the D-72 extraction batch, then turns it into an
// `exchange_snapshot` name interval. Both the daily item and
// `source_store replay --scope delisted_etf_names` call the same pure
// functions, so live and replay cannot drift.
//
// ## Which name
//
// - SZSE (46): the `secName` of the **termination notice**. The
//   `etfNotice_disc` index also carries rename notices, and its `secName` is
//   the name at *that announcement's* publication time (取证 §4/§5), so the
//   newest row in the channel is not necessarily the termination value.
//   [`termination_notice`] selects, per code, rows whose title contains
//   `终止上市`, prefers a formal notice over one marked `提示性`, then takes
//   the newest day. A rename notice can never supply a delisted ETF's name.
//   When `announcements.main` (the D-72 identity main-announcement rule)
//   points at a different URL, evidence counts `identity_notice_mismatch`
//   and the name is still taken from the termination notice; the *identity*
//   rule is unchanged.
// - SSE (82): the `short_name` column of the operator-imported D-72
//   extraction CSV. The SSE index has no short-name field (取证 §4) and the
//   announcement title carries only the fund's full name, which is a
//   different shape and is never substituted. Production code never parses a
//   PDF; `scripts/extract_etf_short_names.py` produces the CSV offline.
// - Neither available → no row. The read surface answers `name: null`,
//   `name_basis: unavailable` (§3, D-76 Q4), never an empty string and never
//   a guessed name.
//
// ## Interval (补充 2026-11-06, replacing the withdrawn §5 of the task)
//
// `effective_from` is the **termination announcement day** = the name's first
// observation day, exactly the basis of the current ETF name rows. The main
// table's `effective_from` is the *listing* day after D-80 (#571), so reading
// it here would push a name announced in 2026 back to 2005. `effective_to` is
// the main table's `effective_to` (the delisting-effective day, exclusive),
// so the name never outlives the security. When the announcement day is not
// earlier than `effective_to`, the interval would be empty: no row is written
// and evidence counts `empty_interval`.
//
// ## known_at
//
// These are backfilled rows: the bytes first entered this system in 2026 even
// when the announcement itself is from 2006–2024. Following
// `risk_warnings::resolve_known_at`, `known_at` is the **Shanghai day the
// deciding bytes first entered the source store**, never the announcement day
// and never the day this code runs. [`arrival_days`] reads the manifest's
// `imported_at` (a one-off import's first arrival, D-72) or `acquired_at`,
// takes the minimum per sha256, and converts with `calendar::shanghai_date_of`.
// Both the announcement page and, for SSE, the extraction CSV are needed, so
// `known_at` is the later of their two first-arrival days, and never earlier
// than `effective_from`.
// ---------------------------------------------------------------------------

/// A class-2 entity the main table has closed. `effective_to` is the
/// identity interval's exclusive upper bound; `None` is not a delisted ETF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelistedEtfTarget {
    pub exchange: Exchange,
    pub code: String,
    pub effective_to: NaiveDate,
}

impl DelistedEtfTarget {
    pub fn entity(&self) -> String {
        format!("{}.{}", self.exchange.as_str(), self.code)
    }
}

/// Read the class-2 delisted ETFs the main table knows about.
///
/// The names item depends on the main table: without a stored
/// `security_master` there is no evidence of *which* ETFs are delisted, so
/// the caller must fail closed (`not_ready`) instead of writing zero rows.
pub fn delisted_targets(
    market: &crate::sqlite_market::backend::MarketBackend,
) -> Result<Vec<DelistedEtfTarget>, EtfDelistingError> {
    let rows = market
        .read(crate::sqlite_market::security::read_security_master)
        .map_err(|error| {
            EtfDelistingError::invalid("security_master_unavailable", error.to_string())
        })?;
    let mut targets = Vec::new();
    for row in rows {
        if row.instrument_class != 2 || !matches!(row.exchange, Exchange::Sh | Exchange::Sz) {
            continue;
        }
        let Some(effective_to) = row.effective_to else {
            continue;
        };
        targets.push(DelistedEtfTarget {
            exchange: row.exchange,
            code: row.code,
            effective_to,
        });
    }
    targets.sort_by(|left, right| {
        (left.exchange, left.code.as_str()).cmp(&(right.exchange, right.code.as_str()))
    });
    Ok(targets)
}

/// The Shanghai day each frozen blob first entered the store, keyed by sha256.
/// `imported_at` wins over `acquired_at` for an archive import: the latter is
/// the archive's own historical time, the former is when this system first
/// held the bytes (source-store §9.4.4). The minimum per sha is the first
/// arrival, so re-registering identical bytes never moves a fact backwards.
pub fn arrival_days(store: &SourceStore) -> Result<BTreeMap<String, NaiveDate>, EtfDelistingError> {
    let mut days: BTreeMap<String, NaiveDate> = BTreeMap::new();
    for record in store.records()? {
        let stamp = record.imported_at.as_deref().unwrap_or(&record.acquired_at);
        let Some(day) = crate::calendar::shanghai_date_of(stamp) else {
            return Err(EtfDelistingError::invalid(
                "source_invalid",
                format!(
                    "source store record {} has no readable arrival time: {stamp:?}",
                    record.sha256
                ),
            ));
        };
        days.entry(record.sha256.clone())
            .and_modify(|seen| *seen = (*seen).min(day))
            .or_insert(day);
    }
    Ok(days)
}

/// The termination notice of one SZSE code (see the module-level rule).
///
/// `rows` is the code's announcement rows from the frozen index. A row whose
/// title has no `终止上市` is a different disclosure (typically a rename) and
/// is excluded. Within the termination rows, a formal notice beats a
/// `提示性` one; ties go to the newest day, then the URL for determinism.
pub fn termination_notice<'a>(rows: &[&'a Announcement]) -> Option<&'a Announcement> {
    let termination: Vec<&&Announcement> = rows
        .iter()
        .filter(|row| row.title.contains("终止上市"))
        .collect();
    if termination.is_empty() {
        return None;
    }
    let formal: Vec<&&Announcement> = termination
        .iter()
        .filter(|row| !row.title.contains("提示性"))
        .copied()
        .collect();
    let candidates = if formal.is_empty() {
        termination
    } else {
        formal
    };
    candidates
        .into_iter()
        .max_by(|left, right| {
            (left.announcement_date, left.url.as_str())
                .cmp(&(right.announcement_date, right.url.as_str()))
        })
        .copied()
}

/// Read every registered announcement index page and extraction batch from
/// the manifest alone: no database, no clock, no window freshness.
///
/// This is the names-only reader. The identity item keeps its own
/// [`read`]/[`run_source`] (window selection with freshness); the two must not
/// be confused. Every page of the two admitted source ids is parsed, deduped
/// by sha256, and deduped again by `(exchange, code, url)` inside
/// [`main_announcements`], so an extra re-frozen window cannot change the
/// chosen main announcement. The daily item and replay both call this
/// function, which is what makes their rows identical.
pub fn read_names_source(
    store: &SourceStore,
) -> Result<(AnnouncementSet, ExtractionSet, BTreeMap<String, NaiveDate>), EtfDelistingError> {
    let mut rows = Vec::new();
    let mut snapshot_ids: SnapshotIdMap = BTreeMap::new();
    let mut parsed = BTreeSet::new();
    for record in store.records()? {
        let Some(kind) = AnnouncementKind::from_source_id(&record.source_id) else {
            continue;
        };
        if record.store_kind != StoreKind::HttpFetch || !parsed.insert(record.sha256.clone()) {
            continue;
        }
        let bytes = store.read_verified(&record.sha256)?;
        let mut page_rows = parse(kind, &bytes)?;
        for row in &mut page_rows {
            row.snapshot_sha256 = record.sha256.clone();
        }
        snapshot_ids
            .entry(kind.label())
            .or_default()
            .push(record.sha256.clone());
        rows.append(&mut page_rows);
    }
    for ids in snapshot_ids.values_mut() {
        ids.sort();
        ids.dedup();
    }
    let main = main_announcements(&rows);
    let extractions = read_extractions(store, &BTreeMap::new())?;
    Ok((
        AnnouncementSet {
            main,
            rows,
            snapshot_ids,
        },
        extractions,
        arrival_days(store)?,
    ))
}

/// What the delisted-name branch decided, for the item's evidence.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DelistedEtfNameEvidence {
    pub targets: usize,
    pub named: usize,
    /// No usable `secName`/`short_name`: `name_basis: unavailable`.
    pub no_name: usize,
    /// The announcement day is not earlier than `effective_to`, so the
    /// interval would be empty; no row, no back-fill to the listing day.
    pub empty_interval: usize,
    /// `announcements.main` (identity) is not the termination notice this
    /// name comes from; counted, never used to guess a name.
    pub identity_notice_mismatch: usize,
    /// The code already has a snapshot name segment that starts at or after
    /// the announcement day; adding this row would overlap, so it is skipped.
    pub overlap_skipped: usize,
    pub missing_announcement: usize,
    /// The deciding bytes have no readable arrival day (should be impossible
    /// for a registered record; counted when a name was available).
    pub missing_known_at: usize,
}

impl DelistedEtfNameEvidence {
    pub fn as_value(&self) -> Value {
        json!({
            "status": "ok",
            "algorithm_version": crate::security_master::names::ETF_ALGORITHM_VERSION,
            "parser_version": PARSER_VERSION,
            "extraction_parser_version": EXTRACT_PARSER_VERSION,
            "basis": "announcement_day",
            "targets": self.targets,
            "rows": self.named,
            "no_name": self.no_name,
            "empty_interval": self.empty_interval,
            "identity_notice_mismatch": self.identity_notice_mismatch,
            "overlap_skipped": self.overlap_skipped,
            "missing_announcement": self.missing_announcement,
            "missing_known_at": self.missing_known_at,
            "limitations": [
                "effective_from is the termination announcement day (the name's first \
                 observation), not the listing day; the main table's effective_from is \
                 the listing day since D-80",
                "effective_to is the main table's effective_to (exclusive delisting day)",
                "an announcement day not earlier than effective_to writes no row and is \
                 counted as empty_interval; nothing is back-filled to the listing day",
                "known_at is the Shanghai day the deciding bytes first entered the source \
                 store, never the announcement day",
                "SZSE names come only from a termination-titled notice; SSE names only \
                 from the extraction CSV's short_name; a fund's full name is never used",
            ],
        })
    }
}

/// The delisted-name rows plus the evidence of what they leave open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DelistedEtfNames {
    pub rows: Vec<crate::security_master::names::NameInterval>,
    pub entities: BTreeSet<(Exchange, String)>,
    pub evidence: DelistedEtfNameEvidence,
}

/// Build the delisted ETF name intervals. Pure function of the targets, the
/// frozen announcement/extraction bytes and their manifest arrival days.
///
/// This is the single rule shared by the daily item and replay. It never
/// reads a clock, the database or the announcement titles' full names.
pub fn assemble_delisted_etf_names(
    targets: &[DelistedEtfTarget],
    announcements: &AnnouncementSet,
    extractions: &ExtractionSet,
    arrivals: &BTreeMap<String, NaiveDate>,
) -> DelistedEtfNames {
    use crate::security_master::names::{NameCertainty, NameInterval, NameSourceKind};

    let mut out = DelistedEtfNames::default();
    out.evidence.targets = targets.len();
    // Group once so the termination-notice rule sees the whole time series.
    let mut by_code: BTreeMap<(Exchange, &str), Vec<&Announcement>> = BTreeMap::new();
    for row in &announcements.rows {
        by_code
            .entry((row.exchange, row.code.as_str()))
            .or_default()
            .push(row);
    }
    for target in targets {
        let key = (target.exchange, target.code.clone());
        out.entities.insert(key.clone());
        let main = announcements.main.get(&key);
        let main_row = main.and_then(|main| {
            announcements.rows.iter().find(|row| {
                row.exchange == target.exchange && row.code == target.code && row.url == main.url
            })
        });
        let code_rows = by_code
            .get(&(target.exchange, target.code.as_str()))
            .cloned()
            .unwrap_or_default();
        // The name's first observation day is the termination announcement
        // day. The identity main-announcement is the D-72 rule; when it is a
        // rename notice from the same channel, the termination notice is the
        // name's date instead (and the mismatch is counted).
        let notice = match target.exchange {
            Exchange::Sz => termination_notice(&code_rows).or(main_row),
            Exchange::Sh | Exchange::Bj => main_row,
        };
        let Some(notice) = notice else {
            out.evidence.missing_announcement += 1;
            continue;
        };
        if main.is_some_and(|main| main.url != notice.url) {
            out.evidence.identity_notice_mismatch += 1;
        }
        let observed_from = notice.announcement_date;
        if observed_from >= target.effective_to {
            out.evidence.empty_interval += 1;
            continue;
        }
        let name = match target.exchange {
            Exchange::Sz => notice
                .sec_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty()),
            Exchange::Sh => extractions
                .records
                .get(&key)
                .and_then(|record| record.short_name.as_deref())
                .map(str::trim)
                .filter(|name| !name.is_empty()),
            Exchange::Bj => None,
        };
        let Some(name) = name else {
            out.evidence.no_name += 1;
            continue;
        };
        // known_at: the later of the page's and the extraction CSV's first
        // arrival days (both bytes are needed), never before the announcement.
        let mut known_at = arrivals.get(&notice.snapshot_sha256).copied();
        if target.exchange == Exchange::Sh {
            for sha in &extractions.extraction_shas {
                if let Some(day) = arrivals.get(sha) {
                    known_at = Some(known_at.map_or(*day, |current| current.max(*day)));
                }
            }
        }
        let Some(known_at) = known_at.map(|day| day.max(observed_from)) else {
            out.evidence.missing_known_at += 1;
            continue;
        };
        out.rows.push(NameInterval {
            exchange: target.exchange,
            code: target.code.clone(),
            effective_from: observed_from,
            effective_to: Some(target.effective_to),
            name: name.to_string(),
            certainty: NameCertainty::Interval,
            known_at: Some(known_at),
            source_kind: NameSourceKind::ExchangeSnapshot,
            // Q5: one vocabulary, prefix distinguishes the announcement.
            source_record_id: Some(format!("{}:{}", notice.source_label, notice.url)),
        });
    }
    out.rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
    out.evidence.named = out.rows.len();
    out
}

/// Attach the delisted rows to the current ETF name set without overlapping
/// intervals (补充 §5): a snapshot chain's last segment is closed at the
/// announcement day and the announcement name becomes the next segment.
///
/// `entities` is extended with every target so the writer can validate a row
/// of a code whose only name is the announcement one. A set whose last
/// snapshot segment already starts at/after the announcement day is left
/// alone and counted as `overlap_skipped` — no duplicate primary key and no
/// overlapping interval can reach `validate_set_for`.
pub fn merge_delisted_names(
    current: crate::security_master::names::EtfNameSet,
    delisted: &mut DelistedEtfNames,
) -> Result<crate::security_master::names::EtfNameSet, crate::security_master::names::Error> {
    use crate::security_master::names::{EtfNameSet, NameCertainty, NameSourceKind};

    let mut rows = current.rows;
    let mut entities = current.entities;
    entities.extend(delisted.entities.iter().cloned());
    // The last `exchange_snapshot` row of each code (rows are sorted by
    // identity then effective_from, so a later one wins).
    let mut last: BTreeMap<(Exchange, String), usize> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        if row.source_kind == NameSourceKind::ExchangeSnapshot {
            last.insert((row.exchange, row.code.clone()), index);
        }
    }
    let mut kept = Vec::with_capacity(delisted.rows.len());
    for row in delisted.rows.drain(..) {
        if let Some(&index) = last.get(&(row.exchange, row.code.clone())) {
            if rows[index].effective_from >= row.effective_from {
                delisted.evidence.overlap_skipped += 1;
                continue;
            }
            rows[index].effective_to = Some(row.effective_from);
            rows[index].certainty = NameCertainty::Interval;
        }
        kept.push(row);
    }
    rows.extend(kept);
    rows.sort_by(|left, right| {
        (left.exchange, left.code.as_str(), left.effective_from).cmp(&(
            right.exchange,
            right.code.as_str(),
            right.effective_from,
        ))
    });
    delisted.evidence.named = delisted
        .evidence
        .named
        .saturating_sub(delisted.evidence.overlap_skipped);
    crate::security_master::names::validate_set_for(&rows, &entities)?;
    Ok(EtfNameSet { rows, entities })
}

/// The one entry both `derived:security_names` and
/// `source_store replay --scope delisted_etf_names` use.
///
/// It is fail-closed on the dependency: a database without a stored
/// `security_master` cannot say which ETFs are delisted, so the caller must
/// report `not_ready` (writes nothing) rather than replace the ETF subset
/// with zero rows. With a master but no announcement/extraction bytes the
/// answer is an empty set with `targets` counted: the names simply are not
/// available, not "there are no delisted names".
pub fn delisted_names_from_store(
    store: &SourceStore,
    market: &crate::sqlite_market::backend::MarketBackend,
) -> Result<(Vec<DelistedEtfTarget>, DelistedEtfNames), EtfDelistingError> {
    let targets = delisted_targets(market)?;
    let (announcements, extractions, arrivals) = read_names_source(store)?;
    let names = assemble_delisted_etf_names(&targets, &announcements, &extractions, &arrivals);
    Ok((targets, names))
}

/// Whether the database carries a `security_master` at all. Used by the
/// replay scope's readiness gate: the table itself may legitimately be empty
/// (a fresh database has no master rows yet), which is also `not_ready`.
pub fn delisted_names_ready(
    market: &crate::sqlite_market::backend::MarketBackend,
) -> Result<bool, EtfDelistingError> {
    let rows = market
        .read(crate::sqlite_market::security::read_security_master)
        .map_err(|error| {
            EtfDelistingError::invalid("security_master_unavailable", error.to_string())
        })?;
    Ok(rows.iter().any(|row| row.instrument_class == 2))
}

#[derive(Debug, thiserror::Error)]
pub enum EtfDelistingError {
    #[error(transparent)]
    Select(#[from] select::SelectError),
    #[error(transparent)]
    Store(#[from] crate::source_store::StoreError),
    #[error("{code}: {detail}")]
    Invalid { code: &'static str, detail: String },
}

impl EtfDelistingError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Select(error) => error.code(),
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

fn split_entity(entity: &str) -> Option<(Exchange, String)> {
    let (exchange, code) = entity.split_once('.')?;
    let exchange = Exchange::parse(exchange)?;
    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((exchange, code.to_string()))
}

fn market(exchange: Exchange) -> u8 {
    match exchange {
        Exchange::Sz => 0,
        Exchange::Sh => 1,
        Exchange::Bj => 2,
    }
}

fn check_code(label: &str, code: &str) -> Result<(), EtfDelistingError> {
    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(EtfDelistingError::invalid(
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

fn parse_day(text: &str) -> Option<NaiveDate> {
    let text = text.trim();
    if text.len() == 8 && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return NaiveDate::from_ymd_opt(
            text[..4].parse().ok()?,
            text[4..6].parse().ok()?,
            text[6..8].parse().ok()?,
        );
    }
    NaiveDate::parse_from_str(text.get(..10).unwrap_or(text), "%Y-%m-%d").ok()
}

/// Minimal RFC 4180 row reader: quoted fields, doubled quotes and embedded
/// newlines. The extraction file is produced by the operator's one-off script,
/// but the importer must not silently split a snippet that contains a comma.
fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => quoted = true,
            ',' => {
                row.push(std::mem::take(&mut field));
            }
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            '\r' => {}
            _ => field.push(ch),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests;
