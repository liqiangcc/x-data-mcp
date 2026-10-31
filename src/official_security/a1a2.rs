//! A-1/A-2 exchange listing and delisting adapters (M3 PR-3, issue-446
//! §7.1/§7.2; decisions D-45/D-46).
//!
//! Layer position (separation point 1): this is a **source adapter**. It turns
//! the frozen bytes of four exchange lists into the domain value the assembly
//! already knows how to merge —
//! [`ExchangeSnapshot`](crate::security_master::assembly::ExchangeSnapshot) —
//! and nothing else. It does not open a database writer, does not assemble
//! intervals and never invents a fact the source did not carry.
//!
//! ```text
//! plans()  ──► exchange_source::Entry (PR-2) ──► frozen bytes (source store)
//!                                                      │
//!                                     parse (this module, PARSER_VERSION)
//!                                                      ▼
//!                    ExchangeSnapshot { as_of, listing, delisting, migrations }
//!                                                      │
//!                        assembly::exchange_source(Some(..)) (M2 seam)
//! ```
//!
//! ## What is approved, and what stays `unknown`
//!
//! D-45/D-46 admit the A-share listing day, the SSE terminated-list listing
//! day, the SZSE delisted-code listing day and the two delisting days. They do
//! **not** admit a delisting reason class, an issuer key, a B-share listing
//! day, or any historical version of the lists, so this adapter leaves those
//! fields empty (`DelistingReason::Unknown`, `issuer_key: None`,
//! `b_listing_date: None`) instead of filling them from a neighbouring row.
//!
//! ## Filtering rules (§7.1, tested)
//!
//! - SSE: `STOCK_TYPE=1` (main board A) and `=8` (STAR) are A shares; `=2`
//!   (B shares) is skipped. An unknown `STOCK_TYPE` fails closed. The request
//!   already fixes the stock type, so a missing row-level field is not a fact
//!   change — but a *present* one that disagrees is either a B row to skip or
//!   an unknown vocabulary to refuse.
//! - SZSE: A-share prefixes `000/001/002/003/300/301`; `200` is a B share and
//!   is skipped; any other prefix fails closed (the list has no security-type
//!   column, #398).
//! - A+B companies return two rows with different listing days. Filtering by
//!   stock type/prefix is what keeps the A row from becoming a false listing
//!   conflict (§7.1).
//!
//! ## The two fixed facts (§7.1, D-46)
//!
//! `sz.300114 → sz.302132` (2025-02-17) is a **confirmed** migration: the old
//! code's interval ends there and the new code starts there. The exchange list
//! reports `302132` with the *inherited subject-level* A-share listing day
//! (2010-08-27), which must not open the new code's interval (#396 finding 2),
//! so that one listing fact is suppressed and counted in the evidence.
//! `sh.600849 → sh.601607` has only an announcement index and stays a
//! **candidate**: it is recorded, never used to collapse two histories.
//!
//! ## Failure semantics (§6.3)
//!
//! Acquisition failures come straight from PR-2's entry
//! (`source_unavailable` / `source_invalid` / `source_not_admitted` /
//! `partially_failed` / `page_size_ignored`); a source page that cannot be
//! parsed is `source_invalid`. `page_size_ignored` is the single-page list
//! dialect failing closed: the SSE list must arrive in one page (2026-10-30
//! evidence), so a `pageCount>1` answer means the request's `pageHelp.pageSize`
//! was ignored and the first page cannot stand for the whole list.
//! The caller (the `derived:security_master` item) turns any of them into a
//! failed item, so the previously committed main-table version stays readable
//! and the TDX daily run is not blocked.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::NaiveDate;
use serde_json::{Value, json};

use crate::exchange_source::validate;
use crate::exchange_source::{
    AcquisitionPlan, Client, Entry, PagingFormat, RunCache, SnapshotBinding, SourceId,
    TransportError,
};
use crate::security_master::assembly::{
    CodeMigrationFact, DATASET_KIND, ExchangeCodeMigration, ExchangeDelistingFact,
    ExchangeListingFact, ExchangeSnapshot, MigrationCertainty, PendingSource,
};
use crate::security_master::{DelistingReason, Exchange};
use crate::source_store::{Policy, SourceStore, StoreError};
use crate::sqlite_market::backend::MarketBackend;

