//! A-3 suspension/resumption adapter (M3 PR-11, issue-446 §6.2 #4, D-49/D-60).
//!
//! Layer position (separation point 1): this is a **source adapter**. It turns
//! the frozen bytes of the SSE structured suspension/resumption event table
//! `GW_PL_JYTS_TFPXX` into the domain value the suspension seam already knows —
//! [`ConfirmedSuspensionFact`](crate::security_master::suspensions::ConfirmedSuspensionFact) —
//! and nothing else. It does not open a database writer, does not derive gaps
//! and never invents an event the table did not carry.
//!
//! ```text
//! windows()  ──► exchange_source::Entry (PR-2) ──► frozen bytes (source store)
//!                                                        │
//!                                       parse (this module, PARSER_VERSION)
//!                                                        ▼
//!             ConfirmedSuspensionSnapshot { as_of, facts }  + evidence
//!                                                        │
//!                    suspensions::suspension_source(Some(..)) (M2 seam)
//! ```
//!
//! ## What is approved, and what stays `unresolved_gap`
//!
//! D-49/D-60 admit the **SSE** structured table since 2008 (`type=LXTP/LSTP`,
//! `endStopDate` = the day before resumption) as `APPROVED_SOURCE`, and they
//! explicitly leave the SZSE side without `confirmed`: this module therefore
//! only ever produces `exchange = sh` facts. An SZ gap stays an
//! `unresolved_gap` and SZ tradability stays `unknown`; there is no "nothing
//! found therefore not suspended" conclusion here (BR-M3-10).
//!
//! Not admitted and therefore never used as a full-day suspension:
//!
//! - intraday halts (`stopTime` = `AM`/`PM`/`930`, D-49 "盘中临时停牌
//!   `NOT_AVAILABLE`") — counted as skipped, never written;
//! - an announcement publication instant: the table has no announcement day,
//!   so `known_at` stays `None` (D-49: never proxy `known_at` with the event
//!   day, BR-M2-2).
//!
//! ## Which rows become a fact (§6.2 #4, D-60)
//!
//! | row | meaning | written |
//! |---|---|---|
//! | `LXTP`, `stopTime = ""` | suspension over a day range | `[startStopDate, endStopDate]` |
//! | `LXTP`/`LSTP`, `stopTime = "WH"` | whole-day halt | `[startStopDate, endStopDate]` |
//! | `LSTP`, `stopTime = ""` | not observed in the table | skipped (counted) |
//! | any type, `stopTime ∈ {AM, PM, 930}` | intraday halt | skipped (counted) |
//! | empty `endStopDate` | not resumed yet | `end_day` = the window end it was observed in |
//!
//! `endStopDate` is the last suspended calendar day (the day before
//! resumption), so it is used as the inclusive `end_day` without arithmetic.
//!
//! ## Identity and filtering
//!
//! Only 6-digit codes starting with `6` are SSE A shares; other products
//! (bonds, preferred stock, funds) are skipped and counted. A row whose entity
//! is not in the main table cannot be placed in a member window, so it is
//! skipped with a count and a sample of codes instead of failing the item:
//! the domain seam (`assemble_suspensions`) is fail-closed on unknown
//! entities, and a convertible bond must not fail a whole daily run.
//! Surviving intervals are clipped to the member window (§11.1); one whose
//! intersection is empty is dropped (counted).
//!
//! ## Failure semantics (§6.3)
//!
//! Acquisition failures come straight from PR-2's entry
//! (`source_unavailable` / `source_invalid` / `source_not_admitted` /
//! `partially_failed`); a row that cannot be read is `source_invalid`. The
//! caller (the `derived:suspensions` item) turns any of them into a failed
//! item, so no `confirmed` row is written and the previous dataset stays
//! readable (§10.1).
//!
//! A 0-row window is **not** "no suspension": the table genuinely answers
//! nothing for 2008 Q1 while 2008 Q2 answers 2,306 rows (D-60 probe, PR-11
//! re-check). Such a window is `not_observed` (BR-M3-10): it produces no fact,
//! it is named in the evidence, and it never becomes an absence conclusion.
//! Format, count and truncation failures still fail the whole run.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::NaiveDate;
use serde_json::{Value, json};

