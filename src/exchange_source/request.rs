//! Canonical request identity, paging plans and the SSE bulletin segmentation
//! (design §6.3/§6.4).
//!
//! Identity is the request, not the response: one [`ExchangeRequest`] carries
//! the URL plus every parameter (including the pagination and segment bounds),
//! and [`ExchangeRequest::fingerprint`] is its canonical JSON. Two fetches with
//! the same fingerprint and the same response sha256 therefore register one
//! source-store line (§6.3 "同指纹、同响应 sha256 不产生新快照") and the
//! fingerprint is also what replay reads back, so no separate side table is
//! needed to rebuild `url`/page bounds.
//!
//! Paging quirks that the entry encodes instead of leaving to consumers:
//!
//! - SSE only honours `beginPage`/`endPage`; a request that sends `pageNo`
//!   alone repeats page 1 (§6.3, evidence E-1/E-2).
//! - SZSE pages by `PAGENO`; its `pageSize` is not adopted (design §17 item 10).
//! - The SSE bulletin index archive starts 2000-01-01 and one request may span
//!   at most one quarter, asserted as ≤ [`MAX_SEGMENT_DAYS`] days (§6.4).

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

/// Earliest date the SSE bulletin index covers (§6.4). Earlier windows are not
/// requested at all; there is no source for them.
pub const BULLETIN_ARCHIVE_START: &str = "2000-01-01";
/// One request spans at most one quarter, which is ≤93 days (§6.4).
pub const MAX_SEGMENT_DAYS: i64 = 93;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RequestError {
    #[error("segment_backwards: {start} is after {end}")]
    Backwards { start: String, end: String },
    #[error("segment_before_archive_start: {start} is before 2000-01-01")]
    BeforeArchiveStart { start: String },
    #[error("segment_too_long: {days} days exceeds the 93-day quarter bound")]
    SegmentTooLong { days: i64 },
    #[error("segment_invalid_date: {0}")]
    InvalidDate(String),
    #[error("request_fingerprint_invalid: {0}")]
    Fingerprint(String),
}

impl RequestError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Backwards { .. } => "segment_backwards",
            Self::BeforeArchiveStart { .. } => "segment_before_archive_start",
            Self::SegmentTooLong { .. } => "segment_too_long",
            Self::InvalidDate(_) => "segment_invalid_date",
            Self::Fingerprint(_) => "request_fingerprint_invalid",
        }
    }
}

/// Inclusive archive window of one request (§6.4).
///
/// The 2000-01-01 lower bound is the strictest known archive start; a consumer
/// with a later start (A-3 suspension, 2008) simply never asks for earlier
/// windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    start: NaiveDate,
    end: NaiveDate,
}

impl Segment {
    pub fn new(start: NaiveDate, end: NaiveDate) -> Result<Self, RequestError> {
        if end < start {
            return Err(RequestError::Backwards {
                start: date_text(start),
                end: date_text(end),
            });
        }
        let archive_start = archive_start();
        if start < archive_start {
            return Err(RequestError::BeforeArchiveStart {
                start: date_text(start),
            });
        }
        let segment = Self { start, end };
        let days = segment.days();
        if days > MAX_SEGMENT_DAYS {
            return Err(RequestError::SegmentTooLong { days });
        }
        Ok(segment)
    }

    pub fn parse(start: &str, end: &str) -> Result<Self, RequestError> {
        Self::new(parse_date(start)?, parse_date(end)?)
    }

    pub fn start(self) -> NaiveDate {
        self.start
    }

    pub fn end(self) -> NaiveDate {
        self.end
    }

    /// Inclusive day count: one quarter is 90–92 days, never more than 93.
    pub fn days(self) -> i64 {
        (self.end - self.start).num_days() + 1
    }

    pub fn start_text(self) -> String {
        date_text(self.start)
    }

    pub fn end_text(self) -> String {
        date_text(self.end)
    }

    /// True when the whole window lies after `today`, i.e. the only window the
    /// entry may accept 0 rows for without evidence (§6.3).
    pub fn is_future(self, today: NaiveDate) -> bool {
        self.start > today
    }

    /// Put the window into the request parameters the source expects.
    pub fn apply(self, params: &mut BTreeMap<String, String>, from: &str, to: &str) {
        params.insert(from.to_string(), self.start_text());
        params.insert(to.to_string(), self.end_text());
    }
}

/// Quarter-aligned windows covering `[start, end]` without gaps or overlap.
///
/// Every returned window is accepted by [`Segment::new`], so a caller cannot
/// accidentally build a 5-quarter request; the test asserts contiguity.
pub fn quarter_segments(start: NaiveDate, end: NaiveDate) -> Result<Vec<Segment>, RequestError> {
    if end < start {
        return Err(RequestError::Backwards {
            start: date_text(start),
            end: date_text(end),
        });
    }
    let mut segments = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        let quarter_end = quarter_end(cursor);
        let window_end = quarter_end.min(end);
        segments.push(Segment::new(cursor, window_end)?);
        let Some(next) = window_end.succ_opt() else {
            break;
        };
        cursor = next;
    }
    Ok(segments)
}