/// Business-field parser version of this adapter. The frozen snapshots record
/// PR-2's *envelope* parser version, because replay must re-derive the row/page
/// counts from the store alone; this consumer version is registered on the
/// derived item's outcome evidence (`Admitted::evidence`), which is what a
/// re-derivation must pin. It stays out of the generation binding evidence on
/// purpose: that digest must be identical for every whole-market item of one
/// generation (`derived:suspensions` fails closed otherwise).
pub const PARSER_VERSION: &str = "exchange-a1a2-v4";

/// Environment variable naming the deployment's source store (§9.4.1). The
/// deploy script uses `<deploy-state>/source-store`; resolution (and the
/// strict/optional difference) lives in [`crate::source_store::configured`].
pub const SOURCE_STORE_ENV: &str = crate::source_store::CONFIG_ENV;

/// SSE `commonQuery.do` endpoint (both lists share it, `sqlId` differs).
pub const SSE_URL: &str = "https://query.sse.com.cn/sseQuery/commonQuery.do";
/// SSE listing/terminated-list sqlId (M2 design §10 S-1/S-2).
pub const SSE_LIST_SQL_ID: &str = "COMMON_SSE_CP_GPJCTPZ_GPLB_GP_L";
/// SZSE `ShowReport` data endpoint. `SHOWTYPE=json` and `/ShowReport` answer
/// HTTP 500; only the `/data` path serves JSON (#398 finding 1).
pub const SZSE_URL: &str = "https://www.szse.cn/api/report/ShowReport/data";
/// SZSE current A-share list (only currently listed codes).
pub const SZSE_LISTING_CATALOG: &str = "1110";
/// SZSE suspended/terminated list.
pub const SZSE_DELISTING_CATALOG: &str = "1793_ssgs";

/// SSE `STOCK_TYPE` value of the main-board A shares.
const SSE_MAIN_BOARD_A: &str = "1";
/// SSE `STOCK_TYPE` value of STAR-market A shares.
const SSE_STAR_A: &str = "8";
/// SSE `STOCK_TYPE` value of B shares — never an A-share fact.
const SSE_B_SHARE: &str = "2";

/// SZSE A-share code prefixes (main board + ChiNext; `302` is the
/// renumbered ChiNext block the 2025 code migration targets).
const SZSE_A_PREFIXES: [&str; 7] = ["000", "001", "002", "003", "300", "301", "302"];
/// SZSE B-share code prefix.
const SZSE_B_PREFIX: &str = "200";
/// `TABKEY` both SZSE terminated lists declare. The envelope and the row
/// parser both take the selected block from this request parameter, so the
/// declaration and the selection cannot drift apart (#446 D1).
const SZSE_DELISTING_TABKEY: &str = "tab2";

/// The seven acquisitions of this adapter: one per request the two exchanges
/// need. The list is static so a run performs the same requests in the same
/// order and the run cache has a fixed key space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ListKind {
    SseListingMain,
    SseListingStar,
    SseDelistingMain,
    SseDelistingStar,
    SzseListing,
    SzseDelistingMain,
    SzseDelistingShortName,
}

impl ListKind {
    /// Evidence label of this acquisition.
    pub fn label(self) -> &'static str {
        match self {
            Self::SseListingMain => "sse_listing_main",
            Self::SseListingStar => "sse_listing_star",
            Self::SseDelistingMain => "sse_delisting_main",
            Self::SseDelistingStar => "sse_delisting_star",
            Self::SzseListing => "szse_listing",
            Self::SzseDelistingMain => "szse_delisting_main",
            Self::SzseDelistingShortName => "szse_delisting_nm",
        }
    }

    fn listing(self) -> bool {
        matches!(
            self,
            Self::SseListingMain | Self::SseListingStar | Self::SzseListing
        )
    }

    fn sse(self) -> bool {
        matches!(
            self,
            Self::SseListingMain
                | Self::SseListingStar
                | Self::SseDelistingMain
                | Self::SseDelistingStar
        )
    }
}

