//! Historical ST/*ST source adapter (issue-308, issue-446 PR-10, D-58).
//!
//! Layer position (separation point 1): this is a **source adapter**. It turns
//! frozen exchange responses into the domain values
//! [`SzNameChange`](crate::security_master::risk_warnings::SzNameChange) and
//! [`RiskWarningStatusEvent`](crate::security_master::risk_warnings::RiskWarningStatusEvent)
//! and hands them to the domain assembly. It never opens a writer and never
//! invents an interval.
//!
//! ```text
//! plans() ──► exchange_source::Entry (PR-2) ──► frozen bytes (source store)
//!                                                     │
//!                                  parse (this module, PARSER_VERSION)
//!                                                     ▼
//!             assemble_sz / assemble_sh (domain rules, D-58 tiers)
//!                                                     │
//!                               RiskWarningSet ──► writer (PR-10)
//! ```
//!
//! ## Admission tiers (D-58)
//!
//! - SZ: the dated `SSGSGMXX`/`tab2` short-name change table is
//!   `APPROVED_SOURCE`; its change date opens/closes the interval.
//! - SH: the 2000-01+ announcement index is `APPROVED_SOURCE` at the event
//!   level (implementation/revocation + announcement day). The effective day
//!   is owner-risk: an explicit date in the title when present, otherwise the
//!   announcement day. It is recorded as `OWNER_RISK_ACCEPTED` in the item
//!   evidence, never claimed as a dated name history.
//! - SH before 2000-01 and BJ are not requested and not inferred.
//!
//! ## 0 rows
//!
//! A filtered quarter with no rows is `unverified_zero` from PR-2, not proof
//! that nothing happened. The adapter records it as a `not_observed` coverage
//! window; the domain cuts every open SH interval at that gap. No row ever
//! says `normal` because evidence is absent.
//!
//! ## Failure semantics
//!
//! Acquisition failures come from PR-2 (`source_unavailable`,
//! `source_invalid`, `source_not_admitted`, `partially_failed`); a page that
//! is not the recorded row shape is `source_invalid`. The caller turns any of
//! them into a failed item, so the previous version remains readable.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use serde_json::{Value, json};

use crate::exchange_source::{
    AcquireError, AcquisitionPlan, Client, Entry, PagingFormat, RequestError, RunCache,
    SnapshotBinding, SourceId, TransportError, quarter_segments,
};
use crate::security_master::assembly::PendingSource;
use crate::security_master::risk_warnings::{
    self, CoverageWindow, RiskWarningAction, RiskWarningSet, RiskWarningStatusEvent, SzNameChange,
};
use crate::security_master::{Exchange, RiskStatus};
use crate::source_store::{SourceStore, StoreError};
use crate::sqlite_market::backend::MarketBackend;

/// Business-field parser version of this adapter. It is registered on the
/// derived item's outcome evidence, not in PR-2's envelope manifest.
pub const PARSER_VERSION: &str = "risk-warnings-v1";
/// Environment variable naming the deployment's source store, shared with
/// A-1/A-2 (`X_DATA_MCP_SOURCE_STORE`).
pub const SOURCE_STORE_ENV: &str = "X_DATA_MCP_SOURCE_STORE";
/// SZSE `ShowReport` data endpoint.
pub const SZSE_URL: &str = "https://www.szse.cn/api/report/ShowReport/data";
/// SZSE short-name change catalog (`SSGSGMXX`, tab 2).
pub const SZSE_SHORT_NAME_CATALOG: &str = "SSGSGMXX";
/// SSE company-bulletin index endpoint.
pub const SSE_BULLETIN_URL: &str =
    "https://query.sse.com.cn/security/stock/queryCompanyBulletinNew.do";
/// Title filters that cover the pre- and post-2012 warning vocabulary.
pub const SSE_TITLE_FILTERS: [&str; 2] = ["风险警示", "特别处理"];

const SZSE_A_PREFIXES: [&str; 7] = ["000", "001", "002", "003", "300", "301", "302"];
const SZSE_B_PREFIX: &str = "200";

/// One acquisition of this adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    SzseShortName,
    SseRiskWarning,
    SseSpecialTreatment,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::SzseShortName => "szse_short_name",
            Self::SseRiskWarning => "sse_bulletin_risk_warning",
            Self::SseSpecialTreatment => "sse_bulletin_special_treatment",
        }
    }

    pub fn source_id(self) -> SourceId {
        match self {
            Self::SzseShortName => SourceId::SzseShortName,
            Self::SseRiskWarning | Self::SseSpecialTreatment => SourceId::SseBulletin,
        }
    }

    fn sse(self) -> bool {
        !matches!(self, Self::SzseShortName)
    }
}

