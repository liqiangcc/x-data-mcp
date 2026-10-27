//! Stateless market-update plans (generic-data-v7).
//!
//! `plan_market_update` resolves the whole current-catalog∩collected
//! universe inside one repository read snapshot and returns only a digest
//! plus counts. `run_market_update` re-runs the identical resolution under
//! a fresh snapshot and requires the client-supplied `plan_id` to match
//! exactly — a mismatch is `plan_stale`, never a partial execution. No plan
//! state is persisted: the digest binds the dataset, the plan contract
//! version, the active verified generation, the target day and every
//! per-entity resolved fact, and the admitted task's frozen work is
//! rebuilt deterministically from the same canonical payload.
use super::wire::{Dataset, PlanCounts, PlanIneligibleEntity, PlanReasonCount, PlanView};
use crate::market_data::Period;
use crate::market_storage::Scope;
use crate::sqlite_market::{head, is_latest_only_schema, series};
use crate::task_control::{CommandKey, Contract, Error, FrozenWork, ItemSpec};
use chrono::{Datelike, NaiveDate, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;
use std::collections::BTreeMap;

/// Plan contract version bound into every `plan_id` digest. The wire
/// `contract_version` stays `v1`; this string pins the canonical plan
/// shape the digest covers.
pub const PLAN_CONTRACT_VERSION: &str = "market-update-plan-v1";
/// Contract version of tasks admitted by `run_market_update`. The worker
/// dispatches them on the normal `update_data` operation; the version
/// distinguishes plan tasks for progress/reporting.
pub const RUN_CONTRACT_VERSION: &str = "market-plan-run-task-v1";
/// Frozen-input key stamped with the plan's generation id — the same key
/// the `Bound` decorator uses so worker session opens are unchanged.
const SOURCE_GENERATION_KEY: &str = super::generation::SOURCE_GENERATION_KEY;
/// Most ineligible entities `PlanView::ineligible_entities` will carry.
/// Entity names are small, but a whole-catalog plan can hold thousands, so
/// the diagnostic list is capped; `ineligible_entities_truncated` flags a
/// cut. Never part of the canonical plan or its `plan_id`.
pub const PLAN_INELIGIBLE_ENTITY_LIMIT: usize = 500;

/// `io::Write` adapter so canonical JSON streams into sha256 without a
/// materialized byte buffer (`sha2` does not implement `io::Write` here).
struct DigestWriter(sha2::Sha256);
impl std::io::Write for DigestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn invalid(message: impl ToString) -> Error {
    Error::InvalidCommand(message.to_string())
}
fn iinvalid(message: impl ToString) -> crate::market_storage::Error {
    crate::market_storage::Error::Invalid(message.to_string())
}
fn istorage(message: impl ToString) -> crate::market_storage::Error {
    crate::market_storage::Error::Storage(message.to_string())
}

/// Plan-level failure boundary; the handler maps these to wire codes.
#[derive(Debug)]
pub enum PlanError {
    /// Caller-side shape/range problems — `invalid_arguments`.
    Invalid(String),
    /// Storage or integrity reads failed — `storage_error`.
    Storage(String),
}

/// One canonical range `[start_date, end_date]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRange {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
/// One open proven gap pinned into a daily plan item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanGap {
    pub gap_id: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub evidence_digest: String,
}
/// Eligible daily item — everything the frozen default work needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyPlanItem {
    pub entity: String,
    pub base_version_id: String,
    pub base_integrity_sha256: String,
    pub refresh_range: PlanRange,
    pub proven_gap_ranges: Vec<PlanGap>,
}
/// Yearly head the plan expects to supersede, with its content digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanComparison {
    pub version_id: String,
    pub integrity_sha256: String,
}
/// Eligible yearly item — everything the frozen yearly work needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct YearlyPlanItem {
    pub entity: String,
    pub base_daily_version_id: String,
    pub base_integrity_sha256: String,
    pub years: Vec<i32>,
    pub expected_yearly_head: Option<String>,
    pub comparison: Option<PlanComparison>,
}
/// Canonical per-entity work record (untagged: the dataset column pins the
/// variant, so each row serializes as the plain struct).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PlanItem {
    Daily(DailyPlanItem),
    Yearly(YearlyPlanItem),
}
impl PlanItem {
    fn entity(&self) -> &str {
        match self {
            Self::Daily(i) => &i.entity,
            Self::Yearly(i) => &i.entity,
        }
    }
}
/// One non-eligible entity with a fixed reason string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanIneligible {
    pub entity: String,
    pub reason: String,
}