use crate::exchange_source::{
    AcquireError, AcquisitionPlan, Client, Entry, PagingFormat, RunCache, Segment, SnapshotBinding,
    SnapshotRow, SourceId,
};
use crate::security_master::assembly::PendingSource;
use crate::security_master::suspensions::{
    ConfirmedSuspensionFact, ConfirmedSuspensionSnapshot, DATASET_KIND,
};
use crate::security_master::{Exchange, SecurityLifecycleFact, SuspensionReason};
use crate::source_store::{SourceStore, StoreError};

/// Business-field parser version of this adapter. Like A-1/A-2 it is registered
/// on the derived item's outcome evidence, not on the manifest (replay only
/// rebuilds the envelope counts), and it stays out of the generation binding
/// evidence that every whole-market item shares.
pub const PARSER_VERSION: &str = "exchange-a3-v1";

/// Environment variable naming the deployment's source store (§9.4.1).
pub const SOURCE_STORE_ENV: &str = "X_DATA_MCP_SOURCE_STORE";

/// SSE `commonSoaQuery.do` endpoint (the structured tables, not the `sseQuery`
/// list service A-1/A-2 use).
pub const SSE_SOA_URL: &str = "https://query.sse.com.cn/commonSoaQuery.do";
/// `sqlId` of the suspension/resumption event table (§6.2 #4).
pub const SQL_ID: &str = "GW_PL_JYTS_TFPXX";
/// D-60: the structured table is admitted from 2008 onwards.
pub const ARCHIVE_START: &str = "2008-01-01";
/// Inclusive window bound parameter names of the endpoint.
pub const FROM_PARAM: &str = "startStopDate";
pub const TO_PARAM: &str = "endStopDate";
/// `source_kind` of a confirmed row produced here (the evidence vocabulary is
/// the domain's, never a publisher name).
pub const SOURCE_KIND: &str = "exchange_suspension_event";

/// The nine row fields the endpoint answers with. All are strings; a renamed
/// or added field fails closed instead of being guessed.
const ROW_FIELDS: [&str; 9] = [
    "startStopDate",
    "stopReason",
    "productCode",
    "endStopReason",
    "controlType",
    "endStopDate",
    "stopTime",
    "type",
    "productName",
];

/// Event type of a continuous suspension.
const TYPE_CONTINUOUS: &str = "LXTP";
/// Event type of a temporary suspension.
const TYPE_TEMPORARY: &str = "LSTP";
/// `stopTime` of a whole-day halt.
const STOP_TIME_WHOLE_DAY: &str = "WH";
/// `stopTime` values of intraday halts (D-49 `NOT_AVAILABLE`).
const STOP_TIME_INTRADAY: [&str; 3] = ["AM", "PM", "930"];

