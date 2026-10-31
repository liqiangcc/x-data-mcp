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
//! Identity and wire form are separate: [`ExchangeRequest::fingerprint`] is
//! computed from `params` only, while [`ExchangeRequest::display_url`] is the
//! ready-to-send URL with RFC 3986-encoded query components (XDM-144). Every
//! outbound caller uses that one function, so no request path can send raw
//! non-ASCII text to the stdlib helper.
//!
//! Paging quirks that the entry encodes instead of leaving to consumers:
//!
//! - SSE list services (`commonQuery.do`) ignore `beginPage`/`endPage` — both
//!   spellings answer the same first 50 rows — and instead honour
//!   `pageHelp.pageNo`/`pageHelp.pageSize` (2026-10-30 evidence: one request with
//!   `pageHelp.pageSize=2000` returned all 1,845 rows, `pageCount=1`), which is
//!   [`PagingFormat::SsePageHelp`]. The older `commonSoaQuery` services are the
//!   same way round (PR-11 evidence), which is [`PagingFormat::SseSoaPageNo`].
//! - SZSE pages by `PAGENO`; its `pageSize` is not adopted (design §17 item 10).
//! - The SSE bulletin index archive starts 2000-01-01 and one request may span
//!   at most one quarter, asserted as ≤ [`MAX_SEGMENT_DAYS`] days (§6.4).

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use super::SourceId;

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

    /// Put the window into the request parameters the source expects, as
    /// `YYYY-MM-DD` (see [`DateFormat`] for the compact shape).
    pub fn apply(self, params: &mut BTreeMap<String, String>, from: &str, to: &str) {
        self.apply_with(params, from, to, DateFormat::Iso);
    }

    /// Same, in the date shape the source documents.
    pub fn apply_with(
        self,
        params: &mut BTreeMap<String, String>,
        from: &str,
        to: &str,
        format: DateFormat,
    ) {
        params.insert(from.to_string(), format.text(self.start));
        params.insert(to.to_string(), format.text(self.end));
    }
}

/// How a window bound is written into a request parameter.
///
/// It is part of the canonical fingerprint, so the choice cannot be lost
/// between acquisition and replay: `commonSoaQuery` silently answers 0 rows for
/// dashed dates (PR-11 evidence), which would read as a quiet window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DateFormat {
    Iso,
    Compact,
}

impl DateFormat {
    pub fn text(self, date: NaiveDate) -> String {
        match self {
            Self::Iso => date_text(date),
            Self::Compact => date.format("%Y%m%d").to_string(),
        }
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
    /// SSE list services (`sseQuery/commonQuery.do`), paged by
    /// `pageHelp.pageNo` with a fixed page size of [`SSE_PAGE_SIZE`]. The
    /// endpoint ignores `beginPage`/`endPage` (2026-10-30 evidence: both
    /// spellings answered the same first 50 rows) but honours
    /// `pageHelp.pageSize`; the lists are small enough to arrive in one page.
    SsePageHelp,
    /// SSE `commonSoaQuery.do` services (A-3 suspension events), paged by
    /// `pageHelp.pageNo` with a fixed page size of [`SSE_SOA_PAGE_SIZE`]; the
    /// same endpoint ignores `beginPage`/`endPage` and always reports
    /// `pageHelp.beginPage=1` (PR-11 evidence: a request with
    /// `beginPage=2&endPage=2` answered page 1).
    SseSoaPageNo,
    /// SZSE JSON array of `{metadata:{tabkey,pagecount,...},data:[...]}`,
    /// paged by `PAGENO`. The block is chosen by the `TABKEY` the request
    /// declared; a request without one takes the only block that carries
    /// rows, and several non-empty blocks fail closed (#446 D1).
    SzseMetadata,
    /// SZSE `metadata` envelopes written while the block was chosen by
    /// `tabkey=tab2` first, unique non-empty block second. Kept only for
    /// replay: the same bytes must keep their old counts.
    SzseMetadataV2,
    /// SZSE `metadata` envelopes frozen before the block-selection change:
    /// these always read the first block. Kept only for replay.
    SzseMetadataV1,
}

/// Parser version recorded for SSE `pageHelp` envelopes.
pub const SSE_PARSER_VERSION: &str = "exchange-sse-page-v2";
/// The SSE `pageHelp` version written before the 2026-10-30 dialect change.
/// Old snapshots stay replayable: they carry no `pageHelp.pageNo`, so both
/// versions read the page from `beginPage`.
pub const SSE_PARSER_VERSION_V1: &str = "exchange-sse-page-v1";
/// Parser version recorded for SSE `commonSoaQuery.do` envelopes.
pub const SSE_SOA_PARSER_VERSION: &str = "exchange-sse-soa-page-v1";
/// Parser version recorded for SZSE `metadata` envelopes. v3 selects the
/// block the request declared (`TABKEY`), otherwise the only non-empty block;
/// several non-empty blocks fail closed (#446 D1: `CATALOGID=1110` answers four
/// blocks and only tab1 carries the current list).
pub const SZSE_PARSER_VERSION: &str = "exchange-szse-page-v3";
/// The SZSE `metadata` version that chose `tabkey=tab2` first and the only
/// non-empty block second. Frozen snapshots keep that meaning on replay: the
/// same bytes must not silently change their row/page counts.
pub const SZSE_PARSER_VERSION_V2: &str = "exchange-szse-page-v2";
/// The SZSE `metadata` version written before the block-selection fix. Old
/// snapshots stay replayable with their old meaning: v1 always read the first
/// block, so replay must not re-read them as tab2.
pub const SZSE_PARSER_VERSION_V1: &str = "exchange-szse-page-v1";
/// `pageHelp.pageSize` the SSE list services answer with; the largest list
/// measured 1,845 rows, so one page covers the whole window.
pub const SSE_PAGE_SIZE: u32 = 2000;
/// `pageHelp.pageSize` the SSE `commonSoaQuery.do` services answer with.
pub const SSE_SOA_PAGE_SIZE: u32 = 25;

impl PagingFormat {
    pub fn parser_version(self) -> &'static str {
        match self {
            Self::SsePageHelp => SSE_PARSER_VERSION,
            Self::SseSoaPageNo => SSE_SOA_PARSER_VERSION,
            Self::SzseMetadata => SZSE_PARSER_VERSION,
            Self::SzseMetadataV2 => SZSE_PARSER_VERSION_V2,
            Self::SzseMetadataV1 => SZSE_PARSER_VERSION_V1,
        }
    }