/// The three acquisitions: one SZSE table (paged), plus two SSE bulletin title
/// filters (quarterly windows). The list is static so a rerun requests the
/// same fingerprints in the same order.
pub fn plans() -> Vec<(Kind, AcquisitionPlan)> {
    let szse = AcquisitionPlan::new(
        SourceId::SzseShortName,
        SZSE_URL,
        PagingFormat::SzseMetadata,
    )
    .param("SHOWTYPE", "JSON")
    .param("CATALOGID", SZSE_SHORT_NAME_CATALOG)
    .param("TABKEY", "tab2");
    let bulletin = |kind: Kind, title: &str| {
        let plan = AcquisitionPlan::new(
            SourceId::SseBulletin,
            SSE_BULLETIN_URL,
            PagingFormat::SsePageHelp,
        )
        .param("isPagination", "true")
        .param("pageHelp.pageSize", "100")
        .param("TITLE", title);
        (kind, plan)
    };
    vec![
        (Kind::SzseShortName, szse),
        bulletin(Kind::SseRiskWarning, SSE_TITLE_FILTERS[0]),
        bulletin(Kind::SseSpecialTreatment, SSE_TITLE_FILTERS[1]),
    ]
}

/// Why the adapter could not produce a settled row set.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Acquire(#[from] AcquireError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Request(#[from] RequestError),
    #[error(transparent)]
    Domain(#[from] risk_warnings::Error),
    #[error("{code}: {detail}")]
    Invalid { code: &'static str, detail: String },
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Acquire(error) => error.code(),
            Self::Store(error) => error.code(),
            Self::Transport(error) => error.code(),
            Self::Request(error) => error.code(),
            Self::Domain(_) => "source_invalid",
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

/// An admitted row set plus everything the derived item must register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted {
    pub set: RiskWarningSet,
    pub bindings: Vec<SnapshotBinding>,
    /// `label → snapshot ids`, in acquisition order.
    pub snapshot_ids: BTreeMap<&'static str, Vec<String>>,
    pub sz_rows: usize,
    pub sz_skipped_codes: usize,
    pub sh_rows: usize,
    pub sh_events: usize,
    pub sh_ignored_rows: usize,
}

impl Admitted {
    pub fn evidence(&self) -> Value {
        json!({
            "adapter": "historical ST/risk warnings (D-58)",
            "parser_version": PARSER_VERSION,
            "source_bindings": self.bindings,
            "snapshot_ids": self.snapshot_ids,
            "sz_name_change_rows": self.sz_rows,
            "sz_skipped_codes": self.sz_skipped_codes,
            "sh_bulletin_rows": self.sh_rows,
            "sh_status_events": self.sh_events,
            "sh_ignored_rows": self.sh_ignored_rows,
            "interval_rows": self.set.rows.len(),
            "observed_windows": self.set.observed_windows(),
            "not_observed_windows": self.set.not_observed_windows(),
            "coverage": self.set.coverage.iter().map(|window| json!({
                "source_kind": window.source_kind,
                "exchange": window.exchange.as_str(),
                "from": window.from.to_string(),
                "to": window.to.to_string(),
                "observed": window.observed,
                "rows": window.rows,
                "reason": window.reason,
            })).collect::<Vec<_>>(),
            // D-58 says the SH effective day is not in the announcement source;
            // this is the owner-risk anchor the writer records.
            "effective_date_basis": {
                "sz": "source_change_date (APPROVED_SOURCE)",
                "sh": "explicit title date when present, otherwise announcement day (OWNER_RISK_ACCEPTED)",
                "sh_status": "owner_risk_accepted",
            },
            "absence_policy": "not_observed is a coverage window, never a `normal` row; \
                a missing interval remains unknown",
        })
    }
}

/// The daily item's answer: an admitted set, or the reason the source is
/// pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunSource {
    Admitted(Admitted),
    Pending(PendingSource),
}

