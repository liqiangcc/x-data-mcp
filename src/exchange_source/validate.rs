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
    /// SZSE `metadata.recordcount`), when the envelope exposes one.
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
        PagingFormat::SzseMetadata => parse_szse(&value),
    }
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
    let page = help.get("beginPage").and_then(number).unwrap_or(1);
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

fn parse_szse(value: &Value) -> Result<PageEnvelope, ValidateError> {
    let blocks = value.as_array().ok_or_else(|| ValidateError::Format {
        detail: "top level is not an array of blocks".to_string(),
    })?;
    let block = blocks.first().ok_or_else(|| ValidateError::Format {
        detail: "empty block array".to_string(),
    })?;
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
        page: clamp_u32(metadata.get("pageNo").and_then(number).unwrap_or(1)),
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
) -> Result<PageSet, ValidateError> {
    let first = pages.first().ok_or_else(|| ValidateError::Format {
        detail: "window produced no page".to_string(),
    })?;
    let page_count = first.page_count;
    if page_count == 0 {
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
    if let Some(total) = total {
        if total != row_count as u64 {
            return Err(ValidateError::Truncated {
                detail: format!("declared total {total} but collected {row_count} row(s)"),
            });
        }
    }
    if row_count == 0 {
        return match policy {
            EmptyPolicy::Forbidden => Err(ValidateError::UnverifiedZero {
                pages: pages.len(),
                rows: row_count,
            }),
            EmptyPolicy::DeclaredEmptyWindow => Ok(PageSet {
                pages: pages.to_vec(),
                row_count,
                page_count,
            }),
        };
    }
    Ok(PageSet {
        pages: pages.to_vec(),
        row_count,
        page_count,
    })
}