/// A resolved plan: deterministic, timestamp-free, small enough to return.
#[derive(Debug)]
pub struct Plan {
    pub dataset: Dataset,
    pub generation_id: Option<String>,
    pub target_day: NaiveDate,
    pub items: Vec<PlanItem>,
    pub ineligible: Vec<PlanIneligible>,
}
/// Canonical digest input. Declaration order IS the serialization order;
/// items are sorted by entity and `ineligible` by (entity, reason) before
/// this is hashed, so the digest is reproducible across processes.
#[derive(Serialize)]
struct CanonicalPlan<'a> {
    dataset: &'a str,
    contract_version: &'a str,
    generation_id: Option<&'a str>,
    target_day: NaiveDate,
    items: &'a [PlanItem],
    ineligible: &'a [PlanIneligible],
}
impl Plan {
    fn dataset_name(&self) -> &'static str {
        match self.dataset {
            Dataset::Daily => "market.daily",
            Dataset::Yearly => "market.yearly",
            Dataset::Catalog => "securities.catalog",
            Dataset::Index => "market.index",
            Dataset::AdjustmentEvents => "market.adjustment_events",
            Dataset::Calendar => "market.calendar",
            Dataset::SecurityMaster => "market.security_master",
            Dataset::SecurityNames => "market.security_names",
            Dataset::CapitalEvents => "market.capital_events",
            Dataset::RiskWarnings => "market.risk_warnings",
            Dataset::Suspensions => "market.suspensions",
            Dataset::PriceLimitRules => "market.price_limit_rules",
        }
    }
    /// Canonical serialization — the exact digest input.
    fn canonical(&self) -> CanonicalPlan<'_> {
        CanonicalPlan {
            dataset: self.dataset_name(),
            contract_version: PLAN_CONTRACT_VERSION,
            generation_id: self.generation_id.as_deref(),
            target_day: self.target_day,
            items: &self.items,
            ineligible: &self.ineligible,
        }
    }
    /// `plan_id` = sha256 over the canonical plan — deterministic for the
    /// same snapshot facts and parameters. The canonical byte stream is
    /// identical to `serde_json::to_vec` of the same struct; it is fed
    /// through the digest writer so a whole-market plan never needs its
    /// serialized form materialized.
    pub fn plan_id(&self) -> Result<String, PlanError> {
        let mut hasher = DigestWriter(sha2::Sha256::new());
        serde_json::to_writer(&mut hasher, &self.canonical())
            .map_err(|e| PlanError::Invalid(format!("plan serialization: {e}")))?;
        Ok(hasher
            .0
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
    /// The accepted-request intent: a compact digest-bound descriptor.
    /// Item payloads live in the task's frozen work, not the request
    /// binding — keeping them out of `command_json` is what makes a
    /// whole-market run acceptance fit in bounded memory.
    pub fn run_intent(&self) -> Result<Value, PlanError> {
        serde_json::to_value(RunIntent {
            contract_version: PLAN_CONTRACT_VERSION.into(),
            plan_id: self.plan_id()?,
            dataset: self.dataset_name().into(),
            generation_id: self.generation_id.clone(),
            target_day: self.target_day,
            item_count: self.items.len() as u64,
        })
        .map_err(|e| PlanError::Invalid(format!("plan intent serialization: {e}")))
    }
    /// Decompose into the streamed-acceptance parts: the shared evidence,
    /// the occupied scope set, and the items the caller converts to
    /// `ItemSpec`s one at a time. The digest is recomputed here so every
    /// evidence marker provably matches the plan that produced them.
    pub fn run_parts(self) -> Result<RunWorkParts, PlanError> {
        let plan_id = self.plan_id()?;
        let counts = self.counts();
        let shared_evidence = json!({
            "kind": "market_update_plan",
            "evidence_digest": plan_id,
            "plan_id": plan_id,
            "dataset": self.dataset_name(),
            "contract_version": PLAN_CONTRACT_VERSION,
            "generation_id": self.generation_id.clone(),
            "target_day": self.target_day,
            "counts": counts,
            "ineligible_reasons": self.ineligible_reasons(),
        });
        let scopes = self
            .items
            .iter()
            .map(item_scope)
            .collect::<std::collections::BTreeSet<_>>();
        Ok(RunWorkParts {
            plan_id,
            generation: self.generation_id.map(Value::String).unwrap_or(Value::Null),
            scopes,
            shared_evidence,
            items: self.items,
        })
    }
    fn eligible(&self) -> u64 {
        self.items.len() as u64
    }
    fn reason_count(&self, reason: &str) -> u64 {
        self.ineligible
            .iter()
            .filter(|i| i.reason == reason)
            .count() as u64
    }
    pub fn counts(&self) -> PlanCounts {
        PlanCounts {
            eligible: self.eligible(),
            already_current: self.reason_count("already_current"),
            absent: self.reason_count("absent") + self.reason_count("no_daily_base"),
            ineligible: self
                .ineligible
                .iter()
                .filter(|i| {
                    !matches!(
                        i.reason.as_str(),
                        "already_current" | "absent" | "no_daily_base"
                    )
                })
                .count() as u64,
        }
    }
    /// Aggregated ineligible reasons (excluding `already_current`/`absent`,
    /// which have their own count fields), sorted by reason.
    fn ineligible_reasons(&self) -> Vec<PlanReasonCount> {
        let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
        for i in &self.ineligible {
            if matches!(
                i.reason.as_str(),
                "already_current" | "absent" | "no_daily_base"
            ) {
                continue;
            }
            *counts.entry(i.reason.as_str()).or_default() += 1;
        }
        counts
            .into_iter()
            .map(|(reason, count)| PlanReasonCount {
                reason: reason.into(),
                count,
            })
            .collect()
    }
    pub fn view(&self) -> Result<PlanView, PlanError> {
        let mut ineligible_entities: Vec<PlanIneligibleEntity> = self
            .ineligible
            .iter()
            .filter(|i| {
                !matches!(
                    i.reason.as_str(),
                    "already_current" | "absent" | "no_daily_base"
                )
            })
            .map(|i| PlanIneligibleEntity {
                entity: i.entity.clone(),
                reason: i.reason.clone(),
            })
            .collect();
        let ineligible_entities_truncated =
            ineligible_entities.len() > PLAN_INELIGIBLE_ENTITY_LIMIT;
        ineligible_entities.truncate(PLAN_INELIGIBLE_ENTITY_LIMIT);
        Ok(PlanView {
            plan_id: self.plan_id()?,
            dataset: self.dataset,
            contract_version: super::wire::ContractVersion::V1,
            generation_ref: self.generation_id.clone(),
            target_day: self.target_day,
            target_day_source: None,
            counts: self.counts(),
            ineligible_reasons: self.ineligible_reasons(),
            ineligible_entities,
            ineligible_entities_truncated,
            snapshot_time: Utc::now(),
        })
    }
    /// Sorted-by-entity copies suitable for canonical hashing.
    fn sort(&mut self) {
        self.items.sort_by(|a, b| a.entity().cmp(b.entity()));
        self.ineligible
            .sort_by(|a, b| a.entity.cmp(&b.entity).then(a.reason.cmp(&b.reason)));
    }
}
/// `true` for a well-formed `plan_id` (64 lowercase hex).
pub fn valid_plan_id(plan_id: &str) -> bool {
    plan_id.len() == 64
        && plan_id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Non-terminal task statuses (claims still hold scope occupancy).
const ACTIVE_STATUSES: &[&str] = &[
    "pending",
    "running",
    "cancel_requested",
    "needs_verification",
];

/// Non-terminal update work discovered inside the plan read snapshot.
#[derive(Debug)]
pub struct InflightUpdate {
    pub task_id: String,
    pub dataset: String,
}
/// Scan the tasks table inside the caller's read snapshot for non-terminal
/// update-family tasks on `datasets`. Update-family operations overlap
/// entities' scope claims, so any non-terminal one conflicts with a new
/// whole-market task — report it before reaching `accept`'s range_conflict.
fn inflight_updates(c: &Connection) -> Result<Vec<InflightUpdate>, crate::market_storage::Error> {
    let active = ACTIVE_STATUSES
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(",");
    let mut q = c
        .prepare(&format!(
            "SELECT task_id,command_key_json FROM tasks WHERE status IN ({active})"
        ))
        .map_err(|e| istorage(format!("inflight task scan: {e}")))?;
    let mut found = vec![];
    for row in q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| istorage(format!("inflight task scan: {e}")))?
    {
        let (task_id, key_json) = row.map_err(|e| istorage(format!("inflight task scan: {e}")))?;
        let key: Value = serde_json::from_str(&key_json)
            .map_err(|e| istorage(format!("inflight task key decode: {e}")))?;
        let dataset = key["dataset"].as_str().unwrap_or_default();
        let operation = key["operation"].as_str().unwrap_or_default();
        if matches!(
            operation,
            "update_data" | "backfill_data" | "republish_data"
        ) && matches!(dataset, "market.daily" | "market.yearly")
        {
            found.push(InflightUpdate {
                task_id,
                dataset: dataset.to_string(),
            });
        }
    }
    Ok(found)
}

/// Resolved run request: recomputed plan plus the in-flight update scan,
/// all inside one repository read snapshot.
pub struct RunResolution {
    pub plan: Plan,
    pub inflight: Vec<InflightUpdate>,
}

/// How a plan's `target_day` was chosen (reported on the plan response
/// only; never part of the canonical plan or its `plan_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetDaySource {
    Explicit,
    GenerationLatest,
    TodayFallback,
}
impl TargetDaySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::GenerationLatest => "generation_latest",
            Self::TodayFallback => "today_fallback",
        }
    }
}
/// Securities sampled to find the active generation's latest trade day:
/// the first N of the sorted plan universe, so the choice is a pure
/// function of the generation bytes and the database state.
const TARGET_DAY_SAMPLE: usize = 32;