/// Resolve the source store and fetch client, then acquire.
///
/// Not configured is not a fetch failure: the item records `pending_source`
/// and writes zero rows instead of failing the daily run. A configured store
/// whose fetch fails is a real failure.
pub fn run_source(market: &MarketBackend) -> Result<RunSource, Error> {
    let Some(root) = std::env::var_os(SOURCE_STORE_ENV) else {
        return Ok(RunSource::Pending(PendingSource {
            dataset: risk_warnings::DATASET_KIND,
            source: "exchange_risk_warning_announcement",
            reason: "the M3 source store is not configured for this process \
                     (X_DATA_MCP_SOURCE_STORE); historical ST stays pending_source",
        }));
    };
    let store = SourceStore::new(root);
    let client = Client::helper()?;
    Ok(RunSource::Admitted(acquire_today(
        &store,
        market,
        &client,
        Utc::now().date_naive(),
    )?))
}

fn acquire_today(
    store: &SourceStore,
    market: &MarketBackend,
    client: &Client,
    today: NaiveDate,
) -> Result<Admitted, Error> {
    let entry = Entry::new(store, market, client)
        .with_cache(Arc::new(RunCache::new()))
        .with_today(today);
    let mut snapshot_ids: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut sz_changes = Vec::new();
    let mut sz_skipped = 0usize;
    let mut sz_snapshot_rows = 0usize;

    let (sz_kind, sz_plan) = plans()
        .into_iter()
        .find(|(kind, _)| *kind == Kind::SzseShortName)
        .expect("the SZSE plan is static");
    let sz_outcome = entry.acquire(&sz_plan)?;
    for snapshot in &sz_outcome.snapshots {
        let bytes = store.read_verified(snapshot.snapshot_id())?;
        let mut parsed = parse_sz_page(&bytes, snapshot.snapshot_id())?;
        sz_snapshot_rows += parsed.1;
        sz_skipped += parsed.2;
        sz_changes.append(&mut parsed.0);
        snapshot_ids
            .entry(sz_kind.label())
            .or_default()
            .push(snapshot.snapshot_id().to_string());
    }
    let sz_days: Vec<NaiveDate> = sz_changes.iter().map(|change| change.day).collect();
    let sz_coverage = match (sz_days.iter().min(), sz_days.iter().max()) {
        (Some(from), Some(to)) => vec![CoverageWindow {
            source_kind: "szse_name_history".into(),
            exchange: Exchange::Sz,
            from: *from,
            to: *to,
            observed: true,
            rows: sz_changes.len(),
            reason: "the frozen short-name table is non-empty".into(),
        }],
        _ => {
            return Err(Error::invalid(
                "the frozen SZSE short-name table has no A-share row",
            ));
        }
    };

    let mut sh_rows = 0usize;
    let mut sh_ignored = 0usize;
    let mut sh_events = Vec::new();
    let mut seen_bulletins = BTreeSet::new();
    let mut quarter_rows: BTreeMap<(NaiveDate, NaiveDate), usize> = BTreeMap::new();
    let mut quarter_observed: BTreeSet<(NaiveDate, NaiveDate)> = BTreeSet::new();
    let mut quarter_failures: BTreeMap<(NaiveDate, NaiveDate), String> = BTreeMap::new();
    let archive_start = NaiveDate::from_ymd_opt(2000, 1, 1).expect("2000-01-01 is valid");
    let windows = quarter_segments(archive_start, today)?;
    for (kind, plan) in plans().into_iter().filter(|(kind, _)| kind.sse()) {
        for window in &windows {
            let key = (window.start(), window.end());
            match entry.acquire_window(&plan, *window, "START_DATE", "END_DATE") {
                Ok(outcome) => {
                    quarter_observed.insert(key);
                    for snapshot in &outcome.snapshots {
                        let bytes = store.read_verified(snapshot.snapshot_id())?;
                        let mut parsed = parse_sh_page(&bytes, snapshot.snapshot_id())?;
                        sh_rows += parsed.1;
                        sh_ignored += parsed.2;
                        for event in &mut parsed.0 {
                            if seen_bulletins.insert(event.source_record_id.clone()) {
                                sh_events.push(event.clone());
                            }
                        }
                        quarter_rows
                            .entry(key)
                            .and_modify(|count| *count += parsed.0.len())
                            .or_insert(parsed.0.len());
                        snapshot_ids
                            .entry(kind.label())
                            .or_default()
                            .push(snapshot.snapshot_id().to_string());
                    }
                }
                Err(AcquireError::Invalid {
                    code: "unverified_zero",
                    ..
                }) => {
                    // A title-filtered quarter may genuinely have no matching
                    // announcement. That is not "no warning": keep it as an
                    // unobserved window and cut intervals at its start.
                    quarter_failures.insert(key, "unverified_zero".into());
                }
                Err(error) => return Err(Error::Acquire(error)),
            }
        }
    }
    let mut sh_coverage = Vec::with_capacity(windows.len());
    let mut gaps = Vec::new();
    for window in &windows {
        let key = (window.start(), window.end());
        let observed = quarter_observed.contains(&key);
        let rows = quarter_rows.get(&key).copied().unwrap_or(0);
        if !observed {
            gaps.push(key);
        }
        sh_coverage.push(CoverageWindow {
            source_kind: "exchange_announcement".into(),
            exchange: Exchange::Sh,
            from: window.start(),
            to: window.end(),
            observed,
            rows,
            reason: if observed {
                "at least one title filter returned an indexed window".into()
            } else {
                format!(
                    "not_observed: {}; no row is read as 'no risk warning'",
                    quarter_failures
                        .get(&key)
                        .map_or("no indexed announcement", String::as_str)
                )
            },
        });
    }
    let mut set = risk_warnings::assemble_sz(&sz_changes, sz_coverage)?;
    let sh = risk_warnings::assemble_sh(&sh_events, &gaps, sh_coverage)?;
    set.rows.extend(sh.rows);
    set.coverage.extend(sh.coverage);
    set.pending_sources.extend(sh.pending_sources);

    let mut bindings = Vec::new();
    for source in [SourceId::SzseShortName, SourceId::SseBulletin] {
        // The SH source may have no frozen page only if every window was
        // unobserved; in that case there is no binding to register and the
        // item must not claim an admitted set.
        match crate::exchange_source::bind::bind(
            market,
            source,
            crate::exchange_source::BasisStatus::ApprovedSource,
        ) {
            Ok(binding) => bindings.push(binding),
            Err(crate::exchange_source::BindError::NotFound { .. })
                if source == SourceId::SseBulletin && sh_events.is_empty() =>
            {
                return Err(Error::invalid(
                    "the SSE bulletin source produced no frozen snapshot; the ST event set \
                     cannot be bound",
                ));
            }
            Err(error) => return Err(Error::Acquire(AcquireError::Bind(error))),
        }
    }
    bindings.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    Ok(Admitted {
        set,
        bindings,
        snapshot_ids,
        sz_rows: sz_snapshot_rows,
        sz_skipped_codes: sz_skipped,
        sh_rows,
        sh_events: sh_events.len(),
        sh_ignored_rows: sh_ignored,
    })
}