/// One request plan plus the parser that will read its frozen bytes.
pub fn plans() -> Vec<(ListKind, AcquisitionPlan)> {
    let sse_plan = |kind: ListKind, stock_type: &str, delisting: bool| {
        let mut plan =
            AcquisitionPlan::new(SourceId::SseListing, SSE_URL, PagingFormat::SsePageHelp)
                .param("sqlId", SSE_LIST_SQL_ID)
                .param("STOCK_TYPE", stock_type)
                .param("isPagination", "true")
                // The list is one page when the endpoint honours
                // `pageHelp.pageSize` (`SSE_PAGE_SIZE`); a larger `pageCount`
                // fails closed instead of paging (2026-10-30 evidence).
                .single_page_only();
        if delisting {
            // Only `COMPANY_STATUS=3` means 终止上市; `=7` is a risk warning and
            // must never be read as a delisting (§7.2). `STATE_CODE` is
            // silently ignored by the endpoint (#396 finding 5).
            plan = plan.param("COMPANY_STATUS", "3");
            plan.source_id = SourceId::SseDelisting;
        }
        (kind, plan)
    };
    let szse_plan = |kind: ListKind, catalog: &str, extra: &[(&str, &str)]| {
        let mut plan =
            AcquisitionPlan::new(SourceId::SzseListing, SZSE_URL, PagingFormat::SzseMetadata)
                .param("SHOWTYPE", "JSON")
                .param("CATALOGID", catalog);
        for (name, value) in extra {
            plan = plan.param(name, *value);
        }
        if catalog == SZSE_DELISTING_CATALOG {
            plan.source_id = SourceId::SzseDelisting;
        }
        (kind, plan)
    };
    vec![
        sse_plan(ListKind::SseListingMain, SSE_MAIN_BOARD_A, false),
        sse_plan(ListKind::SseListingStar, SSE_STAR_A, false),
        sse_plan(ListKind::SseDelistingMain, SSE_MAIN_BOARD_A, true),
        sse_plan(ListKind::SseDelistingStar, SSE_STAR_A, true),
        szse_plan(ListKind::SzseListing, SZSE_LISTING_CATALOG, &[]),
        szse_plan(
            ListKind::SzseDelistingMain,
            SZSE_DELISTING_CATALOG,
            &[("TABKEY", SZSE_DELISTING_TABKEY), ("selectModule", "main")],
        ),
        szse_plan(
            ListKind::SzseDelistingShortName,
            SZSE_DELISTING_CATALOG,
            &[("TABKEY", SZSE_DELISTING_TABKEY), ("selectModule", "nm")],
        ),
    ]
}

/// The one confirmed code migration of A-2 (D-46).
///
/// The exchange list itself cannot state it — it rewrites the old code's row in
/// place — so the value is the admission decision, not a parsed field. It is
/// therefore a constant with the decision reference as its record id.
pub fn confirmed_migrations() -> Vec<ExchangeCodeMigration> {
    vec![ExchangeCodeMigration {
        from_exchange: Exchange::Sz,
        from_code: "300114".into(),
        to_exchange: Exchange::Sz,
        to_code: "302132".into(),
        effective_from: date(2025, 2, 17),
        source_record_id: Some("docs/decisions.md#D-46".into()),
        known_at: None,
    }]
}

/// The one candidate code migration (D-46):
/// `sh.600849 → sh.601607` has only an announcement index, so it never
/// collapses two histories and is never used for de-duplication (§7.1).
///
/// `effective_from` is the announcement-index date (2010-03-03); the exact
/// effective day is unverified, which is one more reason the value stays a
/// candidate. A candidate only marks an overlap in the read model.
pub fn candidate_migrations() -> Vec<CodeMigrationFact> {
    vec![CodeMigrationFact {
        from_exchange: Exchange::Sh,
        from_code: "600849".into(),
        to_exchange: Exchange::Sh,
        to_code: "601607".into(),
        effective_from: date(2010, 3, 3),
        certainty: MigrationCertainty::Candidate,
        source_kind: "exchange_announcement".into(),
        source_record_id: Some("docs/decisions.md#D-46 (announcement index only)".into()),
        known_at: None,
    }]
}

/// Why the adapter could not produce a snapshot. The codes are PR-2's failure
/// vocabulary plus `source_invalid` for a byte stream that is not the recorded
/// list shape.
#[derive(Debug, thiserror::Error)]
pub enum A1A2Error {
    #[error(transparent)]
    Acquire(#[from] crate::exchange_source::AcquireError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("{code}: {detail}")]
    Invalid { code: &'static str, detail: String },
}

impl A1A2Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Acquire(error) => error.code(),
            Self::Store(error) => error.code(),
            Self::Transport(error) => error.code(),
            Self::Invalid { code, .. } => code,
        }
    }

    fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid {
            code: "source_invalid",
            detail: detail.into(),
        }
    }
}

/// One SZSE block as counted across the pages of one plan (#446 D1 evidence).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SzseBlockRows {
    /// Rows over every page of the plan.
    pub rows: usize,
    /// Pages that carried this block.
    pub pages: usize,
    /// Pages where the block had at least one row.
    pub pages_with_rows: usize,
    /// `metadata.pagecount` the block last reported (0 for an empty block).
    pub page_count: u64,
}