/// Default target day from the latest trade day seen in the sample, capped
/// at `today`; no readable sample means `today`. Pure so it is testable
/// without a generation.
pub(crate) fn pick_target_day(
    latest: impl IntoIterator<Item = Option<NaiveDate>>,
    today: NaiveDate,
) -> (NaiveDate, TargetDaySource) {
    match latest.into_iter().flatten().max() {
        Some(day) => (day.min(today), TargetDaySource::GenerationLatest),
        None => (today, TargetDaySource::TodayFallback),
    }
}

/// `plan_market_update` without `end_date`: the official day for today is
/// published late in the afternoon, so defaulting to today would list every
/// entity as lagging forever. Use the latest trade day the active
/// generation actually contains. Falls back to `today` (the previous
/// behavior) when there is no verified generation or nothing is readable.
pub fn default_target_day(
    market: &crate::sqlite_market::backend::MarketBackend,
    generation: Option<&super::generation::GenerationSource>,
    today: NaiveDate,
) -> (NaiveDate, TargetDaySource) {
    let fallback = (today, TargetDaySource::TodayFallback);
    let Some((_, source)) = generation.and_then(|g| g.active_source()) else {
        return fallback;
    };
    let Ok(mut symbols) = market.read(universe) else {
        return fallback;
    };
    symbols.sort();
    symbols.truncate(TARGET_DAY_SAMPLE);
    let session = source.materialize_session();
    pick_target_day(
        symbols.iter().map(|symbol| {
            session
                .materialize_daily(symbol)
                .ok()
                .and_then(|batch| batch.bars.iter().map(|bar| bar.date).max())
        }),
        today,
    )
}