/// Parse one SZSE page: `(changes, rows_read, skipped_non_a_codes)`.
fn parse_sz_page(
    bytes: &[u8],
    snapshot_id: &str,
) -> Result<(Vec<SzNameChange>, usize, usize), Error> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| Error::invalid(format!("SZSE short-name page is not JSON: {error}")))?;
    let rows = sz_rows(&value)?;
    let total = rows.len();
    let mut changes = Vec::new();
    let mut skipped = 0usize;
    for row in rows {
        let code = text_field(&row, "zqdm")?;
        if code.starts_with(SZSE_B_PREFIX) {
            skipped += 1;
            continue;
        }
        if !SZSE_A_PREFIXES
            .iter()
            .any(|prefix| code.starts_with(prefix))
        {
            skipped += 1;
            continue;
        }
        let day = parse_day(&text_field(&row, "bgrq")?, "bgrq")?;
        let before = text_field(&row, "bgqzqjc")?;
        let after = text_field(&row, "bghzqjc")?;
        if before.trim().is_empty() || after.trim().is_empty() {
            return Err(Error::invalid(format!(
                "SZSE short-name row {code} {day} has an empty before/after name"
            )));
        }
        changes.push(SzNameChange {
            source_record_id: format!("{day}|{code}|{after}"),
            source_document_sha256: snapshot_id.to_string(),
            code,
            day,
            before_name: before,
            after_name: after,
        });
    }
    Ok((changes, total, skipped))
}

fn sz_rows(value: &Value) -> Result<Vec<Value>, Error> {
    let data = if let Some(blocks) = value.as_array() {
        blocks
            .iter()
            .find_map(|block| block.get("data").and_then(Value::as_array))
    } else {
        value.get("data").and_then(Value::as_array)
    };
    data.cloned()
        .ok_or_else(|| Error::invalid("SZSE short-name page has no data row array"))
}

