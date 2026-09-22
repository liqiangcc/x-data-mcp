//! Internal #147 default-range resolution: per-security frozen work derived
//! from one caller-provided SQLite read snapshot plus an injected
//! acceptance-clock date (Asia/Shanghai, sampled once by the caller).
//! No wire/MCP types, no task acceptance, no writes; the future accept path
//! persists this evidence verbatim and replays it without recomputing.
//!
//! Frozen rules (#147 Final Gate):
//! - securities: `resolve_default_daily` semantics inside the caller snapshot;
//!   0 resolved -> EmptySelection, >100 -> whole-batch reject, no truncation.
//! - per selected symbol: base is the `effective_heads(symbol,'day','qfq')`
//!   published head, re-verified by `read_record` (identity, period,
//!   adjustment, digest, typed records). Any missing/corrupt head fails the
//!   whole batch closed; no silent skip.
//! - update: `refresh_range = [base.first_date, today]`; fail closed if
//!   first_date missing, first > last, or last > today.
//! - `proven_gap_ranges` = `read_open_gaps(c, symbol, Day)` preserving each
//!   `gap_id`/`evidence_digest`/range; refresh and gap provenance stay two
//!   distinct classes, never coalesced.
//! - backfill: only `proven_gap_ranges`, no `refresh_range`; securities
//!   without open gaps produce no item; zero total gaps -> EmptySelection.
//! - future guard at resolve time: any resolved end_date > today rejects the
//!   batch before any persistence exists.
use super::default_selection::{DefaultDailySelection, SelectionError, resolve_default_daily_on};
use crate::{
    market_data::Period,
    market_storage::Scope,
    sqlite_market::{SqliteMarket, gaps::read_open_gaps, head, read_record},
    task_control::{self as task, Contract, FrozenWork, ItemSpec, legacy::scope_id},
};
use chrono::NaiveDate;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
/// Maximum securities in one resolved default selection; matches the explicit
/// selection budget. Never truncated or paginated into multiple write tasks.
pub const MAX_DEFAULT_SECURITIES: usize = 100;
/// Which default write the frozen work describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultOperation {
    Update,
    Backfill,
}
/// One closed date range in frozen work evidence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct FrozenRange {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
/// One open proven gap with its durable evidence identity kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrozenGapRange {
    pub gap_id: String,
    pub range: FrozenRange,
    pub evidence_digest: String,
}
/// Frozen work for one selected security: verified base identity plus the
/// ranges this item must satisfy, with refresh/gap provenance separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrozenSecurityWork {
    pub symbol: String,
    pub base_version_id: String,
    pub base_integrity_sha256: String,
    /// `update`: `[base.first_date, today]`; `backfill`: always `None`.
    pub refresh_range: Option<FrozenRange>,
    /// Open proven gaps from the acceptance snapshot, in stored order.
    pub proven_gap_ranges: Vec<FrozenGapRange>,
}
/// Frozen default work: the resolved selection plus one item per security
/// (`update`) or per gap-bearing security (`backfill`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefaultWork {
    pub selection: DefaultDailySelection,
    pub items: Vec<FrozenSecurityWork>,
}
fn invalid(message: impl ToString) -> SelectionError {
    SelectionError::Invalid(message.to_string())
}
/// The default refresh range for one verified base: `[first_date, today]`.
/// Fail-closed on missing/inverted bounds or a last date beyond `today`.
fn refresh_range(
    symbol: &str,
    first: Option<NaiveDate>,
    last: Option<NaiveDate>,
    today: NaiveDate,
) -> Result<FrozenRange, SelectionError> {
    let first = first.ok_or_else(|| invalid(format!("base first_date missing for {symbol}")))?;
    let last = last.ok_or_else(|| invalid(format!("base last_date missing for {symbol}")))?;
    if first > last {
        return Err(invalid(format!(
            "base first_date after last_date for {symbol}"
        )));
    }
    if last > today {
        return Err(invalid(format!(
            "base last_date is in the future for {symbol}"
        )));
    }
    Ok(FrozenRange {
        start_date: first,
        end_date: today,
    })
}
/// Resolve frozen default work for `op` inside one caller-provided read
/// snapshot. `today` is the acceptance-clock Asia/Shanghai date sampled once
/// by the caller; the resolver itself is pure: the same snapshot facts and
/// the same `today` always produce identical frozen work.
pub fn resolve_default_work_on(
    c: &Connection,
    op: DefaultOperation,
    today: NaiveDate,
) -> Result<DefaultWork, SelectionError> {
    let selection = resolve_default_daily_on(c)?;
    if selection.securities.len() > MAX_DEFAULT_SECURITIES {
        return Err(invalid(format!(
            "default security budget exceeded: {} > {MAX_DEFAULT_SECURITIES}",
            selection.securities.len()
        )));
    }
    let mut items = vec![];
    for symbol in &selection.securities {
        let scope = Scope {
            symbol: symbol.clone(),
            period: Period::Day,
        };
        let head_id = head(c, &scope)
            .map_err(|e| invalid(format!("daily head unreadable for {symbol}: {e}")))?
            .ok_or_else(|| invalid(format!("{symbol} has no published qfq daily head")))?;
        let base = read_record(c, &head_id, true, false)
            .map_err(|e| invalid(format!("daily head unverifiable for {symbol}: {e}")))?;
        if base.version().symbol != *symbol
            || base.version().period != Period::Day
            || base.version().version_id != head_id
        {
            return Err(invalid(format!(
                "daily head identity mismatch for {symbol}"
            )));
        }
        let base_integrity_sha256 = base.raw()["integrity_sha256"]
            .as_str()
            .ok_or_else(|| invalid(format!("base digest missing for {symbol}")))?
            .to_owned();
        let refresh = refresh_range(
            symbol,
            base.version().records.first().map(|b| b.date),
            base.version().records.last().map(|b| b.date),
            today,
        )?;
        let mut proven_gap_ranges = vec![];
        for g in read_open_gaps(c, symbol, Period::Day)
            .map_err(|e| invalid(format!("proven gaps unreadable for {symbol}: {e}")))?
        {
            if g.end_date > today {
                return Err(invalid(format!(
                    "proven gap {} ends after acceptance date",
                    g.gap_id
                )));
            }
            proven_gap_ranges.push(FrozenGapRange {
                gap_id: g.gap_id,
                range: FrozenRange {
                    start_date: g.start_date,
                    end_date: g.end_date,
                },
                evidence_digest: g.evidence_digest,
            });
        }
        if op == DefaultOperation::Backfill && proven_gap_ranges.is_empty() {
            continue;
        }
        items.push(FrozenSecurityWork {
            symbol: symbol.clone(),
            base_version_id: head_id,
            base_integrity_sha256,
            refresh_range: match op {
                DefaultOperation::Update => Some(refresh),
                DefaultOperation::Backfill => None,
            },
            proven_gap_ranges,
        });
    }
    if items.is_empty() {
        return Err(SelectionError::EmptySelection);
    }
    Ok(DefaultWork { selection, items })
}
/// Resolution port for the default-range contract: implementors run
/// `resolve_default_work_on` inside one repository-owned SQLite read
/// snapshot so catalog, heads and open gaps are mutually consistent. This
/// keeps `Contract::resolve(&self, &Value)` unchanged — the snapshot scope
/// lives inside the port, not the trait signature.
pub trait DefaultRangeSource {
    fn resolve_default_work(
        &self,
        op: DefaultOperation,
        today: NaiveDate,
    ) -> Result<DefaultWork, crate::market_storage::Error>;
}
impl DefaultRangeSource for SqliteMarket {
    fn resolve_default_work(
        &self,
        op: DefaultOperation,
        today: NaiveDate,
    ) -> Result<DefaultWork, crate::market_storage::Error> {
        self.read(|c| {
            resolve_default_work_on(c, op, today).map_err(|e| match e {
                SelectionError::EmptySelection => {
                    crate::market_storage::Error::Invalid("EMPTY_SELECTION".into())
                }
                SelectionError::Invalid(message) => crate::market_storage::Error::Invalid(message),
            })
        })
    }
}
/// Closed normalized intent for `selection.kind=default`: the command
/// payload stays the deterministic minimal `{"selection":{"kind":"default"}}`
/// and resolved securities, ranges and evidence never enter it, so
/// request_id replay matching stays deterministic across later catalog,
/// gap or clock changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultIntent {
    selection: DefaultMarker,
}
/// Struct marker rather than a tagged unit variant: serde
/// `deny_unknown_fields` does not reject extra keys on internally-tagged
/// unit enum variants, so caller override fields must fail decode here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultMarker {
    kind: DefaultKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DefaultKind {
    Default,
}
fn task_invalid(message: impl ToString) -> task::Error {
    task::Error::InvalidCommand(message.to_string())
}
/// Frozen per-item inputs (serde-stable evidence shape):
/// `selection_evidence` is only the digest marker of the task's shared
/// evidence (schema 18+); the resolved selection object — catalog and
/// execution-start provenance for every selected security — is stored
/// once in the task's `shared_evidence`. Plus verified base identity,
/// `refresh_range` for update only (absent for backfill), and
/// `proven_gap_ranges` as a distinct gap-provenance class.
fn frozen_default(selection: &DefaultDailySelection, item: &FrozenSecurityWork) -> Value {
    let mut frozen = json!({
        "selection_evidence": {"evidence_digest": selection.evidence_digest},
        "base_version_id": item.base_version_id,
        "base_integrity_sha256": item.base_integrity_sha256,
        "proven_gap_ranges": item.proven_gap_ranges,
    });
    if let Some(range) = &item.refresh_range {
        frozen["refresh_range"] = serde_json::to_value(range).unwrap();
    }
    frozen
}
/// Application contract for `selection.kind=default` market.daily
/// update/backfill: first acceptance resolves and freezes snapshot-derived
/// work; callers only pick the operation. Item scope/occupancy match the
/// explicit daily contract (`symbol` x `day`).
pub struct DefaultRangeContract<R> {
    pub operation: crate::market_update::tasks::Operation,
    pub resolver: R,
    /// Asia/Shanghai acceptance date, sampled once per first-time
    /// resolution; injected so tests can pin the clock.
    pub today: fn() -> NaiveDate,
}
impl<R: DefaultRangeSource> Contract for DefaultRangeContract<R> {
    fn key(&self) -> task::CommandKey {
        let mut key = self.operation.key();
        key.contract_version = crate::market_update::tasks::DEFAULT_CONTRACT_VERSION.into();
        key
    }
    fn normalize(&self, raw: Value) -> Result<Value, task::Error> {
        let intent: DefaultIntent = serde_json::from_value(raw).map_err(task_invalid)?;
        serde_json::to_value(intent).map_err(task_invalid)
    }
    fn resolve(&self, intent: &Value) -> Result<FrozenWork, task::Error> {
        let _checked: DefaultIntent =
            serde_json::from_value(intent.clone()).map_err(task_invalid)?;
        let op = match self.operation {
            crate::market_update::tasks::Operation::Update => DefaultOperation::Update,
            crate::market_update::tasks::Operation::Backfill => DefaultOperation::Backfill,
            crate::market_update::tasks::Operation::Republish => {
                return Err(task::Error::InvalidCommand(
                    "republish is explicit-items only; no default selection".into(),
                ));
            }
        };
        let work = self
            .resolver
            .resolve_default_work(op, (self.today)())
            .map_err(crate::market_update::tasks::repository_error)?;
        let mut items = Vec::with_capacity(work.items.len());
        let mut scopes = BTreeSet::new();
        for w in &work.items {
            let scope = scope_id(&w.symbol, "day");
            scopes.insert(scope.clone());
            items.push(ItemSpec {
                item_id: scope.clone(),
                scope_keys: BTreeSet::from([scope]),
                target: json!({"symbol":w.symbol,"period":"day"}),
                progress_unit: "security_period".into(),
                frozen_inputs: frozen_default(&work.selection, w),
            });
        }
        Ok(FrozenWork {
            items,
            scopes,
            shared_evidence: Some(serde_json::to_value(&work.selection).map_err(task_invalid)?),
        })
    }
}
#[cfg(test)]
mod tests;
