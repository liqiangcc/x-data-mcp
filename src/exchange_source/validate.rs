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

use std::collections::BTreeMap;

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
/// interpreting source fields. `declared_tabkey` is the `TABKEY` the request
/// sent (SZSE block selection, #446 D1); it is part of the request fingerprint,
/// so replay can read it back from the store.
pub fn parse_page(
    format: PagingFormat,
    declared_tabkey: Option<&str>,
    body: &[u8],
) -> Result<PageEnvelope, ValidateError> {
    let value: Value = serde_json::from_slice(body).map_err(|error| ValidateError::Format {
        detail: format!("not JSON: {error}"),
    })?;
    match format {
        PagingFormat::SsePageHelp => parse_sse(&value),
        PagingFormat::SseSoaPageNo => parse_sse_soa(&value),
        PagingFormat::SzseMetadata => parse_szse(&value, declared_tabkey),
        PagingFormat::SzseMetadataV2 => parse_szse_v2(&value),
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

/// Where one block of a SZSE response stands, for evidence (#446 D1).
///
/// The evidence is what lets an operator tell "the list moved to another tab"
/// from "the list is empty": the selected `tabkey` alone is not enough when
/// four blocks answer with different `pagecount`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SzseBlockInfo {
    pub tabkey: Option<String>,
    pub rows: usize,
    pub page_count: Option<u64>,
}

impl SzseBlockInfo {
    fn has_rows(&self) -> bool {
        self.rows > 0
    }
}

/// Why [`szse_block`] selected the block it selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SzseBlockBasis {
    /// The request declared a `TABKEY` and a block carries it.
    DeclaredTabkey,
    /// The request declared none and exactly one block carries rows.
    UniqueNonEmpty,
    /// The response has one block only (the old single-block shape).
    OnlyBlock,
}

impl SzseBlockBasis {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::DeclaredTabkey => "declared_tabkey",
            Self::UniqueNonEmpty => "unique_non_empty",
            Self::OnlyBlock => "only_block",
        }
    }
}

/// The selected block plus the evidence of why it was selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SzseBlockSelection {
    pub basis: SzseBlockBasis,
    pub tabkey: Option<String>,
    pub blocks: Vec<SzseBlockInfo>,
}

/// The `TABKEY` a request declared, if any.
///
/// The SZSE requests spell the parameter in upper case (`TABKEY=tab2`); the
/// name is matched case-insensitively so a plan cannot silently lose its
/// declaration, and an empty value counts as "not declared".
pub fn declared_tabkey(params: &BTreeMap<String, String>) -> Option<&str> {
    params
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("TABKEY"))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