/// Resolve `plan_market_update`: canonical facts only, no persistence.
pub fn resolve_plan(
    market: &crate::sqlite_market::backend::MarketBackend,
    generation: Option<&super::generation::GenerationSource>,
    dataset: Dataset,
    end_date: Option<NaiveDate>,
    today: NaiveDate,
) -> Result<Plan, PlanError> {
    let target_day = end_date.unwrap_or(today);
    if target_day > today {
        return Err(PlanError::Invalid(
            "end_date must not be later than today".into(),
        ));
    }
    let generation_id = generation.and_then(|g| g.active_verified_id());
    let mut plan = market
        .read(|c| match dataset {
            Dataset::Daily => resolve_daily_on(c, target_day, today),
            Dataset::Yearly => resolve_yearly_on(c, target_day, today),
            Dataset::Catalog
            | Dataset::Index
            | Dataset::AdjustmentEvents
            | Dataset::Calendar
            | Dataset::SecurityMaster
            | Dataset::SecurityNames
            | Dataset::CapitalEvents
            | Dataset::RiskWarnings
            | Dataset::Suspensions
            | Dataset::PriceLimitRules => Err(iinvalid(
                "plan_market_update supports market.daily and market.yearly",
            )),
        })
        .map_err(|e| PlanError::Storage(e.to_string()))?;
    plan.generation_id = generation_id;
    plan.target_day = target_day;
    plan.sort();
    Ok(plan)
}

/// Re-resolve for `run_market_update`: the plan plus the in-flight update
/// scan, inside ONE read snapshot so the digest comparison and the
/// concurrency check describe the same state.
pub fn resolve_run(
    market: &crate::sqlite_market::backend::MarketBackend,
    generation: Option<&super::generation::GenerationSource>,
    dataset: Dataset,
    target_day: NaiveDate,
    today: NaiveDate,
) -> Result<RunResolution, PlanError> {
    if target_day > today {
        return Err(PlanError::Invalid(
            "target_day must not be later than today".into(),
        ));
    }
    let generation_id = generation.and_then(|g| g.active_verified_id());
    let mut out = market
        .read(|c| {
            let plan = match dataset {
                Dataset::Daily => resolve_daily_on(c, target_day, today),
                Dataset::Yearly => resolve_yearly_on(c, target_day, today),
                Dataset::Catalog
                | Dataset::Index
                | Dataset::AdjustmentEvents
                | Dataset::Calendar
                | Dataset::SecurityMaster
                | Dataset::SecurityNames
                | Dataset::CapitalEvents
                | Dataset::RiskWarnings
                | Dataset::Suspensions
                | Dataset::PriceLimitRules => Err(iinvalid(
                    "run_market_update supports market.daily and market.yearly",
                )),
            }?;
            let inflight = inflight_updates(c)?;
            Ok((plan, inflight))
        })
        .map_err(|e| PlanError::Storage(e.to_string()))?;
    out.0.generation_id = generation_id;
    out.0.target_day = target_day;
    out.0.sort();
    Ok(RunResolution {
        plan: out.0,
        inflight: out.1,
    })
}

/// `market.daily` plan: catalog∩collected universe, per-entity class.
fn resolve_daily_on(
    c: &Connection,
    target_day: NaiveDate,
    today: NaiveDate,
) -> Result<Plan, crate::market_storage::Error> {
    let universe = universe(c)?;
    let mut items = vec![];
    let mut ineligible = vec![];
    for symbol in &universe {
        let scope = Scope {
            symbol: symbol.clone(),
            period: Period::Day,
        };
        let Some(head_id) =
            head(c, &scope).map_err(|e| istorage(format!("daily head read: {e}")))?
        else {
            ineligible.push(PlanIneligible {
                entity: symbol.clone(),
                reason: "absent".into(),
            });
            continue;
        };
        let Some((base_integrity_sha256, first_day, last_day)) = daily_base(c, symbol, &head_id)?
        else {
            ineligible.push(PlanIneligible {
                entity: symbol.clone(),
                reason: "head_unreadable".into(),
            });
            continue;
        };
        if first_day > last_day {
            ineligible.push(PlanIneligible {
                entity: symbol.clone(),
                reason: "invalid_base_dates".into(),
            });
            continue;
        }
        if last_day > today {
            ineligible.push(PlanIneligible {
                entity: symbol.clone(),
                reason: "future_data".into(),
            });
            continue;
        }
        if last_day >= target_day {
            ineligible.push(PlanIneligible {
                entity: symbol.clone(),
                reason: "already_current".into(),
            });
            continue;
        }
        let mut proven_gap_ranges = vec![];
        let mut rejected = false;
        for g in crate::sqlite_market::gaps::read_open_gaps(c, symbol, Period::Day)
            .map_err(|e| istorage(format!("open gap read: {e}")))?
        {
            if g.end_date > target_day {
                ineligible.push(PlanIneligible {
                    entity: symbol.clone(),
                    reason: "gap_beyond_target".into(),
                });
                rejected = true;
                break;
            }
            proven_gap_ranges.push(PlanGap {
                gap_id: g.gap_id,
                start_date: g.start_date,
                end_date: g.end_date,
                evidence_digest: g.evidence_digest,
            });
        }
        if rejected {
            continue;
        }
        items.push(PlanItem::Daily(DailyPlanItem {
            entity: symbol.clone(),
            base_version_id: head_id,
            base_integrity_sha256,
            refresh_range: PlanRange {
                start_date: first_day,
                end_date: target_day,
            },
            proven_gap_ranges,
        }));
    }
    Ok(Plan {
        dataset: Dataset::Daily,
        generation_id: None,
        target_day,
        items,
        ineligible,
    })
}

