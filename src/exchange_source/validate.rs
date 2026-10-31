//! Response validation of the exchange entry (design §6.3/§6.4).
//!
//! Three kinds of checks, all before any consumer parser runs:
//!
//! 1. HTTP status — a non-2xx answer is `source_unavailable`.
//! 2. Format — the body must be the recorded envelope; otherwise
//!    `source_invalid`.
//! 3. Counts — every page must belong to the same page count, the pages we
//!    fetched must be exactly that set, and `pageHelp.total`/`recordcount`
//!    must equal the rows actually collected. A mismatch is `truncated`
//!    (failure), never "some rows are enough" (§6.3/§6.4).
//!
//! A 0-row answer is a failure by default: "0 rows is not evidence" (§6.4,
//! `work/evidence-notes.md` U-1: a 5-quarter request silently returned
//! `data:[]`). Only a window the caller declared empty in advance — in this
//! entry, a fully future window — may be 0 rows ([`EmptyPolicy`]).

use serde_json::Value;

use super::request::{PagingFormat, Segment};
use chrono::NaiveDate;

/// How many rows one page carried and which page of how many it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEnvelope {
    pub page: u32,
    pub page_count: u32,
    pub rows: usize,
    /// Publisher-declared total for the *whole window* (SSE `pageHelp.total`,
    /// SZSE `metadata.recordcount`), when the envelope exposes one. An SSE
    /// `total` of `0` is reported as `None`: that endpoint uses 0 for "no
    /// total", not for "no rows" (§6.2 A-1 evidence).
    pub total: Option<u64>,
}

/// The declared-empty-window rule (§6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyPolicy {
    /// 0 rows fails with `unverified_zero`.
    Forbidden,
    /// 0 rows is acceptable because the whole window is in the future and the
    /// source cannot have anything for it.
    DeclaredEmptyWindow,
}

impl EmptyPolicy {
    /// The entry's only explicit declaration: a window that starts after
    /// `today` may be empty. Everything else must produce rows.
    pub fn for_window(window: Option<Segment>, today: NaiveDate) -> Self {
        match window {
            Some(segment) if segment.is_future(today) => Self::DeclaredEmptyWindow,
            _ => Self::Forbidden,
        }
    }
}

/// What a 0-row window means to one consumer (BR-M3-10, §6.3).
///
/// A 0-row answer is never evidence of absence. The two honest reactions are:
///
/// - [`ZeroPolicy::FailClosed`] (the default): the window is a failure
///   (`unverified_zero`), because a writer that concludes something from the
///   window cannot tell a quiet window from a broken query.
/// - [`ZeroPolicy::NotObserved`]: the consumer only derives *positive* facts
///   from rows (A-3 suspension events, D-60), so the window stays an
///   explicitly unobserved window: no row is produced, the window is named in
///   the item's evidence, and no "no event" conclusion is stored. The source
///   itself answers 0 rows for real windows (the SSE suspension table returns
///   nothing for 2008 Q1 while 2008 Q2 answers 2,306 rows, PR-11 evidence), so
///   failing the whole dataset for one quiet quarter would be a permanent
///   failure rather than caution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZeroPolicy {
    FailClosed,
    NotObserved,
}

/// Failure class of the entry (§6.3 "失败分三类").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Network or HTTP status: nothing usable arrived.
    SourceUnavailable,
    /// Format or count validation: something arrived but it is not admissible.
    SourceInvalid,
    /// A multi-window run where one window failed: frozen snapshots of the
    /// other windows are kept, the uncovered window produces no rows.
    PartiallyFailed,
}

impl FailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceUnavailable => "source_unavailable",
            Self::SourceInvalid => "source_invalid",
            Self::PartiallyFailed => "partially_failed",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidateError {
    #[error("source_unavailable: http status {status}")]
    HttpStatus { status: u16 },
    #[error("source_invalid: {detail}")]
    Format { detail: String },
    #[error("unverified_zero: {rows} rows over {pages} page(s) is not evidence")]
    UnverifiedZero { pages: usize, rows: usize },
    #[error("truncated: {detail}")]
    Truncated { detail: String },
}

impl ValidateError {
    pub fn class(&self) -> FailureClass {
        match self {
            Self::HttpStatus { .. } => FailureClass::SourceUnavailable,
            Self::Format { .. } | Self::UnverifiedZero { .. } | Self::Truncated { .. } => {
                FailureClass::SourceInvalid
            }
        }
    }

    /// Stable reason code for reports and the source store problems list.
    pub fn code(&self) -> &'static str {
        match self {
            Self::HttpStatus { .. } => "source_unavailable",
            Self::Format { .. } => "source_invalid",
            Self::UnverifiedZero { .. } => "unverified_zero",
            Self::Truncated { .. } => "truncated",
        }
    }
}