/// Which block each page of one SZSE plan was read from, and why.
///
/// The selection basis and the per-block counts are evidence in their own
/// right: `page_count is 0` alone cannot tell an empty list from a mis-selected
/// block (#446 D1), so an operator reads the selected `tabkey` and every
/// block's row/page counts from the item's evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockSelectionEvidence {
    pub pages: usize,
    /// `basis → pages`, e.g. `declared_tabkey` or `unique_non_empty`.
    pub basis: BTreeMap<&'static str, usize>,
    /// `selected tabkey → pages` (`<none>` for a block without `metadata.tabkey`).
    pub selected: BTreeMap<String, usize>,
    /// Per block (keyed by `tabkey`) counts over the pages of the plan.
    pub blocks: BTreeMap<String, SzseBlockRows>,
}

impl BlockSelectionEvidence {
    fn add(&mut self, selection: &validate::SzseBlockSelection) {
        self.pages += 1;
        *self.basis.entry(selection.basis.as_str()).or_insert(0) += 1;
        *self
            .selected
            .entry(block_key(selection.tabkey.as_deref()))
            .or_insert(0) += 1;
        for block in &selection.blocks {
            let tally = self
                .blocks
                .entry(block_key(block.tabkey.as_deref()))
                .or_default();
            tally.rows += block.rows;
            tally.pages += 1;
            if block.rows > 0 {
                tally.pages_with_rows += 1;
            }
            if let Some(page_count) = block.page_count {
                tally.page_count = page_count;
            }
        }
    }

    fn to_json(&self) -> Value {
        let blocks: Vec<Value> = self
            .blocks
            .iter()
            .map(|(tabkey, tally)| {
                json!({
                    "tabkey": tabkey,
                    "rows": tally.rows,
                    "pages": tally.pages,
                    "pages_with_rows": tally.pages_with_rows,
                    "page_count": tally.page_count,
                })
            })
            .collect();
        json!({
            "pages": self.pages,
            "basis": self.basis,
            "selected_tabkey": self.selected,
            "blocks": blocks,
        })
    }
}

fn block_key(tabkey: Option<&str>) -> String {
    tabkey
        .map(str::to_string)
        .unwrap_or_else(|| "<none>".to_string())
}

/// An admitted snapshot plus everything the derived item must register:
/// the binding (snapshot ids + parser versions + basis status), the acquisition
/// labels and the rows the A-share filter refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    pub snapshot: ExchangeSnapshot,
    /// One binding per `source_id`, sorted.
    pub bindings: Vec<SnapshotBinding>,
    /// `label → snapshot ids`, in acquisition order.
    pub snapshot_ids: BTreeMap<&'static str, Vec<String>>,
    /// B-share rows the filter dropped (they are not facts).
    pub b_shares_skipped: usize,
    /// Terminated-list rows whose delisting day was empty; they are not
    /// delisting facts, so the count is evidence only.
    pub delisting_rows_without_date: usize,
    /// Listing facts suppressed because the code is a confirmed-migration
    /// target whose exchange date is the inherited subject-level day.
    pub suppressed_listing_targets: Vec<String>,
    /// Per SZSE plan: which block the pages were read from (#446 D1).
    pub block_selection: BTreeMap<&'static str, BlockSelectionEvidence>,
}

impl Admitted {
    /// Everything a re-derivation needs to pin this acquisition (§6.3 "bind",
    /// §9.4.2 replay).
    pub fn evidence(&self) -> Value {
        let block_selection: BTreeMap<String, Value> = self
            .block_selection
            .iter()
            .map(|(label, evidence)| ((*label).to_string(), evidence.to_json()))
            .collect();
        json!({
            "adapter": "A-1/A-2",
            "parser_version": PARSER_VERSION,
            "as_of": self.snapshot.as_of.format("%Y-%m-%d").to_string(),
            "source_bindings": self.bindings,
            "snapshot_ids": self.snapshot_ids,
            "b_shares_skipped": self.b_shares_skipped,
            "delisting_rows_without_date": self.delisting_rows_without_date,
            "suppressed_listing_targets": self.suppressed_listing_targets,
            "block_selection": block_selection,
            "listing_facts": self.snapshot.listing.len(),
            "delisting_facts": self.snapshot.delisting.len(),
            "confirmed_migrations": self.snapshot.migrations.len(),
        })
    }
}

/// The daily item's answer: an admitted snapshot, or the reason the branch is
/// still `pending_source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunSource {
    Admitted(Admitted),
    Pending(PendingSource),
}