    pub fn from_parser_version(value: &str) -> Option<Self> {
        match value {
            SSE_PARSER_VERSION | SSE_PARSER_VERSION_V1 => Some(Self::SsePageHelp),
            SSE_SOA_PARSER_VERSION => Some(Self::SseSoaPageNo),
            SZSE_PARSER_VERSION => Some(Self::SzseMetadata),
            SZSE_PARSER_VERSION_V2 => Some(Self::SzseMetadataV2),
            SZSE_PARSER_VERSION_V1 => Some(Self::SzseMetadataV1),
            _ => None,
        }
    }

    /// Parameters that select one page. SSE list services take
    /// `pageHelp.pageNo` with [`SSE_PAGE_SIZE`] (`beginPage`/`endPage` are
    /// ignored); `commonSoaQuery` takes the same pair with
    /// [`SSE_SOA_PAGE_SIZE`]; SZSE uses `PAGENO`.
    pub fn apply_page(self, params: &mut BTreeMap<String, String>, page: u32) {
        let page = page.to_string();
        match self {
            Self::SsePageHelp => {
                params.insert("pageHelp.pageSize".to_string(), SSE_PAGE_SIZE.to_string());
                params.insert("pageHelp.pageNo".to_string(), page);
            }
            Self::SseSoaPageNo => {
                params.insert(
                    "pageHelp.pageSize".to_string(),
                    SSE_SOA_PAGE_SIZE.to_string(),
                );
                params.insert("pageHelp.pageNo".to_string(), page);
            }
            Self::SzseMetadata | Self::SzseMetadataV2 | Self::SzseMetadataV1 => {
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

    /// The outbound URL: this exact `url?name=value&...` string is what
    /// `fetch` hands to the helper and what `freeze`/`replay` record in the
    /// `url` column. Query names and values are RFC 3986 percent-encoded
    /// (UTF-8 bytes; unreserved `A-Z a-z 0-9 - . _ ~` kept, everything else
    /// `%XX`, space is `%20`), because the HTTP helper only accepts ASCII
    /// (XDM-144, Refs #446). The separators `?`, `&` and `=` stay literal, and
    /// the encoding runs once over the raw parameter text, so a `%` that is
    /// part of a value becomes `%25` (callers pass raw values, never
    /// pre-encoded ones).
    ///
    /// The fingerprint is *not* encoded: it is the canonical JSON of
    /// [`ExchangeRequest`], so [`ExchangeRequest::fingerprint`] is byte-stable
    /// and unchanged by this output form. There are no credentials in these
    /// sources; the fingerprint itself stays the identity.
    pub fn display_url(&self) -> String {
        if self.params.is_empty() {
            return self.url.clone();
        }
        let query = self
            .params
            .iter()
            .map(|(name, value)| {
                format!(
                    "{}={}",
                    encode_query_component(name),
                    encode_query_component(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        format!("{}?{query}", self.url)
    }
}

/// RFC 3986 percent-encoding of one query component (name or value).
///
/// The input is raw text: every byte outside the unreserved set
/// `A-Z a-z 0-9 - . _ ~` is written as `%XX` (uppercase hex), so UTF-8
/// multi-byte characters become one `%XX` per byte and a space is `%20`, never
/// `+`. A literal `%` in the input is not a pre-existing escape: it is encoded
/// to `%25` like any other reserved byte, and this function is never applied
/// twice to the same text.
///
/// The query delimiters `?`, `&` and `=` are added by
/// [`ExchangeRequest::display_url`], never encoded here.
pub(crate) fn encode_query_component(raw: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                write!(out, "%{byte:02X}").expect("writing to a String cannot fail");
            }
        }
    }
    out
}