/// The validated page set of one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSet {
    pub pages: Vec<PageEnvelope>,
    pub row_count: usize,
    pub page_count: u32,
}

/// A non-2xx answer never reaches the parser (§6.3).
pub fn require_status(status: u16) -> Result<(), ValidateError> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(ValidateError::HttpStatus { status })
    }
}

/// Parse the pagination envelope of one page.
///
/// Only the envelope is decoded here — the row payload stays the consumer
/// parser's business (PR-3/PR-10/PR-11/PR-12), so this function must not start
/// interpreting source fields.
pub fn parse_page(format: PagingFormat, body: &[u8]) -> Result<PageEnvelope, ValidateError> {
    let value: Value = serde_json::from_slice(body).map_err(|error| ValidateError::Format {
        detail: format!("not JSON: {error}"),
    })?;
    match format {
        PagingFormat::SsePageHelp => parse_sse(&value),
        PagingFormat::SseSoaPageNo => parse_sse_soa(&value),
        PagingFormat::SzseMetadata => parse_szse(&value),
        PagingFormat::SzseMetadataV1 => parse_szse_first_block(&value),
    }
}

/// `commonSoaQuery.do` envelope (A-3): same `pageHelp` object, but the page
/// number is `pageHelp.pageNo` (the endpoint ignores `beginPage`/`endPage`),
/// the class keeps `pageHelp.total` (it is a real count here, unlike the
/// `GPLB_GP_L` list), and a quiet window answers `pageCount=0` with
/// `data: []`.
fn parse_sse_soa(value: &Value) -> Result<PageEnvelope, ValidateError> {
    let help = value
        .get("pageHelp")
        .and_then(Value::as_object)
        .ok_or_else(|| ValidateError::Format {
            detail: "missing pageHelp object".to_string(),
        })?;
    let rows = help
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ValidateError::Format {
            detail: "pageHelp.data is not an array".to_string(),
        })?;
    let page = help
        .get("pageNo")
        .and_then(number)
        .ok_or_else(|| ValidateError::Format {
            detail: "pageHelp.pageNo is missing".to_string(),
        })?;
    let page_count =
        help.get("pageCount")
            .and_then(number)
            .ok_or_else(|| ValidateError::Format {
                detail: "pageHelp.pageCount is missing".to_string(),
            })?;
    Ok(PageEnvelope {
        page: clamp_u32(page),
        page_count: clamp_u32(page_count),
        rows: rows.len(),
        total: help.get("total").and_then(number),
    })
}

fn parse_sse(value: &Value) -> Result<PageEnvelope, ValidateError> {
    let help = value
        .get("pageHelp")
        .and_then(Value::as_object)
        .ok_or_else(|| ValidateError::Format {
            detail: "missing pageHelp object".to_string(),
        })?;
    let rows = help
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ValidateError::Format {
            detail: "pageHelp.data is not an array".to_string(),
        })?;
    // The list services answer with `pageHelp.pageNo` (2026-10-30 evidence);
    // `beginPage` is only a fallback for older frozen snapshots.
    let page = help
        .get("pageNo")
        .and_then(number)
        .or_else(|| help.get("beginPage").and_then(number))
        .unwrap_or(1);
    let page_count =
        help.get("pageCount")
            .and_then(number)
            .ok_or_else(|| ValidateError::Format {
                detail: "pageHelp.pageCount is missing".to_string(),
            })?;
    Ok(PageEnvelope {
        page: clamp_u32(page),
        page_count: clamp_u32(page_count),
        rows: rows.len(),
        // `pageHelp.total` used to be `0` on several SSE sqlIds even when rows
        // were returned (M2 A-1 evidence); the 2026-10-30 list responses report
        // a real total. A zero therefore still means "not reported", not "no
        // rows": treating it as a count would reject an old frozen page.
        total: help
            .get("total")
            .and_then(number)
            .filter(|total| *total > 0),
    })
}