/// Resolve the deployment's source store and fetch client, then acquire.
///
/// Under [`Policy::Optional`] (stdio, tests, non-ops and legacy entries) not
/// configured is *not* a fetch failure: the security-master item keeps the
/// TDX-only result and records `pending_source`, so an environment that has not
/// enabled A-1/A-2 does not fail its daily run. Under [`Policy::Required`] (the
/// ops entry of `data_service` on a compact backend) a missing or unreadable
/// store fails the item with the stable code `source_store_unconfigured`
/// instead of writing a zero-exchange master. A configured store whose fetch
/// fails is a real failure and reaches the item as such either way.
pub fn run_source(market: &MarketBackend, policy: Policy) -> Result<RunSource, A1A2Error> {
    let Some(store) =
        crate::source_store::configured(policy).map_err(|error| A1A2Error::Invalid {
            code: error.code(),
            detail: error.detail().to_string(),
        })?
    else {
        return Ok(RunSource::Pending(PendingSource {
            dataset: DATASET_KIND,
            source: "exchange_list",
            reason: "the M3 source store is not configured for this process \
                     (X_DATA_MCP_SOURCE_STORE); A-1/A-2 stay pending_source",
        }));
    };
    let client = Client::helper()?;
    Ok(RunSource::Admitted(acquire(&store, market, &client)?))
}

/// Acquire, parse and merge every plan (§7.1). One shared run cache serves all
/// seven requests (§10.1).
pub fn acquire(
    store: &SourceStore,
    market: &MarketBackend,
    client: &Client,
) -> Result<Admitted, A1A2Error> {
    let entry = Entry::new(store, market, client).with_cache(Arc::new(RunCache::new()));
    let mut listing = Vec::new();
    let mut delisting = Vec::new();
    let mut b_shares_skipped = 0usize;
    let mut delisting_rows_without_date = 0usize;
    let mut snapshot_ids: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut bindings: BTreeMap<String, SnapshotBinding> = BTreeMap::new();
    let mut block_selection: BTreeMap<&'static str, BlockSelectionEvidence> = BTreeMap::new();
    let mut as_of: Option<String> = None;
    for (kind, plan) in plans() {
        let outcome = entry.acquire(&plan)?;
        bindings.insert(outcome.source_id.to_string(), outcome.binding.clone());
        // The block selection follows the request's own declaration (#446 D1);
        // the same value is what replay reads back from the fingerprint.
        let declared = validate::declared_tabkey(&plan.params);
        let mut ids = Vec::new();
        for snapshot in &outcome.snapshots {
            let bytes = store.read_verified(snapshot.snapshot_id())?;
            let page = parse_page(kind, declared, &bytes)?;
            if let Some(selection) = &page.block_selection {
                block_selection
                    .entry(kind.label())
                    .or_default()
                    .add(selection);
            }
            listing.extend(page.listing);
            delisting.extend(page.delisting);
            b_shares_skipped += page.b_shares_skipped;
            delisting_rows_without_date += page.delisting_rows_without_date;
            ids.push(snapshot.snapshot_id().to_string());
            let candidate = snapshot.row.as_of.clone();
            as_of = Some(match as_of.take() {
                Some(current) => current.min(candidate),
                None => candidate,
            });
        }
        snapshot_ids.insert(kind.label(), ids);
    }
    let Some(as_of) = as_of.and_then(|text| parse_day(&text)) else {
        return Err(A1A2Error::invalid(
            "no exchange page was frozen, so the snapshot has no as_of",
        ));
    };

    let migrations = confirmed_migrations();
    // The exchange's A-share listing day for a migration target is the code's
    // inherited subject-level day, not the new code's interval start (#396
    // finding 2, D-46). The migration row defines the start instead.
    let targets: BTreeSet<(Exchange, String)> = migrations
        .iter()
        .map(|migration| (migration.to_exchange, migration.to_code.clone()))
        .collect();
    let mut suppressed_listing_targets = Vec::new();
    listing.retain(|fact| {
        if targets.contains(&(fact.exchange, fact.code.clone())) {
            suppressed_listing_targets.push(entity(fact.exchange, &fact.code));
            false
        } else {
            true
        }
    });
    let listing = merge_listing(listing)?;
    let delisting = merge_delisting(delisting)?;
    Ok(Admitted {
        snapshot: ExchangeSnapshot {
            as_of,
            listing,
            delisting,
            migrations,
        },
        bindings: bindings.into_values().collect(),
        snapshot_ids,
        b_shares_skipped,
        delisting_rows_without_date,
        suppressed_listing_targets,
        block_selection,
    })
}