/// Current-catalog∩collected universe; an empty universe resolves to an
/// empty plan (a fact, not a failure).
fn universe(c: &Connection) -> Result<Vec<String>, crate::market_storage::Error> {
    match super::default_selection::resolve_default_daily_on(c) {
        Ok(selection) => Ok(selection.securities),
        Err(super::default_selection::SelectionError::EmptySelection) => Ok(vec![]),
        Err(super::default_selection::SelectionError::Invalid(m)) => {
            Err(iinvalid(format!("selection: {m}")))
        }
    }
}

/// Per-entity published daily head facts: integrity digest and day bounds.
/// `Ok(None)` means the row exists but is inconsistent/corrupt — the
/// caller classifies it as ineligible rather than failing the whole plan.
fn daily_base(
    c: &Connection,
    symbol: &str,
    head_id: &str,
) -> Result<Option<(String, NaiveDate, NaiveDate)>, crate::market_storage::Error> {
    if is_latest_only_schema(c).map_err(|e| istorage(format!("schema probe: {e}")))? {
        let row = match series::read_series(c, symbol, Period::Day) {
            Ok(row) => row,
            Err(crate::market_storage::Error::Integrity(_)) => return Ok(None),
            Err(e) => return Err(istorage(format!("daily series read: {e}"))),
        };
        let Some(row) = row else { return Ok(None) };
        if row.version_id != head_id {
            return Ok(None);
        }
        let first = crate::compact_market::epoch_date(row.first_day)
            .map_err(|e| istorage(format!("base first_day: {e}")))?;
        let last = crate::compact_market::epoch_date(row.last_day)
            .map_err(|e| istorage(format!("base last_day: {e}")))?;
        return Ok(Some((row.version_id.clone(), first, last)));
    }
    let base = match crate::sqlite_market::read_record(c, head_id, true, false) {
        Ok(record) => record,
        Err(crate::market_storage::Error::Integrity(_)) => return Ok(None),
        Err(e) => return Err(istorage(format!("daily head read: {e}"))),
    };
    if base.version().symbol != symbol
        || base.version().period != Period::Day
        || base.version().version_id != head_id
    {
        return Ok(None);
    }
    let digest = base.raw()["integrity_sha256"]
        .as_str()
        .ok_or_else(|| istorage(format!("base digest missing for {symbol}")))?
        .to_owned();
    let (Some(first), Some(last)) = (
        base.version().records.first().map(|b| b.date),
        base.version().records.last().map(|b| b.date),
    ) else {
        return Ok(None);
    };
    Ok(Some((digest, first, last)))
}

/// `market.yearly` plan: same universe; requires the latest-only schema
/// (yearly stale classification is series/lineage-based).
fn resolve_yearly_on(
    c: &Connection,
    target_day: NaiveDate,
    _today: NaiveDate,
) -> Result<Plan, crate::market_storage::Error> {
    if !is_latest_only_schema(c).map_err(|e| istorage(format!("schema probe: {e}")))? {
        return Err(iinvalid(
            "plan_market_update market.yearly requires the latest-only market schema",
        ));
    }
    let universe = universe(c)?;
    let mut items = vec![];
    let mut ineligible = vec![];
    for symbol in &universe {
        let daily = match series::read_series(c, symbol, Period::Day) {
            Ok(row) => row,
            Err(crate::market_storage::Error::Integrity(_)) => {
                ineligible.push(PlanIneligible {
                    entity: symbol.clone(),
                    reason: "head_unreadable".into(),
                });
                continue;
            }
            Err(e) => return Err(istorage(format!("daily series read: {e}"))),
        };
        let Some(daily) = daily else {
            ineligible.push(PlanIneligible {
                entity: symbol.clone(),
                reason: "no_daily_base".into(),
            });
            continue;
        };
        let years: std::collections::BTreeSet<i32> =
            series::years_in_range(c, daily.series_key, daily.first_day, daily.last_day)
                .map_err(|e| istorage(format!("year presence read: {e}")))?
                .into_iter()
                .filter(|y| *y <= target_day.year())
                .collect();
        let yearly = series::read_series(c, symbol, Period::Year)
            .map_err(|e| istorage(format!("yearly series read: {e}")))?;
        match yearly {
            None => {
                if years.is_empty() {
                    ineligible.push(PlanIneligible {
                        entity: symbol.clone(),
                        reason: "no_derivable_years".into(),
                    });
                    continue;
                }
                items.push(PlanItem::Yearly(YearlyPlanItem {
                    entity: symbol.clone(),
                    base_daily_version_id: daily.version_id.clone(),
                    base_integrity_sha256: daily.version_id.clone(),
                    years: years.iter().copied().collect(),
                    expected_yearly_head: None,
                    comparison: None,
                }));
            }
            Some(yearly) => {
                let state = match series::yearly_state(c, &yearly) {
                    Ok(state) => state,
                    Err(crate::market_storage::Error::Integrity(_)) => {
                        ineligible.push(PlanIneligible {
                            entity: symbol.clone(),
                            reason: "integrity_failure".into(),
                        });
                        continue;
                    }
                    Err(e) => return Err(istorage(format!("yearly state: {e}"))),
                };
                match state {
                    series::YearlyState::Fresh => ineligible.push(PlanIneligible {
                        entity: symbol.clone(),
                        reason: "already_current".into(),
                    }),
                    series::YearlyState::Behind { .. } => {
                        if years.is_empty() {
                            ineligible.push(PlanIneligible {
                                entity: symbol.clone(),
                                reason: "no_derivable_years".into(),
                            });
                            continue;
                        }
                        items.push(PlanItem::Yearly(YearlyPlanItem {
                            entity: symbol.clone(),
                            base_daily_version_id: daily.version_id.clone(),
                            base_integrity_sha256: daily.version_id.clone(),
                            years: years.iter().copied().collect(),
                            expected_yearly_head: Some(yearly.version_id.clone()),
                            comparison: Some(PlanComparison {
                                version_id: yearly.version_id.clone(),
                                integrity_sha256: yearly.version_id.clone(),
                            }),
                        }));
                    }
                    series::YearlyState::Invalid(reason) => ineligible.push(PlanIneligible {
                        entity: symbol.clone(),
                        reason: match reason {
                            series::YearlyInvalidReason::DailyReplaced => {
                                "daily_replaced_since_derivation"
                            }
                            series::YearlyInvalidReason::SemanticsChanged => {
                                "value_semantics_changed"
                            }
                        }
                        .into(),
                    }),
                }
            }
        }
    }
    Ok(Plan {
        dataset: Dataset::Yearly,
        generation_id: None,
        target_day,
        items,
        ineligible,
    })
}