/// Parse one SSE bulletin page: `(events, rows_read, ignored_rows)`.
fn parse_sh_page(
    bytes: &[u8],
    snapshot_id: &str,
) -> Result<(Vec<RiskWarningStatusEvent>, usize, usize), Error> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| Error::invalid(format!("SSE bulletin page is not JSON: {error}")))?;
    let rows = sh_rows(&value)?;
    let total = rows.len();
    let mut events = Vec::new();
    let mut ignored = 0usize;
    for row in rows {
        let code = text_field(&row, "SECURITY_CODE")?;
        if !is_sh_a_code(&code) {
            ignored += 1;
            continue;
        }
        let title = text_field(&row, "TITLE")?;
        let Some((action, status)) = classify_title(&title) else {
            ignored += 1;
            continue;
        };
        let announced = parse_day(&text_field(&row, "SSEDATE")?, "SSEDATE")?;
        let effective = explicit_effective_day(&title).unwrap_or(announced);
        let record_id = row
            .get("ORG_BULLETIN_ID")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                row.get("URL")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                Error::invalid(format!("SSE bulletin row {code} {announced} has no id"))
            })?;
        events.push(RiskWarningStatusEvent {
            code,
            day: effective,
            action,
            status,
            announced_at: Some(announced.to_string()),
            known_at: Some(announced),
            raw_status_code: match (action, status) {
                (RiskWarningAction::Implement, Some(status)) => {
                    format!("implement:{}", status.as_str())
                }
                (RiskWarningAction::Revoke, _) => "revoke".into(),
                _ => return Err(Error::invalid("an implementation event has no status")),
            },
            source_kind: "exchange_announcement".into(),
            source_record_id: record_id,
            source_document_sha256: snapshot_id.to_string(),
        });
    }
    Ok((events, total, ignored))
}

fn sh_rows(value: &Value) -> Result<Vec<Value>, Error> {
    let data = value
        .get("pageHelp")
        .and_then(|help| help.get("data"))
        .and_then(Value::as_array)
        .or_else(|| value.get("data").and_then(Value::as_array));
    data.cloned()
        .ok_or_else(|| Error::invalid("SSE bulletin page has no data row array"))
}

fn classify_title(title: &str) -> Option<(RiskWarningAction, Option<RiskStatus>)> {
    let title = title.trim();
    if !title.contains("风险警示") && !title.contains("特别处理") {
        return None;
    }
    // An application, a proposal, or a prediction is not an event. D-58
    // admits implementation/revocation, not intent.
    if title.contains("申请")
        || title.contains("拟")
        || title.contains("可能")
        || title.contains("将被")
    {
        return None;
    }
    let status = if title.contains("退市风险警示") {
        Some(RiskStatus::DelistingRiskWarning)
    } else if title.contains("特别处理") || title.contains("风险警示") {
        Some(RiskStatus::OtherRiskWarning)
    } else {
        None
    };
    if title.contains("撤销") || title.contains("取消") {
        return Some((RiskWarningAction::Revoke, None));
    }
    if title.contains("实施") || title.contains("实行") {
        return status.map(|status| (RiskWarningAction::Implement, Some(status)));
    }
    None
}

fn is_sh_a_code(code: &str) -> bool {
    code.len() == 6 && code.starts_with('6') && code.bytes().all(|byte| byte.is_ascii_digit())
}

/// A title such as “自2024年5月6日起” carries an explicit effective day; use
/// it when present. This is still an owner-risk anchor (the announcement index
/// is not a dated name history), but it is the publisher's own date.
fn explicit_effective_day(title: &str) -> Option<NaiveDate> {
    let chars: Vec<char> = title.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] != '年' {
            index += 1;
            continue;
        }
        let year_start = index.saturating_sub(4);
        let year: String = chars[year_start..index].iter().collect();
        if year.len() != 4 || !year.bytes().all(|byte| byte.is_ascii_digit()) {
            index += 1;
            continue;
        }
        let Some(month_end) = chars[index + 1..].iter().position(|ch| *ch == '月') else {
            index += 1;
            continue;
        };
        let month: String = chars[index + 1..index + 1 + month_end].iter().collect();
        let Some(day_end) = chars[index + 1 + month_end + 1..]
            .iter()
            .position(|ch| *ch == '日')
        else {
            index += 1;
            continue;
        };
        let day: String = chars[index + 1 + month_end + 1..index + 1 + month_end + 1 + day_end]
            .iter()
            .collect();
        let (Ok(year), Ok(month), Ok(day)) = (year.parse(), month.parse(), day.parse()) else {
            index += 1;
            continue;
        };
        if let Some(date) = NaiveDate::from_ymd_opt(year, month, day) {
            // Only accept a date followed by “起”; a date in “截至…” is not an
            // effective start.
            let after = index + 1 + month_end + 1 + day_end + 1;
            if chars.get(after) == Some(&'起') {
                return Some(date);
            }
        }
        index += 1;
    }
    None
}