/// Facts of one page. A page never carries both lists, but keeping them apart
/// makes the merge explicit.
#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedPage {
    listing: Vec<ExchangeListingFact>,
    delisting: Vec<ExchangeDelistingFact>,
    b_shares_skipped: usize,
    /// Terminated-list rows without a delisting day. They are not delisting
    /// facts (a suspended company also appears on some of these lists) but the
    /// count is kept so a field rename cannot look like "nothing delisted".
    delisting_rows_without_date: usize,
    /// SZSE only: which block the envelope and the rows were read from (#446 D1).
    block_selection: Option<validate::SzseBlockSelection>,
}

fn parse_page(
    kind: ListKind,
    declared: Option<&str>,
    bytes: &[u8],
) -> Result<ParsedPage, A1A2Error> {
    let (rows, block_selection) = rows(kind, declared, bytes)?;
    let row_count = rows.len();
    let mut page = ParsedPage {
        block_selection,
        ..ParsedPage::default()
    };
    for row in rows {
        let code = code_of(kind, &row)?;
        if kind.sse() {
            if sse_is_b_share(&row, &code) {
                page.b_shares_skipped += 1;
                continue;
            }
            sse_code_check(kind, &code)?;
            // The list advertises the stock type in the request; a row that
            // disagrees is either an unknown vocabulary change or a mis-filed
            // row, never an A-share fact.
            if let Some(value) = stock_type(&row) {
                let accepted = match kind {
                    ListKind::SseListingStar | ListKind::SseDelistingStar => SSE_STAR_A,
                    _ => SSE_MAIN_BOARD_A,
                };
                if value != accepted {
                    return Err(A1A2Error::invalid(format!(
                        "{}: {} has STOCK_TYPE={value:?}; the list advertises {accepted}",
                        kind.label(),
                        code
                    )));
                }
            }
        } else if code.starts_with(SZSE_B_PREFIX) {
            page.b_shares_skipped += 1;
            continue;
        } else if !SZSE_A_PREFIXES
            .iter()
            .any(|prefix| code.starts_with(prefix))
        {
            return Err(A1A2Error::invalid(format!(
                "{}: {} is not an A-share/B-share code of the SZSE list (#398: the list has no \
                 security-type column, so an unknown prefix is not guessed)",
                kind.label(),
                code
            )));
        }
        let (exchange, listing_field, delisting_field) = match kind {
            ListKind::SseListingMain | ListKind::SseListingStar => {
                (Exchange::Sh, "LIST_DATE", None)
            }
            ListKind::SseDelistingMain | ListKind::SseDelistingStar => {
                (Exchange::Sh, "LIST_DATE", Some("DELIST_DATE"))
            }
            ListKind::SzseListing => (Exchange::Sz, "agssrq", None),
            ListKind::SzseDelistingMain | ListKind::SzseDelistingShortName => {
                (Exchange::Sz, "ssrq", Some("zzrq"))
            }
        };
        let listing_date = optional_day(kind, &code, &row, listing_field)?;
        match (kind.listing(), delisting_field) {
            (true, _) => page.listing.push(ExchangeListingFact {
                exchange,
                code,
                listing_date,
                // The lists do not carry a B-share date for the same company
                // in an A row, and pairing by name is not an admitted key.
                b_listing_date: None,
                // Company/org id is not an approved field of A-1/A-2; the
                // assembly then attributes the row by code+date (Q3).
                issuer_key: None,
                instrument_class: 0,
            }),
            (false, Some(field)) => {
                let Some(delisting_date) = optional_day(kind, &code, &row, field)? else {
                    // A terminated-list row with an empty date is not an
                    // admitted delisting fact (the same list carries suspended
                    // companies). Counting it keeps a renamed field visible
                    // instead of reading as "nothing delisted"; a page where
                    // *no* row has the date fails below.
                    page.delisting_rows_without_date += 1;
                    continue;
                };
                // A terminated row's listing day is approved by D-45/D-46, so
                // it is a second source for codes A-1 cannot cover.
                if let Some(listing_date) = listing_date {
                    page.listing.push(ExchangeListingFact {
                        exchange,
                        code: code.clone(),
                        listing_date: Some(listing_date),
                        b_listing_date: None,
                        issuer_key: None,
                        instrument_class: 0,
                    });
                }
                page.delisting.push(ExchangeDelistingFact {
                    exchange,
                    code,
                    delisting_date,
                    // Reason classes are NOT_AVAILABLE (D-46): the list has no
                    // reason field and the announcement text is out of scope.
                    reason_class: DelistingReason::Unknown,
                });
            }
            (false, None) => unreachable!("a delisting plan always names its date field"),
        }
    }
    // A terminated page is only a shape change when it *has* A-share rows and
    // none of them carries the delisting day. A page of B shares has no fact
    // (the 9th SZSE main page is ten B shares, #446 D3), so the guard counts
    // rows after the same filter that produces the facts.
    let a_share_rows = row_count.saturating_sub(page.b_shares_skipped);
    if !kind.listing() && page.delisting.is_empty() && a_share_rows > 0 {
        return Err(A1A2Error::invalid(format!(
            "{}: page has {row_count} row(s), {a_share_rows} A-share row(s) after filtering \
             and {} B-share row(s) skipped, but none of the A-share rows carries the delisting \
             date; the list shape changed",
            kind.label(),
            page.b_shares_skipped
        )));
    }
    Ok(page)
}