/// Pick the SZSE block that carries this list (#446 D1).
///
/// Two real shapes exist (2026-10-30 and 2026-10-31 forensics):
///
/// - `CATALOGID=1793_ssgs` always answers two blocks: `tab1` (暂停上市, zero
///   rows, `pagecount=0`) followed by `tab2` (终止上市, the data). The request
///   declares `TABKEY=tab2`, so the declaration decides.
/// - `CATALOGID=1110` answers four blocks and only `tab1` carries the current
///   list (`tab2`/`tab3`/`tab4` are empty, `pagecount=0`). The request declares
///   no tabkey, so "the only non-empty block" decides.
///
/// The rule is therefore:
///
/// 1. a block whose `metadata.tabkey` equals the declared tabkey (case
///    insensitively); a declared tabkey that no block carries fails closed;
/// 2. otherwise the only block whose `data` array is non-empty;
/// 3. otherwise the only block (the old single-block shape);
/// 4. otherwise fail closed instead of preferring `tab2` or `[0]`: several
///    blocks with rows are ambiguous, and a request declares which one it
///    wants.
///
/// `a1a2::rows` calls this same function so the row array and the pagination
/// metadata can never come from different blocks.
pub(crate) fn szse_block<'a>(
    value: &'a Value,
    declared: Option<&str>,
) -> Result<(&'a Value, SzseBlockSelection), ValidateError> {
    let blocks = value.as_array().ok_or_else(|| ValidateError::Format {
        detail: "top level is not an array of blocks".to_string(),
    })?;
    if blocks.is_empty() {
        return Err(ValidateError::Format {
            detail: "empty block array".to_string(),
        });
    }
    let infos: Vec<SzseBlockInfo> = blocks.iter().map(block_info).collect();
    let selected = |basis: SzseBlockBasis, index: usize| SzseBlockSelection {
        basis,
        tabkey: infos[index].tabkey.clone(),
        blocks: infos.clone(),
    };
    if let Some(declared) = declared.map(str::trim).filter(|key| !key.is_empty()) {
        let matching: Vec<usize> = infos
            .iter()
            .enumerate()
            .filter(|(_, info)| {
                info.tabkey
                    .as_deref()
                    .is_some_and(|key| key.eq_ignore_ascii_case(declared))
            })
            .map(|(index, _)| index)
            .collect();
        return match matching.as_slice() {
            [index] => Ok((
                &blocks[*index],
                selected(SzseBlockBasis::DeclaredTabkey, *index),
            )),
            // No fallback to "the only block": a request that declares a
            // tabkey and gets a response that does not carry it is a source
            // change, even when there is only one block.
            [] => Err(ValidateError::Format {
                detail: format!(
                    "the request declared tabkey {declared:?} but no block carries it (blocks: {})",
                    describe(&infos)
                ),
            }),
            _ => Err(ValidateError::Format {
                detail: format!(
                    "the request declared tabkey {declared:?} but {} blocks carry it; \
                     refusing to pick one (blocks: {})",
                    matching.len(),
                    describe(&infos)
                ),
            }),
        };
    }
    let mut non_empty = infos.iter().enumerate().filter(|(_, info)| info.has_rows());
    let first = non_empty.next();
    let second = non_empty.next();
    match (first, second) {
        (Some((index, _)), None) => Ok((
            &blocks[index],
            selected(SzseBlockBasis::UniqueNonEmpty, index),
        )),
        (None, _) if blocks.len() == 1 => Ok((&blocks[0], selected(SzseBlockBasis::OnlyBlock, 0))),
        (None, _) => Err(ValidateError::Format {
            detail: format!(
                "no block carries rows and the response has {} blocks; \
                 refusing to pick one (blocks: {})",
                blocks.len(),
                describe(&infos)
            ),
        }),
        (Some(_), Some(_)) => Err(ValidateError::Format {
            detail: format!(
                "several blocks carry rows and the request declared no tabkey; \
                 refusing to pick one (blocks: {})",
                describe(&infos)
            ),
        }),
    }
}

/// Replay of `exchange-szse-page-v2`: `tab2` first, then the only non-empty
/// block. The same bytes must keep the counts they were indexed with.
fn szse_block_v2(value: &Value) -> Result<&Value, ValidateError> {
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

fn block_info(block: &Value) -> SzseBlockInfo {
    SzseBlockInfo {
        tabkey: block_tabkey(block).map(str::to_string),
        rows: block
            .get("data")
            .and_then(Value::as_array)
            .map_or(0, |data| data.len()),
        page_count: block
            .get("metadata")
            .and_then(|metadata| metadata.get("pagecount"))
            .and_then(number),
    }
}

/// Human-readable block list for a fail-closed message: tabkey plus the
/// counts that tell an empty list from a mis-selected one.
fn describe(infos: &[SzseBlockInfo]) -> String {
    infos
        .iter()
        .map(|info| {
            let tabkey = info.tabkey.as_deref().unwrap_or("<none>");
            let page_count = info
                .page_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "none".to_string());
            format!("{tabkey}: {} row(s), pagecount {page_count}", info.rows)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn block_tabkey(block: &Value) -> Option<&str> {
    block
        .get("metadata")
        .and_then(|metadata| metadata.get("tabkey"))
        .and_then(Value::as_str)
        .map(str::trim)
}

fn parse_szse(value: &Value, declared: Option<&str>) -> Result<PageEnvelope, ValidateError> {
    parse_szse_block(szse_block(value, declared)?.0)
}

/// v2 replay: `tab2` first, then the only non-empty block.
fn parse_szse_v2(value: &Value) -> Result<PageEnvelope, ValidateError> {
    parse_szse_block(szse_block_v2(value)?)
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