fn text_field(row: &Value, field: &str) -> Result<String, Error> {
    let Some(value) = row.get(field).and_then(Value::as_str) else {
        return Err(Error::invalid(format!("row has no string field {field}")));
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::invalid(format!("row has an empty field {field}")));
    }
    Ok(value.to_string())
}

fn parse_day(text: &str, field: &str) -> Result<NaiveDate, Error> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .map_err(|_| Error::invalid(format!("{field}={text:?} is not a YYYY-MM-DD day")))
}

/// `--scope risk_warnings` replay support: parse frozen pages from the source
/// store and rebuild the domain set without any HTTP request.
///
/// `pages` are `(source_id, snapshot_id, bytes)` in manifest order. The
/// caller chooses which SZ short-name table snapshot is current; all SSE
/// bulletin pages are de-duplicated by their announcement id.
pub fn replay(
    pages: &[(&str, String, Vec<u8>)],
    gaps: &[(NaiveDate, NaiveDate)],
    coverage: Vec<CoverageWindow>,
) -> Result<RiskWarningSet, Error> {
    let mut sz_changes = Vec::new();
    let mut sh_events = Vec::new();
    let mut seen_bulletins = BTreeSet::new();
    for (source_id, snapshot_id, bytes) in pages {
        match *source_id {
            "szse_short_name" => {
                let (mut changes, _, _) = parse_sz_page(bytes, snapshot_id)?;
                sz_changes.append(&mut changes);
            }
            "sse_bulletin" => {
                let (events, _, _) = parse_sh_page(bytes, snapshot_id)?;
                for event in events {
                    if seen_bulletins.insert(event.source_record_id.clone()) {
                        sh_events.push(event);
                    }
                }
            }
            _ => {}
        }
    }
    let mut sz_coverage = Vec::new();
    let mut sh_coverage = Vec::new();
    for window in coverage {
        if window.exchange == Exchange::Sz {
            sz_coverage.push(window);
        } else {
            sh_coverage.push(window);
        }
    }
    let mut set = risk_warnings::assemble_sz(&sz_changes, sz_coverage)?;
    let sh = risk_warnings::assemble_sh(&sh_events, gaps, sh_coverage)?;
    set.rows.extend(sh.rows);
    set.coverage.extend(sh.coverage);
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("tests/fixtures/exchange-risk-warnings/{name}"))
            .expect("risk-warning fixture exists")
    }

    #[test]
    fn sz_page_parses_a_change_chain() {
        let (changes, rows, skipped) =
            parse_sz_page(&fixture("szse-name-changes.json"), "a").expect("fixture parses");
        assert_eq!(rows, 4);
        assert_eq!(skipped, 1, "a 200 B-share row is skipped");
        assert_eq!(changes.len(), 3);
    }

    #[test]
    fn sh_page_parses_only_actual_status_events() {
        let (events, rows, ignored) =
            parse_sh_page(&fixture("sse-bulletins.json"), "b").expect("fixture parses");
        assert_eq!(rows, 5);
        assert_eq!(events.len(), 3);
        assert_eq!(
            ignored, 2,
            "a proposal and an unrelated bulletin are ignored"
        );
    }

    #[test]
    fn explicit_title_date_wins_over_announcement_day() {
        let (events, _, _) =
            parse_sh_page(&fixture("sse-bulletins.json"), "b").expect("fixture parses");
        assert_eq!(events[0].day, NaiveDate::from_ymd_opt(2024, 5, 6).unwrap());
        assert_eq!(events[0].announced_at.as_deref(), Some("2024-04-30"));
    }

    #[test]
    fn proposals_are_not_events() {
        assert!(classify_title("关于申请撤销公司股票其他风险警示的公告").is_none());
        assert!(classify_title("关于公司股票可能被实施退市风险警示的提示性公告").is_none());
        assert_eq!(
            classify_title("关于公司股票被实施其他风险警示的公告"),
            Some((
                RiskWarningAction::Implement,
                Some(RiskStatus::OtherRiskWarning)
            ))
        );
    }
}