/// The `data` array of one envelope plus, for SZSE, the block selection used to
/// reach it. The envelope itself was already validated by PR-2; this only
/// re-reads the rows with the same selection (declared `TABKEY`, otherwise the
/// unique non-empty block, #446 D1), so the rows and the pagination counts can
/// never come from different blocks.
fn rows(
    kind: ListKind,
    declared: Option<&str>,
    bytes: &[u8],
) -> Result<(Vec<Value>, Option<validate::SzseBlockSelection>), A1A2Error> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| A1A2Error::invalid(format!("{}: not JSON: {error}", kind.label())))?;
    let (data, selection) = if kind.sse() {
        (
            value
                .get("pageHelp")
                .and_then(|help| help.get("data"))
                .and_then(Value::as_array),
            None,
        )
    } else {
        let (block, selection) = validate::szse_block(&value, declared)
            .map_err(|error| A1A2Error::invalid(format!("{}: {error}", kind.label())))?;
        (block.get("data").and_then(Value::as_array), Some(selection))
    };
    let Some(data) = data else {
        return Err(A1A2Error::invalid(format!(
            "{}: the frozen page has no row array",
            kind.label()
        )));
    };
    Ok((data.clone(), selection))
}

fn code_of(kind: ListKind, row: &Value) -> Result<String, A1A2Error> {
    let code = if kind.sse() {
        // The 2026-10-30 real list rows carry `A_STOCK_CODE` (the listed A
        // share) and `COMPANY_CODE`; `SECURITY_CODE` no longer exists. A row
        // where both exist and disagree is a shape change, not a guess.
        let a_stock = row.get("A_STOCK_CODE").and_then(Value::as_str);
        let company = row.get("COMPANY_CODE").and_then(Value::as_str);
        match (a_stock, company) {
            (Some(a_stock), Some(company)) if a_stock.trim() != company.trim() => {
                return Err(A1A2Error::invalid(format!(
                    "{}: A_STOCK_CODE {a_stock:?} and COMPANY_CODE {company:?} disagree",
                    kind.label()
                )));
            }
            (Some(a_stock), _) => a_stock,
            (None, Some(company)) => company,
            (None, None) => {
                return Err(A1A2Error::invalid(format!(
                    "{}: a row has neither A_STOCK_CODE nor COMPANY_CODE",
                    kind.label()
                )));
            }
        }
    } else {
        // Two SZSE list dialects: the terminated lists carry `zqdm`, the
        // current A-share list (`CATALOGID=1110`) carries `agdm` (#446 D2).
        // Neither is a plan-name constant: the row states its code, and a row
        // where both exist and disagree is a shape change, not a guess.
        let zqdm = row
            .get("zqdm")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|code| !code.is_empty());
        let agdm = row
            .get("agdm")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|code| !code.is_empty());
        match (zqdm, agdm) {
            (Some(zqdm), Some(agdm)) if zqdm != agdm => {
                return Err(A1A2Error::invalid(format!(
                    "{}: zqdm {zqdm:?} and agdm {agdm:?} disagree",
                    kind.label()
                )));
            }
            (Some(code), _) | (None, Some(code)) => code,
            (None, None) => {
                return Err(A1A2Error::invalid(format!(
                    "{}: a row has neither zqdm nor agdm",
                    kind.label()
                )));
            }
        }
    };
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(A1A2Error::invalid(format!(
            "{}: {:?} is not a 6-digit code",
            kind.label(),
            code
        )));
    }
    Ok(code.to_string())
}