pub fn parse_date(text: &str) -> Result<NaiveDate, RequestError> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| RequestError::InvalidDate(text.to_string()))
}

fn archive_start() -> NaiveDate {
    NaiveDate::from_ymd_opt(2000, 1, 1).expect("2000-01-01 is a valid date")
}

fn date_text(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn quarter_end(date: NaiveDate) -> NaiveDate {
    let (month, day) = match date.month() {
        1..=3 => (3, 31),
        4..=6 => (6, 30),
        7..=9 => (9, 30),
        _ => (12, 31),
    };
    NaiveDate::from_ymd_opt(date.year(), month, day).expect("quarter end is a valid date")
}

/// Pagination dialect of one source (§6.3): also selects the envelope parser
/// whose version is recorded in the manifest, so replay can re-derive the
/// row/page counts without a source-specific parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PagingFormat {
    /// SSE `pageHelp` envelope, paged by `beginPage`/`endPage`.
    SsePageHelp,
    /// SZSE JSON array of `{metadata:{pagecount,...},data:[...]}`, paged by
    /// `PAGENO`.
    SzseMetadata,
}

/// Parser version recorded for SSE `pageHelp` envelopes.
pub const SSE_PARSER_VERSION: &str = "exchange-sse-page-v1";
/// Parser version recorded for SZSE `metadata` envelopes.
pub const SZSE_PARSER_VERSION: &str = "exchange-szse-page-v1";

impl PagingFormat {
    pub fn parser_version(self) -> &'static str {
        match self {
            Self::SsePageHelp => SSE_PARSER_VERSION,
            Self::SzseMetadata => SZSE_PARSER_VERSION,
        }
    }

    pub fn from_parser_version(value: &str) -> Option<Self> {
        match value {
            SSE_PARSER_VERSION => Some(Self::SsePageHelp),
            SZSE_PARSER_VERSION => Some(Self::SzseMetadata),
            _ => None,
        }
    }

    /// Parameters that select one page. SSE keeps `beginPage == endPage` (the
    /// only form it honours); SZSE uses `PAGENO`.
    pub fn apply_page(self, params: &mut BTreeMap<String, String>, page: u32) {
        let page = page.to_string();
        match self {
            Self::SsePageHelp => {
                params.insert("beginPage".to_string(), page.clone());
                params.insert("endPage".to_string(), page);
            }
            Self::SzseMetadata => {
                params.insert("PAGENO".to_string(), page);
            }
        }
    }

    /// First page every dialect starts at.
    pub fn first_page(self) -> u32 {
        1
    }
}

/// One canonical request: URL + sorted parameters, nothing else.
///
/// Field order is fixed, so [`ExchangeRequest::fingerprint`] is byte-stable for
/// equal requests; `params` is a `BTreeMap`, so parameter order cannot leak
/// into the identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeRequest {
    /// Fingerprint format version; a mismatch is refused, not guessed.
    pub version: u32,
    pub source_id: SourceId,
    pub url: String,
    pub params: BTreeMap<String, String>,
}

impl ExchangeRequest {
    pub const VERSION: u32 = 1;

    pub fn new(source_id: SourceId, url: impl Into<String>) -> Self {
        Self {
            version: Self::VERSION,
            source_id,
            url: url.into(),
            params: BTreeMap::new(),
        }
    }

    pub fn param(mut self, name: &str, value: impl Into<String>) -> Self {
        self.params.insert(name.to_string(), value.into());
        self
    }

    pub fn with_params(mut self, params: &BTreeMap<String, String>) -> Self {
        for (name, value) in params {
            self.params.insert(name.clone(), value.clone());
        }
        self
    }

    pub fn paged(mut self, format: PagingFormat, page: u32) -> Self {
        format.apply_page(&mut self.params, page);
        self
    }

    /// Canonical request fingerprint: the JSON of this struct, deterministic
    /// because the fields are ordered and the parameters are sorted.
    pub fn fingerprint(&self) -> String {
        serde_json::to_string(self).expect("an exchange request always serializes")
    }

    pub fn from_fingerprint(text: &str) -> Result<Self, RequestError> {
        let request: Self = serde_json::from_str(text)
            .map_err(|error| RequestError::Fingerprint(error.to_string()))?;
        if request.version != Self::VERSION {
            return Err(RequestError::Fingerprint(format!(
                "unsupported fingerprint version {}",
                request.version
            )));
        }
        Ok(request)
    }

    /// Deterministic `url?a=1&b=2` form for logs and reports (no credentials
    /// exist in these sources; the fingerprint itself stays the identity).
    pub fn display_url(&self) -> String {
        if self.params.is_empty() {
            return self.url.clone();
        }
        let query = self
            .params
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("&");
        format!("{}?{query}", self.url)
    }
}