// ---------------------------------------------------------------------------
// run contract
// ---------------------------------------------------------------------------

/// Accepted `run_market_update` intent — a compact digest-bound
/// descriptor. The item list deliberately stays in the task's frozen
/// work, not the request binding: a whole-market payload decoded into
/// `command_json` is what made the acceptance spike ~950MB at 5.2k
/// members (TASK-DEVIN-PLANRUN-MEMORY).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunIntent {
    contract_version: String,
    plan_id: String,
    dataset: String,
    generation_id: Option<String>,
    target_day: NaiveDate,
    item_count: u64,
}
/// Parts the `run_market_update` handler feeds into streamed acceptance:
/// per-item conversion happens lazily so the frozen-work tree is never
/// materialized whole.
pub struct RunWorkParts {
    /// Digest every item's `selection_evidence` marker must carry — the
    /// recomputed `plan_id`, proven to cover these items.
    pub plan_id: String,
    /// `source_generation_id` stamped onto every frozen input.
    pub generation: Value,
    /// The occupied update scope set (one `[market,symbol,period]` per
    /// item) — written to `scope_occupancy` by acceptance.
    pub scopes: std::collections::BTreeSet<String>,
    /// Shared evidence persisted once in `frozen_work_json`; `TaskSummary`
    /// reads `plan_id`/`counts` back out of it.
    pub shared_evidence: Value,
    /// Executable plan members — streamed through `item_spec`.
    pub items: Vec<PlanItem>,
}
fn item_scope(item: &PlanItem) -> String {
    match item {
        PlanItem::Daily(i) => crate::task_control::legacy::scope_id(&i.entity, "day"),
        PlanItem::Yearly(i) => crate::task_control::legacy::scope_id(&i.entity, "year"),
    }
}
/// Frozen `ItemSpec` for one plan member — field-for-field the same shape
/// the previous bulk `resolve` produced.
pub fn item_spec(item: PlanItem, plan_id: &str, generation: &Value) -> ItemSpec {
    let (scope, entity, period, frozen) = match item {
        PlanItem::Daily(i) => {
            let mut value = json!({
                "selection_evidence": {"evidence_digest": plan_id},
                "base_version_id": i.base_version_id,
                "base_integrity_sha256": i.base_integrity_sha256,
                "refresh_range": {
                    "start_date": i.refresh_range.start_date,
                    "end_date": i.refresh_range.end_date,
                },
                "proven_gap_ranges": i.proven_gap_ranges.iter().map(|g| json!({
                    "gap_id": g.gap_id,
                    "range": {"start_date": g.start_date, "end_date": g.end_date},
                    "evidence_digest": g.evidence_digest,
                })).collect::<Vec<_>>(),
            });
            value[SOURCE_GENERATION_KEY] = generation.clone();
            (
                crate::task_control::legacy::scope_id(&i.entity, "day"),
                i.entity,
                "day",
                value,
            )
        }
        PlanItem::Yearly(i) => {
            let intent = crate::market_storage::PublishIntent::CurrentHead {
                expected_daily_head: i.base_daily_version_id.clone(),
                expected_yearly_head: i.expected_yearly_head.clone(),
            };
            let mut value = crate::yearly_publication::tasks::frozen(
                &i.base_daily_version_id,
                &i.base_integrity_sha256,
                &i.years
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                &intent,
            );
            value["comparison"] = i
                .comparison
                .as_ref()
                .map(|p| {
                    json!({
                        "version_id": p.version_id,
                        "integrity_sha256": p.integrity_sha256,
                    })
                })
                .unwrap_or(Value::Null);
            value[SOURCE_GENERATION_KEY] = generation.clone();
            (
                crate::task_control::legacy::scope_id(&i.entity, "year"),
                i.entity,
                "year",
                value,
            )
        }
    };
    ItemSpec {
        item_id: scope.clone(),
        scope_keys: [scope].into(),
        target: json!({
            "symbol": entity,
            "period": period,
        }),
        progress_unit: "security_period".into(),
        frozen_inputs: frozen,
    }
}
/// Contract validating the compact run intent. Task materialization
/// happens through `TaskControl::accept_streamed` — the plan's items
/// never re-enter the intent payload — so `resolve` refuses; the contract
/// exists so requests/snapshots remain decodable on the read path.
pub struct PlanRunContract {
    pub dataset: &'static str,
}
impl Contract for PlanRunContract {
    fn key(&self) -> CommandKey {
        CommandKey {
            operation: "update_data".into(),
            dataset: self.dataset.into(),
            contract_version: RUN_CONTRACT_VERSION.into(),
        }
    }
    fn normalize(&self, raw: Value) -> Result<Value, Error> {
        let input: RunIntent = serde_json::from_value(raw).map_err(invalid)?;
        if input.dataset != self.dataset {
            return Err(invalid("plan-run dataset/key mismatch"));
        }
        if input.contract_version != PLAN_CONTRACT_VERSION {
            return Err(invalid("plan-run plan contract mismatch"));
        }
        if !valid_plan_id(&input.plan_id) {
            return Err(invalid("plan_id must be 64 lowercase hex"));
        }
        if input.item_count == 0 {
            return Err(invalid("plan-run item_count must be positive"));
        }
        serde_json::to_value(input).map_err(invalid)
    }
    fn resolve(&self, _raw: &Value) -> Result<FrozenWork, Error> {
        Err(invalid(
            "plan-run tasks admit through accept_streamed, not contract resolve",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn target_day_follows_the_generation_and_never_exceeds_today() {
        let today = d(2026, 10, 14);
        // The generation ends at 10-13: that is the target, not today.
        assert_eq!(
            pick_target_day([Some(d(2026, 10, 12)), None, Some(d(2026, 10, 13))], today),
            (d(2026, 10, 13), TargetDaySource::GenerationLatest)
        );
        // A generation claiming a later day than today is capped.
        assert_eq!(
            pick_target_day([Some(d(2026, 10, 20))], today),
            (today, TargetDaySource::GenerationLatest)
        );
        // Nothing readable: the previous behavior (today), reported as such.
        assert_eq!(
            pick_target_day([None, None], today),
            (today, TargetDaySource::TodayFallback)
        );
        assert_eq!(
            pick_target_day(std::iter::empty(), today),
            (today, TargetDaySource::TodayFallback)
        );
    }

    #[test]
    fn target_day_source_names_are_stable() {
        assert_eq!(TargetDaySource::Explicit.as_str(), "explicit");
        assert_eq!(
            TargetDaySource::GenerationLatest.as_str(),
            "generation_latest"
        );
        assert_eq!(TargetDaySource::TodayFallback.as_str(), "today_fallback");
    }

    /// Byte-format lock: `plan_id` must stay identical for the same
    /// canonical plan across refactors — the digest is what clients carry
    /// between `plan` and `run`. The golden value was computed from the
    /// canonical JSON (`{"dataset":"market.daily",...}`) produced by the
    /// field order declared above.
    #[test]
    fn plan_id_matches_canonical_byte_format() {
        let plan = Plan {
            dataset: Dataset::Daily,
            generation_id: Some("gen-x".into()),
            target_day: NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
            items: vec![PlanItem::Daily(DailyPlanItem {
                entity: "sh.600000".into(),
                base_version_id: "v-1".into(),
                base_integrity_sha256: "aa".repeat(32),
                refresh_range: PlanRange {
                    start_date: NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
                    end_date: NaiveDate::from_ymd_opt(2026, 10, 12).unwrap(),
                },
                proven_gap_ranges: vec![PlanGap {
                    gap_id: "gap-1".into(),
                    start_date: NaiveDate::from_ymd_opt(2026, 10, 8).unwrap(),
                    end_date: NaiveDate::from_ymd_opt(2026, 10, 8).unwrap(),
                    evidence_digest: "bb".repeat(32),
                }],
            })],
            ineligible: vec![PlanIneligible {
                entity: "sz.000001".into(),
                reason: "already_current".into(),
            }],
        };
        assert_eq!(
            plan.plan_id().unwrap(),
            "749f17267b83f15bf3c3c4c51b5c8a2336eaf84385cdfaf850fe0605ea104acf"
        );
    }

    fn inelig(entity: &str, reason: &str) -> PlanIneligible {
        PlanIneligible {
            entity: entity.into(),
            reason: reason.into(),
        }
    }

    fn plan_with_ineligible(ineligible: Vec<PlanIneligible>) -> Plan {
        Plan {
            dataset: Dataset::Yearly,
            generation_id: None,
            target_day: d(2026, 10, 14),
            items: vec![],
            ineligible,
        }
    }

    /// The view list is filtered with the same口径 as `ineligible_reasons`:
    /// `already_current`/`absent`/`no_daily_base` stay out, the rest appear
    /// by (entity, reason).
    #[test]
    fn view_ineligible_entities_match_the_reason_filter() {
        let mut plan = plan_with_ineligible(vec![
            inelig("sh.600000", "already_current"),
            inelig("sh.600001", "absent"),
            inelig("sh.600002", "no_daily_base"),
            inelig("sz.000001", "daily_replaced_since_derivation"),
            inelig("sz.000002", "value_semantics_changed"),
            inelig("sz.000003", "head_unreadable"),
            inelig("sz.000004", "value_semantics_changed"),
        ]);
        plan.sort();
        let view = plan.view().unwrap();
        let named: Vec<(String, String)> = view
            .ineligible_entities
            .iter()
            .map(|e| (e.entity.clone(), e.reason.clone()))
            .collect();
        assert_eq!(
            named,
            vec![
                (
                    "sz.000001".to_string(),
                    "daily_replaced_since_derivation".into()
                ),
                ("sz.000002".to_string(), "value_semantics_changed".into()),
                ("sz.000003".to_string(), "head_unreadable".into()),
                ("sz.000004".to_string(), "value_semantics_changed".into()),
            ]
        );
        assert!(!view.ineligible_entities_truncated);
        let mut view_reasons: Vec<&str> = view
            .ineligible_entities
            .iter()
            .map(|e| e.reason.as_str())
            .collect();
        view_reasons.sort_unstable();
        view_reasons.dedup();
        let aggregate_reasons: Vec<&str> = view
            .ineligible_reasons
            .iter()
            .map(|r| r.reason.as_str())
            .collect();
        assert_eq!(view_reasons, aggregate_reasons);
        assert_eq!(
            view.ineligible_reasons
                .iter()
                .map(|r| (r.reason.as_str(), r.count))
                .collect::<Vec<_>>(),
            vec![
                ("daily_replaced_since_derivation", 1),
                ("head_unreadable", 1),
                ("value_semantics_changed", 2),
            ]
        );
    }

    /// 501 routing-eligible entities yield 500 named plus the truncation
    /// flag; exactly 500 does not set it.
    #[test]
    fn view_truncates_ineligible_entities_at_the_limit() {
        let over: Vec<PlanIneligible> = (0..PLAN_INELIGIBLE_ENTITY_LIMIT + 1)
            .map(|n| inelig(&format!("sh.{n:06}"), "value_semantics_changed"))
            .collect();
        let mut plan = plan_with_ineligible(over);
        plan.sort();
        let view = plan.view().unwrap();
        assert_eq!(view.ineligible_entities.len(), PLAN_INELIGIBLE_ENTITY_LIMIT);
        assert!(view.ineligible_entities_truncated);

        let at: Vec<PlanIneligible> = (0..PLAN_INELIGIBLE_ENTITY_LIMIT)
            .map(|n| inelig(&format!("sh.{n:06}"), "value_semantics_changed"))
            .collect();
        let mut plan = plan_with_ineligible(at);
        plan.sort();
        let view = plan.view().unwrap();
        assert_eq!(view.ineligible_entities.len(), PLAN_INELIGIBLE_ENTITY_LIMIT);
        assert!(!view.ineligible_entities_truncated);
    }

    /// The new view fields are diagnostics: `plan_id` covers only the
    /// canonical keys, so populating the list cannot move the digest.
    #[test]
    fn plan_id_does_not_depend_on_the_view_entity_list() {
        let plan = plan_with_ineligible(vec![
            inelig("sh.600000", "value_semantics_changed"),
            inelig("sh.600001", "already_current"),
        ]);
        let id = plan.plan_id().unwrap();
        assert_eq!(plan.view().unwrap().plan_id, id);
        let canonical = serde_json::to_value(plan.canonical()).unwrap();
        let keys: std::collections::BTreeSet<&str> = canonical
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "dataset",
                "contract_version",
                "generation_id",
                "target_day",
                "items",
                "ineligible",
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
        );
    }

    /// `skip_serializing_if` keeps the response compact: an empty list emits
    /// neither field, and an untruncated list omits only the flag.
    #[test]
    fn view_omits_the_entity_fields_when_empty() {
        let plan = plan_with_ineligible(vec![
            inelig("sh.600000", "already_current"),
            inelig("sh.600001", "absent"),
            inelig("sh.600002", "no_daily_base"),
        ]);
        let view = plan.view().unwrap();
        assert!(view.ineligible_entities.is_empty());
        assert!(!view.ineligible_entities_truncated);
        let value = serde_json::to_value(view).unwrap();
        let object = value.as_object().unwrap();
        assert!(!object.contains_key("ineligible_entities"), "{value}");
        assert!(
            !object.contains_key("ineligible_entities_truncated"),
            "{value}"
        );

        let plan = plan_with_ineligible(vec![inelig("sh.600000", "head_unreadable")]);
        let value = serde_json::to_value(plan.view().unwrap()).unwrap();
        assert_eq!(value["ineligible_entities"][0]["entity"], "sh.600000");
        assert!(
            !value
                .as_object()
                .unwrap()
                .contains_key("ineligible_entities_truncated"),
            "{value}"
        );

        let over: Vec<PlanIneligible> = (0..PLAN_INELIGIBLE_ENTITY_LIMIT + 1)
            .map(|n| inelig(&format!("sh.{n:06}"), "value_semantics_changed"))
            .collect();
        let value = serde_json::to_value(plan_with_ineligible(over).view().unwrap()).unwrap();
        assert_eq!(
            value["ineligible_entities_truncated"],
            serde_json::json!(true),
            "{value}"
        );
    }
}