/// Pick the SZSE block that carries this list.
///
/// The 2026-10-30 real responses always contain two blocks: `tab1` (暂停上市,
/// zero rows, `pagecount=0`) followed by `tab2` (终止上市, the data). Taking
/// `blocks.first()` therefore reads the empty block and fails later with
/// `page_count is 0`. The rule is:
///
/// 1. `metadata.tabkey == "tab2"` — the terminated list;
/// 2. otherwise the only block whose `data` array is non-empty;
/// 3. otherwise the only block (the old single-block shape);
/// 4. otherwise fail closed instead of silently picking the first block.
///
/// `a1a2::rows` calls this same function so the row array and the pagination
/// metadata can never come from different blocks.
pub(crate) fn szse_block(value: &Value) -> Result<&Value, ValidateError> {
    let blocks = value.as_array().ok_or_else(|| ValidateError::Format {
        detail: "top level is not an array of blocks".to_string(),
    })?;
    if blocks.is_empty() {
        return Err(ValidateError::Format {
            detail: "empty block array".to_string(),
        });
    }
    if let Some(block) = blocks
        .iter()
        .find(|block| block_tabkey(block) == Some("tab2"))
    {
        return Ok(block);
    }
    let mut non_empty = blocks.iter().filter(|block| {
        block
            .get("data")
            .and_then(Value::as_array)
            .is_some_and(|data| !data.is_empty())
    });
    match (non_empty.next(), non_empty.next()) {
        (Some(block), None) => Ok(block),
        (None, _) if blocks.len() == 1 => Ok(&blocks[0]),
        (None, _) => Err(ValidateError::Format {
            detail: "no block has metadata.tabkey=\"tab2\" and no unique block has data; \
                     refusing to pick the first block"
                .to_string(),
        }),
        (Some(_), Some(_)) => Err(ValidateError::Format {
            detail: "no block has metadata.tabkey=\"tab2\" and several blocks have data; \
                     refusing to pick one"
                .to_string(),
        }),
    }
}

/// Pick the SZSE block the request declared with `TABKEY`.
///
/// The short-name change table (`SSGSGMXX&TABKEY=tab2`) answers with the same
/// two-block shape as the delisting lists: `tab1` is an empty tab and `tab2`
/// carries the rows. A consumer that reads "the first block with a `data`
/// array" therefore reads `tab1`'s zero rows and a whole-market table looks
/// empty. The declared tab is the request's own selector, so the row array must
/// come from that block and from nowhere else:
///
/// 1. `declared` present: exactly the block whose `metadata.tabkey` equals it; a
///    response without that block is refused instead of reading another one;
/// 2. no declaration: the only block with a non-empty `data` array, or the only
///    block at all (the old single-block shape);
/// 3. several non-empty blocks and no declaration: fail closed instead of
///    picking the first.
///
/// `a1a2`'s delisting lists use [`szse_block`], whose `tab2` preference is part
/// of that list's own contract; this function exists for consumers whose request
/// names its tab, so the parser can never drift from the plan.
pub(crate) fn szse_block_for<'a>(
    value: &'a Value,
    declared: Option<&str>,
) -> Result<&'a Value, ValidateError> {
    let blocks = value.as_array().ok_or_else(|| ValidateError::Format {
        detail: "top level is not an array of blocks".to_string(),
    })?;
    if blocks.is_empty() {
        return Err(ValidateError::Format {
            detail: "empty block array".to_string(),
        });
    }
    if let Some(declared) = declared.map(str::trim).filter(|tab| !tab.is_empty()) {
        return blocks
            .iter()
            .find(|block| block_tabkey(block) == Some(declared))
            .ok_or_else(|| ValidateError::Format {
                detail: format!(
                    "no block has metadata.tabkey={declared:?}; the request declared that tab, \
                     refusing to read another block"
                ),
            });
    }
    let mut non_empty = blocks.iter().filter(|block| {
        block
            .get("data")
            .and_then(Value::as_array)
            .is_some_and(|data| !data.is_empty())
    });
    match (non_empty.next(), non_empty.next()) {
        (Some(block), None) => Ok(block),
        (None, _) if blocks.len() == 1 => Ok(&blocks[0]),
        (None, _) => Err(ValidateError::Format {
            detail: "no block has a non-empty data array and no tabkey was declared; \
                     refusing to pick the first block"
                .to_string(),
        }),
        (Some(_), Some(_)) => Err(ValidateError::Format {
            detail: "several blocks have a non-empty data array and no tabkey was declared; \
                     refusing to pick one"
                .to_string(),
        }),
    }
}

fn block_tabkey(block: &Value) -> Option<&str> {
    block
        .get("metadata")
        .and_then(|metadata| metadata.get("tabkey"))
        .and_then(Value::as_str)
        .map(str::trim)
}

fn parse_szse(value: &Value) -> Result<PageEnvelope, ValidateError> {
    parse_szse_block(szse_block(value)?)
}