/// Why the adapter could not produce a snapshot. The codes are PR-2's failure
/// vocabulary plus `source_invalid` for bytes that are not the recorded shape.
#[derive(Debug, thiserror::Error)]
pub enum A3Error {
    #[error(transparent)]
    Acquire(#[from] AcquireError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Transport(#[from] crate::exchange_source::TransportError),
    #[error("{code}: {detail}")]
    Invalid { code: &'static str, detail: String },
}

impl A3Error {
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

/// The one acquisition plan of this adapter. The window bounds and paging are
/// added by the shared entry.
pub fn plan() -> AcquisitionPlan {
    AcquisitionPlan::new(
        SourceId::SseSuspension,
        SSE_SOA_URL,
        PagingFormat::SseSoaPageNo,
    )
    .param("isPagination", "true")
    .param("sqlId", SQL_ID)
    .param("productCode", "")
    .param("keyWords", "")
    // A quiet window is not evidence of absence (BR-M3-10); it is recorded
    // as `not_observed` instead of failing every quarter forever.
    .zero_rows_not_observed()
}

/// The quarter-aligned windows of one derivation: `2008-01-01` through
/// **yesterday**.
///
/// The window is closed before "today" so every request covers a complete
/// period: an event announced today is picked up by the next run instead of
/// being read as an incomplete window (§6.4 "0 行不是证据").
pub fn windows(today: NaiveDate) -> Result<Vec<Segment>, A3Error> {
    let start =
        NaiveDate::parse_from_str(ARCHIVE_START, "%Y-%m-%d").expect("2008-01-01 is a valid date");
    let Some(end) = today.pred_opt() else {
        return Err(A3Error::invalid("today has no previous day"));
    };
    if end < start {
        return Err(A3Error::invalid(format!(
            "no complete suspension window: today {today} is before the {ARCHIVE_START} archive start"
        )));
    }
    crate::exchange_source::quarter_segments(start, end)
        .map_err(|error| A3Error::invalid(format!("{}: {error}", error.code())))
}

/// One suspension event read from the table, after classification.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedEvent {
    code: String,
    start_day: NaiveDate,
    end_day: NaiveDate,
    /// The response carried no `endStopDate`: the interval ran through the
    /// window end (`unclosed`, D-49 "not resumed yet").
    unclosed: bool,
    snapshot_id: String,
}

/// Counters that explain every row the adapter did not turn into a fact, plus
/// the number of complete windows that were acquired.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counters {
    windows: usize,
    rows_read: usize,
    non_a_share_skipped: usize,
    intraday_skipped: usize,
    unclassified_skipped: usize,
    duplicate_rows: usize,
    merged_overlaps: usize,
    unknown_entities: usize,
    unknown_entity_samples: Vec<String>,
    clipped: usize,
    outside_window_dropped: usize,
}

/// What one page carried.
#[derive(Debug, Default)]
struct ParsedPage {
    events: Vec<ParsedEvent>,
    rows_read: usize,
    non_a_share_skipped: usize,
    intraday_skipped: usize,
    unclassified_skipped: usize,
}

/// An admitted snapshot plus everything the derived item must register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    /// The domain value handed to the M2 seam.
    pub snapshot: ConfirmedSuspensionSnapshot,
    /// One binding per `source_id` (a single source here), sorted by the entry.
    pub bindings: Vec<SnapshotBinding>,
    /// Snapshot ids of the pages the facts came from, in window order.
    pub snapshot_ids: Vec<String>,
    /// Windows the source answered with 0 rows (`not_observed`, BR-M3-10).
    pub not_observed_windows: Vec<String>,
    /// Counters behind the facts and the skips.
    pub counters: Counters,
    pub windows: usize,
    /// Intervals the source left open (`endStopDate` empty), resolved to this
    /// run's window end.
    pub unclosed_rows: usize,
}

impl Admitted {
    fn new(
        today: NaiveDate,
        members: &[SecurityLifecycleFact],
        events: Vec<ParsedEvent>,
        mut counters: Counters,
        not_observed_windows: Vec<String>,
        bindings: Vec<SnapshotBinding>,
        snapshot_ids: Vec<String>,
    ) -> Self {
        let (events, unclosed_rows) = merge_overlaps(events, &mut counters);
        let Confirmed {
            facts,
            unknown_entities,
            unknown_entity_samples,
            clipped,
            outside_window_dropped,
        } = confirm(events, members);
        counters.unknown_entities = unknown_entities;
        counters.unknown_entity_samples = unknown_entity_samples;
        counters.clipped = clipped;
        counters.outside_window_dropped = outside_window_dropped;
        let windows = counters.windows;
        Self {
            snapshot: ConfirmedSuspensionSnapshot {
                as_of: today,
                facts,
            },
            bindings,
            snapshot_ids,
            not_observed_windows,
            unclosed_rows,
            counters,
            windows,
        }
    }

    pub fn confirmed_rows(&self) -> usize {
        self.snapshot.facts.len()
    }