/// B-share detection on an SSE row: the row-level stock type when present, the
/// `900xxx`/`SHB` code shape otherwise. The two disagreeing cases are left to
/// [`sse_code_check`], which fails closed on an unknown vocabulary.
fn sse_is_b_share(row: &Value, code: &str) -> bool {
    match stock_type(row) {
        Some(value) => value == SSE_B_SHARE,
        None => code.starts_with("900"),
    }
}

/// Validate an SSE A-share row: a known stock type that matches the plan and a
/// code shape consistent with it.
fn sse_code_check(kind: ListKind, code: &str) -> Result<(), A1A2Error> {
    if code.starts_with("900") {
        return Err(A1A2Error::invalid(format!(
            "{}: {} looks like a B share but carries an A-share STOCK_TYPE",
            kind.label(),
            code
        )));
    }
    if !code.starts_with('6') {
        return Err(A1A2Error::invalid(format!(
            "{}: {} is not an SSE A-share code",
            kind.label(),
            code
        )));
    }
    Ok(())
}

fn stock_type(row: &Value) -> Option<String> {
    match row.get("STOCK_TYPE") {
        Some(Value::String(value)) => Some(value.trim().to_string()),
        Some(Value::Number(value)) => Some(value.to_string()),
        _ => None,
    }
}

/// One date column, `YYYYMMDD` (SSE) or `YYYY-MM-DD` (some SZSE exports). The
/// empty string and `-` mean "the source did not carry it"; anything else that
/// is not a real day fails closed.
fn optional_day(
    kind: ListKind,
    code: &str,
    row: &Value,
    field: &str,
) -> Result<Option<NaiveDate>, A1A2Error> {
    let Some(value) = row.get(field) else {
        return Ok(None);
    };
    let text = match value {
        Value::String(text) => text.trim().to_string(),
        Value::Null => return Ok(None),
        other => other.to_string(),
    };
    if text.is_empty() || text == "-" {
        return Ok(None);
    }
    let Some(day) = parse_day(&text) else {
        return Err(A1A2Error::invalid(format!(
            "{}: {} has {}={text:?}, which is not a calendar day",
            kind.label(),
            code,
            field
        )));
    };
    Ok(Some(day))
}

/// `YYYYMMDD` (both exchanges) or `YYYY-MM-DD` (some SZSE exports).
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

/// Merge the listing facts of every page.
///
/// A code can arrive twice (SSE's unfiltered list and its terminated list;
/// SZSE's two delisting modules). Equal dates collapse; two different listing
/// days for one exchange code mean the two lists disagree about the same fact,
/// which is a source defect — never a value to pick from.
fn merge_listing(facts: Vec<ExchangeListingFact>) -> Result<Vec<ExchangeListingFact>, A1A2Error> {
    let mut merged: BTreeMap<(Exchange, String), ExchangeListingFact> = BTreeMap::new();
    for fact in facts {
        match merged.get_mut(&(fact.exchange, fact.code.clone())) {
            None => {
                merged.insert((fact.exchange, fact.code.clone()), fact);
            }
            Some(existing) => match (existing.listing_date, fact.listing_date) {
                (Some(left), Some(right)) if left != right => {
                    return Err(A1A2Error::invalid(format!(
                        "listing_conflict: {} is {left} in one list and {right} in another \
                         (A-1/A-2 disagree; not averaged or picked)",
                        entity(fact.exchange, &fact.code)
                    )));
                }
                (left, right) => {
                    existing.listing_date = left.or(right);
                }
            },
        }
    }
    Ok(merged.into_values().collect())
}

fn merge_delisting(
    facts: Vec<ExchangeDelistingFact>,
) -> Result<Vec<ExchangeDelistingFact>, A1A2Error> {
    let mut merged: BTreeMap<(Exchange, String), ExchangeDelistingFact> = BTreeMap::new();
    for fact in facts {
        match merged.get(&(fact.exchange, fact.code.clone())) {
            None => {
                merged.insert((fact.exchange, fact.code.clone()), fact);
            }
            Some(existing) if existing.delisting_date != fact.delisting_date => {
                return Err(A1A2Error::invalid(format!(
                    "delisting_conflict: {} is {} in one list and {} in another",
                    entity(fact.exchange, &fact.code),
                    existing.delisting_date,
                    fact.delisting_date
                )));
            }
            Some(_) => {}
        }
    }
    Ok(merged.into_values().collect())
}

fn entity(exchange: Exchange, code: &str) -> String {
    format!("{}.{code}", exchange.as_str())
}

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("fixture date is valid")
}

#[cfg(test)]
mod tests;
