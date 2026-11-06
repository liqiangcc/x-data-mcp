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

/// Build delisted ETF name chains from announcements and extractions (PR-4).
///
/// Name source rules:
/// - SZSE: Use `sec_name` from the main announcement (the termination notice's
///   `secName` is the name as of the announcement day).
/// - SSE: Use `short_name` from the extraction CSV (operator-extracted from the
///   PDF, which is the field name from the termination notice).
/// - Fallback: If neither is available, the name is unknown (no row).
///
/// The interval runs from `announcement_date` to `effective_to` (the delisting
/// day when known, or the day after the announcement). One name per code.
///
/// `source_kind='exchange_snapshot'`, `source_record_id` prefixes distinguish
/// the announcement source (`sse_fund_delist_ann:URL` or
/// `szse_etf_notice_ann:URL`).
pub fn delisted_etf_name_chains(
    candidates: &BTreeSet<String>,
    announcements: &AnnouncementSet,
    extractions: &ExtractionSet,
    effective_to_map: &BTreeMap<String, NaiveDate>,
) -> Vec<crate::security_master::names::EtfNameChain> {
    use crate::security_master::names::{EtfNameChain, EtfNameStep};

    let mut chains = Vec::new();
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

        // Decide the name: SZSE uses announcement secName, SSE uses extraction short_name
        let name = match exchange {
            Exchange::Sz => announcement.sec_name.as_ref(),
            Exchange::Sh => extractions
                .records
                .get(&(exchange, code.clone()))
                .and_then(|record| record.short_name.as_ref()),
            Exchange::Bj => None,
        };

        let Some(name) = name else {
            continue;
        };

        let name = name.trim();
        if name.is_empty() {
            continue;
        }

        // Build source_record_id: prefix the announcement URL with the source label
        let source_record_id = format!("{}:{}", announcement.source_label, announcement.url);

        // effective_to is the delisting date from the facts
        let ended = effective_to_map.get(entity).copied();

        chains.push(EtfNameChain {
            exchange,
            code: code.clone(),
            steps: vec![EtfNameStep {
                observed_from: announcement.announcement_date,
                known_at: announcement.announcement_date,
                name: name.to_string(),
                source_record_id,
            }],
            ended,
        });
    }
    chains
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