    /// Everything a re-derivation needs to pin this acquisition (§6.3 "bind",
    /// §9.4.2 replay).
    pub fn evidence(&self) -> Value {
        json!({
            "adapter": "A-3",
            "parser_version": PARSER_VERSION,
            "sql_id": SQL_ID,
            "as_of": self.snapshot.as_of.format("%Y-%m-%d").to_string(),
            "source_bindings": self.bindings,
            "snapshot_ids": self.snapshot_ids,
            "windows": self.windows,
            "not_observed_windows": self.not_observed_windows,
            "confirmed_rows": self.confirmed_rows(),
            "unclosed_rows": self.unclosed_rows,
            "rows_read": self.counters.rows_read,
            "non_a_share_skipped": self.counters.non_a_share_skipped,
            "intraday_skipped": self.counters.intraday_skipped,
            "unclassified_skipped": self.counters.unclassified_skipped,
            "duplicate_rows": self.counters.duplicate_rows,
            "merged_overlaps": self.counters.merged_overlaps,
            "unknown_entities": self.counters.unknown_entities,
            "unknown_entity_samples": self.counters.unknown_entity_samples,
            "clipped_to_member_window": self.counters.clipped,
            "outside_member_window_dropped": self.counters.outside_window_dropped,
            "known_at": "not_available (the event table carries no announcement day; D-49)",
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
/// Not configured is *not* a fetch failure: the item keeps the gap-only result
/// and records `pending_source`, so a deployment without the M3 source store
/// does not fail its daily run. A configured store whose fetch fails is a real
/// failure and reaches the item as such (§10.1).
pub fn run_source(
    market: &crate::sqlite_market::backend::MarketBackend,
    facts: &[SecurityLifecycleFact],
    today: NaiveDate,
) -> Result<RunSource, A3Error> {
    let Some(root) = std::env::var_os(SOURCE_STORE_ENV) else {
        return Ok(RunSource::Pending(PendingSource {
            dataset: DATASET_KIND,
            source: "exchange_suspension_event",
            reason: "the M3 source store is not configured for this process \
                     (X_DATA_MCP_SOURCE_STORE); A-3 stays pending_source and every \
                     missing bar stays an unresolved_gap",
        }));
    };
    let store = SourceStore::new(root);
    let client = Client::helper()?;
    Ok(RunSource::Admitted(acquire(
        &store, market, &client, facts, today,
    )?))
}

/// Acquire every window, parse every page and assemble the snapshot.
pub fn acquire(
    store: &SourceStore,
    market: &crate::sqlite_market::backend::MarketBackend,
    client: &Client,
    facts: &[SecurityLifecycleFact],
    today: NaiveDate,
) -> Result<Admitted, A3Error> {
    let windows = windows(today)?;
    let entry = Entry::new(store, market, client)
        .with_cache(Arc::new(RunCache::new()))
        .with_today(today);
    let outcome = entry.acquire_windows(&plan(), &windows, FROM_PARAM, TO_PARAM)?;
    let mut events = Vec::new();
    let mut counters = Counters::default();
    let mut not_observed_windows = Vec::new();
    let mut snapshot_ids = Vec::new();
    for result in &outcome.results {
        if result.row_count == 0 {
            not_observed_windows.push(window_label(result.window));
        }
        for snapshot in &result.snapshots {
            let bytes = store.read_verified(snapshot.snapshot_id())?;
            let page = parse_page(&bytes, result.window, snapshot.snapshot_id())?;
            counters.rows_read += page.rows_read;
            counters.non_a_share_skipped += page.non_a_share_skipped;
            counters.intraday_skipped += page.intraday_skipped;
            counters.unclassified_skipped += page.unclassified_skipped;
            events.extend(page.events);
            snapshot_ids.push(snapshot.snapshot_id().to_string());
        }
    }
    counters.windows = outcome.results.len();
    Ok(Admitted::new(
        today,
        facts,
        events,
        counters,
        not_observed_windows,
        vec![outcome.binding],
        snapshot_ids,
    ))
}

/// Re-derive the same facts from the source store alone (`--scope exchange`
/// replay / offline verification, AC-M3-9).
///
/// The exchange index is a rebuildable projection of the store, so reading it
/// after a replay and re-parsing the frozen bytes must reproduce the facts of
/// the acquisition that froze them. When a request fingerprint was answered
/// more than once (the page changed between runs), the newest answer wins;
/// ties break on the snapshot id, so the choice is deterministic. No HTTP call
/// happens here.
pub fn reparse(
    store: &SourceStore,
    index: &[SnapshotRow],
    facts: &[SecurityLifecycleFact],
    today: NaiveDate,
) -> Result<Admitted, A3Error> {
    let source_id = SourceId::SseSuspension.as_str();
    let mut latest: BTreeMap<String, &SnapshotRow> = BTreeMap::new();
    for row in index.iter().filter(|row| row.source_id == source_id) {
        let chosen = latest.get(&row.request_fingerprint);
        let newer = match chosen {
            None => true,
            Some(previous) => {
                (row.fetched_at.as_str(), row.snapshot_id.as_str())
                    > (previous.fetched_at.as_str(), previous.snapshot_id.as_str())
            }
        };
        if newer {
            latest.insert(row.request_fingerprint.clone(), row);
        }
    }
    let mut windows: BTreeMap<(NaiveDate, NaiveDate), (bool, Vec<&SnapshotRow>)> = BTreeMap::new();
    for row in latest.values() {
        let request =
            crate::exchange_source::ExchangeRequest::from_fingerprint(&row.request_fingerprint)
                .map_err(|error| {
                    A3Error::invalid(format!(
                        "{}: the registered fingerprint is not readable: {error}",
                        row.snapshot_id
                    ))
                })?;
        let start = request.params.get(FROM_PARAM).ok_or_else(|| {
            A3Error::invalid(format!(
                "{}: the fingerprint names no {FROM_PARAM}",
                row.snapshot_id
            ))
        })?;
        let end = request.params.get(TO_PARAM).ok_or_else(|| {
            A3Error::invalid(format!(
                "{}: the fingerprint names no {TO_PARAM}",
                row.snapshot_id
            ))
        })?;
        let window = Segment::parse(start, end)
            .map_err(|error| A3Error::invalid(format!("{}: {error}", row.snapshot_id)))?;
        let entry = windows
            .entry((window.start(), window.end()))
            .or_insert((true, Vec::new()));
        entry.0 &= row.row_count == 0;
        entry.1.push(row);
    }
    let mut events = Vec::new();
    let mut counters = Counters::default();
    let mut not_observed_windows = Vec::new();
    let mut snapshot_ids = Vec::new();
    for ((start, end), (empty, rows)) in &windows {
        let (from, to) = (
            start.format("%Y-%m-%d").to_string(),
            end.format("%Y-%m-%d").to_string(),
        );
        let window =
            Segment::parse(&from, &to).map_err(|error| A3Error::invalid(error.to_string()))?;
        if *empty {
            not_observed_windows.push(window_label(window));
            continue;
        }
        let mut rows = rows.clone();
        rows.sort_by(|left, right| left.snapshot_id.cmp(&right.snapshot_id));
        for row in rows {
            let bytes = store.read_verified(&row.snapshot_id)?;
            let page = parse_page(&bytes, window, &row.snapshot_id)?;
            counters.rows_read += page.rows_read;
            counters.non_a_share_skipped += page.non_a_share_skipped;
            counters.intraday_skipped += page.intraday_skipped;
            counters.unclassified_skipped += page.unclassified_skipped;
            events.extend(page.events);
            snapshot_ids.push(row.snapshot_id.clone());
        }
    }
    counters.windows = windows.len();
    Ok(Admitted::new(
        today,
        facts,
        events,
        counters,
        not_observed_windows,
        Vec::new(),
        snapshot_ids,
    ))
}

fn window_label(window: Segment) -> String {
    format!(
        "{}..{}",
        window.start().format("%Y-%m-%d"),
        window.end().format("%Y-%m-%d")
    )
}

/// Read one frozen page. `window` supplies the end of an unclosed interval
/// (empty `endStopDate`); `snapshot_id` becomes part of `source_record_id`.
fn parse_page(bytes: &[u8], window: Segment, snapshot_id: &str) -> Result<ParsedPage, A3Error> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| A3Error::invalid(format!("{snapshot_id}: not JSON: {error}")))?;
    let data = value
        .get("pageHelp")
        .and_then(|help| help.get("data"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            A3Error::invalid(format!(
                "{snapshot_id}: the frozen page has no pageHelp.data array"
            ))
        })?;
    let mut page = ParsedPage::default();
    for row in data {
        let row = row.as_object().ok_or_else(|| {
            A3Error::invalid(format!("{snapshot_id}: a row is not a JSON object"))
        })?;
        let mut fields = BTreeMap::new();
        for name in ROW_FIELDS {
            let value = row.get(name).and_then(Value::as_str).ok_or_else(|| {
                A3Error::invalid(format!(
                    "{snapshot_id}: a row has no string {name}; the table shape changed"
                ))
            })?;
            fields.insert(name, value);
        }
        if row.len() != ROW_FIELDS.len() {
            return Err(A3Error::invalid(format!(
                "{snapshot_id}: a row carries {} fields, the recorded shape has {}",
                row.len(),
                ROW_FIELDS.len()
            )));
        }
        page.rows_read += 1;
        let code = fields["productCode"].trim();
        if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(A3Error::invalid(format!(
                "{snapshot_id}: productCode {:?} is not a 6-digit code",
                fields["productCode"]
            )));
        }
        let kind = fields["type"].trim();
        if kind != TYPE_CONTINUOUS && kind != TYPE_TEMPORARY {
            return Err(A3Error::invalid(format!(
                "{snapshot_id}: {code} has type {:?}; only LXTP/LSTP are admitted",
                fields["type"]
            )));
        }
        let stop_time = fields["stopTime"].trim();
        if STOP_TIME_INTRADAY.contains(&stop_time) {
            page.intraday_skipped += 1;
            continue;
        }
        if !stop_time.is_empty() && stop_time != STOP_TIME_WHOLE_DAY {
            return Err(A3Error::invalid(format!(
                "{snapshot_id}: {code} has stopTime {stop_time:?}, which is not WH/AM/PM/930"
            )));
        }
        // Only A shares are in scope (D-60: SSE confirmed only; B shares and
        // the bond/preferred/fund products share the table).
        if !code.starts_with('6') {
            page.non_a_share_skipped += 1;
            continue;
        }
        let start_day = parse_day(fields["startStopDate"]).ok_or_else(|| {
            A3Error::invalid(format!(
                "{snapshot_id}: {code} has startStopDate {:?}, which is not a calendar day",
                fields["startStopDate"]
            ))
        })?;
        let end_text = fields["endStopDate"].trim();
        // A `WH` halt is a whole day, so a multi-day `LSTP` row cannot be read
        // as "halted every day in between" (Karkinos only concludes a full-day
        // halt for a single-day LSTP); it is left out and counted.
        if kind == TYPE_TEMPORARY && stop_time == STOP_TIME_WHOLE_DAY && !end_text.is_empty() {
            let end = parse_day(end_text).ok_or_else(|| {
                A3Error::invalid(format!(
                    "{snapshot_id}: {code} has endStopDate {end_text:?}, which is not a calendar day"
                ))
            })?;
            if end != start_day {
                page.unclassified_skipped += 1;
                continue;
            }
        }
        if kind == TYPE_TEMPORARY && stop_time.is_empty() {
            // A temporary halt without a stop time is not the recorded shape.
            page.unclassified_skipped += 1;
            continue;
        }
        let end_day = if end_text.is_empty() {
            // Not resumed yet: the interval runs through the last day this
            // window observed. The next run extends it; `unclosed` counts it.
            window.end()
        } else {
            parse_day(end_text).ok_or_else(|| {
                A3Error::invalid(format!(
                    "{snapshot_id}: {code} has endStopDate {end_text:?}, which is not a calendar day"
                ))
            })?
        };
        if end_day < start_day {
            return Err(A3Error::invalid(format!(
                "{snapshot_id}: {code} ends ({end_day}) before it starts ({start_day})"
            )));
        }
        page.events.push(ParsedEvent {
            code: code.to_string(),
            start_day,
            end_day,
            unclosed: end_text.is_empty(),
            snapshot_id: snapshot_id.to_string(),
        });
    }
    Ok(page)
}

/// One event identity is `(code, start_day)`; the same suspension is returned
/// again by every window that overlaps it, and an unclosed interval gets a
/// later `end_day` from a later window. Keep the widest observation, then
/// merge intervals of one code that overlap into their union (the security is
/// suspended on every day of either interval, so the union is not a new fact).
fn merge_overlaps(
    mut events: Vec<ParsedEvent>,
    counters: &mut Counters,
) -> (Vec<ParsedEvent>, usize) {
    events.sort_by(|left, right| {
        (left.code.as_str(), left.start_day, left.end_day).cmp(&(
            right.code.as_str(),
            right.start_day,
            right.end_day,
        ))
    });
    let mut unique: Vec<ParsedEvent> = Vec::with_capacity(events.len());
    for event in events {
        match unique.last_mut() {
            Some(previous)
                if previous.code == event.code && previous.start_day == event.start_day =>
            {
                counters.duplicate_rows += 1;
                previous.unclosed &= event.unclosed;
                if event.end_day > previous.end_day {
                    previous.end_day = event.end_day;
                    previous.snapshot_id = event.snapshot_id;
                }
            }
            _ => unique.push(event),
        }
    }
    let mut merged: Vec<ParsedEvent> = Vec::with_capacity(unique.len());
    for event in unique {
        match merged.last_mut() {
            Some(previous)
                if previous.code == event.code && event.start_day <= previous.end_day =>
            {
                counters.merged_overlaps += 1;
                if event.end_day > previous.end_day {
                    previous.end_day = event.end_day;
                }
            }
            _ => merged.push(event),
        }
    }
    let unclosed = merged.iter().filter(|event| event.unclosed).count();
    (merged, unclosed)
}

/// What the member-window filter did with the events.
#[derive(Debug, Default)]
struct Confirmed {
    facts: Vec<ConfirmedSuspensionFact>,
    unknown_entities: usize,
    unknown_entity_samples: Vec<String>,
    clipped: usize,
    outside_window_dropped: usize,
}

/// Turn the event table into domain facts for entities the main table carries.
///
/// The member window is §11.1's (`effective_from ≤ T < effective_to`,
/// `listing_date ≤ T`, `T < delisting_date`), the same rule
/// `derive_gaps` uses; an interval is clipped into it so a suspension recorded
/// before listing or after delisting cannot claim a day the entity did not
/// exist (BR-M3-2).
fn confirm(events: Vec<ParsedEvent>, members: &[SecurityLifecycleFact]) -> Confirmed {
    let mut window: BTreeMap<String, Vec<(NaiveDate, Option<NaiveDate>)>> = BTreeMap::new();
    for fact in members {
        let start = match fact.listing_date {
            Some(listing) if listing > fact.effective_from => listing,
            _ => fact.effective_from,
        };
        let end = match (fact.effective_to, fact.delisting_date) {
            (Some(interval_end), Some(delisting)) => Some(interval_end.min(delisting)),
            (interval_end, delisting) => interval_end.or(delisting),
        };
        window.entry(fact.entity()).or_default().push((start, end));
    }
    let mut confirmed = Confirmed::default();
    for event in events {
        let entity = format!("{}.{}", Exchange::Sh.as_str(), event.code);
        let Some(windows) = window.get(&entity) else {
            confirmed.unknown_entities += 1;
            if confirmed.unknown_entity_samples.len() < 20
                && !confirmed.unknown_entity_samples.contains(&entity)
            {
                confirmed.unknown_entity_samples.push(entity);
            }
            continue;
        };
        let mut placed = false;
        for (start, end) in windows {
            let clipped_start = event.start_day.max(*start);
            let clipped_end = match end {
                Some(end) => event.end_day.min(*end),
                None => event.end_day,
            };
            if clipped_end < clipped_start {
                continue;
            }
            if clipped_start != event.start_day || clipped_end != event.end_day {
                confirmed.clipped += 1;
            }
            placed = true;
            confirmed.facts.push(ConfirmedSuspensionFact {
                exchange: Exchange::Sh,
                code: event.code.clone(),
                start_day: clipped_start,
                end_day: clipped_end,
                reason: SuspensionReason::ExchangeAnnouncement,
                // The event table has no announcement day; D-49 forbids
                // proxying it, so the row carries no `known_at`.
                known_at: None,
                source_kind: SOURCE_KIND.to_string(),
                source_record_id: Some(format!(
                    "{}:{}:{}",
                    event.snapshot_id,
                    event.code,
                    event.start_day.format("%Y%m%d")
                )),
            });
        }
        if !placed {
            confirmed.outside_window_dropped += 1;
        }
    }
    confirmed.facts.sort_by(|left, right| {
        (left.code.as_str(), left.start_day).cmp(&(right.code.as_str(), right.start_day))
    });
    confirmed
}

/// `YYYYMMDD`, the date shape of the event table.
fn parse_day(text: &str) -> Option<NaiveDate> {
    let text = text.trim();
    if text.len() != 8 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    NaiveDate::from_ymd_opt(
        text[..4].parse().ok()?,
        text[4..6].parse().ok()?,
        text[6..8].parse().ok()?,
    )
}

#[cfg(test)]
mod tests;