/// v1 replay: the first block, exactly as the old parser read it.
fn parse_szse_first_block(value: &Value) -> Result<PageEnvelope, ValidateError> {
    let blocks = value.as_array().ok_or_else(|| ValidateError::Format {
        detail: "top level is not an array of blocks".to_string(),
    })?;
    let block = blocks.first().ok_or_else(|| ValidateError::Format {
        detail: "empty block array".to_string(),
    })?;
    parse_szse_block(block)
}

fn parse_szse_block(block: &Value) -> Result<PageEnvelope, ValidateError> {
    let metadata = block
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or_else(|| ValidateError::Format {
            detail: "missing metadata object".to_string(),
        })?;
    let rows =
        block
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| ValidateError::Format {
                detail: "block.data is not an array".to_string(),
            })?;
    let page_count =
        metadata
            .get("pagecount")
            .and_then(number)
            .ok_or_else(|| ValidateError::Format {
                detail: "metadata.pagecount is missing".to_string(),
            })?;
    Ok(PageEnvelope {
        // The SZSE endpoint spells this `pageno`; older fixtures captured
        // `pageNo`. Both are the same field and neither is guessed from the
        // request page (a missing one would be reported as page 1 and rejected
        // as a mismatch on page 2). Both are read from the selected block.
        page: clamp_u32(
            metadata
                .get("pageNo")
                .or_else(|| metadata.get("pageno"))
                .and_then(number)
                .unwrap_or(1),
        ),
        page_count: clamp_u32(page_count),
        rows: rows.len(),
        total: metadata.get("recordcount").and_then(number),
    })
}

/// Numbers arrive as JSON numbers or as decimal strings depending on the
/// endpoint; both are accepted, anything else is not a count.
fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

fn clamp_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Validate the whole page set of one window.
///
/// The checks are intentionally redundant: a truncated page set that happens to
/// sum to `total` is still refused because the page numbers must be exactly
/// `1..=page_count`.
pub fn validate_pages(
    pages: &[PageEnvelope],
    policy: EmptyPolicy,
    zero: ZeroPolicy,
) -> Result<PageSet, ValidateError> {
    let first = pages.first().ok_or_else(|| ValidateError::Format {
        detail: "window produced no page".to_string(),
    })?;
    let page_count = first.page_count;
    if page_count == 0 {
        // A quiet window of a `commonSoaQuery` source answers `pageCount=0`
        // with no rows. Only a consumer that treats 0 rows as an unobserved
        // window may accept it, and only when nothing was returned.
        if zero == ZeroPolicy::NotObserved && pages.iter().all(|page| page.rows == 0) {
            return Ok(PageSet {
                pages: pages.to_vec(),
                row_count: 0,
                page_count: 0,
            });
        }
        return Err(ValidateError::Format {
            detail: "page_count is 0".to_string(),
        });
    }
    let mut seen = Vec::new();
    let mut row_count = 0usize;
    let mut total: Option<u64> = None;
    for page in pages {
        if page.page_count != page_count {
            return Err(ValidateError::Truncated {
                detail: format!(
                    "page {} reports pageCount {} but the window has {}",
                    page.page, page.page_count, page_count
                ),
            });
        }
        if page.page == 0 || page.page > page_count {
            return Err(ValidateError::Truncated {
                detail: format!("page {} is outside 1..={}", page.page, page_count),
            });
        }
        if seen.contains(&page.page) {
            return Err(ValidateError::Truncated {
                detail: format!("page {} was collected twice", page.page),
            });
        }
        seen.push(page.page);
        row_count += page.rows;
        match (total, page.total) {
            (Some(previous), Some(current)) if previous != current => {
                return Err(ValidateError::Truncated {
                    detail: format!("total changed between pages: {previous} then {current}"),
                });
            }
            (None, Some(current)) => total = Some(current),
            _ => {}
        }
    }
    if seen.len() != page_count as usize {
        return Err(ValidateError::Truncated {
            detail: format!(
                "collected {} page(s) but the window declares {page_count}",
                seen.len()
            ),
        });
    }
    if let Some(total) = total
        && total != row_count as u64
    {
        return Err(ValidateError::Truncated {
            detail: format!("declared total {total} but collected {row_count} row(s)"),
        });
    }
    if row_count == 0 {
        return match (policy, zero) {
            (EmptyPolicy::DeclaredEmptyWindow, _) | (_, ZeroPolicy::NotObserved) => Ok(PageSet {
                pages: pages.to_vec(),
                row_count,
                page_count,
            }),
            (EmptyPolicy::Forbidden, ZeroPolicy::FailClosed) => {
                Err(ValidateError::UnverifiedZero {
                    pages: pages.len(),
                    rows: row_count,
                })
            }
        };
    }
    Ok(PageSet {
        pages: pages.to_vec(),
        row_count,
        page_count,
    })
}
